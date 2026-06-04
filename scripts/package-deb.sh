#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERSION="$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)"
PKG_NAME="kerything"
ARCH="amd64"
BUILD_ROOT="$ROOT_DIR/target/deb"
PKG_DIR="$BUILD_ROOT/${PKG_NAME}_${VERSION}_${ARCH}"
DIST_DIR="$ROOT_DIR/dist"
OUTPUT_DEB="$DIST_DIR/${PKG_NAME}_${VERSION}_${ARCH}.deb"

build_deb_without_dpkg() {
  local pkg_dir="$1"
  local output="$2"
  local tmp_dir="$BUILD_ROOT/manual-deb"

  rm -rf "$tmp_dir"
  mkdir -p "$tmp_dir"
  printf '2.0\n' >"$tmp_dir/debian-binary"

  (
    cd "$pkg_dir/DEBIAN"
    tar --owner=0 --group=0 --numeric-owner -czf "$tmp_dir/control.tar.gz" .
  )
  (
    cd "$pkg_dir"
    tar --owner=0 --group=0 --numeric-owner --exclude='./DEBIAN' -czf "$tmp_dir/data.tar.gz" .
  )

  rm -f "$output"
  (
    cd "$tmp_dir"
    ar rcs "$output" debian-binary control.tar.gz data.tar.gz
  )
}

rm -rf "$PKG_DIR"
mkdir -p \
  "$PKG_DIR/DEBIAN" \
  "$PKG_DIR/usr/bin" \
  "$PKG_DIR/usr/share/applications" \
  "$PKG_DIR/usr/share/icons/hicolor/16x16/apps" \
  "$PKG_DIR/usr/share/icons/hicolor/32x32/apps" \
  "$PKG_DIR/usr/share/icons/hicolor/48x48/apps" \
  "$PKG_DIR/usr/share/icons/hicolor/256x256/apps" \
  "$PKG_DIR/usr/share/doc/kerything" \
  "$PKG_DIR/usr/share/polkit-1/actions" \
  "$DIST_DIR"

install -Dm755 "$ROOT_DIR/target/release/kerything" "$PKG_DIR/usr/bin/kerything"
install -Dm755 "$ROOT_DIR/target/release/kerything-scanner-helper" "$PKG_DIR/usr/bin/kerything-scanner-helper"

install -Dm644 "$ROOT_DIR/net.reikooters.kerything.desktop" "$PKG_DIR/usr/share/applications/net.reikooters.kerything.desktop"
install -Dm644 "$ROOT_DIR/net.reikooters.kerything.policy" "$PKG_DIR/usr/share/polkit-1/actions/net.reikooters.kerything.policy"
install -Dm644 "$ROOT_DIR/LICENSE" "$PKG_DIR/usr/share/doc/kerything/copyright"

for size in 16 32 48 256; do
  install -Dm644 "$ROOT_DIR/icons/${size}-apps-kerything.png" "$PKG_DIR/usr/share/icons/hicolor/${size}x${size}/apps/kerything.png"
done

installed_size="$(du -sk "$PKG_DIR/usr" | cut -f1)"
cat >"$PKG_DIR/DEBIAN/control" <<EOF
Package: kerything
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Maintainer: Kerything Maintainers <kerything@example.invalid>
Installed-Size: $installed_size
Depends: libc6 (>= 2.31), libgcc-s1, policykit-1, xdg-utils, hicolor-icon-theme, libgl1, libx11-6, libxcb1, libxkbcommon0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libfontconfig1
Homepage: https://github.com/Reikooters/kerything
Description: Fast filename search for NTFS and EXT4 devices
 Kerything is a Linux desktop filename search utility built with Rust and egui.
 It keeps an unprivileged GUI and uses a small Polkit-authorized scanner helper
 for raw block-device metadata scans.
EOF

desktop-file-validate "$PKG_DIR/usr/share/applications/net.reikooters.kerything.desktop"
if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --build --root-owner-group "$PKG_DIR" "$OUTPUT_DEB"
else
  build_deb_without_dpkg "$PKG_DIR" "$OUTPUT_DEB"
fi
