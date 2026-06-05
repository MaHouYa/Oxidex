# Oxidex User Guide

This guide is for people using Oxidex as a desktop filename search app. It covers installation setup, indexing disks, searching, rofi/CLI usage, troubleshooting, and exporting logs for debugging.

Oxidex has three main pieces:

- `oxidex`: the graphical search app.
- `oxidexd`: your unprivileged per-user search daemon.
- `oxidex-scannerd`: the privileged scanner daemon used only when Oxidex needs to read raw filesystem metadata from `/dev/...`.

The GUI and user daemon do not run as root. When a raw disk scan is needed, Polkit authorizes the scanner daemon.

The app has been rebranded to Oxidex, but the current compatibility release keeps the existing `oxidex` command names, config paths, index paths, and scanner group name.

## First-Time Setup

After installing Oxidex from a package, make sure your user can connect to the scanner daemon socket:

```sh
sudo usermod -aG oxidex "$USER"
```

Then log out and log back in. For a temporary current-terminal session, you can run:

```sh
newgrp oxidex
```

Check the setup:

```sh
oxidex-cli doctor
```

You want the scanner socket, Polkit action, config, and index checks to be `OK`. A warning about the daemon not running is usually harmless before the first GUI launch.

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

The GUI and CLI can also start `oxidexd` themselves if the user socket is not active. The scanner daemon should normally be reached through `/run/oxidex/scannerd.sock`; if it is unavailable, Oxidex may fall back to the compatibility helper.

## Indexing A Disk

Open the GUI and click **Indexes**.

The Indexes window shows known NTFS, EXT4, and Btrfs devices. For each device, it shows the filesystem type, mount state, device node, entry count, scan jobs, and recent errors when available.

To index a device:

1. Click **Index** beside the device.
2. Approve the Polkit prompt if asked.
3. Wait for the job progress to finish.
4. Search results become available immediately after the snapshot is saved.

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
```

Then log out and log back in, or run:

```sh
newgrp oxidex
```

### Polkit Action Is Not Registered

Symptoms:

```text
Action org.mahouya.oxidex.connect-scanner is not registered
```

Check:

```sh
pkaction | grep oxidex
```

You should see:

```text
org.mahouya.oxidex.connect-scanner
org.mahouya.oxidex.run-scanner
```

If you are running from the source tree for development, install the local Polkit policy:

```sh
scripts/dev-install-polkit.sh
```

If you installed a package, reinstall the package or verify that this file exists:

```text
/usr/share/polkit-1/actions/org.mahouya.oxidex.policy
```

### Polkit Prompt Does Not Appear

Make sure your desktop session has a Polkit authentication agent running. Many desktop environments start one automatically.

You can test Polkit manually:

```sh
pkcheck \
  --action-id org.mahouya.oxidex.connect-scanner \
  --process $$ \
  --allow-user-interaction
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

pkaction | grep oxidex > "$bundle/polkit-actions.txt" 2>&1 || true
id > "$bundle/id.txt" 2>&1 || true
ls -l /run/oxidex /run/oxidex/scannerd.sock > "$bundle/scanner-socket.txt" 2>&1 || true
ls -l "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/oxidex" > "$bundle/user-runtime-socket.txt" 2>&1 || true

tar -C "$(dirname "$bundle")" -czf "$bundle.tar.gz" "$(basename "$bundle")"
echo "$bundle.tar.gz"
```

Attach the printed `.tar.gz` file to the bug report after reviewing it.

### Foreground Logs

If you can reproduce the bug manually, foreground logs are often easier to read.

Stop the packaged user daemon if it is running:

```sh
systemctl --user stop oxidexd.service oxidexd.socket
```

Run the user daemon in one terminal:

```sh
RUST_BACKTRACE=1 oxidexd --foreground 2>&1 | tee /tmp/oxidexd.log
```

Run the scanner daemon in another terminal:

```sh
sudo env RUST_BACKTRACE=1 oxidex-scannerd --foreground 2>&1 | tee /tmp/oxidex-scannerd.log
```

Run the GUI in a third terminal:

```sh
RUST_BACKTRACE=1 oxidex 2>&1 | tee /tmp/oxidex-gui.log
```

Then reproduce the issue and collect:

```sh
tar -czf /tmp/oxidex-foreground-logs.tar.gz \
  /tmp/oxidexd.log \
  /tmp/oxidex-scannerd.log \
  /tmp/oxidex-gui.log
```

### Scanner Helper Logs

The compatibility helper writes binary scan data to stdout, so do not paste stdout into bug reports. If you need helper diagnostics, redirect stdout to a file and attach only stderr unless asked:

```sh
pkexec oxidex-scanner-helper /dev/YOUR_DEVICE ext4 \
  > /tmp/oxidex-scan.bin \
  2> /tmp/oxidex-helper.log
```

Usually `/tmp/oxidex-helper.log` is enough. The `.bin` file can be large and may indirectly reveal filesystem metadata, so do not share it publicly unless a maintainer asks for it.

## Privacy Notes

Oxidex indexes file names and internal paths. Debug bundles and logs can reveal:

- user names
- mount points
- device IDs
- file and directory names
- config include/exclude rules
- scanner errors containing paths

Before posting logs publicly, review them and redact anything sensitive.
