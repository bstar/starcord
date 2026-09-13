#!/usr/bin/env bash
# Build every release artifact into dist/, locally.
#
# Everything here runs in a container, and that is not a preference. NixOS
# cannot produce any of these natively: cargo-deb's $auto dependency resolution
# reads a dpkg database that does not exist here, makepkg is not packaged, and
# a nix-built binary asks for /nix/store/...-glibc/ld-linux.so, which no other
# distribution has. The same scripts run in CI, inside the same images, so "it
# worked locally" means something.
#
#   ./scripts/build-dist.sh              everything
#   ./scripts/build-dist.sh deb arch     just those
set -euo pipefail
cd "$(dirname "$0")/.."

CONTAINER=${CONTAINER:-$(command -v docker || command -v podman)}
[ -n "$CONTAINER" ] || { echo "need docker or podman" >&2; exit 1; }

ver=$(sed -n '0,/^version = /s/^version = "\(.*\)"/\1/p' Cargo.toml)
mkdir -p dist
echo "starcord $ver, via $(basename "$CONTAINER")"

# A separate target directory per image, mounted rather than shared with the
# host's. Letting a container write into ./target leaves a Debian binary where
# the next `cargo run` expects a NixOS one, and cargo-deb would happily package
# whichever it found.
run() { # image script...
  local image=$1; shift
  local vol="starcord-target-${image//[:\/]/-}"
  # The registry is a named volume rather than a bind of ~/.cargo: a container
  # runs as root, and bind-mounting the host's registry hands root ownership of
  # files the host's own cargo has to write to next.
  "$CONTAINER" run --rm \
    -v "$PWD:/src" -w /src \
    -v "$vol:/build/target" \
    -v starcord-cargo:/root/.cargo \
    -e CARGO_TARGET_DIR=/build/target \
    -e CARGO_HOME=/root/.cargo \
    "$image" bash -c "$* ; rc=\$?; chown -R $(id -u):$(id -g) /src/dist 2>/dev/null || true; exit \$rc"
}

want() { [ $# -eq 0 ] || [ "$targets" = all ] || [[ " $targets " == *" $1 "* ]]; }
targets=${*:-all}

if want source; then
  scripts/dist/source.sh
fi

if want tarball; then
  echo "== portable tarball (debian:bullseye-slim, glibc 2.31)"
  run debian:bullseye-slim 'scripts/dist/portable.sh'
fi

if want appimage; then
  echo "== AppImage (debian:bullseye-slim)"
  run debian:bullseye-slim 'scripts/dist/appimage.sh'
fi

if want deb; then
  # One .deb per Debian generation. They differ only in the glibc version
  # $auto writes into Depends, which is the whole of the dependency list: there
  # is no ffmpeg here to make a package release-specific for a better reason.
  for image in debian:bookworm-slim debian:trixie-slim ubuntu:24.04; do
    suffix=$(echo "$image" | tr ':/' '--' | sed 's/-slim//;s/debian-//;s/ubuntu-/ubuntu/')
    echo "== .deb ($image)"
    run "$image" "
      set -e
      . scripts/dist/deps-debian.sh
      . \$HOME/.cargo/env
      cargo install cargo-deb --locked
      cargo deb --deb-revision '1~$suffix' --output /src/dist/
      apt-get install -y -qq /src/dist/starcord_*1~$suffix*.deb
      starcord --version"
  done
fi

if want arch; then
  echo "== Arch package (archlinux:latest)"
  # Built for inspection and to keep the PKGBUILD honest, not for release:
  # Arch users should get this from the AUR, where the PKGBUILD carries a real
  # checksum for a released tarball.
  "$CONTAINER" run --rm -v "$PWD:/src:ro" -v "$PWD/dist:/out" archlinux:latest bash -c "
    set -e
    pacman -Syu --noconfirm base-devel git rust
    useradd -m build
    cp -r /src/packaging /home/build/pkg
    tar -czf /home/build/pkg/starcord-$ver.tar.gz \
      --transform 's,^\.,starcord-$ver,' --exclude=./.git --exclude=./target -C /src .
    sed -i 's|^source=.*|source=(\"starcord-$ver.tar.gz\")|' /home/build/pkg/PKGBUILD
    chown -R build:build /home/build/pkg
    su build -c 'cd /home/build/pkg && makepkg --nodeps --noconfirm'
    pacman -U --noconfirm /home/build/pkg/starcord-*-x86_64.pkg.tar.zst
    starcord --version
    cp /home/build/pkg/starcord-*.pkg.tar.zst /out/
    chown -R $(id -u):$(id -g) /out"
fi

if [ "$targets" = all ]; then
  ( cd dist && sha256sum -- * > SHA256SUMS.tmp && mv SHA256SUMS.tmp SHA256SUMS )
  echo
  ls -la dist/
fi
