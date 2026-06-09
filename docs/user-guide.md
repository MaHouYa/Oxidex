# Oxidex User Guide

This guide is for people using Oxidex as a desktop filename search app. It covers installation setup, indexing disks, searching, rofi/CLI usage, troubleshooting, and exporting logs for debugging.

Oxidex has three main pieces:

- `oxidex`: the graphical search app.
- `oxidexd`: your unprivileged per-user search daemon.
- `oxidex-scannerd`: the privileged scanner daemon used when Oxidex reads raw filesystem metadata from `/dev/...` or watches mounted indexed filesystems.

The GUI and user daemon do not run as root. Access to the scanner daemon is controlled by the `/run/oxidex/scannerd.sock` Unix socket, normally owned by `root:oxidex` with mode `0660`.

The app has been rebranded to Oxidex, but the current compatibility release keeps the existing `oxidex` command names, config paths, index paths, and scanner group name.

## First-Time Setup

After installing Oxidex from a package, make sure your user can connect to the scanner daemon socket:

```sh
sudo usermod -aG oxidex "$USER"
sudo systemctl enable --now oxidex-scannerd.socket
```

Then log out and log back in. For a temporary current-terminal session, you can run:

```sh
newgrp oxidex
```

Check the setup:

```sh
oxidex-cli doctor
```

You want the scanner socket, group membership, config, and index checks to be `OK`. A warning about the daemon not running is usually harmless before the first GUI launch.

## Starting Oxidex

Start the GUI from your desktop launcher or terminal:

```sh
oxidex
```

By default the GUI connects to `oxidexd`. If the daemon is not already running, the GUI or CLI will try to start it.

For recovery or development, you can run the old in-process mode:

```sh
oxidex --standalone
```

Standalone mode is useful if the daemon socket or service setup is broken, but normal use should go through `oxidexd`.

## Chinese, Japanese, Korean, And IME Setup

Oxidex stores file names and search text as UTF-8, so CJK file names work in the index format already. The GUI also loads installed system CJK fonts at startup and uses egui/winit's native Linux IME path for composition input.

Oxidex does not bundle large CJK fonts. Install at least one system CJK font package:

Arch Linux:

```sh
sudo pacman -S noto-fonts-cjk
# or:
sudo pacman -S wqy-microhei
```

Debian/Ubuntu:

```sh
sudo apt install fonts-noto-cjk
# or:
sudo apt install fonts-wqy-microhei
```

Open **Settings** -> **Appearance** in the GUI. The CJK font status should report that fallback fonts were loaded. If it says no CJK fallback font was found, install one of the packages above and restart Oxidex. You can also set a preferred family name, for example `Noto Sans CJK SC`, then click **Apply**.

### Language Selection

The default language is `system`. Oxidex uses Simplified Chinese automatically when your system locale starts with `zh`, such as `zh_CN.UTF-8` or `zh_Hans_CN`. Otherwise it uses English.

You can override this in **Settings** -> **Appearance**:

- **System**: follow the current locale.
- **English**: force English.
- **Simplified Chinese**: force zh-CN.

The same values can be configured manually:

```toml
[ui]
language = "system" # system, en_us, or zh_cn
cjk_font_fallback = true
cjk_preferred_font = ""
```

### Fcitx5

Install Fcitx5 and a Chinese engine:

Arch Linux:

```sh
sudo pacman -S fcitx5 fcitx5-configtool fcitx5-chinese-addons
```

Debian/Ubuntu package names vary by release, but usually start with:

```sh
sudo apt install fcitx5 fcitx5-chinese-addons
```

Many Wayland desktop sessions configure input methods automatically. If your IME does not open in Oxidex, log out and back in after setting these environment variables in your desktop session:

```sh
GTK_IM_MODULE=fcitx
QT_IM_MODULE=fcitx
XMODIFIERS=@im=fcitx
SDL_IM_MODULE=fcitx
```

Start or restart Fcitx5, then test in Oxidex by composing `测试` in the search box, extension/path filter fields, and Settings text fields. During active composition, Enter/Escape should not open a selected result or clear your selection; after commit, search updates normally.

### IBus

Install IBus and a CJK engine:

Arch Linux:

```sh
sudo pacman -S ibus ibus-libpinyin
# or:
sudo pacman -S ibus ibus-rime
```

Debian/Ubuntu:

```sh
sudo apt install ibus ibus-libpinyin
# or:
sudo apt install ibus-rime
```

Run:

```sh
ibus-setup
```

If your desktop session does not configure IBus automatically, set:

```sh
GTK_IM_MODULE=ibus
QT_IM_MODULE=ibus
XMODIFIERS=@im=ibus
```

