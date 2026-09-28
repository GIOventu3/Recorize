mod config;
mod error;
mod exclude;
mod immutable;
mod manifest;
mod os_release;
mod pacman_conf;
mod qkk;
mod recovery;
mod snapshot;
mod store;
mod util;

use clap::{Parser, Subcommand};
use config::{Config, SaveScope, CONFIRM_REINSTALL, CONFIG_SYSTEM_PATH};
use error::{Error, Result};
use exclude::Exclude;
use immutable::VolumeSeal;
use manifest::{parse_explicit, parse_pacman_q, StoredText, SystemMeta};
use os_release::parse_os_release;
use pacman_conf::parse_pacman_conf;
use std::fs;
use std::path::{Path, PathBuf};
use std::io::{self, Write};
use std::process::Command;
use store::{SaveRequest, VerifyReport};
use util::{hostname, now_unix, path_key, valid_package_name};

#[derive(Debug, Parser)]
#[command(name = "recorize", version, about = "Immutable recovery snapshots for Arch and CachyOS")]
struct Cli {
    /// Use an alternate TOML configuration file.
    #[arg(long, global = true, default_value = CONFIG_SYSTEM_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create the default configuration and initialize the immutable recovery store.
    Init,
    /// Save watched paths into a new immutable recovery generation.
    Save {
        /// Save paths watched for manual and transaction backups.
        #[arg(long, default_value = "all", value_parser = ["all", "transaction"])]
        scope: String,
        /// Show the estimated file count and size without copying data.
        #[arg(long)]
        dry_run: bool,
        /// Suppress per-file warning output (used by the pacman hook).
        #[arg(long, short)]
        quiet: bool,
    },
    /// List saved recovery generations.
    List,
    /// Verify a generation's file hashes and immutable flag.
    Verify {
        /// Generation number; defaults to the latest generation.
        id: Option<u64>,
    },
    /// Restore files from a generation, preserving existing paths unless --overwrite is set.
    Restore {
        /// Generation number; defaults to the latest generation.
        id: Option<u64>,
        /// Restore only this absolute path. Can be repeated; directories include their descendants.
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Root directory of an offline Linux installation. Defaults to the current root.
        #[arg(long, default_value = "/")]
        sysroot: PathBuf,
        /// Replace existing files and restore saved directory metadata.
        #[arg(long)]
        overwrite: bool,
    },
    /// Export an unchanged generation to another mounted directory.
    Export {
        /// Generation number; defaults to the latest generation.
        id: Option<u64>,
        /// Mounted destination directory.
        destination: PathBuf,
    },
    /// Open the text recovery menu used by the supplied systemd recovery unit.
    Recovery,
    /// Check installed package files and optionally reinstall affected packages.
    Repair,
    /// Reinstall a generation's package set and restore its saved files to a mounted empty root.
    Reinstall {
        /// Generation number; defaults to the latest generation.
        id: Option<u64>,
        /// Separate mounted root filesystem. ReCorize does not format it.
        #[arg(long)]
        target: PathBuf,
        /// Required confirmation token: REINSTALL.
        #[arg(long)]
        confirm: String,
    },
    /// Show the current configuration path and recovery directory.
    Config,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("recorize: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => initialize(&cli.config),
        Commands::Save {
            scope,
            dry_run,
            quiet,
        } => save(&cli.config, &scope, dry_run, quiet),
        Commands::List => list(&cli.config),
        Commands::Verify { id } => verify(&cli.config, id),
        Commands::Restore {
            id,
            paths,
            sysroot,
            overwrite,
        } => restore(&cli.config, id, &paths, &sysroot, overwrite),
        Commands::Export { id, destination } => export(&cli.config, id, &destination),
        Commands::Recovery => recovery_menu(&cli.config),
        Commands::Repair => repair_packages(),
        Commands::Reinstall { id, target, confirm } => {
            reinstall(&cli.config, id, &target, &confirm)
        }
        Commands::Config => show_config(&cli.config),
    }
}

