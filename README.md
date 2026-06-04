# Kerything

Kerything is a Linux desktop filename search utility inspired by Voidtools Everything. This branch rewrites the application in Rust with an unprivileged `egui` GUI and a small privileged scanner helper launched through `pkexec`.

The V1 Rust app indexes NTFS and EXT4 devices by reading filesystem metadata instead of crawling mounted directories or reading file contents. Btrfs is represented in the data model, device discovery, helper interface, snapshot metadata, and UI as a planned V2 backend; the V2 scanner is intended to be a native read-only metadata-tree parser using Rust crates rather than `libbtrfs` bindings.

Kerything is a community project and is not affiliated with Voidtools.

![Screenshot](screenshot.png)

## Features

- Rust-native desktop GUI built with `eframe`/`egui`.
- Unprivileged GUI; only `kerything-scanner-helper` is run through Polkit.
- Persistent multi-device indexes under `$XDG_DATA_HOME/kerything/indexes/`.
- Stable device IDs using `partuuid:<id>`, then `uuid:<filesystem-uuid>`, then `dev:<canonical-dev-node>`.
- NTFS V1 scanner reads MFT metadata, preserves hard-link names as separate entries, filters duplicate DOS 8.3 aliases, and hides early `$` system files.
- EXT4 V1 scanner reads filesystem metadata, inode metadata, and directory entries through a Rust-native crate.
- Search uses Unicode lowercase folding plus byte trigrams for tokens of length three or more, with substring refinement and short-token fallback.
- Multi-device search, device-scope filtering, result sorting, mounted/unmounted path display, and persisted snapshot reload on restart.
- Guaranteed actions: open file, open containing folder, copy file name, and copy full path.

## What Was Removed

The Rust build does not use Qt6, KDE Frameworks, KIO, Solid, a D-Bus indexing daemon, systemd daemon activation, libblkid, e2fsprogs/libext2fs, or Intel OneTBB. Polkit remains because raw block-device scanning is privileged.

The old C++ daemon snapshot format is intentionally not imported. Users rescan once into the new Rust snapshot format.

## Architecture

The Cargo workspace contains three crates:

- `crates/kerything`: the `eframe`/`egui` GUI.
- `crates/kerything-scanner-helper`: the privileged scanner CLI.
- `crates/kerything-core`: shared device discovery, scan stream, indexing, search, snapshots, path resolution, and scanner backends.

The GUI discovers known devices from `/dev/disk/by-*`, `/run/udev/data`, and `/proc/self/mountinfo`. It stores snapshots in the user data directory and launches the helper only when a rescan is requested.

The helper validates the device path, resolves symlinks, rejects unsafe inputs, scans the requested filesystem, reports progress on stderr, and writes only binary scan data to stdout.

## Helper CLI

```shell
kerything-scanner-helper --version
kerything-scanner-helper <absolute-/dev-device> <ntfs|ext4>
```

The V2 interface extends the filesystem argument to:

```shell
kerything-scanner-helper <absolute-/dev-device> <ntfs|ext4|btrfs>
```

At the moment, `btrfs` is accepted by shared types but the backend returns an explicit unsupported error until the V2 raw metadata scanner is implemented.

Progress is emitted on stderr in this format:

```text
KERYTHING_PROGRESS <0-100>
```

Stdout is reserved for the binary `ScanStreamV1` payload.

## Building

Install Rust and the native libraries needed by `eframe`/`winit` for Linux desktop rendering. On Arch Linux:

```shell
sudo pacman -S cargo polkit libx11 libxcb libxkbcommon wayland libglvnd vulkan-icd-loader fontconfig xdg-utils hicolor-icon-theme
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
target/release/kerything
```

For local helper testing, the GUI first looks for `kerything-scanner-helper` beside the running `kerything` binary and then falls back to `PATH`. Installed systems should use the Polkit policy that authorizes `/usr/bin/kerything-scanner-helper`.

## Arch Package

From the repository root:

```shell
makepkg -si -f -c
```

The package installs:

- `/usr/bin/kerything`
- `/usr/bin/kerything-scanner-helper`
- `/usr/share/applications/net.reikooters.kerything.desktop`
- `/usr/share/polkit-1/actions/net.reikooters.kerything.policy`
- hicolor app icons
- the GPL license

It does not install the previous D-Bus service, systemd daemon service, Qt/KDE files, or CMake build outputs.

## Current Scanner Status

### NTFS V1

The NTFS scanner reads the MFT and file-name attributes. It records parent relationships, name, size, modification time, directory flag, and symlink/reparse-point flag. Multiple hard-link names are indexed as separate records.

### EXT4 V1

The EXT4 scanner reads filesystem metadata through the Rust `ext4` crate. It does not scan the whole disk byte-by-byte and does not read regular file contents. Directory entries provide names and parent relationships, while inodes provide size, modification time, and type.

### Btrfs V2

Btrfs support is planned as a native raw metadata-tree scanner. The intended backend will extract metadata from Btrfs inode items and names/parent relationships from inode refs, extended inode refs, directory items, and directory indexes. Subvolumes and snapshots should be searchable as separate roots so identical inode numbers in different roots do not collide.

If existing Rust Btrfs crates cannot expose the required metadata efficiently, the V2 path should add a small custom read-only parser for the needed tree items rather than linking dynamic Btrfs libraries.

## Development Notes

- The GUI must not run as root.
- Helper stdout must contain only binary scan data.
- Path/device validation in the helper is security-sensitive.
- Old C++ snapshots are intentionally ignored.
- Live fanotify updates, background daemon indexing, D-Bus APIs, and rich drag-out/file-URI clipboard support are outside the V1 guarantee.

## License

This project is licensed under GPL-3.0-or-later. See [LICENSE](LICENSE) for details.
