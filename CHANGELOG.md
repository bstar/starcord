# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **The borders are STAR/AMP's**: every module, the login box and the overlays
  are framed in double lines. The titles were already seamed with `═` --
  `═ servers ═` is STAR/KIT's, and STAR/AMP draws every panel with it -- so a
  single-line frame put a heavier mark on a lighter edge and the title read as
  stuck on rather than sitting in the border. The conversation's scroll marker
  is a full block for the same reason: on a `║` a half block covers one of
  the two strokes and reads as a gap in the frame.

## [0.0.2] - 2026-09-14

### Changed

- **The window is one column of modules rather than a dock of panels.** The
  servers, the channels, the conversation, the composer and the members, top
  to bottom in the order you drill through them, with the status line under
  them. Every module is always there, so there is nothing to open, nothing to
  close and nothing to arrange; what the first release shipped was a
  representation of the web client, and this is STAR/AMP's shape instead.
- **The three lists are an accordion.** The one you are choosing in is
  expanded and the others fold to a single line saying what is chosen in
  them: the server, the channel or conversation, how many people are about.
  Choosing a server folds the servers and opens its channels; opening a
  channel folds both, and `esc` walks back up the way it came down and folds
  at the top.
- **Home is the first entry in the server list**, and with it chosen the
  second module lists your conversations and then the friends who have not
  started one. The separate DM module, its tab and its fold are gone, which
  were three ways of reaching the same list.
- **The composer is where the keyboard rests.** Opening a channel puts the
  caret in it, so letters type; `tab`, `esc` and every `alt+…` still leave it,
  and none of them is a letter. A channel read from the composer is still
  acknowledged, and one you are only browsing past in a list is not.
- **`alt+1` to `alt+5` are the column, in order**, `alt+m` opens and folds the
  member list, and `alt+s` opens the focused module's settings. `alt+g`,
  `alt+c`, `alt+d`, `alt+z` and `alt+x` are gone with the panels they opened
  and closed.
- **The window needs 60 by 21** rather than 60 by 12, because five modules are
  always drawn. The conversation wraps to the width of the window now rather
  than to a column of it.

### Removed

- **`[layout]`**. The column has no widths to remember, no panels to close and
  no seams to drag. A file written by 0.0.1 still loads: the table is ignored
  rather than refused.

## [0.0.1] - 2026-09-13

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
- **Packages, documentation and a release that builds itself.** A Nix flake
  with a home-manager module and an overlay, an Arch `PKGBUILD`, a `.deb` per
  Debian generation, a portable tarball built against glibc 2.31, and an
  AppImage that is started on eight distributions before a release is drafted.
  The dependency list for all of them is the C runtime and nothing else, which
  is worth stating because it is unusual: there is no ffmpeg, no ALSA and no
  libdbus anywhere in this tree. The documentation grew a page per task —
  installing, configuring, theming, what to check when something is wrong, and
  an honest status page saying which parts are built and which are not.
- **A conversation you can read.** Messages by the same person within
  `[chat] group_window_secs` are drawn as one block with a single name and
  time; a reply, a system message or a change of author breaks it. Day
  dividers, a `new messages` marker at the first thing you have not seen,
  reactions as chips with the ones you added marked, link previews as a card,
  a gifv as a chip that opens elsewhere, and a typing line at the bottom. The
  list is anchored at the end rather than scrolled from the top, which is why
  a message arriving does not move what you are reading and a page of history
  arriving above does not either.
- **One measurement per message, and everything built from it.** The renderer
  produces the rows and the height together, a cache keyed by the message, the
  width, the theme, the reactions, the revealed spoilers and the timestamp
  setting sits in front of it, and the virtual list stacks exactly those
  heights. The wrapping is done cluster by cluster rather than with ratatui's
  paragraph wrapper, because the panel has to know where it put things: which
  cell a link starts at so a click opens it, which run of cells covers a
  spoiler so `space` uncovers one and only one, and which rows a picture was
  given before its bytes have arrived.
- **Writing, with the sentence kept.** A draft per channel, restored when you
  come back to it and written to `session.toml` as you type. Reply and edit
  modes with a banner saying which. `@`, `#` and `:` completion over the
  channel's recent authors, the server's channels, and unicode and custom
  emoji. `esc` undoes one thing at a time — the popup, then the mode, then
  focus — and `up` on an empty box opens your last message for editing.
- **`Ctrl+K`, the quick switcher**, over every server, channel, conversation
  and friend, ranked by a fuzzy matcher rather than by a substring test so
  that `gen` finds `#general` before it finds `#gardening-notes`.
- **Draggable seams.** Both column borders and the one between the channel and
  message lists; the numbers land in `[layout]` in `config.toml` a second after
  the pointer stops, written a line at a time so the comments in the file
  survive. The settings overlay writes the same way.
- **`docs/keys-and-mouse.md`**, generated from the key table rather than
  written beside it, with a test that fails when the two disagree.
