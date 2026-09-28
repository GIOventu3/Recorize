# ReCorize (for CoraOS)

ReCorize is the native, early-stage system state backup and recovery tool designed for **CoraOS** (and compatible with Arch Linux / CachyOS). It captures user-selected configuration files and system paths into versioned recovery generations, logs installed package lists and repository metadata, and seals saved generations using Linux immutable inode flags (`chattr +i`) to prevent accidental deletion or tampering.

---

## Features & Current Status

The ReCorize CLI provides a complete recovery toolkit for CoraOS systems:

- **Snapshot Generation & Dry-Runs:** Calculate snapshot sizes before execution and trigger scope-aware system saves.
- **Immutable State Sealing:** Automatically seals completed generations using `CAP_LINUX_IMMUTABLE` on supported filesystems (`ext4`, `btrfs`, `xfs`, `f2fs`).
- **Granular Restoration:** Restore individual files or entire system configurations to your active system or an offline mount (`--sysroot`).
- **Package Repair & Bootstrapping:** Reinstall saved package selections into fresh target mounts using Arch install scripts (`pacstrap` / `genfstab`), preserving your environment across clean OS rebuilds.
- **Systemd & Pacman Integration:** Includes hook templates for automated transaction-scoped saves during `pacman` updates and a dedicated text-based emergency recovery target (`recorize-recovery.target`).

> **Note:** Immutable flags protect against unintended edits and software failures, but do not replace off-device hardware backups against physical disk failure. Bootloader setup during reinstallations remains a manual step.

---

## Build Requirements

Building ReCorize requires Rust 1.74 or newer:

```sh
cargo build --release
