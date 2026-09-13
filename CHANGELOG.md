# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **A Discord core that connects, identifies and holds a session**, with no
  terminal code anywhere in it. The gateway state machine, the rate-limited
  HTTP client, the token store and the authoritative in-memory state all live
  under `src/discord/`, behind a `Handle` the UI will talk to through two
  channels and a lock. A test greps those sources for `ratatui`, `crossterm`
  and `crate::ui` and fails if any of them appear, because the reason the core
  is testable is that it cannot draw.
- **`starcord probe`**, a headless client that reads a token from standard
  input, connects, prints every connection transition with a timestamp, and
  reports the READY payload as a user, a guild count and a DM count. It is the
  only way to exercise the core until there is a UI, and it is meant to stay
  after there is one: a defect that reproduces without a terminal is a defect
  with a much shorter report. `STARCORD_RECORD_GATEWAY` writes every dispatch
  to a directory so a real session can become a test fixture.
