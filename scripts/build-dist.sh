#!/usr/bin/env bash
# Build the supported package for this host. Linux AppImages use an isolated
# Bullseye target directory so they never replace the local Nix-built binary.
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:-$(if [ "$(uname -s)" = Darwin ]; then echo macos; else echo appimage; fi)}
case "$target" in
  nix) exec nix build .#default --print-build-logs ;;
  macos) exec scripts/dist/macos.sh ;;
  appimage) ;;
  *) echo "usage: $0 [nix|appimage|macos]" >&2; exit 2 ;;
esac
CONTAINER=${CONTAINER:-$(command -v docker || command -v podman || true)}
[ -n "$CONTAINER" ] || { echo "need docker or podman" >&2; exit 1; }
mkdir -p dist
"$CONTAINER" run --rm \
  -v "$PWD:/src" -w /src \
  -v starcord-target-appimage:/build/target \
  -v starcord-cargo:/root/.cargo \
  -e CARGO_TARGET_DIR=/build/target -e CARGO_HOME=/root/.cargo \
  -e DIST_UID="$(id -u)" -e DIST_GID="$(id -g)" \
  debian:bullseye-slim bash -c '
    cleanup() { chown -R "$DIST_UID:$DIST_GID" /src/dist; }
    trap cleanup EXIT
    scripts/dist/appimage.sh
  '
