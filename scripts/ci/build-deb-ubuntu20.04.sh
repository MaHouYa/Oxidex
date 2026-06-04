#!/usr/bin/env bash
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

apt-get update
apt-get install -y --no-install-recommends \
  ca-certificates \
  build-essential \
  pkg-config \
  libssl-dev \
  libx11-dev \
  libxcb1-dev \
  libxkbcommon-dev \
  libwayland-dev \
  libgl1-mesa-dev \
  libfontconfig1-dev \
  libxi-dev \
  libxcursor-dev \
  libxrandr-dev \
  libxinerama-dev \
  libasound2-dev \
  desktop-file-utils \
  dpkg-dev \
  file

command -v cargo >/dev/null 2>&1 || {
  echo "cargo is not installed; run this script after setting up a Rust toolchain" >&2
  exit 127
}

cargo build --release --locked --workspace
scripts/package-deb.sh
