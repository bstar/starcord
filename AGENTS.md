# Working notes

Context that is not derivable from the code or the history, kept here rather
than in any one machine's notes because this is developed on both Linux and
macOS, with more than one assistant, and the repository is the only thing all
of them see.

This file is the only assistant-facing notes file in the repository. Do not add
a tool-specific notes file or directory beside it; every assistant reads
`AGENTS.md`.

## Building

Use the Nix flake for all builds and checks. Do not assume `cargo` is on the
ambient `PATH`.

```sh
nix develop -c cargo build --release
nix develop -c cargo test --all
```

There are no system libraries. That is unusual enough to be worth stating: TLS
is rustls with the `ring` provider, the keyring reaches secret-service through
zbus rather than libdbus, notifications default to zbus for the same reason,
and nothing in the tree runs bindgen. A change that adds a `-sys` crate has to
add the library to `flake.nix` and the apt line to `.github/workflows/ci.yml`
in the same commit.

## STAR/KIT

The shared foundation — paths, logging, private file writes, themes, the dock
layout engine, terminal images — lives in `starkit`, the crate STAR/AMP uses
too. The dependency is commented out in `Cargo.toml` for now, and `src/paths.rs`
and `src/logging.rs` carry local copies of the two pieces the core needs, with
the same signatures.

Two things block the swap, neither of them the crate being unfinished. A path
dependency on a sibling checkout cannot be built by the flake, whose `src` is
this directory, so it waits for the git dependency and
`cargoLock.allowBuiltinFetchGit`. And `starkit::paths::Paths` has no
`session_file()` or `media_cache_dir()`; either those move upstream or the local
file keeps them as an extension over the shared type.

Once `starkit` is a git dependency, a local checkout is used through an
uncommitted `.cargo/config.toml`:

```toml
[patch."https://github.com/bstar/starkit"]
starkit = { path = "../starkit" }
```

`.gitignore` already covers it. Every public `starkit` item has two consumers;
check STAR/AMP before changing a signature.

## Nothing under src/discord/ draws

No `ratatui`, no `crossterm`, no `crate::ui` anywhere under `src/discord/`.
A test in `discord/mod.rs` greps the module's own sources and fails if any of
them appear.

This is not tidiness. It is why `starcord probe` can exercise the whole
protocol with no terminal attached, why the gateway and the state machine are
testable from fixtures, and why a rendering change cannot break a protocol. The
core owns the truth behind an `RwLock` and emits `Event`s that are only
*notifications*: the UI may coalesce or drop them and still render the truth on
the next frame, which is what makes a bounded event channel safe.

The dependency runs the other way as well. `src/ui/` never reaches into
`discord::state` internals; it talks to `Handle` and to the read-side query API
on `State`, and `Handle::from_parts` exists so the UI can be driven by a fake
core in tests.

## The token

A token is written in exactly two places: the OS keyring, or `credentials.toml`
at mode 0600 when there is no keyring. Never the config file, never the session
file, never the log, never an error message, never `stdout`, and never a
command-line argument — `probe` reads it from standard input for that reason,
because argv is world-readable in `/proc`.

`Token`'s `Debug` prints `Token(<redacted>)`, so a `{:?}` on any struct that
transitively holds one is safe. Keep it that way; a derived `Debug` on a new
type that holds a token is a leak.

`download()` in `http/mod.rs` sends only a `User-Agent`. Attachment and avatar
URLs point at `cdn.discordapp.com` and at media proxies, and an
`Authorization` header on a request to a host Discord does not control is how a
session gets handed to somebody else.

## The account-safety boundary

STAR/CORD signs in as a user account, because there is no other way for a
terminal client to read your DMs. The position it takes is that the client does
only what a person at the keyboard does, and that is enforced by what `Command`
can express rather than by intention:

- Absent by design: friend requests, relationship changes, guild join and
  leave, invite use or creation, DMs to non-friends, bulk operations of any
  kind, profile scraping.
- One `ClientProps` instance produces the IDENTIFY properties, the
  `X-Super-Properties` header, the `User-Agent` and the remote-auth socket's
  headers, so they cannot drift apart and describe two different clients.
- IDENTIFY presence is `online` with no activities. No Rich Presence — that is
  STAR/AMP's job, over the local desktop IPC socket, with no token involved.
- Typing is sent at most every nine seconds per channel, acks are coalesced to
  the highest id per channel per second and never sent for a channel the user
  is not looking at, member ranges are requested only for the open guild, and
  history is one request per channel.

A pull request that adds an automated action needs an argument that starts with
why a human would have pressed a key for it.

## What has to be confirmed against a live session

The core was written from userdoccers (`docs.discord.food`) and from reading —
not copying — concord, endcord and discordo. Several things there are
descriptions of behaviour rather than a specification, and the notes below say
which ones are still guesses. Replace this section with what was observed, do
not leave it as folklore.

- **The READY shape** under the capabilities this client identifies with.
  `read_state` is accepted both as a bare array and as
  `{version, partial, entries}`; a guild is accepted both flat and as
  `{id, properties, channels, ...}`. Both stay supported whatever the first
  recording shows, because Discord has shipped both.
- **`op 37` versus `op 14`** for member-list subscriptions. 37 is what the web
  client sends today; 14 is kept behind `legacy_lazy_request`, and
  `starcord probe --channel <id> --legacy-lazy-request` sends it. Either way the
  opcode that went out is printed, because a subscription Discord ignores looks
  exactly like one it accepted: no error comes back, the member list simply
  never arrives.
- **The exact IDENTIFY payload**, in particular whether `capabilities` as sent
  here changes the READY shape.
- **The pinned build number.** `PINNED_BUILD_NUMBER` in `props.rs` is a
  last-resort fallback behind live discovery and a 24-hour cache; the comment
  there records when it was taken and from where.

There is one more, added with the message milestone: **whether a `nonce` sent
on a `POST /messages` comes back on the gateway echo** as well as in the
response body. The optimistic-send path assumes it does, and falls back to the
response body after ten seconds if no echo carrying it arrives.
`starcord probe --channel <id> --send "…"` prints both, so a live session says
which happened.

`STARCORD_RECORD_GATEWAY=<dir> starcord probe --token-from-stdin` writes every
dispatch as `<seq>_<event>.json`. `testdata/gateway/README.md` has the scrub
procedure. The fixture in that directory today is **synthetic**, hand-written
from the documented field lists, and says so.

## Tests

In-module `#[cfg(test)]`, as in STAR/AMP. Anything that parses foreign input
gets a proptest as well as table tests — the inflater, the model
deserialisers, the markdown parser. `cargo test` must pass on a machine that
has never logged in; anything needing an account is gated on
`STARCORD_TEST_TOKEN`.