- **Pictures, where the terminal can draw one.** Inline images and the first
  frame of an animation, avatars in the gutter, server icons in the rail,
  custom emoji inline and on reaction chips, and a thumbnail against the right
  edge of a link card. The rows are reserved from the size the picture says it
  is, before any bytes arrive, so nothing reflows when they do and a reader's
  place is kept. `[ui] graphics = "off"` leaves the chip that names the file,
  `"blocks"` draws two pixels to a cell in any terminal at all, and a
  rectangle a scroll has cut is drawn as blocks unless the protocol tolerates
  a clipped placement — kitty does; sixel and iTerm2 would paint over what is
  below the panel.
- **A code to scan, instead of a token to paste.** The login screen draws the
  remote-auth matrix as a picture where there is a graphics protocol and as
  half blocks where there is not, both black on white whatever the theme is,
  because a scanner is looking for contrast rather than for taste. It counts
  down, replaces a code nobody scanned, names whoever scanned it, and puts a
  failure's close code on the screen in full rather than shortening it to
  "login failed". A terminal too short for the code gives the URL instead:
  half a code scans as nothing.
- **`starcord --replay` shows real pictures and plays a whole QR login.** The
  fixture carries three tiny synthetic files and a map from what a media key
  boils down to; the scripted login has no stored token, a code, a phone
  reading it three seconds later and a session three seconds after that.

- **Animated GIFs, and a rule about which of them may move.** A picture that
  was not drawn last frame never advances, at most four move at once, and a
  frame delay under fifty milliseconds is played at fifty. `[media] animate`
  and `alt+n` decide whether any of it happens; the event loop wakes for the
  next frame that is due, or a hundred-millisecond animation would arrive a
  frame late every time. A video's thumbnail never moves however many frames it
  has: the marker over it says the terminal will not play it.
- **One picker for emoji, reactions and GIFs.** `ctrl+e` from the composer, `+`
  on a message, `ctrl+g` for the GIF grid and from inside the emoji grid. The
  emoji half offers the server's own before every unicode one; the reaction
  half is told which reactions are already yours, so choosing one of them takes
  it off; the GIF half asks what is trending when it opens and searches three
  hundred milliseconds after the typing stops, with the tile under the cursor
  the only thing on the screen that moves.
- **Attaching a file.** `alt+a` types a path, with `~` expanded, and checks it
  before it becomes a chip: it has to exist and be under
  `[media] max_attachment_mib`, because a chip that fails at send time is a
  message somebody believes they sent. `ctrl+v` does the same for a picture on
  the clipboard, encoded to PNG at the edge so the size on the chip is the size
  that goes out. Clicking a chip's `×` takes it off, and the row a message
  waits on says how far its files have got.
- **A viewer for one picture.** `enter` on a message with something viewable
  opens every picture in the channel, in order: `h` and `l` walk them, `z` is
  fitted or actual size, `s` writes the file to `[media] save_dir` -- the whole
  file, asked for again, not the thumbnail that was on screen -- `o` opens it
  elsewhere and `y` copies its link. A save never replaces a file that is
  already there.
- **Search, and a way back from it.** `/` for this channel and `alt+f` for the
  whole server, paged twenty-five at a time. `enter` on a hit fetches the page
  around the message and lands on it; `G` asks for the present back, because
  the bottom of a page out of the middle of a channel is not the newest thing
  anybody said.
- **Threads, unread hopping and the terminal's own notifications.** A thread is
  listed under the channel it belongs to and marked on the message it was
  started from, and `space` opens it. `alt+up` and `alt+down` walk what is
  unread, mentions first and wrapping. A mention rings the bell and writes a
  line saying who and where, and never for the channel already on screen while
  the window is in front.
- **A menu on a message**, on the right button, listing what can be done to it
  and naming the key for each -- reply, react, edit, delete, copy, open, copy a
  link. `ctrl+y` copies a link to a message from anywhere.
- **A scrollbar that can be dragged**, `↓ n new` that can be pressed, and
  `[channels] show_voice` where the other settings are.

### Changed

- **A picture arriving re-measures only the messages that draw it.** The
  generation in the wrap cache's key is per message now rather than one counter
  for the window, so a channel of photographs no longer re-measures all five
  hundred of them for each one that lands. An attachment that arrives without
  saying how big it is -- which is most of what a bot posts -- is a chip that
  asks to be measured, and becomes a picture on the measurement after the bytes
  arrive.
- **`[notify] desktop` is what turns the desktop notification on.** It was
  documented that way and wired the other: the core's switch is the desktop
  popup and nothing else, and the bell and the line in the status bar are the
  terminal's own. Anding the two is what stops a mention being announced twice.
- **The frame after an overlay closes is a whole one.** The terminal is a
  shared surface, and a cell this program's buffer does not know changed is a
  cell the diff will never repaint -- which showed as a box left behind after
  the quick switcher closed. `ctrl+l` asks for the same at any time.
- **The day rule is held to a contrast the date on it can be read at.** It was
  the one `[chat]` role taken from the panel chrome with no floor under it, and
  on one of the sixteen themes it resolved to 1.46:1. The legibility test now
  walks it, and walks the desktop's own palette as well where one is set.
