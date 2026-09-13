#!/usr/bin/env bash
# The source tarball, reproducibly.
#
# This is what the PKGBUILD downloads and checksums. GitHub's generated
# /archive/ tarballs are not byte-stable -- their gzip implementation has
# changed under the AUR before and invalidated every checksum at once -- so the
# release ships one built here instead. `git archive` normalises mode and
# owner; `gzip -n` drops the timestamp.
set -euo pipefail
cd "$(dirname "$0")/../.."

ver=$(sed -n '0,/^version = /s/^version = "\(.*\)"/\1/p' Cargo.toml)
out=${DIST_DIR:-dist}
mkdir -p "$out"

git archive --format=tar --prefix="starcord-$ver/" HEAD \
  | gzip -n -9 > "$out/starcord-$ver.tar.gz"
echo "wrote $out/starcord-$ver.tar.gz"
