# STAR/CORD

A Winamp-feel terminal Discord client, built on the same foundation as
[STAR/AMP](https://github.com/bstar/staramp): dockable panels, a theme engine
that follows the rest of the desktop, and pictures in the terminal where the
terminal can draw them.

Chat, servers and channels, DMs, emoji, GIFs, and inline images and animated
GIFs. Not voice, not video, not server administration, and not account
settings — all of that stays in Discord's own client, where it belongs.

## Status

Early. The Discord core connects, identifies and holds a gateway session; the
UI is not built yet. What works today is the headless probe:

```sh
starcord probe --token-from-stdin < token.txt
```

See [`docs/cli.md`](docs/cli.md).

## Building

```sh
nix develop -c cargo build --release
```

Without Nix, a Rust toolchain of 1.90 or newer and nothing else: there are no
system libraries to install.

## Account safety

STAR/CORD signs in as a user account, the way discordo, endcord and Vesktop do,
because there is no other way for a terminal client to read your DMs. It does
only what a person at the keyboard does: no bulk requests, no scraping, no
automation of anything you did not press a key for. Read
`docs/account-safety.md` before you use it — it will be written alongside the
QR login it describes.

## Licence

MIT.
