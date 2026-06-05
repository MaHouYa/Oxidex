# Repository Guidelines

## Project Structure & Module Organization

Kerything is currently a Rust 2024 Cargo workspace that rewrites the old Qt/KDE application as a Linux desktop filename search tool. The workspace contains these active crates:

- `crates/kerything`: the unprivileged `eframe`/`egui` GUI. Default launch connects to `kerythingd`; `--standalone` keeps the in-process fallback.
- `crates/kerything-client`: shared Unix-socket client library for GUI and CLI frontends.
- `crates/kerything-core`: shared device discovery, scan streams, snapshots, indexing/search, path reconstruction, and scanner backends.
- `crates/kerything-daemon`: `kerythingd`, the unprivileged per-user daemon that owns config, loaded indexes, search, scan requests, and snapshot persistence.
- `crates/kerything-scannerd`: `kerything-scannerd`, the privileged scanner daemon that owns raw `/dev/...` scans only.
- `crates/kerything-cli`: CLI and rofi/script integration client.
- `crates/kerything-scanner-helper`: compatibility privileged scanner CLI launched through `pkexec`.

`kerythingd` persists indexes under `$XDG_DATA_HOME/kerything/indexes/` and config under `$XDG_CONFIG_HOME/kerything/config.toml`. Runtime metadata formats live in `crates/kerything-core/src/stream.rs` and `crates/kerything-core/src/snapshot.rs`; V4 index health sidecars live beside snapshots as `*.state.json`; search/index logic is in `crates/kerything-core/src/index.rs`; config and include/exclude rules are in `crates/kerything-core/src/config.rs` and `crates/kerything-core/src/rules.rs`; scanner backends are in `crates/kerything-core/src/scanner/`; setup diagnostics are in `crates/kerything-core/src/doctor.rs`.

Legacy C++/Qt/KDE directories and files may still be present in the tree for history or transition, but the active build is Rust/Cargo. Do not reintroduce Qt6, KDE Frameworks, KIO, Solid, D-Bus APIs/activation, libblkid, e2fsprogs/libext2fs runtime dependencies, `libbtrfs` bindings, or `wgpu` as a default renderer unless explicitly approved.

Packaging and desktop integration files live at the repository root and under `scripts/` and `.github/`:

- `PKGBUILD`: Arch package build.
- `net.reikooters.kerything.desktop`: desktop entry.
- `net.reikooters.kerything.policy`: Polkit policy for the compatibility helper and scanner-daemon connection authorization.
- `systemd/user/`: user service/socket units for `kerythingd`.
- `systemd/system/`: system service/socket units for `kerything-scannerd`.
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

Run the standalone fallback from the build tree:

```bash
cargo run --release -p kerything -- --standalone
```

Run the daemons in foreground development mode:

```bash
scripts/dev-install-polkit.sh
cargo run --release -p kerything-daemon -- --foreground
sudo target/release/kerything-scannerd --foreground
```

For manual scanner-daemon testing, install the Polkit action first or expect `Action net.reikooters.kerything.connect-scanner is not registered`. The user running `kerythingd` must also be able to connect to `/run/kerything/scannerd.sock` before Polkit can authorize the session. The foreground scanner daemon attempts to create the socket as `root:kerything` with mode `0660`; make sure the `kerything` group exists and the test user is in that group, or expect `Permission denied (os error 13)`.

Run the CLI:

