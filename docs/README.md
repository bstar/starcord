# STAR/CORD documentation

- [`cli.md`](cli.md) — the command line, and `starcord probe` in particular.

Not written yet, and listed here so it is obvious what is missing:

- `account-safety.md` — what signing in as a user account means, what this
  client does and deliberately does not do, and why QR login is preferred over
  typing a password into a terminal. Arrives with the QR login it describes.
- `auth.md` — the two ways in, where the token is kept, and how to get rid of
  it.
- `keys-and-mouse.md` — generated from the keymap table, with a test that greps
  this document for every key string. Arrives with the UI.
- `themes.md` — deferred to STAR/KIT, which owns the theme engine both this and
  STAR/AMP use.

Two things that are documented in the source rather than here, because they are
the kind of thing that goes stale the moment it moves away from the code:

- `src/discord/mod.rs` explains why nothing in the Discord core knows the
  terminal exists.
- `testdata/gateway/README.md` explains that the READY fixture is synthetic,
  and how to record and scrub a real one.
