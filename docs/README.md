# STAR/CORD documentation

The [README](../README.md) is the short version. These pages are the long one,
grouped by what you are trying to do.

## Getting started

- [Installing](installing.md): Nix, AppImage, `.deb`, tarball, Arch, macOS, and
  from source. Which terminals draw pictures.
- [Account safety](account-safety.md): what signing in as a user account means,
  what this client deliberately cannot do, and why scanning a code beats typing
  a password into a terminal. Read it first.
- [Signing in](auth.md): the two ways in, where the token is kept, and how to
  get rid of it.
- [Keys and mouse](keys-and-mouse.md): every binding and gesture, per panel.
- [Configuration](configuration.md): every setting, the themes, and where the
  files live.
- [If something is wrong](troubleshooting.md): the first things to check, and
  where the logs are.

## Going further

- [Themes](themes.md): the format, your own themes, and the `[chat]` roles.
- [On the command line](cli.md): `starcord probe`, which exercises the whole
  Discord core with no terminal attached.

## About the project

- [Status](status.md): what is done, what is in progress, and what is not
  started.
- [Contributing](../CONTRIBUTING.md), [Security](../SECURITY.md),
  [Changelog](../CHANGELOG.md).

## Documented in the source instead

Some things go stale the moment they move away from the code, so they stayed
there:

- `src/discord/mod.rs` explains why nothing in the Discord core knows the
  terminal exists.
- `src/discord/markdown/mod.rs` explains what the parser guarantees.
- `src/session.rs` explains why `session.toml` is mode 0600.
- `testdata/gateway/README.md` explains that the READY fixture is synthetic,
  and how to record and scrub a real one.
- `AGENTS.md` keeps the list of protocol details that are still descriptions
  rather than observations.
