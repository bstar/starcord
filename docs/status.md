# Status

Not finished. The Discord core is complete and tested; the interface is being
built on top of it a milestone at a time. This page says which is which, so
that the README's list of what it does can be read against what is actually
there today.

The honest one-line summary: the protocol side works and can be exercised
headlessly with `starcord probe`; the window you would sit in front of is
partly built.

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

## In progress

| Area | State |
| --- | --- |
| The window | login screen, server rail, channel and DM lists, status line, help overlay, the dock and its degradation are built. The message list, the composer, the quick switcher and seam dragging are being written now |

## Not started

| Area | State |
| --- | --- |
| Pictures in the chat | avatars, inline attachments and custom emoji. The pipeline that decodes them is done; nothing draws them yet |
| The QR login screen | the handshake works headlessly; drawing the code in the window is next |
| Animated GIFs, the pickers, attachments from the clipboard, the media viewer | |
| Search, threads, unread hopping and desktop notifications in the window | the core does all four; none of them has a key yet |

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

`AGENTS.md` keeps that list, and `starcord probe` prints what actually happened
for each of them so a live run can replace a guess with an observation.

## Not planned

Voice, video, screen share, server administration, account settings and friend
requests. Those are not missing; they are deliberately absent, and
[Account safety](account-safety.md) says why.
