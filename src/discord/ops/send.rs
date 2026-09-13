//! Sending, editing and deleting.
//!
//! A message appears the instant it is typed, before Discord has heard of it.
//! That is an optimistic row keyed by a random nonce, and the nonce is what
//! makes the trick safe: it goes out with the POST, comes back on the message
//! through the gateway, and matching it turns the optimistic row into the real
//! one rather than leaving the sender looking at their message twice.
//!
//! Three things can go wrong and all three are handled here rather than left to
//! the UI:
//!
//! - **The POST fails.** The row stays, marked failed, with the text still in
//!   it. `RetrySend` sends it again with the same nonce; `CancelSend` throws it
//!   away. A message that vanishes silently is a message the sender believes
//!   they sent.
//! - **The echo never arrives.** The gateway is a separate connection from the
//!   REST call, and it can be down or behind. Ten seconds after a successful
//!   POST, if the optimistic row is still there, the response body is inserted
//!   in its place. The nonce means the echo cannot then duplicate it.
//! - **The message is too long.** Refused before a request is built. On a user
//!   account a rejected request is a line in somebody's ledger, and the length
//!   is knowable without asking.

use rand::Rng as _;

use crate::discord::handle::{Event, MessagesChange, Nonce, Note};
use crate::discord::http::api::{self, AllowedMentions, CreateMessage, ReplyTo, MAX_CONTENT};
use crate::discord::model::Message;
use crate::discord::snowflake::{ChannelId, MessageId};
use crate::discord::state::messages::{PendingSend, PendingState};

use super::Ops;

/// How long to wait for the gateway echo before inserting the HTTP response.
pub const ECHO_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// A fresh nonce.
///
/// Random rather than sequential, and bounded to 53 bits so that it survives a
/// trip through anything that treats it as a JavaScript number — which Discord's
/// own web client does.
fn fresh_nonce() -> Nonce {
    Nonce(rand::rng().random_range(1..(1u64 << 53)))
}

/// `Command::SendMessage`.
pub async fn send_message(
    ops: &Ops,
    channel: ChannelId,
    content: String,
    reply_to: Option<MessageId>,
    mention_author: bool,
) {
    let nonce = fresh_nonce();

    if content.chars().count() > MAX_CONTENT {
        // No optimistic row for something that was never going to send.
        ops.note(Note::error(
            "too-long",
            format!("a message may be at most {MAX_CONTENT} characters"),
        ));
        ops.emit(Event::SendResult {
            nonce,
            result: Err(format!("longer than {MAX_CONTENT} characters")),
        });
        return;
    }

    {
        let mut state = ops.state_mut();
        state.messages_mut(channel).add_pending(PendingSend::new(
            nonce,
            content.clone(),
            reply_to,
            mention_author,
        ));
    }
    ops.shared().sends.insert(nonce, channel);
    // The user has stopped typing, so the next keystroke should send a fresh
    // indicator rather than wait out the gate.
    super::typing::forget(ops, channel);
    ops.emit(Event::Messages(channel, MessagesChange::Pending(nonce)));

    post(ops, channel, nonce, &content, reply_to, mention_author).await;
}

/// `Command::RetrySend`.
pub async fn retry(ops: &Ops, nonce: Nonce) {
    let channel = {
        let shared = ops.shared();
        match shared.sends.get(&nonce) {
            Some(channel) => *channel,
            None => {
                tracing::debug!("nothing to retry for {nonce}");
                return;
            }
        }
    };

    let pending = {
        let state = ops.state();
        let Some(store) = state.messages(channel) else {
            return;
        };
        match store.pending().iter().find(|p| p.nonce == nonce) {
            Some(pending) => pending.clone(),
            None => return,
        }
    };

    {
        let mut state = ops.state_mut();
        if let Some(row) = state.messages_mut(channel).pending_mut(nonce) {
            row.state = PendingState::Sending;
        }
    }
    ops.emit(Event::Messages(channel, MessagesChange::Pending(nonce)));

    post(
        ops,
        channel,
        nonce,
        &pending.content,
        pending.reply_to,
        pending.mention_author,
    )
    .await;
}

