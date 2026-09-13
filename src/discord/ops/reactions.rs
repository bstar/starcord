//! Adding and removing this account's own reactions.
//!
//! Two things here are worth stating.
//!
//! **The chip changes before Discord has heard about it.** Clicking a reaction
//! is the most immediate thing in a chat client and a round trip is a hundred
//! milliseconds of nothing happening, so the count and the `me` flag move
//! locally first. If the request fails the change is put back — which is why
//! the update is expressed as an add or a remove rather than as a new value:
//! two clicks and a gateway event can interleave, and reversing the *operation*
//! lands in the right place where writing back a remembered number does not.
//!
//! **Only this account's own reaction can be touched.** There is no route here
//! for removing somebody else's; that is a moderation action, and this client
//! does not have moderation actions. See `docs/account-safety.md`.
//!
//! The emoji is a path segment, which is the one place in Discord's API where a
//! segment is neither a snowflake nor a fixed word. `http::route` percent-
//! encodes it with the unreserved set, so a custom emoji whose name somehow
//! contains a slash cannot rewrite the request.

use crate::discord::handle::{EmojiRef, Event, MessagesChange, Note};
use crate::discord::http::api;
use crate::discord::snowflake::{ChannelId, MessageId};

use super::Ops;

/// `Command::AddReaction`.
pub async fn add(ops: &Ops, channel: ChannelId, message: MessageId, emoji: EmojiRef) {
    react(ops, channel, message, emoji, true).await;
}

/// `Command::RemoveReaction`.
pub async fn remove(ops: &Ops, channel: ChannelId, message: MessageId, emoji: EmojiRef) {
    react(ops, channel, message, emoji, false).await;
}

async fn react(ops: &Ops, channel: ChannelId, message: MessageId, emoji: EmojiRef, adding: bool) {
    let partial = emoji.as_partial();

    // Optimistic. `true` for `me` both ways: this is always this account's own
    // reaction, and the flag is what decides whether the chip is highlighted.
    let changed = apply_local(ops, channel, message, &partial, adding);
    if changed {
        ops.emit(Event::Messages(channel, MessagesChange::Reactions(message)));
    }

    let sent = if adding {
        ops.rest(api::add_reaction(&ops.http, channel, message, &emoji))
            .await
    } else {
        ops.rest(api::remove_reaction(&ops.http, channel, message, &emoji))
            .await
    };

    let Err(e) = sent else {
        return;
    };

    tracing::warn!("could not change the reaction on {message}: {e}");
    if changed {
        // Put it back. The inverse operation rather than a remembered count,
        // because somebody else's reaction may have arrived in between.
        apply_local(ops, channel, message, &partial, !adding);
        ops.emit(Event::Messages(channel, MessagesChange::Reactions(message)));
    }
    ops.note(Note::warning(
        "reaction",
        format!("the reaction did not stick: {e}"),
    ));
}

