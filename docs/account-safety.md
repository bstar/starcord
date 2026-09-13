# Account safety

Read this before you use STAR/CORD. It is short, and none of it is reassurance.

## What this is

STAR/CORD signs in as **your user account**, not as a bot. There is no other
way for a terminal client to read your DMs: Discord's bot API cannot see them,
and there is no third kind of credential. Every terminal Discord client works
this way — discordo, endcord, concord — and so does every desktop one that is
not Discord's own, including Vesktop and Legcord, which are browser shells
around the same web client and hold the same kind of session.

## What Discord's terms say

Discord's Terms of Service prohibit "using self-bots or user-bots". The
enforcement that people actually report is against **automation**: accounts that
scrape member lists, mass-join servers, spam messages, or run unattended. What
has not, to our knowledge, produced action is a person reading and writing their
own messages through something other than the official client.

That is a statement about what has been observed, not a promise. The rule as
written is broad enough to cover this client, the interpretation is Discord's
alone, and the consequence for a violation is account termination. Nobody can
give you an assurance here and you should be suspicious of anyone who does.

**If your account matters to you more than a terminal client does, use the
official client.** That is a real answer, not a disclaimer.

## What this client refuses to do

The position STAR/CORD takes is that it should do only what a person at the
keyboard does. That is enforced by what the core *can express*, not by
intention: the following are absent from the `Command` enum, so no amount of
UI work can reach them.

- Friend requests, and any change to a relationship.
- Joining or leaving a server; using or creating an invite.
- Opening a DM with somebody who is not already a friend.
- Anything bulk: no "delete all", no "react to everything", no export.
- Profile scraping: there is no way to ask for a user this client is not
  already showing you.

A change that adds one of these needs an argument that starts with why a human
would have pressed a key for it.

## What it does to stay ordinary

- **One description of the client, everywhere.** A single `ClientProps` value
  produces the IDENTIFY properties, the `X-Super-Properties` header, the
  `User-Agent` and the remote-auth socket's headers, so they cannot drift apart
  and describe two different browsers. The build number is looked up from
  Discord's own assets and cached for a day.
- **Presence is `online` with no activity.** No Rich Presence, ever. That is
  STAR/AMP's job, over a local desktop socket, with no token involved.
- **Requests are paced.** The rate limiter waits before sending rather than
  reacting to a 429, because on a user account a 429 is not a retry, it is a
  line in somebody's ledger. Typing goes out at most once every nine seconds
  per channel. Read marks are coalesced to the newest message per channel per
  second, skipped when the channel is already read, and never sent for a
  channel you are not looking at. Member lists are requested only for the
  server you have open. History is one request per channel.
- **A rejected token stops everything.** A 401 is never retried and closes the
  session, because a client hammering a rejected credential is the single most
  conspicuous thing on Discord's side of the connection.
- **Nothing runs unattended.** There is no daemon, no background sync, no
  scheduled anything.

## Sign in by scanning, not by typing

Two ways in. **Scan the QR code**, which is what
[`auth.md`](auth.md) describes and what you should use.

Scanning means your password is never typed into a terminal. The terminal
generates a keypair, Discord seals a session token to it, and the token goes
straight into your keyring — it is never displayed, never in your clipboard,
never in your shell history. Your phone shows you what is asking before it
agrees, and you can revoke the session from Discord's own settings at any time.

Pasting a token works and is supported, and it is worse: the token exists in a
clipboard, usually in a file, and in your head as "the thing I copy". A Discord
user token is not a password. It **is** the session, it does not expire on its
own, and two-factor authentication does not protect it. Anyone who reads one is
logged in as you until you invalidate it by changing your password.

There is deliberately no way to pass a token as a command-line argument:
`argv` is readable by every other process on the machine through `/proc`, and
it lands in your shell history besides.

## Where the token lives

The OS keyring — secret-service on Linux, the Keychain on macOS — or, when
there is none, `~/.local/starcord/credentials.toml` at mode 0600, written
through a temporary file so it never exists readable even for an instant.

It is never written to the config file, the session file, the log, an error
message, standard output, or a command-line argument. `Token`'s `Debug` prints
`Token(<redacted>)`, so printing any structure that happens to contain one is
safe.

## If something goes wrong

Change your Discord password. That invalidates every session including this
one, which is the only thing that does.

## The rest of your data

`~/.local/starcord/` is mode 0700 throughout, because what is under there is a
list of every server and channel your account is in, your unsent drafts, and a
cache of every picture you have looked at. On a shared machine the default
would make all of that readable by every other account on the box.

Nothing is sent anywhere but Discord. There is no telemetry, no crash
reporting, and no update check.
