# ReCorize

ReCorize is an early-stage Rust recovery tool for Arch Linux and CachyOS. It is intended to save selected system and home-directory files into numbered recovery generations, record package and repository metadata, and seal saved generations with Linux's immutable inode flag.

## Current status

The current CLI supports initialization, snapshot creation, listing, integrity checks, file restoration, exporting a generation, package repair, a text recovery menu, and a guarded reinstall bootstrap. Reinstall installs the saved package selection into a separately mounted, nearly empty target and restores the saved files. It never partitions or formats disks. Bootloader installation and configuration remain manual. Do not rely on this version as the only copy of important data.

Immutable sealing requires Linux, root privileges with `CAP_LINUX_IMMUTABLE`, and a filesystem that supports the immutable flag (such as ext4, btrfs, xfs, or f2fs). The recovery directory should be on a separate device or otherwise protected from disk failure; an immutable flag does not protect against physical failure or a privileged attacker.

## Build

Install Rust 1.74 or newer, then run:

```sh
cargo build --release
```

The executable is `target/release/recorize`.

## Use

Run initialization as root on the target Linux system:

```sh
sudo recorize init
```

This creates `/etc/recorize/config.toml` with default watches for `/etc` before package transactions and `/home` for manual saves. It also initializes `/var/lib/recorize/recovery` and checks that immutable sealing works. Review and adjust the config before relying on automatic saves.

Create a snapshot, or preview its approximate size first:

```sh
sudo recorize save --dry-run
sudo recorize save
sudo recorize save --scope transaction
```

List and verify generations:

```sh
sudo recorize list
sudo recorize verify       # latest generation
sudo recorize verify 1     # a specific generation
sudo recorize restore 1 --path /home/alice/Documents
sudo recorize restore 1 --sysroot /mnt --path /etc/hostname
sudo recorize export 1 /run/media/backup
sudo recorize repair
sudo recorize recovery
sudo recorize reinstall --target /mnt --confirm REINSTALL
```

Restore defaults to the latest generation when no ID is supplied and skips existing files. Use `--overwrite` only when you intend to replace them. For an offline installation, mount its root and pass the mount point with `--sysroot`. Export writes a `recorize-generation-NNNNNNNN` directory containing the generation data and manifest; the exported copy does not inherit filesystem immutable flags.

An alternate config can be selected with `--config PATH` before the subcommand, for example `recorize --config ./recorize.toml config`.

The supplied pacman hook requests a transaction-scope save before package changes. The systemd unit starts the text recovery menu when `recorize-recovery.target` is selected; the timer provides a daily save when installed and enabled by the system administrator. These integration files are templates and are not installed automatically by the current CLI. Booting into the recovery target still needs to be configured by the system administrator.

Reinstall requires `pacstrap` and `genfstab` from `arch-install-scripts`. Mount the target root and any separate boot or EFI partitions first. The target must be mounted separately from the running root and contain no files other than `lost+found` and mount scaffolding. ReCorize verifies the source generation before starting and requires the literal confirmation `REINSTALL`. Packages that are absent from the saved repositories (including AUR packages) may cause `pacstrap` to stop after partially populating the target. Configure a bootloader manually before rebooting into the new installation.

## Recovery data layout

Each generation is stored under:

```text
/var/lib/recorize/recovery/
├── LATEST
└── generations/
    └── 00000001/
        ├── files/       # copies mirroring the saved absolute paths
        └── manifest.json
```

The manifest records file hashes, metadata, package versions, repository configuration, and warnings. Each completed generation is sealed after writing.

## Configuration

`watch` entries accept `manual`, `transaction`, or `always` schedules. For example:

```toml
[[watch]]
path = "/home"
on = "manual"

[[watch]]
path = "/etc"
on = "transaction"
```

`exclude` accepts path globs. Default exclusions skip cache and trash directories. ReCorize does not follow watched-root symlinks and skips special files such as sockets and device nodes.