```bash
cargo run --release -p kerything-cli -- search "ext:rs path:src main"
cargo run --release -p kerything-cli -- doctor
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

Prefer Rust-native crates and standard library facilities. Avoid dynamic C library bindings for filesystem scanners unless the user explicitly approves that tradeoff. The EXT4 scanner currently uses the patched Rust `ext4` crate from `vendor/ext4`; NTFS uses the Rust `ntfs` crate. Btrfs V2-basic uses `btrfs-fs`/`btrfs-disk` for default-root raw metadata scanning and must not become a mounted-path crawler.

The GUI should use `eframe`/`egui` with the `glow` renderer by default. Do not add `wgpu` to default features without explicit approval.

Keep helper stdout reserved for binary scan data. Progress and diagnostics belong on stderr, with progress lines formatted as:

```text
KERYTHING_PROGRESS <0-100>
```

`kerything-scannerd` uses framed Unix-socket IPC instead: progress is a structured event and the final `scanner.start_scan` response carries binary `ScanStreamV1` as the frame payload.

## Testing Guidelines

For GUI changes, manually verify daemon connection, standalone fallback, search, device filtering, filter panel, row selection, sorting including relevance, open/open-folder actions, copy-name/copy-path actions, right-click context menu, properties, progress display, cancellation/error display, index health/watch status, and snapshot reload after restart.

For search or snapshot changes, run unit tests and check path reconstruction, Unicode names, hard links, short-token fallback, trigram matching, wildcard matching, negation, extension/type/path/size/mtime filters, relevance sorting, deterministic explicit sorting, multi-device merging, corruption rejection, sidecar state behavior, and version mismatch behavior.

For scanner changes, validate both mounted and unmounted devices when possible. NTFS should scan MFT metadata and preserve hard-link names. EXT4 should read filesystem metadata, inode metadata, and directory-entry blocks; it must not scan regular file contents or do whole-disk byte-by-byte discovery. Btrfs V2-basic scans only the default/main root, treats other subvolumes as boundaries, and rejects unsupported multi-device layouts clearly.

When changing daemon/client/IPC code, verify `kerythingd --foreground`, `kerything-cli devices`, `kerything-cli indexes`, `kerything-cli search`, `kerything-cli jobs`, `kerything-cli scan --wait`, `kerything-cli cancel`, `kerything-cli doctor`, and scanner authorization/error handling. `kerything-scannerd` should accept only scanner protocol methods, validate every scan request, start cancellable scanner jobs, expose final scan streams only through the framed `scanner.take_result` response, and never expose arbitrary block reads.

When changing packaging, validate at least:

```bash
cargo build --release --locked --workspace
desktop-file-validate net.reikooters.kerything.desktop
bash -n scripts/ci/build-deb-ubuntu20.04.sh scripts/package-deb.sh
scripts/package-deb.sh
```

If Docker is available, the workflow can be tested through the same `ubuntu:20.04` container image used by GitHub Actions, but the checked-in workflow uses a job-level container rather than a nested `docker run`.

## Debian Package Notes

The Debian package installs `kerything`, `kerything-cli`, `kerythingd`, `kerything-scannerd`, `kerything-scanner-helper`, the desktop file, hicolor icons, license, systemd units, and `net.reikooters.kerything.policy` into standard system paths. It creates a `kerything` system group for the scanner daemon socket. This is the preferred portable packaging path because privileged components live under `/usr/bin`.

## Commit & Pull Request Guidelines

Use concise, descriptive commit summaries that state the user-visible or technical effect. Pull requests should include the motivation, touched subsystem, validation steps, and screenshots for GUI changes. Note any behavior involving root privileges, Polkit, raw block devices, Debian packaging constraints, or packaging dependencies.

## Security & Configuration Tips

Raw block-device access is privileged. Keep validation in `crates/kerything-core/src/scanner/mod.rs` strict because it is shared by the helper and scanner daemon: reject empty paths, non-absolute paths, non-`/dev` paths, non-existent paths, non-block devices, world-writable device nodes, and unsupported filesystem types. Resolve symlinks before scanning.

Treat Polkit policy changes as security-sensitive. The GUI and `kerythingd` must remain unprivileged; only `kerything-scannerd` and the compatibility helper should run with elevated privileges. Scanner authorization is per socket connection/session through `net.reikooters.kerything.connect-scanner`; the helper compatibility action is `net.reikooters.kerything.run-scanner`. Avoid logging sensitive full paths unless needed for a clear diagnostic.
