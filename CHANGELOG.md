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
- **Pictures, fetched and decoded.** A picture is named by what it is — a user
  and a hash, a message and an attachment id — rather than by where it
  currently lives, because Discord's attachment URLs are signed and expire
  within the day while the picture does not. The cache strips the signature
  back off before naming the bytes on disk, so the same attachment fetched on
  Monday and on Friday is one file, and a 403 on one is a stale link worth
  exactly one re-signing rather than a failure worth remembering. The queue
  serves what is on screen before what might be, and drops a prefetch the
  reader has already scrolled past.
- **A decoder that assumes the bytes are hostile.** The header is read and
  checked before anything is decoded, so a four-hundred-byte file claiming to
  be forty thousand pixels on a side is refused rather than believed. An
  animation is capped at three hundred frames or fifty million pixels and comes
  back as its first frame past either, with a note saying why it is not moving.
  Frame delays have a twenty-millisecond floor, because a GIF asking to be
  drawn as fast as the machine can manage is asking for the whole terminal to
  be redrawn a thousand times a second.
- **Signing in by scanning a code**, which is now the recommended way in. An
  RSA-2048 keypair is generated per attempt and its private half never leaves
  the process; the password is never typed into a terminal and the token is
  never displayed, never in a clipboard, and never in shell history. The phone
  shows who is asking before it agrees. A code nobody scans is regenerated once
  and then waits to be asked again.
- **`probe --qr` and `probe --media`.** The first draws the code as half-blocks
  and reports who signed in and where the token went; everything up to the
  point the code appears needs no account at all, which makes it the way to
  check that half of the handshake on its own. The second fetches and decodes
  one URL and prints what came back, with no gateway and no token involved.
- **`docs/account-safety.md` and `docs/auth.md`**, which say plainly what a
  user-account session is, what Discord's terms say about one, what this client
  deliberately cannot express, and what actually revokes a token.
- **The GIF picker.** Trending, search and suggestions, with the provider taken
  from configuration rather than compiled in — Discord proxies somebody else's
  service here and has announced a change of provider — and with the spacing
  owned by the core rather than by whatever is calling it. A picker that
  searches on every keystroke still sends one request per pause, and a request
  overtaken while it waits is dropped, because by the time its answer came back
  nobody would want it. Posting a GIF is an ordinary message whose content is
  the link; there is no separate request for it and nothing pretends otherwise.
- **Attachments, in three requests rather than one.** Ask Discord for a slot,
  put the bytes on the storage host it names, then post a message that only
  names the slot. The bytes never touch Discord's API, so a rate limit on the
  message costs one small retry rather than twenty megabytes again, and they
  never carry a token, because the host they go to is not Discord's. A 403 or a
  404 on the slot endpoint falls back to the older multipart form. A file over
  the configured cap is refused before any request is made — a rejected request
  is still a request.
- **Reactions, optimistically.** The chip moves before Discord has heard about
  it, because a round trip is a tenth of a second of nothing happening. The
  change is expressed as an add or a remove rather than as a new count, so
  putting it back after a refusal lands in the right place even when somebody
  else reacted in between. Only this account's own reaction can be touched.
- **Search**, of a server or of one channel, spaced by the same gate the picker
  uses. Results come back on the event rather than going into the message
  store: a search reaches back through a year of a channel nobody has open, and
  inserting what it finds would throw away the window somebody is reading.
- **Threads, under the channel they belong to.** A thread is an ordinary
  channel that the list draws beneath its parent, newest conversation first and
  active only — a thread is archived rather than deleted and an old server has
  thousands. A forum's posts are threads, so the same rule draws a forum with
  no special case in it.
- **Member lists**, which are not fetched but subscribed to by index range and
  arrive as splices against that window: replace a range, insert a row, take
  one out. The ranges are clamped to three windows of a hundred before anything
  is sent, because Discord ignores a larger request rather than refusing it,
  and a subscription that is ignored looks exactly like one that was accepted
  and never delivered.
- **A DM only with a friend.** `OpenDm` is refused for anybody else, with a
  note saying why; a conversation that already exists is opened rather than
  asked for again.
- **Desktop notifications**, decided separately from being delivered so that
  the whole decision is a table test. A muted channel, this account's own
  message, anything not addressed to the reader, the channel already on screen
  while the terminal has focus, and more than one message in two seconds for
  one channel are each a reason to stay quiet — the last collapsing into a
  count rather than a queue of popups. A spoiler stays a spoiler in the body.
- **`probe --gifs`, `--send-file`, `--react`, `--unreact` and `--search`**, so
  that every one of those paths can be run against a real account with no
  terminal UI in the way.
