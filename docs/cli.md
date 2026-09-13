# The command line

```
starcord [--verbose]
starcord probe [--token-from-stdin] [--no-store] [--offline] [--timeout SECONDS]
               [--legacy-lazy-request]
               [--channel ID] [--follow]
               [--send TEXT [--reply-to ID [--ping]]]
               [--media URL]
```

`--verbose` raises the log level to debug. It goes to the log file at
`~/.local/starcord/cache/starcord.log` and never to the terminal: the client
owns the alternate screen, and `probe` writes a report to stdout that a script
may be reading. `STARCORD_LOG` overrides the filter entirely, with the syntax
`tracing`'s `EnvFilter` uses — `STARCORD_LOG=starcord::discord::gateway=trace`,
for instance.

With no subcommand, `starcord` is the client. There is no client yet; it prints
a line saying so and exits 0.

## `starcord probe`

Connects, reports what happened, and exits. It is how the Discord core is
exercised while there is no UI, and it is meant to stay after there is one: a
defect that reproduces with no terminal attached is a defect with a much
shorter report.

```sh
starcord probe --token-from-stdin < token.txt
```

```
[  0.00s] connecting
[  0.31s] signed in as Sam (sam), token in the system keyring
[  0.62s] identifying
[  1.84s] online

READY as Sam, 12 guilds, 5 DMs

guilds, in the order READY sent them:
  A Server                                  18 channels
  Another One                                4 channels
  ...

DMs, newest conversation first:
  Alex •
  Jordan, Sam @2
```

A `•` is unread, `@n` is that many mentions. Nothing is printed for a channel
that is up to date or muted with nothing addressed to you.

### Where the token comes from

| | |
|---|---|
| `--token-from-stdin` | Read the whole of standard input, trim it, use it. |
| neither | Use the stored token — the OS keyring, or `credentials.toml`. |

**There is deliberately no `--token`.** A command-line argument is readable by
every other process on the machine through `/proc`, and it lands in shell
history besides. There is a test asserting that no such flag exists.

`--no-store` uses the token for the run and does not save it, which is what you
want when checking somebody else's report against your own account.

### The other flags

| | |
|---|---|
| `--timeout SECONDS` | How long to wait for READY. Default 45. |
| `--follow` | Stay connected after READY and keep printing until the timeout. Use it to watch a reconnect: pull the network cable and see the backoff. With `--channel` it tails that channel. |
| `--offline` | Do not look up the current web-client build number; use the pinned one. Also what the tests use, because a test suite has no business making a request to a CDN. |
| `--legacy-lazy-request` | Send op 14 rather than op 37 for member-list subscriptions. Same body either way. |
| `--media URL` | Fetch and decode one picture and exit. See below. |

## Tailing a channel

```sh
starcord probe --token-from-stdin --channel 1234567890 --follow < token.txt
```

Opens a channel, prints the last fifty messages, and then prints what happens
to it — new messages, edits, deletions, reactions and who is typing — until the
timeout.

```
[  2.10s] opening 1234567890
[  2.11s] Info: subscribed to 9876543210 with op 37 (update guild subscriptions)
[  2.44s] loaded

12 messages, oldest first:
09:41  Alex: morning
09:44  Jordan: ↩ Alex | did you see the thing
09:44  Jordan: yes [screenshot.png]

following 1234567890; press ctrl-c to stop
[  9.02s] typing: Alex
[ 11.40s] new:
  09:51  Alex: here it is <link>
[ 23.88s] reactions on 5000000000000000123: 👍 2*
```

The message body is `markdown::plain_text` — the parse with every marker taken
out — which is the only rendering the core can do, and exercising it is half of
what this mode is for. A `*` on a reaction count means this account is one of
the people who reacted. The `op 37` line is the other half: whether Discord
accepts the newer member-list opcode or the older `op 14` is something only a
live session can settle, so which one went out is printed rather than logged.
`--legacy-lazy-request` switches it to op 14 with the same body, for a session
where the newer one turns out not to be accepted.

**Tailing never marks anything read.** `probe` sends `SetFocus` and
`OpenChannel`, as the client does when somebody clicks a channel, and never
sends `MarkRead`. A debugging tool that changed what an account had seen would
be worse than no debugging tool.

## Sending a message

```sh
starcord probe --token-from-stdin --channel 1234567890 --send "hello" < token.txt
```

Sends one message and waits for the gateway to echo it back, which is the thing
worth watching: the nonce that goes out with the POST has to come back on the
message, or the optimistic row on screen becomes a second copy of it.

```
[  2.51s] sending 5 characters to 1234567890
[  2.52s] pending as nonce 5722948177327149
[  2.79s] accepted as 5000000000000000456
[  2.83s] echoed back by the gateway
  09:58  Sam: hello
```

`--reply-to ID` makes it a reply; `--ping` makes that reply notify the person
being answered, which it does not do by default.

### Exit status

`0` when READY arrived. Non-zero, with the reason on stderr, when the token was
rejected, when READY did not arrive within the timeout, or when the core could
not start. The last status the connection reached is included in the timeout
message, so "no READY within 45s; the last status was reconnecting" is a
different problem from "…was identifying".

## Fetching one picture

```sh
starcord probe --media https://cdn.discordapp.com/embed/avatars/0.png
```

```
url    https://cdn.discordapp.com/embed/avatars/0.png
cache  ~/.local/starcord/cache/media/79cb793022796e0cf825bed15fe489f5.*
       not fetched yet

1268 bytes in 0.16s
a still picture, 256x256
```

No account, no gateway, no keyring: media comes off a CDN and never carries a
token, which is exactly why it is worth being able to check on its own. The URL
goes through the same fetch, the same cache and the same decoder the client
uses, so running it twice on one URL is also how the cache is checked — the
second run says `already there` and takes no time at all. The `*` in the cache
line is the extension, which is decided by what the server says the bytes are.

An animation reports its frames:

```
an animation, 400x400, 44 frames over 3.96s, looping
frame delays 90ms to 90ms
```

Anything the decoder had to do differently is printed after the answer rather
than hidden — an animation past the three-hundred-frame cap comes back as its
first frame and says so.

### Recording fixtures

```sh
STARCORD_RECORD_GATEWAY=./recordings starcord probe --token-from-stdin < token.txt
```

Writes every dispatch as `<seq>_<n>_<EVENT>.json` — the whole envelope, so it
can be replayed through the decoder exactly as it arrived. `./recordings` is in
`.gitignore` because what lands there is an account's entire contents.
`testdata/gateway/README.md` has the scrub procedure for turning one into a
committed fixture.
