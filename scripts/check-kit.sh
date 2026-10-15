#!/usr/bin/env bash
# Candidate compatibility check in a disposable copy; keeps release locks portable.
set -euo pipefail
app_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
kit_root="${STAR_KIT_CHECKOUT:-$app_root/../starkit}"
cd "$app_root"
exec nix --extra-experimental-features 'nix-command flakes' develop -c python3 "$kit_root/scripts/check-consumer.py" "$app_root" --kit "$kit_root" "$@"
