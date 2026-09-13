# Security

## Reporting

Use GitHub's private vulnerability reporting on this repository
(Security → Report a vulnerability). Please do not open a public issue for
something exploitable.

I work on this in my spare time, so expect a first reply within a week rather
than a day.

## Threat model

Who the attacker is, at each place STAR/CORD takes input from somewhere else.

| Boundary | In scope |
| --- | --- |
| **The gateway** | Yes. Every byte of a session arrives compressed over one websocket and is inflated and parsed here. A payload that crashes the client is worth reporting; one that gets code running is worth reporting urgently. |
| **What other people send you** | Yes. A message body is markdown somebody else wrote, and it reaches a hand-written parser. So are embeds, reactions, file names, display names and server names, all of which end up measured and drawn. |
| **Attachments and avatars** | Yes. Pictures come off a CDN as bytes somebody else produced, and are decoded in this process. A crafted PNG, GIF or WebP is the most plausible hostile file this program will ever open. |
| **The GIF picker and search** | Yes, as far as they can reach. Discord proxies a third party for GIFs; what comes back is a list of titles and URLs from a service neither you nor this program controls. |
| **The token** | Yes. It is the account. Anything that writes it somewhere other than the keyring or the mode-0600 credentials file, prints it, logs it, or sends it to a host that is not Discord's API is a vulnerability regardless of how it happens. |
| **Other local users** | Yes on a shared machine. Everything under the STAR/CORD directory is mode 0700, the credentials and session files are 0600, and the log never carries a token or a message body. |
| **The person running STAR/CORD** | No. A file you attach, a link you follow and a player argv you configure are your own authority. |
| **The build** | Yes. What the release workflow downloads is pinned and checksummed, and what CI runs is pinned to commits. |

## What is worth reporting

STAR/CORD parses a lot of input it did not write:

- gateway frames, inflated through one deflate context shared for the life of
  the connection
- the JSON of every dispatch, and the READY payload in particular, which is the
  largest single document Discord sends
- message markdown, in a hand-written parser with no error type
- PNG, JPEG, WebP and GIF bytes from `cdn.discordapp.com` and from media
  proxies
- JSON from the GIF provider and from message search

Each of those has a stated limit, and a way past one of them is worth a report
on its own:

| Limit | Value |
| --- | --- |
| Compressed gateway frame | 16 MiB |
| Inflated gateway message | 64 MiB |
| Markdown input | 4096 characters, nesting depth 16 |
| Picture dimensions, checked from the header before decoding | 8192 px on a side |
| Decoded allocation | 128 MiB |
| Animation | 300 frames, or 50 million pixels |
| Avatar, emoji, guild icon | 2 MiB |
| Embedded picture | 10 MiB |
| Attachment | `[media] max_attachment_mib`, 25 by default |

A malformed payload or picture that crashes the client is plausible and worth a
report. One that gets code running, that writes outside the STAR/CORD
directory, or that makes a file name from somebody else's string escape the
media cache, is worth one urgently.

Nothing here listens on a network port. The only hosts this program dials are
Discord's API, its gateway, its remote-auth gateway, its CDN and media proxies,
and the storage URL Discord hands back for an upload.

## The token

It is written in exactly two places: the OS keyring, or `credentials.toml` at
mode 0600 when there is no keyring. Never the config file, never the session
file, never the log, never an error message, never standard output, and never a
command-line argument — which is why `starcord probe` reads it from standard
input, because `argv` is readable by every other process on the machine.

`Token`'s `Debug` prints `Token(<redacted>)`, so a `{:?}` on any struct that
transitively holds one is safe, and there is a test that says so. The RSA
private key the QR login is sealed to is redacted the same way.

Requests to a host Discord does not control never carry the token. Attachment
and avatar URLs point at a CDN and at media proxies, and an upload's second leg
goes to Google's storage; all of those go out on a client that sends only a
`User-Agent`, and refuse anything that is not https.

## One accepted advisory

`deny.toml` ignores exactly one, and the reasoning is written out in full
there:

**RUSTSEC-2023-0071**, the Marvin attack against `rsa` 0.9. RSA decryption in
that crate is not constant time, so an attacker who can watch the timing of
many decryptions with one key may recover it. There is no fixed release, and no
other pure-Rust crate implements RSA-OAEP, so the choice is to use it or to
have no QR login. What the advisory needs is a long-lived key and an attacker
who can make it decrypt repeatedly while timing the answers. The key here is
generated for one login attempt, decrypts three payloads, and is dropped when
the socket closes — within six minutes either way — and the only party who
could issue those decryptions is Discord's own remote-auth gateway. It is
revisited when `rsa` 0.10 ships.

## What is not a vulnerability

- Signing in as a user account. That is what this program is, and
  [docs/account-safety.md](docs/account-safety.md) says what it means.
- A message you were sent appearing in your terminal, with the picture that was
  attached to it.
- `[notify] desktop = true` putting somebody's name and words on your screen.
  That is what the setting does, and it is off by default for exactly this
  reason.
- `[media] player` launching the program you configured, with the path of a
  file you asked to open appended to its argv. It is argv and never a shell
  line, which is the part that would be a vulnerability.
