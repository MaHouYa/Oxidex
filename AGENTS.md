# Repository Guidelines

## Project Structure & Module Organization

Kerything is a C++26 Qt/KDE application split into three binaries. The GUI lives in top-level files such as `main.cpp`, `MainWindow.*`, `FileModel.*`, `RemoteFileModel.*`, `PartitionDialog.*`, and `SettingsDialog.*`. The privileged scanner helper is built from `main_helper.cpp`, `ScannerEngine.h`, `ScannerUtils.*`, and parsers under `scanners/`. The DBus/system daemon lives in `kerythingd/`. Vendored UTF-8 helpers are in `lib/utf8/`; icons are in `icons/`. Packaging and desktop integration files are at the root: `PKGBUILD`, `net.reikooters.*`, and `kerythingd.service`.

## Build, Test, and Development Commands

Configure a release build:

```bash
cmake -B build -S . -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr -Wno-dev
```

Build all binaries:

```bash
cmake --build build
```

Install locally, including DBus, Polkit, systemd, desktop, and icons:

```bash
sudo cmake --install build
```

Build the Arch package from the working tree:

```bash
makepkg -si -f -c
```

There is no committed automated test suite. For scanner or daemon changes, validate with real NTFS/EXT4 devices or controlled test partitions.

## Coding Style & Naming Conventions

Follow the existing C++/Qt style: four-space indentation, same-line braces, `m_` prefixes for member fields, `PascalCase` for Qt classes, and `camelCase` for methods and local helpers. Keep headers focused and include Qt macros (`Q_OBJECT`, slots, signals) where needed. Prefer Qt types at UI/DBus boundaries and standard containers in scanner/indexing code. Do not reformat unrelated files.

## Testing Guidelines

When touching search or indexing, verify both correctness and performance on large indexes. Check query behavior, sorting by name/path/size/mtime, path resolution, drag/copy/open actions, and snapshot reloads. For daemon changes, verify DBus activation, `StartIndex`, `Search`, `ResolveEntries`, cancellation, and watch status transitions. For scanner changes, test both mounted and unmounted devices where possible.

## Commit & Pull Request Guidelines

Recent commits use concise descriptive summaries, for example `Added additional flags to fanotify mask` and `Remove debug std::cerr calls added in previous commit`. Use a clear one-line summary that states the user-visible or technical effect. Pull requests should include the motivation, touched subsystem, manual validation steps, and screenshots for GUI changes. Link related issues when available and note any behavior that requires root privileges, Polkit, DBus policy, or systemd activation.

## Security & Configuration Tips

Raw block-device access is privileged. Keep validation in `main_helper.cpp` strict: only supported filesystem types, absolute `/dev/...` block devices, and no unsafe path handling. Treat DBus and Polkit files as security-sensitive configuration. Avoid logging sensitive full paths unless needed for debugging.
