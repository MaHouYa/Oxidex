# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026  Reikooters <https://github.com/Reikooters>

pkgname=kerything
pkgver=1.4.1
pkgrel=1
pkgdesc="Fast Rust/egui filename search for NTFS and EXT4 devices"
arch=('x86_64')
url="https://github.com/Reikooters/kerything"
license=('GPL-3.0-or-later')
depends=('gcc-libs' 'glibc' 'polkit' 'libx11' 'libxcb' 'libxkbcommon' 'wayland' 'libglvnd' 'vulkan-icd-loader' 'fontconfig' 'xdg-utils' 'hicolor-icon-theme')
makedepends=('cargo')

# Disable the creation of the -debug package.
options=('!debug')

#source=("git+${url}.git#tag=v${pkgver}")
#sha256sums=('SKIP')

# By leaving source empty, makepkg expects to be run in the directory with the files
source=()
sha256sums=()

build() {
  cd "$startdir"
  export CARGO_TARGET_DIR="$srcdir/target"
  cargo build --release --locked --workspace
}

package() {
  install -Dm755 "$srcdir/target/release/kerything" "$pkgdir/usr/bin/kerything"
  install -Dm755 "$srcdir/target/release/kerything-scanner-helper" "$pkgdir/usr/bin/kerything-scanner-helper"

  install -Dm644 "$startdir/net.reikooters.kerything.desktop" "$pkgdir/usr/share/applications/net.reikooters.kerything.desktop"
  install -Dm644 "$startdir/net.reikooters.kerything.policy" "$pkgdir/usr/share/polkit-1/actions/net.reikooters.kerything.policy"
  install -Dm644 "$startdir/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"

  for size in 16 32 48 256; do
    install -Dm644 "$startdir/icons/${size}-apps-kerything.png" "$pkgdir/usr/share/icons/hicolor/${size}x${size}/apps/kerything.png"
  done
}
