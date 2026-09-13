# The command line

```
starcord [--verbose]
starcord probe [--token-from-stdin] [--no-store] [--follow] [--offline] [--timeout SECONDS]
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
| `--follow` | Stay connected after READY and keep printing connection changes until the timeout. Use it to watch a reconnect: pull the network cable and see the backoff. |
| `--offline` | Do not look up the current web-client build number; use the pinned one. Also what the tests use, because a test suite has no business making a request to a CDN. |

### Exit status

`0` when READY arrived. Non-zero, with the reason on stderr, when the token was
rejected, when READY did not arrive within the timeout, or when the core could
not start. The last status the connection reached is included in the timeout
message, so "no READY within 45s; the last status was reconnecting" is a
different problem from "…was identifying".

### Recording fixtures

```sh
STARCORD_RECORD_GATEWAY=./recordings starcord probe --token-from-stdin < token.txt
```

Writes every dispatch as `<seq>_<n>_<EVENT>.json` — the whole envelope, so it
can be replayed through the decoder exactly as it arrived. `./recordings` is in
`.gitignore` because what lands there is an account's entire contents.
`testdata/gateway/README.md` has the scrub procedure for turning one into a
committed fixture.
