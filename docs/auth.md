# Signing in

Two ways in, and one of them is better. Read
[`account-safety.md`](account-safety.md) first if you have not: this document
is the mechanics, that one is whether you should.

## Scanning a code

```sh
starcord probe --qr
```

```
[  0.40s] scan this with the Discord app

  ███████████████████████████████████
  ██  ▄▄▄▄▄▄▄  ██ ▀▄█▀ ▄█  ▄▄▄▄▄▄▄ ██
  …

  https://discord.com/ra/DNn8ya4H4fMLuaytL9Dl70StgPcvf7b7ilLE1gPgQ98
  waiting for a scan; it expires in 356s

[ 21.80s] scanned by alex; confirm it on the phone
[ 24.10s] LoggedIn as alex (alex), token in the system keyring
```

Open Discord on your phone, go to **Settings → Scan QR Code**, and point it at
the terminal. The phone shows you the account that is about to be signed in;
tap to confirm, or cancel, and the terminal says which happened.

Your password is never typed. What happens underneath is:

1. The terminal generates an RSA-2048 keypair. The private half never leaves
   the process and is never written anywhere.
2. It sends the public half to Discord's remote-auth gateway and proves it
   holds the private half by decrypting a nonce.
3. Discord returns a fingerprint; the code is that fingerprint as a URL.
4. Your phone scans it and tells Discord who scanned it. Discord sends that
   identity back, sealed to the key, so the terminal can show you the name
   before you confirm.
5. You confirm, and Discord sends a ticket. The terminal exchanges it for a
   session token, also sealed to the key, checks the token works, and stores
   it.

The token is never displayed and never touches the clipboard.

A code lasts about six minutes. If nobody scans it, the terminal generates one
more by itself and then stops — at that point you have walked away, and a
client that keeps regenerating a login code unattended is a client leaving a
door open.

### If the code will not scan

The code assumes a **dark terminal background**: a scanner needs the dark
modules dark, and in a terminal the dark thing is the background, so what gets
painted is the light modules. On a light background that is inside out, and no
camera will read it. Use `--qr-invert`.

If your terminal font is not square, the code may come out stretched. Every
module is drawn two cells wide and one half-block tall, which is right for the
usual two-to-one cell, but a font with different proportions can still throw a
scanner. The URL is printed under the code for exactly this case: any QR
generator will turn it into a code your phone can read, and it is not a secret
until it is scanned — it only identifies the waiting login, which expires.

## Pasting a token

```sh
starcord probe --token-from-stdin < token.txt
```

Or, without a file on disk:

```sh
read -s token; printf %s "$token" | starcord probe --token-from-stdin; unset token
```

Standard input, never an argument. There is no `--token` flag and a test
asserts there never will be: a command-line argument is readable by every
other process on the machine through `/proc`, and it lands in your shell
history.

The token is trimmed, and surrounding quotes are stripped, because the usual
way to obtain one is a copy out of a browser's developer tools and it arrives
with a newline and sometimes with the quotes still attached. Anything with a
space or a control character in it is refused rather than sent: it would be
rejected anyway, and a header value containing a newline is a
request-splitting bug waiting for a worse day.

`--no-store` uses the token for the run and forgets it.

## Where the token is kept

| | |
|---|---|
| the OS keyring | Preferred. secret-service on Linux (GNOME Keyring, KWallet), the Keychain on macOS. Service `starcord`, account `token`. |
| `~/.local/starcord/credentials.toml` | Mode 0600, when there is no keyring. Written to a temporary sibling and renamed, so it never exists readable even for an instant. |

`[auth] store` in `config.toml` chooses:

```toml
[auth]
store = "auto"     # keyring if there is one, file if not — the default
# store = "keyring"  # keyring or fail; never write a file
# store = "file"     # always the file, even where there is a keyring
# store = "none"     # never store anything; sign in every time
```

Only the configured store is *read*. Trying both would be friendlier right up
to the moment somebody sets `store = "file"` to get away from a keyring that is
misbehaving and finds it still being used.

The token is never written to `config.toml`, to `session.toml`, to the log, to
an error message, or to standard output.

### On a headless machine

There is often no secret-service on a server or in a container, and `auto`
falls through to the 0600 file without comment. If you would rather it did not,
set `store = "none"` and paste the token each time, or run a keyring agent.

## Logging out

`Command::Logout` from the client — there is no UI yet — clears **both**
stores, whatever `[auth] store` says. A logout that leaves a working credential
in the other place is not a logout. It also drops the gateway connection and
empties the in-memory state.

By hand:

```sh
rm -f ~/.local/starcord/credentials.toml
secret-tool clear service starcord account token     # Linux, if you use one
```

Neither of those invalidates the session at Discord's end. **Only changing your
Discord password does that**, and it invalidates every other session too. If
you think a token has been seen by somebody else, change the password; deleting
the local copy is housekeeping, not revocation.

You can also see and end the session from Discord itself, under
**Settings → Devices**, which is worth knowing: a session signed in by QR shows
up there like any other.

## What is not supported

**Email and password, with or without a code from an authenticator.** Not
because it is hard, but because Discord gates it behind a captcha for anything
that does not look like a browser, and a terminal cannot solve one. Attempting
it would mean a login that fails in a way the user cannot do anything about.

**Multiple accounts.** One token, one keyring entry. Running a second account
means a second `$STARCORD_DIR`:

```sh
STARCORD_DIR=~/.local/starcord-work starcord probe --qr
```

which gives that account its own config, session, cache and credentials file.
The keyring entry is still shared, so an account kept this way wants
`store = "file"`.
