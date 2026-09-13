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
- **Messages, live and in history.** A channel's messages are held oldest-first
  by snowflake, which is oldest-first by time, so history that arrives out of
  order merges by binary search and there is no sort key to get wrong. Five
  hundred are kept for a channel somebody is reading and fifty for one they are
  not; eviction from the front records that there is older history, so scrolling
  back still works. A message arriving while the reader is scrolled up is
  counted rather than appended, because putting it at the bottom would hide it
  below a gap nobody can see.
- **Discord's markdown, parsed.** Hand-written, no regex, and total: there is no
  error type, because the alternative is a message somebody sent that this
  client refuses to show. The rules are Discord's rather than CommonMark's — `__`
  is underline, `_` respects word boundaries so `snake_case` survives, `*` needs
  a non-space beside it so `2 * 3` is arithmetic, and a masked link only accepts
  http and https. `plain_text` keeps every alphanumeric character of the source
  in order, which a property test holds it to.
- **Sending, editing and deleting**, optimistically. A message appears the
  instant it is typed, keyed by a random nonce that comes back on the gateway
  echo; matching it turns the optimistic row into the real message rather than
  leaving the sender looking at two. A refused send keeps its row with the text
  still in it, and can be retried or thrown away. An echo that never arrives is
  replaced by the response body ten seconds later.
- **Typing indicators, member-list subscriptions and read marks**, all of them
  gated. Typing goes out at most once every nine seconds per channel, so the
  composer may call it on every keystroke. Subscriptions are merged per server,
  re-sent only when the ranges change, and dropped thirty seconds after the last
  channel in that server closes, so clicking between two channels costs nothing.
  Acks are coalesced to the highest message per channel per second, skipped when
  the channel is already read, and never sent for a channel the user is not
  looking at.
- **`session.toml`**, at mode 0600, holding the last channel, the last server,
  per-channel drafts and scroll anchors. The mode is for the drafts: everything
  else there is mildly private, but a draft is the user's own unsent words,
  sitting on disk because they closed the client mid-sentence.
- **`probe --channel`, `--send` and `--legacy-lazy-request`.** The first tails a
  channel — history, then live messages, edits, deletions, reactions and typing.
  The second sends one message and waits for the echo. The third switches
  member-list subscriptions from op 37 to op 14; which one a user-account
  session is expected to send is something only a live connection can settle, so
  the one that went out is printed rather than logged.
