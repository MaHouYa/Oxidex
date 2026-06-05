# Kerything User Guide

This guide is for people using Kerything as a desktop filename search app. It covers installation setup, indexing disks, searching, rofi/CLI usage, troubleshooting, and exporting logs for debugging.

Kerything has three main pieces:

- `kerything`: the graphical search app.
- `kerythingd`: your unprivileged per-user search daemon.
- `kerything-scannerd`: the privileged scanner daemon used only when Kerything needs to read raw filesystem metadata from `/dev/...`.

The GUI and user daemon do not run as root. When a raw disk scan is needed, Polkit authorizes the scanner daemon.

## First-Time Setup

After installing Kerything from a package, make sure your user can connect to the scanner daemon socket:

```sh
sudo usermod -aG kerything "$USER"
```

Then log out and log back in. For a temporary current-terminal session, you can run:

```sh
newgrp kerything
```

Check the setup:

```sh
kerything-cli doctor
```

You want the scanner socket, Polkit action, config, and index checks to be `OK`. A warning about the daemon not running is usually harmless before the first GUI launch.

## Starting Kerything

Start the GUI from your desktop launcher or terminal:

```sh
kerything
```

By default the GUI connects to `kerythingd`. If the daemon is not already running, the GUI or CLI will try to start it.

For recovery or development, you can run the old in-process mode:

```sh
kerything --standalone
```

Standalone mode is useful if the daemon socket or service setup is broken, but normal use should go through `kerythingd`.

## Optional Systemd Socket Activation

Packages install systemd socket units. Socket activation lets systemd start daemons only when something connects.

Enable the user daemon socket:

```sh
systemctl --user enable --now kerythingd.socket
```

Enable the privileged scanner socket:

```sh
sudo systemctl enable --now kerything-scannerd.socket
```

Check them:

```sh
systemctl --user status kerythingd.socket
systemctl status kerything-scannerd.socket
```

The GUI and CLI can also start `kerythingd` themselves if the user socket is not active. The scanner daemon should normally be reached through `/run/kerything/scannerd.sock`; if it is unavailable, Kerything may fall back to the compatibility helper.

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
kerything-cli devices
kerything-cli scan partuuid:YOUR-PARTUUID --wait
kerything-cli jobs
kerything-cli cancel 1
```

## Mounted Live Updates

After a full raw scan, Kerything can keep mounted indexes fresher using normal Linux filename notifications.

This means:

- Creating a file on a mounted indexed device can appear without a full rescan.
- Deleting a file can remove it from results.
- Renaming files and directories can update paths.
- Metadata changes can refresh size and modified time.

This is not raw filesystem delta scanning. If a device is unmounted, changed outside Linux, or if the notification queue overflows, Kerything marks the index stale and you should rescan.

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

Relevance is stateless. Kerything does not store open history or frecency data.

To see how a query is interpreted:

```sh
kerything-cli explain 'ext:rs path:src "scan stream" !target'
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
kerything-cli search "ext:rs path:src main"
kerything-cli search --json "project"
kerything-cli search --limit 100 --sort relevance "main !target"
```

Rofi output:

```sh
kerything-cli rofi "main"
kerything-cli rofi --show-id "main"
kerything-cli rofi --full-path "main"
```

Simple rofi command:

```sh
rofi -dmenu -i -p Kerything < <(kerything-cli rofi "$query")
```

A minimal script that lets rofi choose an item and then opens it:

```sh
#!/bin/sh
query="${*:-}"
selected="$(kerything-cli rofi --show-id "$query" | rofi -dmenu -i -p Kerything)"
[ -n "$selected" ] || exit 0
hit="$(printf '%s\n' "$selected" | awk -F '\t' '{print $NF}')"
kerything-cli open --hit "$hit"
```

Resolve or open a known hit:

```sh
kerything-cli resolve --hit 'partuuid:YOUR-ID:1234'
kerything-cli resolve --json --hit 'partuuid:YOUR-ID:1234'
kerything-cli open --hit 'partuuid:YOUR-ID:1234'
kerything-cli open-folder --hit 'partuuid:YOUR-ID:1234'
kerything-cli copy-path --hit 'partuuid:YOUR-ID:1234'
```

## Configuration

The config file is:

```text
$XDG_CONFIG_HOME/kerything/config.toml
```

Usually this expands to:

```text
~/.config/kerything/config.toml
```

Show the current config:

```sh
kerything-cli config get
```

Set simple values:

```sh
kerything-cli config set ui.theme dark
kerything-cli config set ui.theme light
kerything-cli config set ui.theme system
kerything-cli config set search.default_sort relevance
kerything-cli config set indexing.watch_mounted true
kerything-cli config set rofi.max_results 200
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
kerything-cli doctor
```

Common problems:

### Scanner Socket Permission Denied

Symptoms:

```text
permission denied connecting to scanner daemon socket
```

Check:

```sh
ls -l /run/kerything/scannerd.sock
id -nG
```

The socket should normally be owned by `root:kerything` and have mode `0660`. Your user should be in the `kerything` group.

Fix:

```sh
sudo usermod -aG kerything "$USER"
```

Then log out and log back in, or run:

```sh
newgrp kerything
```

### Polkit Action Is Not Registered

Symptoms:

```text
Action net.reikooters.kerything.connect-scanner is not registered
```

Check:

```sh
pkaction | grep kerything
```

You should see:

```text
net.reikooters.kerything.connect-scanner
net.reikooters.kerything.run-scanner
```

If you are running from the source tree for development, install the local Polkit policy:

```sh
scripts/dev-install-polkit.sh
```

If you installed a package, reinstall the package or verify that this file exists:

```text
/usr/share/polkit-1/actions/net.reikooters.kerything.policy
```

### Polkit Prompt Does Not Appear

Make sure your desktop session has a Polkit authentication agent running. Many desktop environments start one automatically.

You can test Polkit manually:

```sh
pkcheck \
  --action-id net.reikooters.kerything.connect-scanner \
  --process $$ \
  --allow-user-interaction
