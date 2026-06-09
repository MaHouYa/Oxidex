# Oxidex

Oxidex is a Linux desktop filename search utility inspired by Voidtools Everything. This branch rewrites the application in Rust with an unprivileged `egui` GUI, an unprivileged per-user daemon, and a small privileged scanner daemon protected by a `root:oxidex` Unix socket.

The Rust app indexes NTFS, EXT3, EXT4, and basic Btrfs devices by reading filesystem metadata instead of crawling mounted directories or reading file contents. Btrfs V2 support is native and read-only through Rust crates; it indexes the default/main root and treats other subvolumes as boundaries for now.

Oxidex is a community project and is not affiliated with Voidtools.

Credit: Oxidex builds on the original project foundation created by Reikooters. Thank you to Reikooters for the original idea, codebase, and early project work.

![Screenshot](screenshot.png)

For installation, first indexing, search examples, rofi usage, troubleshooting, and exporting debug logs, see the [Oxidex User Guide](docs/user-guide.md).

Rename note: version 2.2.0 continues the public Oxidex line. Commands, crates, config paths, index paths, systemd units, and the scanner socket group use the `oxidex` name.

## Features

- Rust-native desktop GUI built with `eframe`/`egui`, using the `glow` backend by default.
- Unprivileged GUI and user daemon; only `oxidex-scannerd` performs privileged raw metadata scans and mounted live update watching.
- Persistent multi-device indexes under `$XDG_DATA_HOME/oxidex/indexes/`.
- Stable device IDs using `partuuid:<id>`, then `uuid:<filesystem-uuid>`, then `dev:<canonical-dev-node>`.
- NTFS V1 scanner reads MFT metadata, preserves hard-link names as separate entries, filters duplicate DOS 8.3 aliases, and hides early `$` system files.
- EXT3/EXT4 scanner reads filesystem metadata, inode metadata, and directory entries through a Rust-native crate.
- Btrfs V2 scanner reads the default/main root through Rust-native Btrfs metadata APIs and rejects unsupported multi-device layouts clearly.
- Search uses Unicode lowercase folding plus byte trigrams for positive name tokens of length three or more, with substring refinement, short-token fallback, relevance sorting, wildcards, quoted phrases, negation, and `ext:`/`type:`/`path:`/`size:`/`mtime:` filters.
- V4 daemon scans are queued, asynchronous, cancellable, and tracked through job status. Mounted indexed filesystems can be kept fresh through privileged scanner-daemon `notify`/inotify events.
- `oxidex-cli doctor` diagnoses daemon, scanner socket, config, index, systemd, and security setup problems even when `oxidexd` is not running.
- Multi-device search, device-scope filtering, result sorting, mounted/unmounted path display, and persisted snapshot reload on restart.
- Guaranteed actions: open file, open containing folder, copy file name/path, right-click context actions, and properties.

## What Was Removed

The Rust build does not use Qt6, KDE Frameworks, KIO, Solid, a D-Bus indexing daemon, libblkid, e2fsprogs/libext2fs, Intel OneTBB, or `wgpu` by default.

The old C++ daemon snapshot format is intentionally not imported. Users rescan once into the new Rust snapshot format.

## Architecture

The Cargo workspace contains these primary crates:

- `crates/oxidex`: the `eframe`/`egui` GUI.
- `crates/oxidex-daemon`: `oxidexd`, the unprivileged per-user daemon that owns config, loaded indexes, search, scan requests, and snapshot persistence.
- `crates/oxidex-scannerd`: `oxidex-scannerd`, the privileged scanner daemon that validates raw `/dev/...` scan requests, streams `ScanStreamV1` data, and emits mounted live update events.
- `crates/oxidex-client`: shared Unix-socket client library for GUI and CLI frontends.
- `crates/oxidex-cli`: CLI and rofi/script integration client.
- `crates/oxidex-core`: shared device discovery, scan stream, indexing, search, snapshots, path resolution, and scanner backends.

