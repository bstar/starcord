# Configuration

Everything STAR/CORD keeps lives under one directory, and everything it can be
told lives in one file there. This page covers the file, the themes and the
directory layout.

## The config file

`~/.local/starcord/config.toml`, written with comments on first run.

There is no token in it and there never will be one. It lives in the OS keyring
or in `credentials.toml` at mode 0600, so the config file is 0644, safe to copy
between machines, and safe to paste into a bug report.

Every table has defaults, and a file that omits one gets the whole table's
defaults. Editing one key never means writing the other six.

### The window

| Key | Does |
| --- | --- |
| `[ui] theme` | a theme id, or `"system"` to follow the desktop. Default `"catppuccin-mocha"`. See [Theming](#theming) |
| `[ui] graphics` | how pictures are drawn: `auto` asks the terminal, `kitty` insists on the kitty protocol, `blocks` (also spelled `halfblocks`) draws two pixels to a cell in any terminal at all, `off` draws no picture and leaves the chip that names the file. Default `auto` |
| `[ui] padding_x` / `padding_y` | blank columns and rows around the whole layout, for a terminal whose window has none. Default `0` |

### The column

The window is one vertical column of modules — the servers, the channels, the
conversation, the composer and the members, in the order you drill through
them — with the status line under it. Every module is always there, and the
list you are choosing in is the one that is open; the others fold to a single
line saying what is chosen in them. There is nothing to arrange, so there is
nothing written back to this file.

| Key | Does |
| --- | --- |
| `[ui] list_rows` | the tallest an open list may grow to, in rows. A shorter list is only as tall as it has entries; a longer one scrolls. The conversation gets whatever is left. Default `8` |

The window needs 60 columns by 21 rows: five modules with a row of content
each, plus the status line. Below that STAR/CORD says so rather than drawing
something it cannot draw honestly.

A `[layout]` table from 0.0.1 is ignored rather than refused, so a config file
written by the old version still loads.

### The messages

| Key | Does |
| --- | --- |
| `[chat] show_avatars` | draw avatars beside the first message of a block. Default `true` |
| `[chat] timestamps` | `off`, `short` (`14:32`, the default) or `full` (`2026-09-13 14:32`) |
| `[chat] group_window_secs` | how far apart two messages from one person can be and still be drawn as one block. Default `420` |
| `[chat] max_image_rows` | rows an inline picture may take. The rows are worked out from the size the picture says it is and the shape of a terminal cell, then held to this; `0` draws a chip with the file name instead. Default `12` |
| `[chat] emoji_images` | draw custom emoji as pictures rather than as `:name:`. Default `true` |
| `[chat] show_embeds` | draw link previews. Default `true` |
| `[chat] spoilers` | `"hidden"` (default) covers a spoiler until it is asked for; `"shown"` never covers one |

### Pictures, video and the cache

| Key | Does |
| --- | --- |
| `[media] animate` | when an animated picture may move: `always`, `focused` (default), `never`. At most four move at once, and only ones that are on screen; `alt+n` cycles it while running |
| `[media] cache_max_mib` | how large the on-disk media cache may grow before the oldest files are swept, in mebibytes. Default `512` |
| `[media] max_attachment_mib` | the largest attachment worth downloading. Default `25`, which is what an account without Nitro may upload |
| `[media] save_dir` | where the media viewer's `s` puts a file. `~` is expanded. Default `"~/Downloads"` |
| `[media] player` | argv for playing a video, never a shell line. The path is appended. Default `["mpv", "--"]`, where `--` is what keeps a file called `-x` a file rather than an option |

`player` is a list because a file name with a space or a semicolon in it is
somebody else's file name, and it reaches a program without ever reaching a
shell.

### Being told about things

| Key | Does |
| --- | --- |
| `[notify] enabled` | say anything at all about a mention or a DM. Default `true`. Off means no bell, no line in the status bar and no desktop notification |
| `[notify] only_when_unfocused` | say nothing about the channel already on screen while the terminal has focus. Default `true` |
| `[notify] dms_only` | only direct messages, rather than every mention in every server. Default `false` |
| `[notify] bell` | ring the terminal bell. Default `true`. The one notification that needs nothing installed and reaches a machine over ssh |
| `[notify] desktop` | a desktop notification as well, over the session bus. Default `false`: it puts somebody's name and words on a screen that may not be yours alone. With this off the client still beeps and still writes the line |

### Writing

| Key | Does |
| --- | --- |
| `[compose] send_key` | `"enter"` sends and shift+enter makes a newline; `"ctrl-enter"` swaps them. Default `"enter"` |
| `[compose] max_rows` | how tall the composer may grow before it scrolls. Default `10` |
| `[compose] typing_indicator` | tell the channel you are typing. Default `true` |

### The channel list

| Key | Does |
| --- | --- |
| `[channels] show_voice` | voice channels in the list. Default `false`, because this client cannot join one and a row that does nothing when activated is worse than no row |

### Signing in

| Key | Does |
| --- | --- |
| `[auth] store` | `"auto"` (default) uses the keyring if there is one and the file if there is not; `"keyring"` refuses to write a file; `"file"` always writes one; `"none"` stores nothing and asks every time |

[Signing in](auth.md) has the rest, including what to do on a headless machine.

### The GIF picker

| Key | Does |
| --- | --- |
| `[gifs] provider` | which service Discord proxies for the picker. Default `"tenor"` |
| `[gifs] media_format` | `gif`, `mp4` or `tinygif`. Default `"gif"` |
| `[gifs] locale` | what the results are localised to. Default `"en-US"` |

This is configuration rather than a constant because Discord proxies a third
party here and has announced a change of provider. A client that hard-codes one
is a client that stops returning results on the day that happens.

## Theming

`theme = "system"` follows the desktop. STAR/CORD reads Stylix's
`~/.config/stylix/palette.json`, so whatever base16 scheme the rest of the
desktop is set to, the client matches it.

Sixteen themes ship built in, and `t` and `T` cycle them live:

`winamp-classic` · `cosmic` · `catppuccin-mocha` · `catppuccin-latte` ·
`gruvbox-dark` · `nord` · `tokyo-night` · `dracula` · `rose-pine` ·
`everforest` · `solarized-dark` · `one-dark` · `kanagawa` · `ayu-dark` ·
`matte-black` · `terminal`

They are the same sixteen files STAR/AMP uses, because both programs take their
theme engine from [STAR/KIT](https://github.com/bstar/starkit). A theme set in
one looks like the theme set in the other.

Your own themes go in `~/.local/starcord/themes/` as `<id>.toml`, and a file
there wins over a built-in of the same name. [Themes](themes.md) has the
format.

### The chat roles

None of the sixteen shared files says anything about a message list, and none
of them should have to: a theme is a palette. So the `[chat]` table is
*derived* from that palette — one rule per role, run over sixteen schemes and
held to a contrast floor — and a theme file that does state a role has the last
word.

| Role | Is |
| --- | --- |
| `author_fg` | the name at the head of a block |
| `time_fg` | the timestamp beside it |
| `mention_fg` / `mention_bg` | a mention, and the highlight behind one |
| `link_fg` | a link |
| `code_fg` / `code_bg` | inline code and code blocks |
| `spoiler_bg` | the cover over a spoiler |
| `embed_bar` | the bar down the side of a link card |
| `divider_fg` | day and new-message dividers |
| `unread_fg` | an unread channel in the list |
| `reaction_bg` / `reaction_me_bg` | a reaction chip, and one you are part of |
| `presence_online` / `presence_idle` / `presence_dnd` / `presence_offline` | the dots |
| `system_fg` | a join, a pin, a call — the lines nobody typed |
| `disconnected_dim` | everything, while the connection is down |

Every built-in is checked against WCAG AA in the test suite. Anything carrying
words clears 4.5:1 against what it is drawn on; a mark that carries no letters,
like a presence dot, clears 3:1.

## Where it keeps things

Everything lives under one directory, so a whole STAR/CORD setup can be backed
up, moved, or deleted by moving one folder:

```
~/.local/starcord/
├── config.toml        your settings (0644)
├── credentials.toml   the token, only when there is no keyring (0600)
├── session.toml       last channel, drafts, scroll positions (0600)
├── themes/            your own themes
└── cache/             media and the log. Safe to delete
    ├── media/         downloaded attachments, avatars and emoji
    ├── client_build.toml
    └── starcord.log
```

The directories are mode 0700, and not out of taste: what is under there is a
session token, a list of every server and channel the account is in, and a log
that names them.

- `$STARCORD_DIR` relocates all of it; `$STARCORD_CONFIG_DIR` moves just the
  config.
- `session.toml` is 0600 for the drafts. Everything else in it is mildly
  private; a draft is your own unsent words, on disk because you closed the
  client mid-sentence.
- The log never carries a token at any level, and never carries message text
  above `debug`.