/// Move the count and the `me` flag, and say whether anything moved.
fn apply_local(
    ops: &Ops,
    channel: ChannelId,
    message: MessageId,
    emoji: &crate::discord::model::PartialEmoji,
    adding: bool,
) -> bool {
    let mut state = ops.state_mut();
    let store = state.messages_mut(channel);
    if adding {
        store.add_reaction(message, emoji, true)
    } else {
        store.remove_reaction(message, emoji, true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::harness;
    use super::*;
    use crate::discord::snowflake::EmojiId;

    const CHANNEL: ChannelId = ChannelId(7);
    const MESSAGE: MessageId = MessageId(500);

    fn thumb() -> EmojiRef {
        EmojiRef::Unicode("\u{1f44d}".into())
    }

    fn pepe() -> EmojiRef {
        EmojiRef::Custom {
            name: "pepe".into(),
            id: EmojiId(12345),
            animated: false,
        }
    }

    fn seed(ops: &Ops) {
        let message: crate::discord::model::Message = serde_json::from_str(
            r#"{"id":"500","channel_id":"7","content":"hi",
                "author":{"id":"1","username":"sam"}}"#,
        )
        .unwrap();
        ops.state_mut()
            .messages_mut(CHANNEL)
            .replace(vec![message], true);
    }

    fn chips(ops: &Ops) -> Vec<(String, u32, bool)> {
        ops.state()
            .message(CHANNEL, MESSAGE)
            .map(|m| {
                m.reactions
                    .iter()
                    .map(|r| (r.emoji.name.clone().unwrap_or_default(), r.count, r.me))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The table that says what goes in the URL. Getting this wrong is a 400
    /// on a unicode emoji and a 404 on a custom one, and neither says why.
    #[test]
    fn every_emoji_becomes_the_path_segment_discord_expects() {
        use crate::discord::http::route::Route;

        let cases: Vec<(EmojiRef, &str)> = vec![
            (
                EmojiRef::Unicode("\u{1f44d}".into()),
                "/channels/7/messages/500/reactions/%F0%9F%91%8D/@me",
            ),
            (
                // A keycap: three code points, all of them encoded.
                EmojiRef::Unicode("1\u{fe0f}\u{20e3}".into()),
                "/channels/7/messages/500/reactions/1%EF%B8%8F%E2%83%A3/@me",
            ),
            (
                EmojiRef::Custom {
                    name: "pepe".into(),
                    id: EmojiId(12345),
                    animated: false,
                },
                "/channels/7/messages/500/reactions/pepe%3A12345/@me",
            ),
            (
                EmojiRef::Custom {
                    name: "party_blob".into(),
                    id: EmojiId(999),
                    animated: true,
                },
                "/channels/7/messages/500/reactions/party_blob%3A999/@me",
            ),
            (
                // Nothing in a name may end up meaning something in a path.
                EmojiRef::Custom {
                    name: "a/b?c#d".into(),
                    id: EmojiId(1),
                    animated: false,
                },
                "/channels/7/messages/500/reactions/a%2Fb%3Fc%23d%3A1/@me",
            ),
        ];

        for (emoji, expected) in cases {
            let add = Route::AddReaction(CHANNEL, MESSAGE, emoji.clone());
            let remove = Route::RemoveReaction(CHANNEL, MESSAGE, emoji.clone());
            assert_eq!(add.path(), expected, "{emoji:?}");
            assert_eq!(
                remove.path(),
                expected,
                "both directions address the same thing"
            );
            assert_eq!(add.method(), "PUT");
            assert_eq!(remove.method(), "DELETE");
        }
    }

    /// Every emoji on one channel shares one allowance, so the emoji must not
    /// be part of the bucket.
    #[test]
    fn two_different_emoji_share_one_allowance() {
        use crate::discord::http::route::Route;

        let a = Route::AddReaction(CHANNEL, MESSAGE, thumb());
        let b = Route::AddReaction(CHANNEL, MessageId(501), pepe());
        assert_eq!(a.bucket(), b.bucket());
        assert_ne!(
            a.bucket(),
            Route::RemoveReaction(CHANNEL, MESSAGE, thumb()).bucket(),
            "the method is part of the bucket"
        );
    }

    #[tokio::test]
    async fn the_chip_appears_before_discord_has_agreed() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        seed(&h.ops);
        add(&h.ops, CHANNEL, MESSAGE, thumb()).await;

        assert_eq!(chips(&h.ops), vec![("\u{1f44d}".to_string(), 1, true)]);
        assert!(h.events.try_iter().any(
            |e| matches!(e, Event::Messages(c, MessagesChange::Reactions(m)) if c == CHANNEL && m == MESSAGE)
        ));
    }

    /// The whole reason the optimistic change is expressed as an operation: a
    /// refusal has to leave the chip exactly as it was.
    #[tokio::test]
    async fn a_refused_reaction_is_put_back() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "code": 50013, "message": "Missing Permissions"
            })))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        seed(&h.ops);
        add(&h.ops, CHANNEL, MESSAGE, thumb()).await;

        assert!(
            chips(&h.ops).is_empty(),
            "the chip stayed after discord refused it: {:?}",
            chips(&h.ops)
        );
        let notes: Vec<String> = h
            .events
            .try_iter()
            .filter_map(|e| match e {
                Event::Note(note) => Some(note.text),
                _ => None,
            })
            .collect();
        assert!(
            notes.iter().any(|n| n.contains("did not stick")),
            "{notes:?}"
        );
    }

    /// Removing works the same way in reverse, and a chip that only this
    /// account was on disappears entirely.
    #[tokio::test]
    async fn removing_takes_the_chip_away_and_a_refusal_brings_it_back() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        seed(&h.ops);
        h.ops
            .state_mut()
            .messages_mut(CHANNEL)
            .add_reaction(MESSAGE, &thumb().as_partial(), true);
        h.ops
            .state_mut()
            .messages_mut(CHANNEL)
            .add_reaction(MESSAGE, &thumb().as_partial(), false);
        assert_eq!(chips(&h.ops), vec![("\u{1f44d}".to_string(), 2, true)]);

        remove(&h.ops, CHANNEL, MESSAGE, thumb()).await;

        assert_eq!(
            chips(&h.ops),
            vec![("\u{1f44d}".to_string(), 2, true)],
            "somebody else's reaction must survive this account's failed one"
        );
    }

    /// A reaction on a message that has scrolled out of the window still goes
    /// to Discord; there is simply nothing local to change.
    #[tokio::test]
    async fn a_reaction_on_a_message_nobody_holds_is_still_sent() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        add(&h.ops, CHANNEL, MessageId(9999), pepe()).await;

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].url.path().contains("pepe%3A12345"),
            "{}",
            requests[0].url
        );
    }
}
