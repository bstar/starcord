#!/usr/bin/env bash
# Cargo.toml is authoritative. Check its lockfile copy and any release tag.
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