fn initialize(config_path: &Path) -> Result<()> {
    if config_path.exists() {
        return Err(Error::msg(format!(
            "{} already exists; move it aside before initializing",
            config_path.display()
        )));
    }
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|source| error::io_at(parent, source))?;
    }
    fs::write(config_path, config::default_config_toml())
        .map_err(|source| error::io_at(config_path, source))?;
    let config = Config::load(config_path)?;
    let mut sealer = system_sealer();
    if let Err(err) = store::init_store(Path::new(&config.recovery_dir), &mut sealer) {
        let _ = fs::remove_file(config_path);
        return Err(err);
    }
    println!("Configuration created at {}", config_path.display());
    println!("Immutable recovery store initialized at {}", config.recovery_dir);
    println!("Edit the configuration to choose paths to protect, then run `recorize save`.");
    Ok(())
}

fn load_config(path: &Path) -> Result<Config> {
    if path.exists() {
        Config::load(path)
    } else if path == Path::new(CONFIG_SYSTEM_PATH) {
        Err(Error::msg(format!(
            "{} does not exist; run `recorize init` first",
            path.display()
        )))
    } else {
        Config::load(path)
    }
}

fn show_config(path: &Path) -> Result<()> {
    let config = load_config(path)?;
    println!("Configuration: {}", path.display());
    println!("Recovery directory: {}", config.recovery_dir);
    println!("Watched paths:");
    for watch in config.watch {
        println!("  {} ({})", watch.path, watch.on);
    }
    Ok(())
}

fn save(config_path: &Path, scope_name: &str, dry_run: bool, quiet: bool) -> Result<()> {
    let config = load_config(config_path)?;
    let scope = SaveScope::parse(scope_name)?;
    let watches: Vec<PathBuf> = config
        .watches_for(scope)
        .into_iter()
        .map(|watch| PathBuf::from(&watch.path))
        .collect();
    if watches.is_empty() {
        return Err(Error::msg("no configured paths match this save scope"));
    }

    let mut exclude = Exclude {
        globs: config.exclude.clone(),
        prefixes: Vec::new(),
        default_components: config.default_excludes,
    };
    exclude.prefixes.push(path_key(Path::new(&config.recovery_dir)));
    let config_toml = fs::read_to_string(config_path).unwrap_or_default();
    let request = SaveRequest {
        recovery_dir: PathBuf::from(&config.recovery_dir),
        watches,
        exclude,
        scope_label: scope_name.to_string(),
        dry_run,
        meta: collect_system_meta(&config, &mut Vec::new()),
        hostname: hostname(),
        now_unix: now_unix(),
        config_toml,
    };
    let mut sealer = system_sealer();
    let report = store::save_with(&request, &mut sealer)?;
    if report.dry_run {
        println!(
            "Dry run: {} entries, {} bytes estimated; next generation would be {:08}",
            report.files, report.bytes, report.id
        );
    } else {
        println!(
            "Saved generation {:08}: {} entries, {} bytes",
            report.id, report.files, report.bytes
        );
    }
    if !quiet {
        for warning in report.warnings {
            eprintln!("warning: {warning}");
        }
    }
    Ok(())
}

fn list(config_path: &Path) -> Result<()> {
    let config = load_config(config_path)?;
    let recovery = Path::new(&config.recovery_dir);
    let latest = store::latest_id(recovery)?;
    let ids = store::list_ids(recovery)?;
    if ids.is_empty() {
        println!("No recovery generations found in {}", recovery.display());
        return Ok(());
    }
    for id in ids {
        let manifest_path = store::generation_dir(recovery, id).join("manifest.json");
        match manifest::GenerationManifest::read(&manifest_path) {
            Ok(manifest) => println!(
                "{:08}{}  {} files  {}  scope={}  host={}",
                id,
                if latest == Some(id) { " (latest)" } else { "" },
                manifest.files.len(),
                manifest.created_unix,
                manifest.scope,
                manifest.hostname
            ),
            Err(err) => println!("{id:08}  manifest unavailable: {err}"),
        }
    }
    Ok(())
}

fn verify(config_path: &Path, requested_id: Option<u64>) -> Result<()> {
    let config = load_config(config_path)?;
    let recovery = Path::new(&config.recovery_dir);
    let id = match requested_id.or(store::latest_id(recovery)?) {
        Some(id) => id,
        None => return Err(Error::msg("there are no recovery generations to verify")),
    };
    let mut sealer = system_sealer();
    let report = store::verify_generation(recovery, id, &mut sealer)?;
    print_verify(&report);
    if report.problems.is_empty() {
        Ok(())
    } else {
        Err(Error::msg(format!("generation {id:08} has verification problems")))
    }
}

