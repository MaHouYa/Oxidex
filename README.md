# Kerything

Kerything is a Linux desktop filename search utility inspired by Voidtools Everything. This branch rewrites the application in Rust with an unprivileged `egui` GUI, an unprivileged per-user daemon, and a small privileged scanner daemon authorized through Polkit.

The Rust app indexes NTFS, EXT4, and basic Btrfs devices by reading filesystem metadata instead of crawling mounted directories or reading file contents. Btrfs V2 support is native and read-only through Rust crates; it indexes the default/main root and treats other subvolumes as boundaries for now.

Kerything is a community project and is not affiliated with Voidtools.

![Screenshot](screenshot.png)

## Features

- Rust-native desktop GUI built with `eframe`/`egui`, using the `glow` backend by default.
- Unprivileged GUI and user daemon; only `kerything-scannerd` or the compatibility `kerything-scanner-helper` performs privileged raw metadata scans.
- Persistent multi-device indexes under `$XDG_DATA_HOME/kerything/indexes/`.
- Stable device IDs using `partuuid:<id>`, then `uuid:<filesystem-uuid>`, then `dev:<canonical-dev-node>`.
- NTFS V1 scanner reads MFT metadata, preserves hard-link names as separate entries, filters duplicate DOS 8.3 aliases, and hides early `$` system files.
- EXT4 V1 scanner reads filesystem metadata, inode metadata, and directory entries through a Rust-native crate.
- Btrfs V2 scanner reads the default/main root through Rust-native Btrfs metadata APIs and rejects unsupported multi-device layouts clearly.
- Search uses Unicode lowercase folding plus byte trigrams for tokens of length three or more, with substring refinement, short-token fallback, wildcards, quoted phrases, and `ext:`/`type:`/`path:` filters.
- Multi-device search, device-scope filtering, result sorting, mounted/unmounted path display, and persisted snapshot reload on restart.
- Guaranteed actions: open file, open containing folder, copy file name/path, right-click context actions, and properties.

## What Was Removed

The Rust build does not use Qt6, KDE Frameworks, KIO, Solid, a D-Bus indexing daemon, libblkid, e2fsprogs/libext2fs, Intel OneTBB, or `wgpu` by default. Polkit remains because raw block-device scanning is privileged.

The old C++ daemon snapshot format is intentionally not imported. Users rescan once into the new Rust snapshot format.

## Architecture

The Cargo workspace contains these primary crates:

- `crates/kerything`: the `eframe`/`egui` GUI.
- `crates/kerything-daemon`: `kerythingd`, the unprivileged per-user daemon that owns config, loaded indexes, search, scan requests, and snapshot persistence.
- `crates/kerything-scannerd`: `kerything-scannerd`, the privileged scanner daemon that validates raw `/dev/...` scan requests and streams `ScanStreamV1` data.
- `crates/kerything-client`: shared Unix-socket client library for GUI and CLI frontends.
- `crates/kerything-cli`: CLI and rofi/script integration client.
- `crates/kerything-scanner-helper`: the compatibility privileged scanner CLI kept for one release.
- `crates/kerything-core`: shared device discovery, scan stream, indexing, search, snapshots, path resolution, and scanner backends.

`kerythingd` discovers known devices from `/dev/disk/by-*`, `/run/udev/data`, and `/proc/self/mountinfo`. It stores snapshots in the user data directory and connects to `kerything-scannerd` only when a raw rescan is requested. If the scanner daemon is not reachable, it can still fall back to the compatibility helper.

`kerything-scannerd` validates the device path, resolves symlinks, rejects unsafe inputs, scans the requested filesystem, reports progress as structured IPC events, and returns the existing binary scan stream as a framed payload.

The GUI defaults to daemon mode. Use the standalone fallback when developing or recovering from daemon setup problems:

```shell
kerything --standalone
```

Daemon sockets:

```text
$XDG_RUNTIME_DIR/kerything/kerythingd.sock
/run/kerything/scannerd.sock
```

## Helper CLI

The helper remains available for compatibility and manual diagnostics:

```shell
kerything-scanner-helper --version
kerything-scanner-helper <absolute-/dev-device> <ntfs|ext4|btrfs>
```

Progress is emitted on stderr in this format:

```text
KERYTHING_PROGRESS <0-100>
```

Stdout is reserved for the binary `ScanStreamV1` payload.

## Daemon And CLI

Foreground development mode:

```shell
scripts/dev-install-polkit.sh
kerythingd --foreground
sudo kerything-scannerd --foreground
```

The first command installs the local Polkit action file and checks that `pkaction` can see `net.reikooters.kerything.connect-scanner`. Without that, `pkcheck` will fail with `Action net.reikooters.kerything.connect-scanner is not registered`.

For local foreground testing, the scanner daemon socket is still protected by Unix permissions before Polkit can run. Create the socket group, add your user, and start a fresh login session or `newgrp` before running `kerythingd`:

```shell
sudo groupadd --system kerything 2>/dev/null || true
sudo usermod -aG kerything "$USER"
newgrp kerything
scripts/dev-install-polkit.sh
sudo target/release/kerything-scannerd --foreground
```