Log out and back in, then test by typing `测试.txt`, `日本語.md`, and `한글.log` in Oxidex search fields. Copy Name and Copy Path should preserve CJK text.

### CJK Troubleshooting

Missing glyph boxes usually mean Oxidex did not find a CJK fallback font. Install `noto-fonts-cjk`, `fonts-noto-cjk`, `wqy-microhei`, or `fonts-wqy-microhei`, then restart the GUI. If you use an unusual font family, set it as the preferred CJK font in Settings.

If the IME candidate window never appears, first test the same IME in another GTK or Wayland application. Then confirm the relevant environment variables are present in the shell that launches Oxidex:

```sh
env | grep -E 'IM_MODULE|XMODIFIERS'
```

On systemd-based desktops, environment changes often require a full logout/login. For user services, you may also need:

```sh
systemctl --user import-environment GTK_IM_MODULE QT_IM_MODULE XMODIFIERS SDL_IM_MODULE
```

## Optional Systemd Socket Activation

Packages install systemd socket units. Socket activation lets systemd start daemons only when something connects.

Enable the user daemon socket:

```sh
systemctl --user enable --now oxidexd.socket
```

Enable the privileged scanner socket:

```sh
sudo systemctl enable --now oxidex-scannerd.socket
```

Check them:

```sh
systemctl --user status oxidexd.socket
systemctl status oxidex-scannerd.socket
```

The GUI and CLI can also start `oxidexd` themselves if the user socket is not active. The scanner daemon should normally be reached through `/run/oxidex/scannerd.sock`; if it is unavailable, scans fail with setup instructions instead of opening a password prompt.

## Indexing A Disk

Open the GUI and click **Indexes**.

The Indexes window shows known devices, including unsupported filesystems. Oxidex can index NTFS, EXT3, EXT4, and basic Btrfs devices; unsupported rows stay visible with a reason and disabled scan controls.

To index a device:

1. Click **Index** beside the device.
2. Wait for the job progress to finish.
3. Search results become available immediately after the snapshot is saved.

To refresh an existing index:

1. Open **Indexes**.
2. Click **Rescan** beside the device.
3. Existing search results stay available while the scan job runs.

To cancel a scan:

1. Open **Indexes**.
2. Find the queued or running job.
3. Click **Cancel**.

From the CLI:

```sh
oxidex-cli devices
oxidex-cli scan partuuid:YOUR-PARTUUID --wait
oxidex-cli jobs
oxidex-cli cancel 1
```

## Mounted Live Updates

After a full raw scan, Oxidex can keep mounted indexes fresher using normal Linux filename notifications.

This means:

- Creating a file on a mounted indexed device can appear without a full rescan.
- Deleting a file can remove it from results.
- Renaming files and directories can update paths.
- Metadata changes can refresh size and modified time.

This is not raw filesystem delta scanning. If a device is unmounted, changed outside Linux, or if the notification queue overflows, Oxidex marks the index stale and you should rescan.

The default settings are:

```toml
[indexing]
watch_mounted = true
live_update_flush_seconds = 10
live_update_max_dirty_seconds = 60
```

## Searching

Type into the search box. Plain words match file names case-insensitively:

```text
invoice pdf
```

This means the name must contain both `invoice` and `pdf`.

Useful query examples:

```text
*.rs
main !target
"exact phrase"
ext:rs
ext:rs,txt
type:file
type:dir
type:symlink
path:src
size:>10mb
size:<4kb
size:1mb..100mb
mtime:today
mtime:yesterday
mtime:<7d
mtime:2026-01-01..2026-06-01
```

Negation works with name terms and filters:

```text
project !cache !ext:o !path:node_modules
```

The GUI filter panel can also set extension, file type, and path filters. Query filters and panel filters are combined with AND semantics.

Sort modes:

- **Relevance**: best default for normal searching.
- **Name**
- **Path**
- **Size**
- **Date**

Relevance is stateless. Oxidex does not store open history or frecency data.

To see how a query is interpreted:

```sh
oxidex-cli explain 'ext:rs path:src "scan stream" !target'
```

## Result Actions

Select a result in the table, then use the toolbar or right-click menu.

Available actions:

- **Open**: open the file or folder if the device is currently mounted.
- **Open Folder**: open the containing folder if mounted.
- **Copy Name**: copy only the file name.
- **Copy Path**: copy the displayed full path.
- **Rescan This Device**: start a new scan job for the selected result's device.
- **Forget This Index**: remove the saved index for that device.
- **Properties**: show detailed result and index metadata.

If a device is not mounted, text copy actions still work. Open actions require a current mount point.

## Ulauncher Workflow

Oxidex includes a Ulauncher v5 extension in `extensions/ulauncher-oxidex/`.
The extension connects directly to `oxidexd`, so it does not shell out through
`oxidex-cli` for every search.