fn print_verify(report: &VerifyReport) {
    println!(
        "Generation {:08}: {}/{} checked files match; immutable={}",
        report.id,
        report.files_ok,
        report.files_ok + report.problems.len(),
        report.sealed
    );
    for problem in &report.problems {
        eprintln!("problem: {problem}");
    }
}

fn restore(
    config_path: &Path,
    requested_id: Option<u64>,
    paths: &[String],
    sysroot: &Path,
    overwrite: bool,
) -> Result<()> {
    let config = load_config(config_path)?;
    let recovery = Path::new(&config.recovery_dir);
    let id = requested_id
        .or(store::latest_id(recovery)?)
        .ok_or_else(|| Error::msg("there are no recovery generations to restore"))?;
    let report = recovery::restore_generation(recovery, id, sysroot, paths, overwrite)?;
    println!(
        "Generation {id:08}: restored {} entries, skipped {} existing entries",
        report.restored, report.skipped
    );
    Ok(())
}

fn export(config_path: &Path, requested_id: Option<u64>, destination: &Path) -> Result<()> {
    let config = load_config(config_path)?;
    let recovery = Path::new(&config.recovery_dir);
    let id = requested_id
        .or(store::latest_id(recovery)?)
        .ok_or_else(|| Error::msg("there are no recovery generations to export"))?;
    let exported = recovery::export_generation(recovery, id, destination)?;
    println!("Exported generation {id:08} to {}", exported.display());
    Ok(())
}

fn recovery_menu(config_path: &Path) -> Result<()> {
    loop {
        println!("\nReCorize Recovery");
        println!("1. List recovery generations");
        println!("2. Verify latest generation");
        println!("3. Restore files from latest generation");
        println!("4. Export latest generation to another mounted device");
        println!("5. Check and repair installed packages");
        println!("6. Reinstall from a generation into a mounted empty root");
        println!("q. Exit recovery menu");
        let result = match prompt("Choose an option: ")?.trim() {
            "1" => list(config_path),
            "2" => verify(config_path, None),
            "3" => menu_restore(config_path),
            "4" => menu_export(config_path),
            "5" => repair_packages(),
            "6" => menu_reinstall(config_path),
            "q" | "Q" => return Ok(()),
            _ => {
                println!("Choose one of the listed options.");
                Ok(())
            }
        };
        if let Err(err) = result {
            eprintln!("recovery action failed: {err}");
        }
    }
}

fn menu_restore(config_path: &Path) -> Result<()> {
    let path = prompt("Path to restore (blank for all saved paths): ")?;
    let path = path.trim();
    let paths = if path.is_empty() {
        Vec::new()
    } else {
        vec![path.to_string()]
    };
    let sysroot = prompt("Target system root [/]: ")?;
    let sysroot = if sysroot.trim().is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(sysroot.trim())
    };
    println!("Restore into {}. Existing files can be replaced.", sysroot.display());
    if prompt("Type RESTORE to continue: ")?.trim() != "RESTORE" {
        println!("Restore cancelled.");
        return Ok(());
    }
    restore(config_path, None, &paths, &sysroot, true)
}

fn menu_export(config_path: &Path) -> Result<()> {
    let destination = prompt("Mounted export destination: ")?;
    if destination.trim().is_empty() {
        println!("Export cancelled.");
        return Ok(());
    }
    export(config_path, None, Path::new(destination.trim()))
}

fn menu_reinstall(config_path: &Path) -> Result<()> {
    println!("Mount the new root filesystem and any separate /boot or EFI filesystems first.");
    println!("ReCorize will not partition or format disks; the target must be empty except for mount scaffolding.");
    let target = prompt("Mounted target root: ")?;
    if target.trim().is_empty() {
        println!("Reinstall cancelled.");
        return Ok(());
    }
    if prompt("Type REINSTALL to install packages and restore saved files: ")?.trim()
        != CONFIRM_REINSTALL
    {
        println!("Reinstall cancelled.");
        return Ok(());
    }
    reinstall(config_path, None, Path::new(target.trim()), CONFIRM_REINSTALL)
}

