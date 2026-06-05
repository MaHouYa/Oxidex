#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY_SRC="$ROOT_DIR/org.mahouya.oxidex.policy"
POLICY_DST="/usr/share/polkit-1/actions/org.mahouya.oxidex.policy"

sudo install -Dm644 "$POLICY_SRC" "$POLICY_DST"

if command -v systemctl >/dev/null 2>&1; then
  sudo systemctl restart polkit 2>/dev/null || sudo systemctl restart polkit.service 2>/dev/null || true
fi

if command -v pkaction >/dev/null 2>&1; then
  if pkaction | grep -qx 'org.mahouya.oxidex.connect-scanner'; then
    echo "Installed and registered org.mahouya.oxidex.connect-scanner"
  else
    echo "Installed policy, but pkaction does not list org.mahouya.oxidex.connect-scanner yet." >&2
    echo "Try restarting polkit or logging out/in, then run: pkaction | grep oxidex" >&2
    exit 1
  fi
else
  echo "Installed $POLICY_DST"
fi
