#!/usr/bin/env bash
# The AppImage. Run inside the same old-glibc container as portable.sh.
#
# What it carries is, today, nothing: this binary links the C runtime and
# libgcc and that is the whole of its NEEDED list, so the library walk below
# finds nothing worth bundling and the AppDir's usr/lib comes out empty. The
# walk is here anyway, because the first dependency that picks up a `-sys`
# crate should travel with the file rather than be discovered by somebody on
# another distribution.
#
# It is still the right artifact to ship. One file, executable, with its icon
# and its desktop entry inside it, built against a glibc old enough for
# everything still supported -- which is what makes "download it and run it"
# true without a package for each distribution.
#
# Assembled by hand rather than with linuxdeploy, because linuxdeploy ships as
# an AppImage and an AppImage cannot be executed inside a container without
# FUSE. Doing it directly is a dozen lines, needs nothing that has to run, and
# leaves the exclude list somewhere it can be read.
set -euo pipefail
cd "$(dirname "$0")/../.."

. scripts/dist/deps-debian.sh
. "$HOME/.cargo/env"
apt-get install -y -qq --no-install-recommends patchelf squashfs-tools

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

work=$(mktemp -d)
appdir=$work/AppDir
install -Dm755 "$bin"                         "$appdir/usr/bin/starcord"
install -Dm644 packaging/starcord.desktop     "$appdir/starcord.desktop"
install -Dm644 packaging/starcord.png         "$appdir/starcord.png"
install -Dm644 packaging/starcord.desktop     "$appdir/usr/share/applications/starcord.desktop"
install -Dm644 packaging/starcord.png         "$appdir/usr/share/icons/hicolor/256x256/apps/starcord.png"
install -Dm644 packaging/starcord.svg         "$appdir/usr/share/icons/hicolor/scalable/apps/starcord.svg"
install -Dm644 README.md LICENSE -t           "$appdir/usr/share/doc/starcord/"
cp "$appdir/starcord.png" "$appdir/.DirIcon"
mkdir -p "$appdir/usr/lib"

# What stays behind, and why.
#
#   the C runtime    bundling a C library into an AppImage is how they break.
#   libgcc_s,        the host's is never older than bullseye's, and a bundled
#   libstdc++        old one is a real hazard on a newer host.
#
# Nothing else is on this list. There is no libasound here to argue about and
# no libav sonames to chase; the reason this file exists is the glibc floor,
# not a bundled decoder.
keep_out='^(ld-linux|libc\.so|libm\.so|libdl\.so|libpthread\.so|librt\.so|libresolv\.so|libutil\.so|libnsl\.so|libgcc_s\.so|libstdc\+\+\.so)'

# Walk NEEDED transitively. ldd on the binary already reports the whole graph,
# so one pass is enough; the loop is over what it found, not over levels.
ldd "$appdir/usr/bin/starcord" | awk '{print $3}' | grep -E '^/' | sort -u | while read -r lib; do
  base=$(basename "$lib")
  if echo "$base" | grep -qE "$keep_out"; then
    echo "host:   $base"
    continue
  fi
  echo "bundle: $base"
  cp -L "$lib" "$appdir/usr/lib/"
done

# The loader resolves any bundled copies with no environment set at all, and
# RUNPATH beats ld.so.cache, so a host library cannot shadow ours even when the
# soname matches exactly. Harmless while usr/lib is empty, and correct the day
# it is not.
patchelf --set-rpath '$ORIGIN/../lib' "$appdir/usr/bin/starcord"
for so in "$appdir"/usr/lib/*.so*; do
  [ -e "$so" ] || continue
  patchelf --set-rpath '$ORIGIN' "$so"
done

cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
# The binary's RUNPATH is $ORIGIN/../lib, so no LD_LIBRARY_PATH is needed and
# none is exported: nothing this launches should inherit our library path. It
# launches a media player and a browser, both with the user's own environment.
#
# "$@" is not optional. `starcord probe` and its dozen flags are the whole of
# the headless interface, and an AppImage that swallowed argv would be useless
# for every one of them.
HERE=$(dirname "$(readlink -f "$0")")
exec "$HERE/usr/bin/starcord" "$@"
EOF
chmod +x "$appdir/AppRun"

# The type-2 runtime, downloaded rather than executed: this is the small ELF
# that gets prepended to the filesystem image and does the mounting at run
# time. Nothing here has to run it, which is the point.
#
# Pinned to a dated tag and checksummed, because it is the first code that runs
# when anybody opens this AppImage. `continuous` is a rolling tag GitHub
# rewrites in place: a build that consumes it produces an artifact nobody can
# reproduce, and the provenance attestation would faithfully attest a build
# that pulled in whatever was at that URL on the day. The checksum is verified
# before the file is made executable, so bad bytes never reach `cat` below.
#
# To move it: pick a tag from
# https://github.com/AppImage/type2-runtime/releases, download its
# runtime-x86_64, and put its `sha256sum` here.
runtime_tag=20251108
runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

curl -fsSL -o "$work/runtime" \
  "https://github.com/AppImage/type2-runtime/releases/download/$runtime_tag/runtime-x86_64"
echo "$runtime_sha256  $work/runtime" | sha256sum -c -
chmod +x "$work/runtime"

# gzip rather than zstd: every AppImage runtime in the wild can read it, and
# this file is meant for the machines we have not thought of.
mksquashfs "$appdir" "$work/fs.squashfs" -root-owned -noappend -comp gzip -no-progress

target="$out/starcord-$ver-x86_64.AppImage"
cat "$work/runtime" "$work/fs.squashfs" > "$target"
chmod +x "$target"
ls -la "$target"
echo "wrote $target"
