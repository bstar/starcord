#!/usr/bin/env bash
# Native Apple Silicon release package. Run on a macOS builder.
set -euo pipefail
cd "$(dirname "$0")/../.."
[ "$(uname -s)" = Darwin ] || { echo "macOS build requires a Mac" >&2; exit 1; }
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
out=${DIST_DIR:-dist}
mkdir -p "$out"
cargo build --release --locked --target aarch64-apple-darwin
bin="${CARGO_TARGET_DIR:-target}/aarch64-apple-darwin/release/starcord"
"$bin" --version
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage="$work/starcord-$ver"
mkdir -p "$stage"
install -m755 "$bin" "$stage/starcord"
cp README.md LICENSE "$stage/"
[ ! -f NOTICE ] || cp NOTICE "$stage/"
[ ! -d LICENSES ] || cp -R LICENSES "$stage/"
tar -C "$work" -czf "$out/starcord-$ver-aarch64-apple-darwin.tar.gz" "starcord-$ver"
echo "wrote $out/starcord-$ver-aarch64-apple-darwin.tar.gz"
