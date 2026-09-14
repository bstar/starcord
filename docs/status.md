# Status

The Discord core is complete and tested, and the interface on top of it now
covers everything the README describes. This page says what has actually been
looked at rather than only compiled, so that the list of what it does can be
read against what is there today.

The honest one-line summary: the protocol side works and can be exercised
headlessly with `starcord probe`; the window is built and is being used, with
a short list of rough edges below.

## Done

| Area | State |
| --- | --- |
| Gateway session | connect, identify, heartbeat, resume, reconnect with backoff; `compress=zlib-stream` through one shared deflate context |
| REST client | per-bucket and global rate limiting, 429 and 5xx retries, 401 stops everything |
| Signing in | QR remote auth and token paste; keyring, or a mode-0600 file |
| State | servers, channels, DMs, threads, members, presence, relationships, read states and mutes, all authoritative behind one lock |
| Messages | history paging, live messages, edits, deletions, optimistic sends with nonce matching |
| Markdown | Discord's rules rather than CommonMark's, hand-written, total, property-tested |
| Typing, acks, subscriptions | all coalesced and gated, so the UI may call them on every keystroke |
| Reactions | this account's own, both ways |
| Uploads | the two-step attachment slot, with a multipart fallback |
| Media | fetch, cache by identity rather than by URL, decode PNG, JPEG, WebP and GIF under caps |
| GIF picker and search | both spaced three hundred milliseconds apart by the core |
| Notifications | the decision rule, and delivery over zbus |
| `starcord probe` | the headless client: connect, tail, send, upload, react, search, fetch one picture, ask the picker |
| Packaging | Nix flake and home-manager module, PKGBUILD, `.deb`, AppImage, portable tarball, CI |
| The window | the column of modules and the accordion that folds them, the login screen including the QR code, the server, channel, conversation and member lists, the status line, the help overlay and the quick switcher |
| Reading a conversation | grouping, Discord's markdown, replies, reactions, embed cards, code blocks, spoilers, day and unread dividers, the typing line, virtual scrolling, history paging, a scrollbar that can be dragged |
| Writing one | drafts per channel, reply and edit modes, `@`, `#` and `:` autocomplete, sending, editing, deleting with a confirmation |
| Pictures | avatars, server icons, inline attachments, card thumbnails and custom emoji, through the terminal's own protocol or as half blocks |
| Animated GIFs | only what is on screen, at most four at once, stopped by `[media] animate` |
| Pickers | emoji, reactions and GIFs, in one overlay with a search field and a grid |
| Attachments | by path with `alt+a`, from the clipboard with `ctrl+v`, as chips in the composer, with the upload's progress on the row it belongs to |
| The media viewer | `enter` on a picture: fitted or actual size, `h` and `l` through the channel's pictures, save, open, copy the link |
| Search | this channel or the whole server, paged, and `enter` jumps to the message |
| Threads | listed under the channel they belong to, marked on the message they were started from, opened with `space` |
| Unread hopping | `alt+up` and `alt+down`, mentions first |
| Notifications | the bell and a line in the status bar here; the desktop popup is the core's, behind `[notify] desktop` |

## Rough edges

| Area | State |
| --- | --- |
| Pinning | `p` is in the key table and says so: `Command` has no pin, deliberately, and adding one is a milestone of its own |
| A wedged window in some terminals | starting under a terminal that answers the graphics capability query slowly -- a multiplexer passing it through, most often -- can leave the client not drawing and echoing what is typed. It is the query's reader restoring the terminal's mode after the client has taken it. `[ui] graphics = "off"` avoids the query where the environment does not suggest a protocol; `ctrl+l` redraws once it is back |
| Right-clicking | the message menu is drawn and tested, and could not be driven by a synthetic click in the harness used to check it by eye |

## What is still only read, not seen

The core was written from Discord's documented behaviour and from reading —
not copying — the clients that came before it. A few things there are
descriptions rather than a specification, and are not yet settled against a
live session:

- the exact READY shape under the capabilities this client identifies with,
  and whether member-list subscriptions want op 37 or op 14
- everything after the phone scans the code: the handshake gets as far as the
  fingerprint against Discord's own gateway, and the rest is from the
  documentation
- whether the nonce sent with a message comes back on the gateway echo as well
  as in the response body
- what the sixteen themes and the pictures look like in every terminal: the
  interface has been read in kitty and in tmux, and the rest is somebody
  else's screen

`AGENTS.md` keeps that list, and `starcord probe` prints what actually happened
for each of them so a live run can replace a guess with an observation.

## Not planned

Voice, video, screen share, server administration, account settings and friend
requests. Those are not missing; they are deliberately absent, and
[Account safety](account-safety.md) says why.
