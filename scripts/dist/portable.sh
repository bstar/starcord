#!/usr/bin/env bash
# The portable tarball. Run inside the oldest glibc container we support.
#
# Not built on the host: a NixOS binary asks for /nix/store/.../ld-linux.so and
# starts on no other machine, and a build on a current runner needs a glibc
# newer than the .deb's own audience. Building against bullseye's 2.31 covers
# Debian 11+, Ubuntu 20.04+, RHEL 9+ and Fedora 34+.
set -euo pipefail
cd "$(dirname "$0")/../.."

. scripts/dist/deps-debian.sh
. "$HOME/.cargo/env"

ver=$(sed -n '0,/^version = /s/^version = "\(.*\)"/\1/p' Cargo.toml)
out=${DIST_DIR:-dist}
mkdir -p "$out"

# --locked, not --frozen: one dependency comes from a git tag rather than from
# crates.io, and the container has no fetched copy of it yet.
cargo build --release --locked
# Not `target/`: the container is handed its own CARGO_TARGET_DIR so it cannot
# leave a Debian binary where the host's next `cargo run` expects a native one.
bin="${CARGO_TARGET_DIR:-target}/release/starcord"

scripts/dist/glibc-floor.sh "$bin"

stage="starcord-$ver"
rm -rf "/tmp/$stage" && mkdir -p "/tmp/$stage"
install -Dm755 "$bin" "/tmp/$stage/starcord"
install -Dm644 README.md LICENSE -t "/tmp/$stage/"

# A prefix, not a tarbomb: this lands in someone's Downloads directory.
tar --sort=name --owner=0 --group=0 --numeric-owner \
    -C /tmp -czf "$out/starcord-$ver-x86_64-linux-gnu.tar.gz" "$stage"
echo "wrote $out/starcord-$ver-x86_64-linux-gnu.tar.gz"
