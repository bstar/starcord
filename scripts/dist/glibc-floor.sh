#!/usr/bin/env bash
# Assert the binary asks for no glibc newer than the floor we advertise.
#
# Portability is a claim the documentation makes in public, so it is tested
# rather than assumed. Building on a newer host is the one mistake that silently
# breaks it, and it does not show up until somebody else runs the file.
set -euo pipefail
bin=${1:?usage: glibc-floor.sh <binary> [max]}
max=${2:-${GLIBC_FLOOR:-2.31}}

asked=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1)
highest=$(printf '%s\n%s\n' "$asked" "$max" | sort -V | tail -1)

if [ "$highest" != "$max" ]; then
  echo "$bin needs glibc $asked, above the $max floor this artifact promises" >&2
  exit 1
fi
echo "glibc floor ok: needs at most $asked, promises $max"
