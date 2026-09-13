#!/usr/bin/env bash
# The version exists in more than one file and nothing generates it.
#
# flake.nix and the workflows read it from Cargo.toml, so they cannot drift.
# Cargo.lock and packaging/PKGBUILD can: the lockfile is cargo's to write, and
# the PKGBUILD is a standalone file that AUR users fetch on its own, with no
# Cargo.toml beside it to read. This asserts they agree.
#
# Cargo.lock is in the list for a reason that is not obvious: PKGBUILD builds
# with --frozen, so a Cargo.toml bump without `cargo update -w` fails the Arch
# build with an error that points at the lockfile rather than at the bump.
set -euo pipefail
cd "$(dirname "$0")/.."

want=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
rc=0

expect() { # name actual
  if [ "$2" != "$want" ]; then
    printf '%s is %s, Cargo.toml is %s\n' "$1" "$2" "$want" >&2
    rc=1
  fi
}

expect Cargo.lock \
  "$(awk '/^name = "starcord"$/{getline; gsub(/version = "|"/,""); print; exit}' Cargo.lock)"
expect packaging/PKGBUILD "$(sed -n 's/^pkgver=//p' packaging/PKGBUILD)"

grep -q 'cargoToml.package.version' flake.nix || {
  echo "flake.nix hard-codes a version; it should read Cargo.toml" >&2
  rc=1
}
if grep -rn '[0-9]\+\.[0-9]\+\.[0-9]\+' .github/workflows/ >/dev/null 2>&1; then
  echo "a workflow hard-codes a version:" >&2
  grep -rn '[0-9]\+\.[0-9]\+\.[0-9]\+' .github/workflows/ >&2
  rc=1
fi

# On a tag build the tag is one more copy, and the only one that cannot be
# corrected afterwards.
if [ "${GITHUB_REF_TYPE:-}" = tag ]; then
  expect "tag ${GITHUB_REF_NAME}" "${GITHUB_REF_NAME#v}"
fi

[ $rc -eq 0 ] && echo "version $want, consistent everywhere"
exit $rc
