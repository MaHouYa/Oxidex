# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026  Honghao <https://github.com/Rasphino>
# Copyright (C) 2026  Reikooters <https://github.com/Reikooters>


pkgname=oxidex
pkgver=2.3.0
pkgrel=1
pkgdesc="Oxidex, a fast Rust/egui filename search app for NTFS, EXT3, EXT4, and Btrfs devices"
arch=('x86_64')
url="https://github.com/MaHouYa/Oxidex"
license=('GPL-3.0-or-later')
depends=('gcc-libs' 'glibc' 'libx11' 'libxcb' 'libxkbcommon' 'wayland' 'libglvnd' 'fontconfig' 'xdg-utils' 'hicolor-icon-theme')
makedepends=('cargo' 'clang')
optdepends=('noto-fonts-cjk: CJK glyph fallback'
            'wqy-microhei: alternative Chinese glyph fallback'
            'fcitx5: input method support'
            'fcitx5-chinese-addons: Chinese input engines for Fcitx5'
            'ibus: input method support'
            'ibus-libpinyin: Chinese input engine for IBus'
            'ibus-rime: Rime input engine for IBus')
install=oxidex.install

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
  install -Dm755 "$srcdir/target/release/oxidex" "$pkgdir/usr/bin/oxidex"
  install -Dm755 "$srcdir/target/release/oxidex-cli" "$pkgdir/usr/bin/oxidex-cli"
  install -Dm755 "$srcdir/target/release/oxidexd" "$pkgdir/usr/bin/oxidexd"
  install -Dm755 "$srcdir/target/release/oxidex-scannerd" "$pkgdir/usr/bin/oxidex-scannerd"

  install -Dm644 "$startdir/org.mahouya.oxidex.desktop" "$pkgdir/usr/share/applications/org.mahouya.oxidex.desktop"
  install -Dm644 "$startdir/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "$startdir/systemd/user/oxidexd.service" "$pkgdir/usr/lib/systemd/user/oxidexd.service"
  install -Dm644 "$startdir/systemd/user/oxidexd.socket" "$pkgdir/usr/lib/systemd/user/oxidexd.socket"
  install -Dm644 "$startdir/systemd/system/oxidex-scannerd.service" "$pkgdir/usr/lib/systemd/system/oxidex-scannerd.service"
  install -Dm644 "$startdir/systemd/system/oxidex-scannerd.socket" "$pkgdir/usr/lib/systemd/system/oxidex-scannerd.socket"

  for size in 16 32 48 256; do
    install -Dm644 "$startdir/icons/${size}-apps-oxidex.png" "$pkgdir/usr/share/icons/hicolor/${size}x${size}/apps/oxidex.png"
  done
}
