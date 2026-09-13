# STAR/CORD

[![ci](https://github.com/bstar/starcord/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/bstar/starcord/actions/workflows/ci.yml)
[![nix](https://github.com/bstar/starcord/actions/workflows/nix.yml/badge.svg?branch=main)](https://github.com/bstar/starcord/actions/workflows/nix.yml)
[![debian](https://github.com/bstar/starcord/actions/workflows/debian.yml/badge.svg?branch=main)](https://github.com/bstar/starcord/actions/workflows/debian.yml)
[![arch](https://github.com/bstar/starcord/actions/workflows/arch.yml/badge.svg?branch=main)](https://github.com/bstar/starcord/actions/workflows/arch.yml)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A terminal client for Discord that feels like Winamp. Real-time chat with your
friends and servers; the account, the servers and the settings stay in
Discord's own app.

<!--
  The screenshot goes here, as docs/screenshot.png, the way STAR/AMP's README
  carries one. There is no picture yet: the interface is still being built, and
  a screenshot of a half-drawn window would have to be retaken every week.
  Take it in kitty, with pictures on, and link it the same way STAR/AMP does.
-->

[Status and the numbers](docs/status.md) says how much of that is built today,
honestly, area by area.

## Get it

Packages for Linux are on the
[releases page](https://github.com/bstar/starcord/releases/latest): an AppImage
that needs nothing installed, a `.deb` for Debian and Ubuntu, a portable
tarball, and the source the Arch `PKGBUILD` builds.

```sh
nix run github:bstar/starcord        # Nix, on Linux or Apple Silicon macOS
```

[Installing](docs/installing.md) covers every route, including building from
source. There are no system libraries to install first.

## Sign in

```sh
starcord
```

The first run asks how you want to sign in. Scan the code with the Discord
phone app, which is the way to prefer: your password is never typed into a
terminal, and the token never appears on screen. Pasting a token you took out
of the desktop client works too, for a machine with no phone beside it.

The token goes into the OS keyring, or into a mode-0600 file where there is no
keyring. [Signing in](docs/auth.md) has both routes in full, and
[Account safety](docs/account-safety.md) is worth reading before either.

## What it does

- **Chat, in real time.** A gateway session like any other client's: messages,
  edits, deletions, typing, presence and read marks arrive as they happen, and
  a dropped connection resumes rather than starting over.
- **Servers, channels, DMs and threads.** Categories fold, unread and mention
  counts sit beside the names, muted channels stay quiet, and the member list
  loads the part of a server you are looking at.
- **Pictures, in the terminal.** Attachments, avatars, custom emoji and
  animated GIFs are drawn as real pixels in kitty, WezTerm, Ghostty and foot,
  and as half-blocks where there is no graphics protocol.
- **Emoji, reactions and a GIF picker.** Type `:name:` and complete it; react
  with a key; search the GIF picker and post what it finds.
- **Attachments, including what is on your clipboard.** Attach a file by path,
  or paste an image straight out of the clipboard into the composer.
- **Notifications.** A bell and a line in the status bar for a mention or a DM,
  and a desktop notification when you ask for one.
- **Search.** A channel or a whole server, and jumping to a result opens the
  history around it.
- **Dockable panels and sixteen themes.** Winamp-style panels you drag by their
  seams, and the same theme engine STAR/AMP uses, so both match the desktop and
  each other. The mouse works everywhere the keyboard does.

Linux on x86_64 and aarch64, and macOS on Apple Silicon.

## What it does not do

Voice, video, screen share, server administration, account settings and friend
requests. Use the Discord app for those. None of them is missing because it is
hard; they are the things a terminal cannot do honestly, and the things a
client that signs in as a person should not be automating.

## Read more

The [documentation](docs/README.md) has a page for each of those. The ones most
people want first:

- [Account safety](docs/account-safety.md)
- [Signing in](docs/auth.md)
- [Keys and mouse](docs/keys-and-mouse.md)
- [Configuration](docs/configuration.md)
- [If something is wrong](docs/troubleshooting.md)

[CONTRIBUTING.md](CONTRIBUTING.md) is for building and changing it,
[CHANGELOG.md](CHANGELOG.md) for what each release holds, and
[SECURITY.md](SECURITY.md) for reporting a vulnerability.

## A word about Discord's terms

STAR/CORD signs in as a user account, because there is no other way for a
terminal client to read your own DMs, and Discord's terms of service reserve
the right to act against accounts that use a client other than their own.
[Account safety](docs/account-safety.md) says what that means, what this client
deliberately cannot do, and how to decide whether to use it.

Not affiliated with, or endorsed by, Discord.

## License

MIT. See [LICENSE](LICENSE).