`oxidexd` discovers known devices from `/dev/disk/by-*`, `/run/udev/data`, and `/proc/self/mountinfo`. It stores snapshots in the user data directory, keeps index health sidecars beside snapshots, owns the scan job queue, applies include/exclude rules, and connects to `oxidex-scannerd` for raw rescans and privileged mounted live update events.

`oxidex-scannerd` validates the device path, resolves symlinks, rejects unsafe inputs, starts cancellable scanner jobs, reports progress as structured IPC events, returns the existing binary scan stream through `scanner.take_result`, and watches mounted indexed devices as root when requested by `oxidexd`.

The GUI defaults to daemon mode. Use the standalone fallback when developing or recovering from daemon setup problems:

```shell
oxidex --standalone
```

Daemon sockets:

```text
$XDG_RUNTIME_DIR/oxidex/oxidexd.sock
/run/oxidex/scannerd.sock
```

## Daemon And CLI

Foreground development mode:

```shell
sudo groupadd --system oxidex 2>/dev/null || true
sudo usermod -aG oxidex "$USER"
newgrp oxidex
oxidexd --foreground
sudo oxidex-scannerd --foreground
```

For local foreground testing, the scanner daemon socket is protected by Unix permissions. Create the socket group, add your user, and start a fresh login session or `newgrp` before running `oxidexd`:

```shell
sudo groupadd --system oxidex 2>/dev/null || true
sudo usermod -aG oxidex "$USER"
newgrp oxidex
sudo target/release/oxidex-scannerd --foreground
```

If the scanner daemon was already running before the group existed, restart it. The foreground daemon will create `/run/oxidex/scannerd.sock` as `root:oxidex` with mode `0660` when the group is available. Without that, `oxidexd` will see `Permission denied`.

CLI examples:

```shell
oxidex-cli search "ext:rs path:src main"
oxidex-cli search --sort relevance --limit 100 "main !target"
oxidex-cli search --json "foo"
oxidex-cli explain 'ext:rs path:src "scan stream"'
oxidex-cli rofi "foo"
oxidex-cli rofi --show-id "foo"
oxidex-cli indexes
oxidex-cli devices
oxidex-cli scan partuuid:... --wait
oxidex-cli jobs
oxidex-cli cancel 1
oxidex-cli doctor
oxidex-cli config get
oxidex-cli config set ui.theme dark
```

Rofi script mode can call the CLI:

```shell
rofi -dmenu -i -p Oxidex < <(oxidex-cli rofi "$query")
```

A Ulauncher v5 extension is available in `extensions/ulauncher-oxidex/`.
For local development, symlink it into Ulauncher's extension directory and use
the default `ox` keyword:

```shell
mkdir -p ~/.local/share/ulauncher/extensions
ln -s "$PWD/extensions/ulauncher-oxidex" ~/.local/share/ulauncher/extensions/ulauncher-oxidex
```

## Building

Install Rust and the native libraries needed by `eframe`/`winit` for Linux desktop rendering. On Arch Linux:

```shell
sudo pacman -S cargo clang libx11 libxcb libxkbcommon wayland libglvnd fontconfig xdg-utils hicolor-icon-theme
```

Build all Rust crates:

```shell
cargo build --release --locked --workspace
```

Run tests:

```shell
cargo test --workspace
```

Run the GUI from the build tree:

```shell
target/release/oxidex
```

For local scanner testing, `oxidexd` uses `/run/oxidex/scannerd.sock`. If that socket is unavailable or the user is not in the `oxidex` group, scans fail with a setup error instead of prompting for a password.

## Search Syntax

Plain whitespace-separated terms match file names as case-insensitive substrings. V4 supports:

- Wildcards: `*.rs`, `foo*`, `*backup*`
- Quoted phrases: `"exact phrase"`
- Extensions: `ext:rs`, `ext:.RS`, `ext:rs,txt`
- File types: `type:file`, `type:dir`, `type:symlink`
- Path filters: `path:src`
- Negation: `!foo`, `-cache`, `!ext:o`, `!path:target`
- Sizes: `size:0`, `size:>10mb`, `size:<4kb`, `size:1mb..100mb`
- Modification time: `mtime:today`, `mtime:yesterday`, `mtime:<7d`, `mtime:2026-01-01..2026-06-01`

