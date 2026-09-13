# Gateway fixtures

## `ready.json` is synthetic

It was written by hand from the field lists in the userdoccers reference
(`docs.discord.food`), not recorded from a real connection. Nobody had an
account to record from when the core was built, and a fixture that pretends to
be a recording is worse than one that says it is not.

It is *shaped* correctly — both the versioned `read_state` and
`user_guild_settings` envelopes, the deduplicated `users` list, DMs by
`recipient_ids`, a category with text and voice channels under it, a guild mute
and a per-channel mute — and the tests in `src/discord/state/apply.rs` assert
the channel order and the unread and mute marks it produces. What it cannot do
is tell anybody what a live READY actually contains under the capabilities this
client identifies with. Replace it.

Everything in it is made up: ids are sequential and obviously fake
(`1000000000000000NN` for users, `2000000000000000NN` for guild things,
`4000000000000000NN` for DMs, `5000000000000000NN` for messages), names are
`sam`, `alex` and `jordan`, and there are no avatars, no tokens and no real
text.

## Recording a real one

```sh
STARCORD_RECORD_GATEWAY=./recordings starcord probe --token-from-stdin < token.txt
```

Every dispatch is written as `<seq>_<EVENT>.json`, containing the whole
envelope — `op`, `t`, `s` and `d` — so a fixture can be replayed through
`gateway::payload::decode` exactly as it arrived. `./recordings` is in
`.gitignore`: what comes out of it is somebody's entire account and must not be
committed as it stands.

## Scrubbing before it is committed

A recording names every server the account is in, everyone in its DMs, and
whatever was said in the last message of every channel. All of it has to go.
The tests do not care what anything is called; they care about shape and
ordering.

1. **Remove the whole session.** Delete `session_id`, `resume_gateway_url`,
   `auth_session_id_hash`, `analytics_token`, `sessions[]`, `country_code` and
   anything under `user_settings*`. None of it is needed to parse a READY and
   the first two are live credentials for the duration of the resume window.
2. **Renumber every id.** Map each distinct snowflake to a sequential fake one
   in the ranges above, consistently across the file — the tests assert
   *ordering*, so two ids that were adjacent must stay adjacent, and an id that
   is a message id must stay larger than the channel it is in. A snowflake also
   encodes its creation time, so keeping a real one is keeping a timestamp.
3. **Replace every name.** `username`, `global_name`, `nick`, guild `name`,
   channel `name` and `topic`. Keep the *lengths* roughly, because a two-column
   name and a forty-column one exercise different wrapping.
4. **Drop every hash.** `avatar`, `banner`, `icon`, `splash`,
   `discovery_splash`, emoji and sticker ids. A CDN hash is a fetchable URL.
5. **Drop every email, phone and `premium_*` field.**
6. **Check what is left**: `grep -iE '[a-z0-9._%+-]+@[a-z0-9.-]+|mfa\.|[A-Za-z0-9_-]{24}\.[A-Za-z0-9_-]{6}\.'`
   should find nothing, and so should a read-through by a person. Automated
   scrubbing misses the field that was added last week.
7. Replace `ready.json`, delete the "synthetic" section above, and adjust the
   assertions in `src/discord/state/apply.rs` to the new ids. If an assertion
   cannot be made to hold, that is the recording telling you something the
   documentation did not.

## What a real recording should settle

These were written from documentation and prior art, and are guesses until a
live session says otherwise:

- Whether `read_state` and `user_guild_settings` arrive versioned or bare under
  the capability set in `gateway/identify.rs` (`Capabilities::client()`).
- Whether guilds arrive flat or as `{id, properties, channels, …}`.
- Whether `DEDUPE_USER_OBJECTS` puts DM recipients in `recipient_ids` with the
  users in `users`, or leaves `recipients` inline.
- Whether READY_SUPPLEMENTAL carries the private channels that
  `PRIORITIZED_READY_PAYLOAD` deferred, and in what shape.
- Whether a member-list subscription is accepted as op 37 or op 14.