If the scanner daemon was already running before the group existed, restart it. The foreground daemon will create `/run/kerything/scannerd.sock` as `root:kerything` with mode `0660` when the group is available. Without that, `kerythingd` will see `Permission denied` before the Polkit authorization step.

CLI examples:

```shell
kerything-cli search "ext:rs path:src main"
kerything-cli search --json "foo"
kerything-cli rofi "foo"
kerything-cli indexes
kerything-cli devices
kerything-cli scan partuuid:...
kerything-cli config get
kerything-cli config set ui.theme dark
```

Rofi script mode can call the CLI:

```shell
rofi -dmenu -i -p Kerything < <(kerything-cli rofi "$query")
```

## Building

Install Rust and the native libraries needed by `eframe`/`winit` for Linux desktop rendering. On Arch Linux:

```shell
sudo pacman -S cargo polkit libx11 libxcb libxkbcommon wayland libglvnd fontconfig xdg-utils hicolor-icon-theme
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

For local scanner testing, `kerythingd` first tries `/run/kerything/scannerd.sock`. If that daemon is unavailable, it looks for `kerything-scanner-helper` beside the running daemon binary and then falls back to `PATH`. Installed systems should prefer the scanner daemon and keep the helper only as a compatibility fallback.

## Search Syntax

Plain whitespace-separated terms match file names as case-insensitive substrings. V2 also supports:

- Wildcards: `*.rs`, `foo*`, `*backup*`
- Quoted phrases: `"exact phrase"`
- Extensions: `ext:rs`, `ext:.RS`, `ext:rs,txt`
- File types: `type:file`, `type:dir`, `type:symlink`
- Path filters: `path:src`

Typed filters and the GUI filter panel combine with AND semantics. Regex and OR/negation are intentionally outside V2-basic.

## Arch Package

From the repository root:

```shell
makepkg -si -f -c
```

The package installs:

- `/usr/bin/kerything`
- `/usr/bin/kerything-cli`
- `/usr/bin/kerythingd`
- `/usr/bin/kerything-scannerd`
- `/usr/bin/kerything-scanner-helper`
- `/usr/share/applications/net.reikooters.kerything.desktop`
- `/usr/share/polkit-1/actions/net.reikooters.kerything.policy`
- systemd user units for `kerythingd`
- systemd system units for `kerything-scannerd`
- hicolor app icons
- the GPL license

It does not install the previous D-Bus service, systemd daemon service, Qt/KDE files, or CMake build outputs.

## Debian Package

Build a local `.deb` after a release build:

```shell
scripts/package-deb.sh
```

The package is written to `dist/kerything_1.4.1_amd64.deb`. The GitHub Actions workflow in `.github/workflows/deb.yml` builds the same package inside an `ubuntu:20.04` job container and uploads it as a workflow artifact.

The Debian package installs the GUI, CLI, user daemon, scanner daemon, compatibility scanner helper, desktop file, systemd units, Polkit policy, hicolor icons, and license into standard system paths. This avoids the AppImage helper permission issue because privileged components live under `/usr/bin`.

The package creates a system group named `kerything` for the privileged scanner socket. Users who should connect to the scanner daemon can be added to that group by the system administrator. Polkit authorization is still required by default.

An optional passwordless Polkit rule can be installed by administrators, but it is not shipped by default:

```js
polkit.addRule(function(action, subject) {
    if (action.id == "net.reikooters.kerything.connect-scanner" &&
        subject.isInGroup("kerything")) {
        return polkit.Result.YES;
    }
});
```

## Current Scanner Status

### NTFS V1

The NTFS scanner reads the MFT and file-name attributes. It records parent relationships, name, size, modification time, directory flag, and symlink/reparse-point flag. Multiple hard-link names are indexed as separate records.

### EXT4 V1

The EXT4 scanner reads filesystem metadata through the Rust `ext4` crate. It does not scan the whole disk byte-by-byte and does not read regular file contents. Directory entries provide names and parent relationships, while inodes provide size, modification time, and type.

### Btrfs V2

Btrfs support uses the Rust `btrfs-fs` crate on top of `btrfs-disk`. It scans the default/main root directly from metadata and records names, parent relationships, size, modification time, directory flags, and symlink flags.

V2-basic does not recurse into additional subvolumes or snapshots. Those entries are treated as directory-like boundaries and the scan reports that only the default root is indexed. Multi-device Btrfs layouts are rejected clearly until the raw scanner grows full device mapping support.

## Development Notes

- The GUI must not run as root.
- Helper stdout must contain only binary scan data.
- Scanner daemon responses use framed IPC; only the final scan response carries binary `ScanStreamV1`.
- Path/device validation in the helper and scanner daemon is security-sensitive.
- Old C++ snapshots are intentionally ignored.
- Live fanotify updates, native rofi plugin ABI support, D-Bus APIs, full Btrfs subvolume traversal, regex search, OR/negation query syntax, and rich drag-out/file-URI clipboard support are outside the V3 guarantee.

## License

This project is licensed under GPL-3.0-or-later. See [LICENSE](LICENSE) for details.