Install it from the Oxidex repository root:

```sh
mkdir -p ~/.local/share/ulauncher/extensions
ln -s "$PWD/extensions/ulauncher-oxidex" \
  ~/.local/share/ulauncher/extensions/ulauncher-oxidex
```

Restart Ulauncher after installing or changing the extension. For debugging,
start Ulauncher from a terminal:

```sh
ulauncher -v
```

The default keyword is `ox`:

```text
ox main
ox ext:rs path:src
ox :scan
ox :rescan
ox :status
ox :help
```

Search results open an action menu. From there you can open the file, open the
containing folder, copy the resolved path, copy the file name, or queue a
rescan for the result's device.

The inline `:scan` and `:rescan` commands list known and indexed devices, then
queue an Oxidex scan for the selected device. Rescans still use the normal
privileged scanner path: `oxidexd` must be able to connect to
`/run/oxidex/scannerd.sock`, usually by having your user in the `oxidex` group.
Run `oxidex-cli doctor` if scans fail.

## Rofi And CLI Workflows

Basic search:

```sh
oxidex-cli search "ext:rs path:src main"
oxidex-cli search --json "project"
oxidex-cli search --limit 100 --sort relevance "main !target"
```

Rofi output:

```sh
oxidex-cli rofi "main"
oxidex-cli rofi --show-id "main"
oxidex-cli rofi --full-path "main"
```

Simple rofi command:

```sh
rofi -dmenu -i -p Oxidex < <(oxidex-cli rofi "$query")
```

A minimal script that lets rofi choose an item and then opens it:

```sh
#!/bin/sh
query="${*:-}"
selected="$(oxidex-cli rofi --show-id "$query" | rofi -dmenu -i -p Oxidex)"
[ -n "$selected" ] || exit 0
hit="$(printf '%s\n' "$selected" | awk -F '\t' '{print $NF}')"
oxidex-cli open --hit "$hit"
```

Resolve or open a known hit:

```sh
oxidex-cli resolve --hit 'partuuid:YOUR-ID:1234'
oxidex-cli resolve --json --hit 'partuuid:YOUR-ID:1234'
oxidex-cli open --hit 'partuuid:YOUR-ID:1234'
oxidex-cli open-folder --hit 'partuuid:YOUR-ID:1234'
oxidex-cli copy-path --hit 'partuuid:YOUR-ID:1234'
```

## Configuration

The config file is:

```text
$XDG_CONFIG_HOME/oxidex/config.toml
```

Usually this expands to:

```text
~/.config/oxidex/config.toml
```

Show the current config:

```sh
oxidex-cli config get
```

Set simple values:

```sh
oxidex-cli config set ui.theme dark
oxidex-cli config set ui.theme light
oxidex-cli config set ui.theme system
oxidex-cli config set ui.language zh_cn
oxidex-cli config set ui.language system
oxidex-cli config set search.default_sort relevance
oxidex-cli config set indexing.watch_mounted true
oxidex-cli config set rofi.max_results 200
```

Per-device include/exclude rules are edited in `config.toml`. Example:

```toml
[[devices]]
device_id = "partuuid:27f6c992-8ed7-4256-b021-d61db94d86e3"
enabled = true
display_name = "Main Linux"

[[devices.rules]]
kind = "exclude"
pattern = "/var/cache/**"

[[devices.rules]]
kind = "exclude"
pattern = "node_modules"

[[devices.rules]]
kind = "include"
pattern = "/home/hiroshi/**"
```

Rules are applied before snapshots are saved, so excluded paths are not persisted after a rescan or filtered rebuild.

## Troubleshooting

Start with:

```sh
oxidex-cli doctor
```

Common problems:

### Scanner Socket Permission Denied

Symptoms:

```text
permission denied connecting to scanner daemon socket
```

Check:

```sh
ls -l /run/oxidex/scannerd.sock
id -nG
```

The socket should normally be owned by `root:oxidex` and have mode `0660`. Your user should be in the `oxidex` group.

Fix:

```sh
sudo usermod -aG oxidex "$USER"
sudo systemctl enable --now oxidex-scannerd.socket
```

Then log out and log back in, or run:

```sh
newgrp oxidex
```

### Scanner Socket Is Missing

Symptoms:

```text
Scanner socket is not present at /run/oxidex/scannerd.sock
```

Fix:

```sh
sudo systemctl enable --now oxidex-scannerd.socket
oxidex-cli doctor --scanner
```

### Search Results Are Missing Or Stale

Try:

```sh
oxidex-cli indexes
oxidex-cli devices
oxidex-cli jobs
oxidex-cli doctor
```

Then rescan the affected device:

```sh
oxidex-cli scan partuuid:YOUR-PARTUUID --wait
```

If a mounted live update overflow happened, the index may be marked stale until a full rescan completes.

### Btrfs Notes

Current Btrfs support indexes the default/main root. Additional subvolumes and snapshots are not fully traversed in V4.

Multi-device Btrfs filesystems may be rejected by the raw scanner.

## Export Logs For Debugging

When reporting a bug, include a log bundle if possible. Logs may contain file paths, device labels, device IDs, and usernames, so review the bundle before sharing it publicly.

### One-Command Log Bundle

This command creates a compressed debug bundle in `/tmp`:

```sh
bundle="/tmp/oxidex-debug-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$bundle"

oxidex-cli doctor --json > "$bundle/doctor.json" 2> "$bundle/doctor.stderr" || true
oxidex-cli devices > "$bundle/devices.txt" 2> "$bundle/devices.stderr" || true
oxidex-cli indexes > "$bundle/indexes.txt" 2> "$bundle/indexes.stderr" || true
oxidex-cli jobs > "$bundle/jobs.txt" 2> "$bundle/jobs.stderr" || true
oxidex-cli config get > "$bundle/config.toml" 2> "$bundle/config.stderr" || true

systemctl --user status oxidexd.service > "$bundle/oxidexd-user-status.txt" 2>&1 || true
systemctl --user status oxidexd.socket > "$bundle/oxidexd-user-socket-status.txt" 2>&1 || true
systemctl status oxidex-scannerd.service > "$bundle/oxidex-scannerd-status.txt" 2>&1 || true
systemctl status oxidex-scannerd.socket > "$bundle/oxidex-scannerd-socket-status.txt" 2>&1 || true

journalctl --user -u oxidexd.service --since "2 hours ago" > "$bundle/oxidexd-user-journal.log" 2>&1 || true
journalctl -u oxidex-scannerd.service --since "2 hours ago" > "$bundle/oxidex-scannerd-journal.log" 2>&1 || true
journalctl --since "2 hours ago" | grep -i oxidex > "$bundle/oxidex-system-grep.log" 2>&1 || true

id > "$bundle/id.txt" 2>&1 || true
ls -l /run/oxidex /run/oxidex/scannerd.sock > "$bundle/scanner-socket.txt" 2>&1 || true
ls -l "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/oxidex" > "$bundle/user-runtime-socket.txt" 2>&1 || true

tar -C "$(dirname "$bundle")" -czf "$bundle.tar.gz" "$(basename "$bundle")"
echo "$bundle.tar.gz"
```

Attach the printed `.tar.gz` file to the bug report after reviewing it.

### Foreground Logs

If you can reproduce the bug manually, foreground logs are often easier to read.

Oxidex supports a terminal debug mode on the GUI and both daemons:

```sh
oxidex --debug
oxidexd --foreground --debug
sudo oxidex-scannerd --foreground --debug
```

`--debug` is shorthand for debug-level terminal logs. You can choose a level explicitly:

```sh
oxidex --log-level trace
oxidexd --foreground --log-level debug
sudo oxidex-scannerd --foreground --log-level trace
```

If `RUST_LOG` is set, it overrides `--log-level`, so this also works:

```sh
RUST_LOG=debug oxidex
RUST_LOG=trace oxidexd --foreground
```

When `oxidex --debug` auto-starts `oxidexd`, the daemon inherits the same terminal and also receives debug logging. In normal non-debug GUI startup, auto-started daemon output is still silenced so regular launches stay quiet.

Stop the packaged user daemon if it is running:

```sh
systemctl --user stop oxidexd.service oxidexd.socket
```

Run the user daemon in one terminal:

```sh
RUST_BACKTRACE=1 oxidexd --foreground --debug 2>&1 | tee /tmp/oxidexd.log
```

Run the scanner daemon in another terminal:

```sh
sudo env RUST_BACKTRACE=1 oxidex-scannerd --foreground --debug 2>&1 | tee /tmp/oxidex-scannerd.log
```

Run the GUI in a third terminal:

```sh
RUST_BACKTRACE=1 oxidex --debug 2>&1 | tee /tmp/oxidex-gui.log
```

Then reproduce the issue and collect:

```sh
tar -czf /tmp/oxidex-foreground-logs.tar.gz \
  /tmp/oxidexd.log \
  /tmp/oxidex-scannerd.log \
  /tmp/oxidex-gui.log
```

## Privacy Notes

Oxidex indexes file names and internal paths. Debug bundles and logs can reveal:

- user names
- mount points
- device IDs
- file and directory names
- config include/exclude rules
- scanner errors containing paths

Before posting logs publicly, review them and redact anything sensitive.
