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
ox 中
ox ext:rs path:src
ox :scan
ox :rescan
ox :status
ox :help
```

Normal filename search starts after the query contains at least 3 UTF-8 bytes.
This avoids daemon searches for one or two ASCII characters, while one typical
CJK character is enough to start searching. Inline commands beginning with `:`
are not subject to this threshold.

Search results open an action menu. From there you can open the file, open its
folder, copy the resolved path, copy the file name, or queue a rescan for the
result's device.

Rescans still require the normal Oxidex scanner setup: `oxidexd` must be able
to connect to `/run/oxidex/scannerd.sock`, usually through the `oxidex` group.