```

### Search Results Are Missing Or Stale

Try:

```sh
kerything-cli indexes
kerything-cli devices
kerything-cli jobs
kerything-cli doctor
```

Then rescan the affected device:

```sh
kerything-cli scan partuuid:YOUR-PARTUUID --wait
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
bundle="/tmp/kerything-debug-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$bundle"

kerything-cli doctor --json > "$bundle/doctor.json" 2> "$bundle/doctor.stderr" || true
kerything-cli devices > "$bundle/devices.txt" 2> "$bundle/devices.stderr" || true
kerything-cli indexes > "$bundle/indexes.txt" 2> "$bundle/indexes.stderr" || true
kerything-cli jobs > "$bundle/jobs.txt" 2> "$bundle/jobs.stderr" || true
kerything-cli config get > "$bundle/config.toml" 2> "$bundle/config.stderr" || true

systemctl --user status kerythingd.service > "$bundle/kerythingd-user-status.txt" 2>&1 || true
systemctl --user status kerythingd.socket > "$bundle/kerythingd-user-socket-status.txt" 2>&1 || true
systemctl status kerything-scannerd.service > "$bundle/kerything-scannerd-status.txt" 2>&1 || true
systemctl status kerything-scannerd.socket > "$bundle/kerything-scannerd-socket-status.txt" 2>&1 || true

journalctl --user -u kerythingd.service --since "2 hours ago" > "$bundle/kerythingd-user-journal.log" 2>&1 || true
journalctl -u kerything-scannerd.service --since "2 hours ago" > "$bundle/kerything-scannerd-journal.log" 2>&1 || true
journalctl --since "2 hours ago" | grep -i kerything > "$bundle/kerything-system-grep.log" 2>&1 || true

pkaction | grep kerything > "$bundle/polkit-actions.txt" 2>&1 || true
id > "$bundle/id.txt" 2>&1 || true
ls -l /run/kerything /run/kerything/scannerd.sock > "$bundle/scanner-socket.txt" 2>&1 || true
ls -l "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/kerything" > "$bundle/user-runtime-socket.txt" 2>&1 || true

tar -C "$(dirname "$bundle")" -czf "$bundle.tar.gz" "$(basename "$bundle")"
echo "$bundle.tar.gz"
```

Attach the printed `.tar.gz` file to the bug report after reviewing it.

### Foreground Logs

If you can reproduce the bug manually, foreground logs are often easier to read.

Stop the packaged user daemon if it is running:

```sh
systemctl --user stop kerythingd.service kerythingd.socket
```

Run the user daemon in one terminal:

```sh
RUST_BACKTRACE=1 kerythingd --foreground 2>&1 | tee /tmp/kerythingd.log
```

Run the scanner daemon in another terminal:

```sh
sudo env RUST_BACKTRACE=1 kerything-scannerd --foreground 2>&1 | tee /tmp/kerything-scannerd.log
```

Run the GUI in a third terminal:

```sh
RUST_BACKTRACE=1 kerything 2>&1 | tee /tmp/kerything-gui.log
```

Then reproduce the issue and collect:

```sh
tar -czf /tmp/kerything-foreground-logs.tar.gz \
  /tmp/kerythingd.log \
  /tmp/kerything-scannerd.log \
  /tmp/kerything-gui.log
```

### Scanner Helper Logs

The compatibility helper writes binary scan data to stdout, so do not paste stdout into bug reports. If you need helper diagnostics, redirect stdout to a file and attach only stderr unless asked:

```sh
pkexec kerything-scanner-helper /dev/YOUR_DEVICE ext4 \
  > /tmp/kerything-scan.bin \
  2> /tmp/kerything-helper.log
```

Usually `/tmp/kerything-helper.log` is enough. The `.bin` file can be large and may indirectly reveal filesystem metadata, so do not share it publicly unless a maintainer asks for it.

## Privacy Notes

Kerything indexes file names and internal paths. Debug bundles and logs can reveal:

- user names
- mount points
- device IDs
- file and directory names
- config include/exclude rules
- scanner errors containing paths

Before posting logs publicly, review them and redact anything sensitive.