/// `Command::CancelSend`: throw the row away.
pub fn cancel(ops: &Ops, nonce: Nonce) {
    let channel = ops.shared().sends.remove(&nonce);
    let Some(channel) = channel else {
        return;
    };
    ops.state_mut().messages_mut(channel).take_pending(nonce);
    ops.emit(Event::Messages(channel, MessagesChange::Pending(nonce)));
}

async fn post(
    ops: &Ops,
    channel: ChannelId,
    nonce: Nonce,
    content: &str,
    reply_to: Option<MessageId>,
    mention_author: bool,
) {
    let body = CreateMessage {
        content,
        nonce: nonce.to_string(),
        message_reference: reply_to.map(|message_id| ReplyTo {
            message_id,
            channel_id: channel,
            fail_if_not_exists: false,
        }),
        allowed_mentions: AllowedMentions::new(mention_author),
        tts: false,
    };

    let sent = ops
        .rest(api::create_message(&ops.http, channel, &body))
        .await;

    match sent {
        Ok(message) => {
            let id = message.id;
            ops.emit(Event::SendResult {
                nonce,
                result: Ok(id),
            });
            schedule_fallback(ops, channel, nonce, message);
        }
        Err(e) => {
            let reason = e.to_string();
            tracing::warn!("could not send to {channel}: {reason}");
            ops.state_mut()
                .messages_mut(channel)
                .fail_pending(nonce, reason.clone());
            ops.emit(Event::Messages(channel, MessagesChange::Pending(nonce)));
            ops.emit(Event::SendResult {
                nonce,
                result: Err(reason),
            });
        }
    }
}

/// If the gateway echo has not arrived in ten seconds, use what the POST
/// returned.
///
/// The nonce is on the inserted message, so an echo that turns up late is
/// recognised as a duplicate rather than appended a second time.
fn schedule_fallback(ops: &Ops, channel: ChannelId, nonce: Nonce, message: Message) {
    let ops = ops.clone();
    tokio::spawn(async move {
        tokio::time::sleep(ECHO_GRACE).await;

        let still_waiting = {
            let state = ops.state();
            state
                .messages(channel)
                .is_some_and(|store| store.has_pending(nonce))
        };
        if !still_waiting {
            ops.shared().sends.remove(&nonce);
            return;
        }

        tracing::debug!("no gateway echo for {nonce}; using the response body");
        let id = message.id;
        {
            let mut state = ops.state_mut();
            let store = state.messages_mut(channel);
            store.take_pending(nonce);
            store.upsert(message);
        }
        ops.shared().sends.remove(&nonce);
        ops.emit(Event::Messages(channel, MessagesChange::Appended(id)));
    });
}

/// `Command::EditMessage`.
pub async fn edit(ops: &Ops, channel: ChannelId, message: MessageId, content: String) {
    if content.chars().count() > MAX_CONTENT {
        ops.note(Note::error(
            "too-long",
            format!("a message may be at most {MAX_CONTENT} characters"),
        ));
        return;
    }

    let edited = ops
        .rest(api::edit_message(&ops.http, channel, message, &content))
        .await;
    match edited {
        Ok(updated) => {
            // The gateway sends a MESSAGE_UPDATE for this too. Applying the
            // response as well costs one redundant write and means the edit
            // shows immediately on a slow or dropped socket.
            ops.state_mut().messages_mut(channel).upsert(updated);
            ops.emit(Event::Messages(channel, MessagesChange::Updated(message)));
        }
        Err(e) => {
            tracing::warn!("could not edit {message}: {e}");
            ops.note(Note::warning("edit", format!("the edit did not save: {e}")));
        }
    }
}

