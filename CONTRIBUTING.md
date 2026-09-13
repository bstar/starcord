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

## Commit messages

Present tense, plain prose, no conventional-commits prefix, no trailers. What
the commit does and, where it is not obvious, why. The existing log is the
style guide.

## Comments

The codebase explains decisions rather than mechanics, and usually says what
was measured or observed. If you change something a comment justifies, change
the comment. If you leave a comment that says something is a certain way for a
reason, make sure the reason is true.
