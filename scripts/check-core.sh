#!/usr/bin/env bash
# Candidate compatibility check in a disposable copy; keeps release locks portable.
set -euo pipefail
app_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
core_root="${STAR_CORE_CHECKOUT:-$app_root/../starcore}"
cd "$app_root"
exec nix --extra-experimental-features 'nix-command flakes' develop -c python3 "$core_root/scripts/check-consumer.py" "$app_root" --core "$core_root" "$@"
