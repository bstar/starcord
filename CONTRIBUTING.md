# Contributing

Thanks for looking. This is a small project with a maintainer who works on it
in the evenings, so the most useful thing you can do before writing code is
open an issue and say what you have in mind.

## Getting it to build

The one-command path:

```sh
nix develop
cargo build
```

Without Nix you need a Rust toolchain, 1.90 or newer, and nothing else. There
are no system libraries and no `-sys` crates: TLS is rustls, the token store
reaches secret-service through zbus rather than libdbus, and nothing runs
bindgen. If a change adds a system dependency it has to add it to `flake.nix`
and to the CI apt line in the same commit, and say why in the dependency's
comment.

## What CI will run

```sh
./scripts/check-version.sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings -A dead_code
cargo test --all
cargo deny check
```

`dead_code` is allowed because the Discord core lands milestone by milestone,
complete and tested, ahead of the UI that will reach it. Every other lint is
an error.

Tests that need a real Discord account are gated behind `STARCORD_TEST_TOKEN`
and skip cleanly when it is unset, so `cargo test` works on a machine that has
never logged in.

All of it in one line before you push:

```sh
nix develop -c sh -c './scripts/check-version.sh \
  && cargo fmt --check \
  && cargo clippy --all-targets -- -D warnings -A dead_code \
  && cargo test --all \
  && cargo deny check'
```

If you touched a workflow, `nix shell nixpkgs#actionlint -c actionlint
.github/workflows/*.yml` as well. Its shellcheck pass catches the `run:` blocks
nothing else reads.

## Three rules that are not visible from the type system

**A token is never written anywhere but the keyring or the mode-0600
credentials file, and never printed anywhere at all.** `Token`'s `Debug` prints
`Token(<redacted>)` so that a `{:?}` on a struct that happens to contain one
cannot leak it, and there is a test that says so. The log, the config file, the
session file, an error message and a panic are all the wrong place. If you add
a type that can hold a token, redact its `Debug` too.

**Nothing under `src/discord/` knows the terminal exists.** No `ratatui`, no
`crossterm`, no `crate::ui`. A test in `discord/mod.rs` greps the module's own
sources and fails if any of those appear. It is the reason the core can be
driven by a probe binary and by tests without a TTY, and the reason a rendering
change cannot break a protocol.

**The client only does what a person at the keyboard does.** No friend
requests, no relationship changes, no joining or leaving guilds, no invites, no
DMs to people who are not already friends, no bulk anything, no profile
scraping. Those are deliberately absent from `Command` rather than merely
unimplemented, and a pull request that adds one needs an argument that starts
with why a human would have pressed a key for it.

## Packaging

`options=(!lto)` in `packaging/PKGBUILD` must stay. makepkg turns LTO on by
default, which leaves the C and assembly that `ring` and `blake3` compile as
bitcode that rustc's linker cannot read, and their symbols come back undefined.
The release profile does its own LTO regardless. There is a CI job whose whole
purpose is to catch this coming back.

`scripts/build-dist.sh` builds every release artifact locally, in containers.
It has to be containers: cargo-deb's `$auto` dependency resolution reads a dpkg
database, makepkg is not packaged for most systems, and a binary built on NixOS
asks for a loader no other distribution has.

The icon is `packaging/starcord.svg`, and `packaging/starcord.png` is that file
at 256x256. If you change one, regenerate the other:

```sh
nix shell nixpkgs#librsvg -c rsvg-convert -w 256 -h 256 \
  -o packaging/starcord.png packaging/starcord.svg
```

## Taking a new STAR/KIT

Its own commit, "Take STAR/KIT 0.Y", and nothing else in it:

1. Change the `tag` and the `version` requirement on the `starkit` dependency
   in `Cargo.toml`.
2. `cargo update -p starkit` so `Cargo.lock` records the new revision.
3. Run the checks above. STAR/KIT's own CI has a job that builds both of its
   consumers against the tip of the library, so a break should have been caught
   there first, but the version this repository actually pins is the one that
   matters.

Keeping it separate is the point: what arrived with the new version is then one
diff to read rather than a line buried in a feature commit. STAR/KIT is 0.x, so
a minor bump may change an API and a patch bump may not.

To work on STAR/KIT and this at the same time, check it out beside this
repository and point the build at it with an untracked `.cargo/config.toml`:

```toml
[patch."https://github.com/bstar/starkit"]
starkit = { path = "../starkit" }
```

It is in `.gitignore`, because a committed one points CI at a path that does
not exist. It also rewrites `Cargo.lock`, so do not use it and `nix build` in
the same tree. Delete it once the change is tagged and this repository has
taken the new tag.

## Releasing

1. Bump `version` in `Cargo.toml` and `pkgver` in `packaging/PKGBUILD`, and
   reset `pkgrel=1`.
2. `cargo update -w` so `Cargo.lock` follows. The Arch build uses `--frozen`,
   so a stale lockfile fails it with an error that points at the lockfile
   rather than at the bump.
3. `./scripts/check-version.sh`
4. Add the release to `CHANGELOG.md`.
5. Tag `vX.Y.Z` and push it. The release workflow builds everything, starts the
   AppImage on eight distributions, and opens a draft release with the assets,
   their checksums and build provenance attached.
6. Read the draft, replace the generated notes with the changelog entry, and
   publish it.

## Commit messages

Present tense, plain prose, no conventional-commits prefix, no trailers. What
the commit does and, where it is not obvious, why. The existing log is the
style guide.

## Comments

The codebase explains decisions rather than mechanics, and usually says what
was measured or observed. If you change something a comment justifies, change
the comment. If you leave a comment that says something is a certain way for a
reason, make sure the reason is true.
