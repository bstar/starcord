# If something is wrong

The usual suspects, in the order people hit them.

## There are no pictures, only blocks

The terminal has no graphics protocol, or the detection could not see through
ssh or a multiplexer. Half-blocks are the fallback and always work; they are
two rows of colour per cell rather than an image.

| Terminal | Pictures |
| --- | --- |
| kitty, Ghostty | kitty protocol |
| WezTerm, iTerm2 | yes |
| foot, xterm with sixel enabled | sixel |
| Alacritty, GNOME Terminal, Konsole | half-blocks |
| inside tmux | half-blocks |

Over ssh, or anywhere else the question goes unanswered, insist:

```toml
[ui]
graphics = "kitty"
```

The status line's right-hand end says which protocol is in use, which is the
quickest way to tell a terminal that cannot draw from a detection that did not
fire.

## Emoji are boxes, or the wrong width

That is the font, and no terminal program can choose the font it is drawn in.
Install a colour emoji font — `fonts-noto-color-emoji` on Debian,
`noto-fonts-emoji` on Arch — and select it in the terminal's own settings.

Custom emoji are drawn as pictures rather than through the font, so they are a
separate question: if those are missing, it is the section above.

## The QR code will not scan

Two causes, and the fix differs.

The code assumes a **dark terminal background**: a scanner needs the dark
modules dark, and in a terminal the dark thing is the background, so what gets
painted is the light modules. On a light background that is inside out. Use the
other polarity.

If the font renders the half-block characters with gaps, no polarity will help.
**The login URL is printed under the code** for exactly this case: put it
through any QR generator, or open it on the phone directly. It is the same URL
the squares encode.

## It says there is no keyring

On a headless machine, or one with no secret-service daemon running, there is
nowhere for the token to go. STAR/CORD falls back to `credentials.toml` at
mode 0600 under `~/.local/starcord/`, and says so when it signs in.

To choose deliberately:

```toml
[auth]
store = "file"     # always the file, even where there is a keyring
# store = "none"   # store nothing; sign in every time
```

[Signing in](auth.md) has the whole of it.

## "terminal too small"

Below 60 columns or 12 rows there is not enough room to draw a message and a
composer, so the window says so rather than drawing something unreadable.
Above that it degrades in steps: the member list goes first, then the DM list
folds into the channel list, then the server rail. Widening the window brings
each back in the same order, and a panel you closed yourself stays closed.

## It keeps saying it is reconnecting

The status line carries the connection state and a countdown to the next
attempt. Backoff doubles to a minute and resets when the session comes back, so
a long outage is expected to look like this for a while.

What it will not do is silently lose what you typed. A message sent while the
connection is down stays in the composer, and the banner says so.

```sh
starcord probe --token-from-stdin --follow < token.txt
```

reproduces it with no terminal involved: pull the network and watch the
transitions and the backoff printed with timestamps.

## Something is wrong and none of the above

```sh
starcord probe --token-from-stdin < token.txt
```

Connects, prints every connection transition with a timestamp, reports the
READY payload, and exits. A defect that reproduces without a terminal attached
is a defect with a much shorter report. [The command line](cli.md) has every
flag, including tailing a channel, sending a message, fetching one picture and
searching, each of which isolates one part of the client.

## Where the logs are

`~/.local/starcord/cache/starcord.log`. Nothing ever goes to the terminal: the
client owns the screen, and `probe` writes a report to standard output that a
script may be reading.

```sh
starcord -v                                      # debug level
STARCORD_LOG=starcord::discord::gateway=trace starcord    # a full filter
```

`STARCORD_LOG` takes `tracing`'s `EnvFilter` syntax and overrides `-v`
entirely.

The log never carries a token at any level, and never carries message text
above `debug`. It is still worth reading before pasting into an issue: at
`debug` it names the servers and channels you are in.