Typed filters and the GUI filter panel combine with AND semantics. Regex and OR groups remain outside V4.

## V4 Live Updates And Diagnostics

V4 keeps raw unmounted scans as explicit rescan jobs, but mounted indexed devices can be watched by `oxidex-scannerd` as root. The privileged watcher uses normal Linux filename notifications through the Rust `notify` crate and sends structured events to `oxidexd`, which updates the in-memory index, applies include/exclude rules, and flushes dirty snapshots after a short debounce. If notification overflow, scanner disconnect, or ambiguous state is detected, the index is marked stale and a raw rescan is recommended.

Use doctor after installation or when scanning fails:

```shell
oxidex-cli doctor
oxidex-cli doctor --scanner
oxidex-cli doctor --security
oxidex-cli doctor --json
```

Doctor checks the user daemon socket, scanner socket permissions, `oxidex` group membership, config validity, index loading, packaged systemd units, and security setup.

## Arch Package

From the repository root:

```shell
makepkg -si -f -c
```

The package installs:

- `/usr/bin/oxidex`
- `/usr/bin/oxidex-cli`
- `/usr/bin/oxidexd`
- `/usr/bin/oxidex-scannerd`
- `/usr/share/applications/org.mahouya.oxidex.desktop`
- systemd user units for `oxidexd`
- systemd system units for `oxidex-scannerd`
- hicolor app icons
- the GPL license

It does not install the previous D-Bus service, Qt/KDE files, or CMake build outputs.

## Debian Package

Build a local `.deb` after a release build:

```shell
scripts/package-deb.sh
```

The package is written to `dist/oxidex_2.2.0_amd64.deb`. The GitHub Actions workflow in `.github/workflows/deb.yml` builds the same package inside an `ubuntu:20.04` job container and uploads it as a workflow artifact.

The Debian package installs the GUI, CLI, user daemon, scanner daemon, desktop file, systemd units, hicolor icons, and license into standard system paths.

The package creates a system group named `oxidex` for the privileged scanner socket. Users who should scan disks and receive privileged live update events must be added to that group by the system administrator. After that one-time setup, Oxidex scans without password prompts.

## Current Scanner Status

### NTFS V1

The NTFS scanner reads the MFT and file-name attributes. It records parent relationships, name, size, modification time, directory flag, and symlink/reparse-point flag. Multiple hard-link names are indexed as separate records.

### EXT3/EXT4

The EXT3/EXT4 scanner reads filesystem metadata through the Rust `ext4` crate. It does not scan the whole disk byte-by-byte and does not read regular file contents. Directory entries provide names and parent relationships, while inodes provide size, modification time, and type.

### Btrfs V2

Btrfs support uses the Rust `btrfs-fs` crate on top of `btrfs-disk`. It scans the default/main root directly from metadata and records names, parent relationships, size, modification time, directory flags, and symlink flags.

V2-basic does not recurse into additional subvolumes or snapshots. Those entries are treated as directory-like boundaries and the scan reports that only the default root is indexed. Multi-device Btrfs layouts are rejected clearly until the raw scanner grows full device mapping support.

## Development Notes

- The GUI must not run as root.
- `oxidex-scannerd` is the only privileged scanner path in normal builds.
- Scanner daemon responses use framed IPC; only `scanner.take_result` carries binary `ScanStreamV1` payload data.
- Path/device and watch mount validation in the scanner daemon is security-sensitive.
- Old C++ snapshots are intentionally ignored.
- Raw filesystem-specific delta scanning, NTFS USN Journal support, EXT4 journal parsing, Btrfs generation/transid scanning, open history/frecency, native rofi plugin ABI support, D-Bus APIs, full Btrfs subvolume traversal, Snapshot Format V2, regex search, OR groups, and rich drag-out/file-URI clipboard support are outside V4.

## License

This project is licensed under GPL-3.0-or-later. See [LICENSE](LICENSE) for details.