/// `Command::DeleteMessage`.
pub async fn delete(ops: &Ops, channel: ChannelId, message: MessageId) {
    let deleted = ops
        .rest(api::delete_message(&ops.http, channel, message))
        .await;
    match deleted {
        Ok(()) => {
            // Bound rather than used as the `if` condition, so the write guard
            // is released before anything else runs.
            let removed = ops.state_mut().messages_mut(channel).remove(message);
            if removed {
                ops.emit(Event::Messages(channel, MessagesChange::Removed(message)));
            }
        }
        Err(e) => {
            tracing::warn!("could not delete {message}: {e}");
            ops.note(Note::warning(
                "delete",
                format!("the message was not deleted: {e}"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::harness;
    use super::*;
    use crate::discord::state::messages::Received;

    const CHANNEL: ChannelId = ChannelId(7);

    fn echo(id: u64, nonce: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id.to_string(),
            "channel_id": "7",
            "content": "hello",
            "nonce": nonce,
            "author": {"id": "1", "username": "sam"}
        })
    }

    fn pending_nonces(ops: &Ops) -> Vec<Nonce> {
        ops.state()
            .messages(CHANNEL)
            .map(|s| s.pending().iter().map(|p| p.nonce).collect())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_message_appears_before_discord_has_heard_of_it() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(echo(500, "1")))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        h.ops.state_mut().messages_mut(CHANNEL).set_at_latest(true);
        send_message(&h.ops, CHANNEL, "hello".into(), None, false).await;

        let events: Vec<Event> = h.events.try_iter().collect();
        let pending = events
            .iter()
            .any(|e| matches!(e, Event::Messages(c, MessagesChange::Pending(_)) if *c == CHANNEL));
        assert!(pending, "the optimistic row was never announced");
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::SendResult { result: Ok(_), .. })));

        // The row is still there: the gateway echo has not arrived.
        assert_eq!(pending_nonces(&h.ops).len(), 1);

        // And the body it was sent with carried the nonce.
        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["content"], "hello");
        assert!(body["nonce"].as_str().is_some());
    }

    #[tokio::test]
    async fn the_echo_replaces_the_optimistic_row() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(echo(500, "1")))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        h.ops.state_mut().messages_mut(CHANNEL).set_at_latest(true);
        send_message(&h.ops, CHANNEL, "hello".into(), None, false).await;

        let nonce = pending_nonces(&h.ops)[0];
        let arrived: Message = serde_json::from_value(echo(500, &nonce.to_string())).unwrap();
        let received = h.ops.state_mut().messages_mut(CHANNEL).receive(arrived);

        assert_eq!(received, Received::Appended);
        assert!(
            pending_nonces(&h.ops).is_empty(),
            "the sender is looking at their message twice"
        );
        assert_eq!(h.ops.state().messages(CHANNEL).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_refused_send_keeps_its_row_and_can_be_retried() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"message": "Missing Access", "code": 50001})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(echo(500, "1")))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        send_message(&h.ops, CHANNEL, "hello".into(), None, false).await;

        let nonce = pending_nonces(&h.ops)[0];
        {
            let state = h.ops.state();
            let store = state.messages(CHANNEL).unwrap();
            assert!(store.pending()[0].failed(), "a failed send lost its row");
            assert_eq!(store.pending()[0].content, "hello");
        }

        retry(&h.ops, nonce).await;
        {
            let state = h.ops.state();
            let store = state.messages(CHANNEL).unwrap();
            assert!(
                !store.pending()[0].failed(),
                "the retry did not clear the failure"
            );
        }

        // The retry must reuse the nonce, or the echo matches nothing.
        let requests = server.received_requests().await.unwrap();
        let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(first["nonce"], second["nonce"]);
    }

    #[tokio::test]
    async fn a_cancelled_send_takes_its_row_with_it() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        send_message(&h.ops, CHANNEL, "hello".into(), None, false).await;
        let nonce = pending_nonces(&h.ops)[0];

        cancel(&h.ops, nonce);
        assert!(pending_nonces(&h.ops).is_empty());
        cancel(&h.ops, nonce);
    }

    #[tokio::test]
    async fn an_overlong_message_is_refused_before_any_request() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        send_message(&h.ops, CHANNEL, "x".repeat(MAX_CONTENT + 1), None, false).await;

        assert!(
            pending_nonces(&h.ops).is_empty(),
            "a message that was never going to send got a row"
        );
        let events: Vec<Event> = h.events.try_iter().collect();
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::SendResult { result: Err(_), .. })));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    /// The gateway is a separate connection from the REST call and can be
    /// behind or down. Ten seconds later the response body is used instead.
    ///
    /// Driven with the clock stopped and nothing on the other end: a paused
    /// clock and a real socket do not mix, because the runtime auto-advances
    /// whenever it parks on I/O.
    #[tokio::test(start_paused = true)]
    async fn a_send_with_no_echo_falls_back_to_the_response_body() {
        let h = harness("http://127.0.0.1:1");
        let nonce = Nonce(4242);
        let sent: Message = serde_json::from_value(echo(500, &nonce.to_string())).unwrap();

        {
            let mut state = h.ops.state_mut();
            let store = state.messages_mut(CHANNEL);
            store.set_at_latest(true);
            store.add_pending(PendingSend::new(nonce, "hello".into(), None, false));
        }
        h.ops.shared().sends.insert(nonce, CHANNEL);
        schedule_fallback(&h.ops, CHANNEL, nonce, sent);

        tokio::time::sleep(ECHO_GRACE - std::time::Duration::from_secs(1)).await;
        assert_eq!(
            pending_nonces(&h.ops).len(),
            1,
            "the fallback fired before the grace period was up"
        );

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;

        assert!(
            pending_nonces(&h.ops).is_empty(),
            "the optimistic row outlived the grace period"
        );
        assert!(h
            .ops
            .state()
            .messages(CHANNEL)
            .unwrap()
            .contains(MessageId(500)));
        assert!(
            !h.ops.shared().sends.contains_key(&nonce),
            "the send was still tracked after it landed"
        );
    }

    /// And the echo arriving first means the fallback does nothing at all.
    #[tokio::test(start_paused = true)]
    async fn an_echo_that_arrives_in_time_leaves_the_fallback_with_nothing_to_do() {
        let h = harness("http://127.0.0.1:1");
        let nonce = Nonce(4243);
        let sent: Message = serde_json::from_value(echo(500, &nonce.to_string())).unwrap();

        {
            let mut state = h.ops.state_mut();
            let store = state.messages_mut(CHANNEL);
            store.set_at_latest(true);
            store.add_pending(PendingSend::new(nonce, "hello".into(), None, false));
        }
        schedule_fallback(&h.ops, CHANNEL, nonce, sent.clone());
        h.ops.state_mut().messages_mut(CHANNEL).receive(sent);

        tokio::time::sleep(ECHO_GRACE * 2).await;
        tokio::task::yield_now().await;
        assert_eq!(
            h.ops.state().messages(CHANNEL).unwrap().len(),
            1,
            "the fallback inserted a message the echo had already delivered"
        );
    }

    #[tokio::test]
    async fn a_reply_carries_its_reference() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(echo(500, "1")))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        send_message(&h.ops, CHANNEL, "answer".into(), Some(MessageId(499)), true).await;

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["message_reference"]["message_id"], "499");
        assert_eq!(body["allowed_mentions"]["replied_user"], true);
    }

    #[tokio::test]
    async fn an_edit_shows_without_waiting_for_the_gateway() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/channels/7/messages/500"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "500",
                "channel_id": "7",
                "content": "edited",
                "author": {"id": "1", "username": "sam"}
            })))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        edit(&h.ops, CHANNEL, MessageId(500), "edited".into()).await;
        assert_eq!(
            h.ops
                .state()
                .message(CHANNEL, MessageId(500))
                .unwrap()
                .content,
            "edited"
        );
    }

    #[tokio::test]
    async fn a_delete_removes_the_row_and_a_refused_one_does_not() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/channels/7/messages/500"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/channels/7/messages/501"))
            .respond_with(ResponseTemplate::new(403).set_body_json(
                serde_json::json!({"message": "Missing Permissions", "code": 50013}),
            ))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        {
            let mut state = h.ops.state_mut();
            let store = state.messages_mut(CHANNEL);
            store.set_at_latest(true);
            for id in [500u64, 501] {
                store.receive(serde_json::from_value(echo(id, "x")).unwrap());
            }
        }

        delete(&h.ops, CHANNEL, MessageId(500)).await;
        assert!(!h
            .ops
            .state()
            .messages(CHANNEL)
            .unwrap()
            .contains(MessageId(500)));

        delete(&h.ops, CHANNEL, MessageId(501)).await;
        assert!(
            h.ops
                .state()
                .messages(CHANNEL)
                .unwrap()
                .contains(MessageId(501)),
            "a refused delete removed the message anyway"
        );
    }

    #[test]
    fn a_nonce_survives_a_trip_through_a_javascript_number() {
        for _ in 0..1000 {
            let nonce = fresh_nonce();
            assert!(nonce.0 > 0);
            assert!(
                nonce.0 < (1u64 << 53),
                "{nonce} would lose its low bits in the web client"
            );
            assert_eq!(nonce.to_string().parse::<u64>().unwrap(), nonce.0);
        }
    }
}
