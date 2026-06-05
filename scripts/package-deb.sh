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
  "$PKG_DIR/usr/lib/systemd/user" \
  "$PKG_DIR/usr/lib/systemd/system" \
  "$DIST_DIR"

install -Dm755 "$ROOT_DIR/target/release/kerything" "$PKG_DIR/usr/bin/kerything"
install -Dm755 "$ROOT_DIR/target/release/kerything-cli" "$PKG_DIR/usr/bin/kerything-cli"
install -Dm755 "$ROOT_DIR/target/release/kerythingd" "$PKG_DIR/usr/bin/kerythingd"
install -Dm755 "$ROOT_DIR/target/release/kerything-scannerd" "$PKG_DIR/usr/bin/kerything-scannerd"
install -Dm755 "$ROOT_DIR/target/release/kerything-scanner-helper" "$PKG_DIR/usr/bin/kerything-scanner-helper"

install -Dm644 "$ROOT_DIR/net.reikooters.kerything.desktop" "$PKG_DIR/usr/share/applications/net.reikooters.kerything.desktop"
install -Dm644 "$ROOT_DIR/net.reikooters.kerything.policy" "$PKG_DIR/usr/share/polkit-1/actions/net.reikooters.kerything.policy"
install -Dm644 "$ROOT_DIR/LICENSE" "$PKG_DIR/usr/share/doc/kerything/copyright"
install -Dm644 "$ROOT_DIR/systemd/user/kerythingd.service" "$PKG_DIR/usr/lib/systemd/user/kerythingd.service"
install -Dm644 "$ROOT_DIR/systemd/user/kerythingd.socket" "$PKG_DIR/usr/lib/systemd/user/kerythingd.socket"
install -Dm644 "$ROOT_DIR/systemd/system/kerything-scannerd.service" "$PKG_DIR/usr/lib/systemd/system/kerything-scannerd.service"
install -Dm644 "$ROOT_DIR/systemd/system/kerything-scannerd.socket" "$PKG_DIR/usr/lib/systemd/system/kerything-scannerd.socket"

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
Depends: libc6 (>= 2.31), libgcc-s1, policykit-1, adduser, xdg-utils, hicolor-icon-theme, libgl1, libx11-6, libxcb1, libxkbcommon0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libfontconfig1
Homepage: https://github.com/Reikooters/kerything
Description: Fast filename search for NTFS, EXT4, and Btrfs devices
 Kerything is a Linux desktop filename search utility built with Rust and egui.
 It keeps an unprivileged GUI/user daemon and uses a small Polkit-authorized
 scanner daemon for raw block-device metadata scans.
EOF

cat >"$PKG_DIR/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e

if command -v addgroup >/dev/null 2>&1; then
  addgroup --system kerything >/dev/null 2>&1 || true
elif command -v groupadd >/dev/null 2>&1; then
  groupadd -r kerything >/dev/null 2>&1 || true
fi

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi

cat <<'MSG'
Kerything installed.

To let your user daemon connect to the privileged scanner daemon, add your user
to the kerything group, then log out and back in or run newgrp:

  sudo usermod -aG kerything "$USER"
  newgrp kerything

Check the setup with:

  kerything-cli doctor
MSG

exit 0
EOF
chmod 755 "$PKG_DIR/DEBIAN/postinst"

desktop-file-validate "$PKG_DIR/usr/share/applications/net.reikooters.kerything.desktop"
if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --build --root-owner-group "$PKG_DIR" "$OUTPUT_DEB"
else
  build_deb_without_dpkg "$PKG_DIR" "$OUTPUT_DEB"
fi
