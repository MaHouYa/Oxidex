# Oxidex Ulauncher Extension

This is a Ulauncher v5 extension for searching Oxidex indexes through the
per-user `oxidexd` daemon.

## Install

From the Oxidex repository root:

```sh
mkdir -p ~/.local/share/ulauncher/extensions
ln -s "$PWD/extensions/ulauncher-oxidex" \
  ~/.local/share/ulauncher/extensions/ulauncher-oxidex
```

Restart Ulauncher, or run `ulauncher -v` from a terminal while testing.

## Usage

The default keyword is `ox`.

```text
ox main
ox ext:rs path:src
ox :scan
ox :rescan
ox :status
ox :help
```

Search results open an action menu. From there you can open the file, open its
folder, copy the resolved path, copy the file name, or queue a rescan for the
result's device.

Rescans still require the normal Oxidex scanner setup: `oxidexd` must be able
to connect to `/run/oxidex/scannerd.sock`, usually through the `oxidex` group.
