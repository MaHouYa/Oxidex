# Repository Guidelines

## Project Structure & Module Organization

Kerything is currently a Rust 2024 Cargo workspace that rewrites the old Qt/KDE application as a Linux desktop filename search tool. The workspace contains three active crates:

- `crates/kerything`: the unprivileged `eframe`/`egui` GUI.
- `crates/kerything-scanner-helper`: the privileged scanner CLI launched through `pkexec`.
- `crates/kerything-core`: shared device discovery, scan streams, snapshots, indexing/search, path reconstruction, and scanner backends.

The GUI persists indexes under `$XDG_DATA_HOME/kerything/indexes/`. Runtime metadata formats live in `crates/kerything-core/src/stream.rs` and `crates/kerything-core/src/snapshot.rs`; search/index logic is in `crates/kerything-core/src/index.rs`; scanner backends are in `crates/kerything-core/src/scanner/`.

Legacy C++/Qt/KDE directories and files may still be present in the tree for history or transition, but the active build is Rust/Cargo. Do not reintroduce Qt6, KDE Frameworks, KIO, Solid, D-Bus daemon activation, systemd daemon activation, libblkid, or e2fsprogs/libext2fs runtime dependencies unless explicitly approved.

Packaging and desktop integration files live at the repository root and under `scripts/` and `.github/`:

- `PKGBUILD`: Arch package build.
- `net.reikooters.kerything.desktop`: desktop entry.
- `net.reikooters.kerything.policy`: Polkit policy for the scanner helper.
- `scripts/package-deb.sh`: local Debian package build.
- `scripts/ci/build-deb-ubuntu20.04.sh`: Ubuntu 20.04 package build script.
- `.github/workflows/deb.yml`: GitHub Actions Debian package workflow.

## Build, Test, And Development Commands

Build all active Rust crates:

```bash
cargo build --release --locked --workspace
```

Run the GUI from the build tree:

```bash
cargo run --release -p kerything
```

Run the scanner helper directly:

```bash
cargo run --release -p kerything-scanner-helper -- --version
```

Run tests and checks:

```bash
cargo test --workspace
cargo check --workspace
cargo clippy --workspace
cargo fmt --all
```

Build the Arch package from the working tree:

```bash
makepkg -si -f -c
```

Build a Debian package locally after a release build:

```bash
scripts/package-deb.sh
```

The GitHub Actions Debian package workflow runs inside an `ubuntu:20.04` job container, installs Rust with `dtolnay/rust-toolchain@stable`, and uploads `dist/*.deb`.

## Coding Style & Naming Conventions

Use idiomatic Rust and keep formatting under `cargo fmt`. Prefer small, explicit modules over broad abstractions, and follow the existing crate boundaries before adding new ones. Use `snake_case` for functions, modules, and locals; `PascalCase` for types and enum variants; and `SCREAMING_SNAKE_CASE` for constants.

Prefer Rust-native crates and standard library facilities. Avoid dynamic C library bindings for filesystem scanners unless the user explicitly approves that tradeoff. The EXT4 scanner currently uses the patched Rust `ext4` crate from `vendor/ext4`; NTFS uses the Rust `ntfs` crate. Btrfs is planned as V2 raw metadata-tree support, not as a mounted-path crawler.

Keep helper stdout reserved for binary scan data. Progress and diagnostics belong on stderr, with progress lines formatted as:

```text
KERYTHING_PROGRESS <0-100>
```

## Testing Guidelines

For GUI changes, manually verify search, device filtering, row selection, sorting, open/open-folder actions, copy-name/copy-path actions, progress display, cancellation, and snapshot reload after restart.

For search or snapshot changes, run unit tests and check path reconstruction, Unicode names, hard links, short-token fallback, trigram matching, deterministic sorting, multi-device merging, corruption rejection, and version mismatch behavior.

For scanner changes, validate both mounted and unmounted devices when possible. NTFS should scan MFT metadata and preserve hard-link names. EXT4 should read filesystem metadata, inode metadata, and directory-entry blocks; it must not scan regular file contents or do whole-disk byte-by-byte discovery. Btrfs V2 should treat subvolumes and snapshots as separate searchable roots so identical inode numbers in different roots do not collide.

When changing packaging, validate at least:

```bash
cargo build --release --locked --workspace
desktop-file-validate net.reikooters.kerything.desktop
bash -n scripts/ci/build-deb-ubuntu20.04.sh scripts/package-deb.sh
scripts/package-deb.sh
```

If Docker is available, the workflow can be tested through the same `ubuntu:20.04` container image used by GitHub Actions, but the checked-in workflow uses a job-level container rather than a nested `docker run`.

## Debian Package Notes

The Debian package installs `kerything`, `kerything-scanner-helper`, the desktop file, hicolor icons, license, and `net.reikooters.kerything.policy` into standard system paths. This is the preferred portable packaging path because Polkit authorizes `/usr/bin/kerything-scanner-helper` directly.

## Commit & Pull Request Guidelines

Use concise, descriptive commit summaries that state the user-visible or technical effect. Pull requests should include the motivation, touched subsystem, validation steps, and screenshots for GUI changes. Note any behavior involving root privileges, Polkit, raw block devices, Debian packaging constraints, or packaging dependencies.

## Security & Configuration Tips

Raw block-device access is privileged. Keep validation in `crates/kerything-scanner-helper/src/main.rs` strict: reject empty paths, non-absolute paths, non-`/dev` paths, non-existent paths, non-block devices, world-writable device nodes, and unsupported filesystem types. Resolve symlinks before scanning.

Treat Polkit policy changes as security-sensitive. The GUI must remain unprivileged; only the helper should run with elevated privileges. Avoid logging sensitive full paths unless needed for a clear diagnostic.