fn reinstall(
    config_path: &Path,
    requested_id: Option<u64>,
    target: &Path,
    confirmation: &str,
) -> Result<()> {
    if confirmation != CONFIRM_REINSTALL {
        return Err(Error::msg("reinstall requires --confirm REINSTALL"));
    }
    let config = load_config(config_path)?;
    let recovery = Path::new(&config.recovery_dir);
    let id = requested_id
        .or(store::latest_id(recovery)?)
        .ok_or_else(|| Error::msg("there are no recovery generations to reinstall from"))?;
    let mut sealer = system_sealer();
    let report = store::verify_generation(recovery, id, &mut sealer)?;
    if !report.problems.is_empty() {
        print_verify(&report);
        return Err(Error::msg(
            "reinstall stopped because the recovery generation did not verify",
        ));
    }
    println!("Verified {} saved entries in generation {id:08}.", report.files_ok);
    recovery::reinstall_generation(recovery, id, target)
}

fn repair_packages() -> Result<()> {
    let output = Command::new("pacman")
        .args(["-Qkk"])
        .output()
        .map_err(|source| error::io_at("pacman", source))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let packages: Vec<String> = qkk::packages_to_repair(&text)
        .into_iter()
        .filter(|name| valid_package_name(name))
        .collect();
    if packages.is_empty() {
        println!("No missing or modified package files were detected.");
        if !output.status.success() {
            return Err(Error::msg("pacman -Qkk did not complete successfully"));
        }
        return Ok(());
    }
    println!("Packages with missing or changed files:");
    for package in &packages {
        println!("  {package}");
    }
    println!("Repair uses `pacman -Syu` to avoid a partial system upgrade.");
    if prompt("Type REPAIR to synchronize and reinstall these packages: ")?.trim() != "REPAIR" {
        println!("Package repair cancelled.");
        return Ok(());
    }
    let mut command = Command::new("pacman");
    command.args(["-Syu"]).args(&packages);
    let status = command
        .status()
        .map_err(|source| error::io_at("pacman", source))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::msg(format!("pacman -Syu exited with {status}")))
    }
}

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush().map_err(|source| error::io_at("stdout", source))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|source| error::io_at("stdin", source))?;
    Ok(value)
}

fn collect_system_meta(config: &Config, warnings: &mut Vec<String>) -> SystemMeta {
    let mut meta = SystemMeta::default();
    meta.os_release_text = read_optional(Path::new(&config.os_release), warnings);
    meta.os = parse_os_release(&meta.os_release_text);
    meta.pacman_conf_text = read_optional(Path::new(&config.pacman_conf), warnings);
    let parsed = parse_pacman_conf(&meta.pacman_conf_text);
    meta.repos = parsed.repos;
    for repo in &meta.repos {
        for include in &repo.includes {
            let path = Path::new(include);
            if path.is_file() && !meta.mirrorlists.iter().any(|item| item.path == include) {
                meta.mirrorlists.push(StoredText {
                    path: include.clone(),
                    contents: read_optional(path, warnings),
                });
            }
        }
    }
    if let Some(output) = command_output("pacman", &["-Q"]) {
        meta.packages_all = parse_pacman_q(&output);
    } else {
        warnings.push("could not read installed package versions with pacman -Q".into());
    }
    if let Some(output) = command_output("pacman", &["-Qqe"]) {
        meta.packages_explicit = parse_explicit(&output);
    } else {
        warnings.push("could not read explicitly installed packages with pacman -Qqe".into());
    }
    meta.warnings = warnings.clone();
    meta
}

fn read_optional(path: &Path, warnings: &mut Vec<String>) -> String {
    match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            warnings.push(format!("could not read {}: {err}", path.display()));
            String::new()
        }
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "linux")]
fn system_sealer() -> impl VolumeSeal {
    immutable::LinuxSeal::default()
}

#[cfg(not(target_os = "linux"))]
fn system_sealer() -> impl VolumeSeal {
    UnsupportedSeal
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct UnsupportedSeal;

#[cfg(not(target_os = "linux"))]
impl VolumeSeal for UnsupportedSeal {
    fn supports_immutable(&self) -> bool { false }
    fn seal_inode(&mut self, _: &Path) -> Result<()> { Err(Error::msg("immutable sealing requires Linux")) }
    fn unseal_inode(&mut self, _: &Path) -> Result<()> { Err(Error::msg("immutable sealing requires Linux")) }
    fn seal_tree(&mut self, _: &Path) -> Result<()> { Err(Error::msg("immutable sealing requires Linux")) }
    fn unseal_tree(&mut self, _: &Path) -> Result<()> { Err(Error::msg("immutable sealing requires Linux")) }
    fn is_immutable(&mut self, _: &Path) -> Result<bool> { Err(Error::msg("immutable sealing requires Linux")) }
}
