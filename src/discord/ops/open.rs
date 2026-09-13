//! Opening, closing and scrolling a channel.
//!
//! Opening is the one operation with a shape worth stating, because it decides
//! what the first frame looks like. A channel that is already held and already
//! at the bottom is drawn from memory and costs nothing at all; anything else
//! fetches one page of fifty. Closing trims to fifty and schedules the
//! member-list subscription to lapse, so that clicking between two channels in
//! a server is free.
//!
//! **Opening does not ack.** Marking a channel read is what happens when
//! somebody looks at it, which is `SetFocus` and the scroll position, not the
//! act of loading it. A client that acks on open marks a whole channel read the
//! moment it is clicked, which is the behaviour everybody complains about.
//!
//! `LoadOlder` is interlocked on the store's `loading` flag and on `has_older`.
//! Without the first, a scroll that reaches the top while a fetch is in flight
//! asks again; without the second, reaching the beginning of a channel asks
//! forever.

use std::time::Instant;

use crate::discord::handle::{Event, MessagesChange, Note};
use crate::discord::http::api;
use crate::discord::http::route::History;
use crate::discord::snowflake::{ChannelId, GuildId, MessageId};
use crate::discord::state::messages::PAGE;

use super::Ops;

fn guild_of(ops: &Ops, channel: ChannelId) -> Option<GuildId> {
    ops.state().channel(channel).and_then(|c| c.guild_id)
}

/// Announce that a fetch started or finished, for the spinner row.
fn loading(ops: &Ops, channel: ChannelId, loading: bool) {
    ops.state_mut().messages_mut(channel).set_loading(loading);
    ops.emit(Event::Messages(channel, MessagesChange::Loading(loading)));
}

/// `Command::OpenChannel`.
pub async fn open_channel(ops: &Ops, channel: ChannelId) {
    let needs_fetch = {
        let mut state = ops.state_mut();
        let store = state.messages_mut(channel);
        store.set_open(true);
        // Held and at the bottom: the window is already the truth.
        store.is_empty() || !store.at_latest()
    };

    // The member list and the typing indicator for this guild, merged with
    // whatever else is open in it.
    if let Some(guild) = guild_of(ops, channel) {
        let payload = ops.shared().subscriptions.open(guild, channel);
        if let Some(payload) = payload {
            let opcode = ops.shared().subscriptions.describe();
            if ops.to_gateway(payload) {
                tracing::debug!("subscribed to {guild} with {opcode}");
                ops.note(Note::info(format!("subscribed to {guild} with {opcode}")));
            }
        }
    }

    if !needs_fetch {
        ops.emit(Event::Messages(channel, MessagesChange::Replaced));
        return;
    }

    loading(ops, channel, true);
    let fetched = ops
        .rest(api::messages(&ops.http, channel, History::Latest))
        .await;
    match fetched {
        Ok(messages) => {
            ops.state_mut()
                .messages_mut(channel)
                .replace(messages, true);
            loading(ops, channel, false);
            ops.emit(Event::Messages(channel, MessagesChange::Replaced));
        }
        Err(e) => {
            loading(ops, channel, false);
            tracing::warn!("could not load {channel}: {e}");
            ops.note(Note::warning(
                "history",
                format!("could not load that channel: {e}"),
            ));
        }
    }
}

/// `Command::CloseChannel`.
pub fn close_channel(ops: &Ops, channel: ChannelId) {
    ops.state_mut().messages_mut(channel).set_open(false);
    super::typing::forget(ops, channel);

    if let Some(guild) = guild_of(ops, channel) {
        let payload = ops
            .shared()
            .subscriptions
            .close(guild, channel, Instant::now());
        if let Some(payload) = payload {
            ops.to_gateway(payload);
        }
    }
}

/// `Command::LoadOlder`.
pub async fn load_older(ops: &Ops, channel: ChannelId) {
    let before = {
        let state = ops.state();
        let Some(store) = state.messages(channel) else {
            return;
        };
        if store.loading() {
            tracing::trace!("a fetch for {channel} is already in flight");
            return;
        }
        if !store.has_older() {
            tracing::trace!("{channel} has no more history");
            return;
        }
        match store.oldest() {
            Some(oldest) => oldest.id,
            // Nothing held: this is an open, not a scroll.
            None => return,
        }
    };

    loading(ops, channel, true);
    let fetched = ops
        .rest(api::messages(&ops.http, channel, History::Before(before)))
        .await;
    match fetched {
        Ok(messages) => {
            let full = messages.len() >= PAGE;
            let added = ops.state_mut().messages_mut(channel).prepend(messages);
            loading(ops, channel, false);
            ops.emit(Event::Messages(channel, MessagesChange::Prepended(added)));
            tracing::debug!("{added} older messages in {channel} (full page: {full})");
        }
        Err(e) => {
            loading(ops, channel, false);
            tracing::debug!("could not load older messages in {channel}: {e}");
            ops.note(Note::warning(
                "history",
                format!("could not load older messages: {e}"),
            ));
        }
    }
}

/// `Command::LoadNewer`, which only exists after a jump has left a gap below.
pub async fn load_newer(ops: &Ops, channel: ChannelId) {
    let after = {
        let state = ops.state();
        let Some(store) = state.messages(channel) else {
            return;
        };
        if store.loading() || store.at_latest() {
            return;
        }
        match store.newest() {
            Some(newest) => newest.id,
            None => return,
        }
    };

    loading(ops, channel, true);
    let fetched = ops
        .rest(api::messages(&ops.http, channel, History::After(after)))
        .await;
    match fetched {
        Ok(messages) => {
            // A short page means there was nothing more to come: the bottom of
            // what arrived is the bottom of the channel.
            let reached_bottom = messages.len() < PAGE;
            ops.state_mut()
                .messages_mut(channel)
                .append(messages, reached_bottom);
            loading(ops, channel, false);
            ops.emit(Event::Messages(channel, MessagesChange::Replaced));
        }
        Err(e) => {
            loading(ops, channel, false);
            tracing::debug!("could not load newer messages in {channel}: {e}");
        }
    }
}

/// `Command::JumpTo`: a search result, a reply, or an unread marker.
///
/// `around` returns a page centred on the message, so there is history on both
/// sides of what comes back and the store is left knowing it is not at the
/// bottom.
pub async fn jump_to(ops: &Ops, channel: ChannelId, message: MessageId) {
    {
        let mut state = ops.state_mut();
        let store = state.messages_mut(channel);
        store.set_open(true);
    }
    loading(ops, channel, true);

    let fetched = ops
        .rest(api::messages(&ops.http, channel, History::Around(message)))
        .await;
    match fetched {
        Ok(messages) => {
            let landed = messages.iter().any(|m| m.id == message);
            {
                let mut state = ops.state_mut();
                let store = state.messages_mut(channel);
                store.replace(messages, false);
            }
            loading(ops, channel, false);
            ops.emit(Event::Messages(channel, MessagesChange::Replaced));
            if !landed {
                ops.note(Note::info("that message is no longer there"));
            }
        }
        Err(e) => {
            loading(ops, channel, false);
            ops.note(Note::warning("history", format!("could not jump: {e}")));
        }
    }
}

/// Send any subscription whose grace period has run out.
///
/// Called on a timer from `core`, which is the only way a lapse can happen:
/// nothing arrives to say a guild has been left for thirty seconds.
pub fn sweep_subscriptions(ops: &Ops) {
    let due = ops.shared().subscriptions.due(Instant::now());
    for payload in due {
        ops.to_gateway(payload);
    }
}

/// Re-send every live subscription after a reconnect.
///
/// The gateway remembers nothing across a socket, so what it was told before is
/// no longer true of the connection that exists now.
pub fn resubscribe(ops: &Ops) {
    let payloads = ops.shared().subscriptions.resend_all();
    for payload in payloads {
        ops.to_gateway(payload);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::harness;
    use super::*;
    use crate::discord::gateway::Control;
    use crate::discord::model::Channel;
    use crate::discord::snowflake::MessageId;

    const CHANNEL: ChannelId = ChannelId(7);

    fn page(from: u64, count: u64) -> serde_json::Value {
        serde_json::Value::Array(
            (0..count)
                .map(|n| {
                    serde_json::json!({
                        "id": (from - n).to_string(),
                        "channel_id": "7",
                        "content": format!("m{}", from - n),
                        "author": {"id": "2", "username": "alex"}
                    })
                })
                .collect(),
        )
    }

    fn in_guild(ops: &Ops) {
        let channel: Channel =
            serde_json::from_str(r#"{"id":"7","type":0,"guild_id":"3","name":"general"}"#).unwrap();
        ops.state_mut().upsert_channel(channel);
    }

    #[tokio::test]
    async fn opening_an_empty_channel_fetches_one_page() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/7/messages"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(100, 3)))
            .expect(1)
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        open_channel(&h.ops, CHANNEL).await;

        let state = h.ops.state();
        let store = state.messages(CHANNEL).expect("a store was made");
        assert_eq!(store.len(), 3);
        assert!(store.at_latest());
        assert!(store.is_open());
        assert!(!store.loading(), "the spinner was left on");
        assert!(!store.has_older(), "three back is not a full page");
    }

    /// A channel that is held and at the bottom draws from memory.
    #[tokio::test]
    async fn reopening_a_channel_that_is_current_makes_no_request() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(100, 3)))
            .expect(1)
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        open_channel(&h.ops, CHANNEL).await;
        open_channel(&h.ops, CHANNEL).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    /// And one that has been scrolled up into history re-fetches, because what
    /// is held is a window somewhere in the middle.
    #[tokio::test]
    async fn reopening_a_channel_that_was_left_scrolled_up_refetches() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(100, 3)))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        open_channel(&h.ops, CHANNEL).await;
        h.ops.state_mut().messages_mut(CHANNEL).set_at_latest(false);
        open_channel(&h.ops, CHANNEL).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn opening_a_guild_channel_subscribes_to_it_once() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(100, 3)))
            .mount(&server)
            .await;

        let mut h = harness(&server.uri());
        in_guild(&h.ops);
        open_channel(&h.ops, CHANNEL).await;

        let sent = h.sent.try_recv().expect("a subscription went out");
        let Control::Send(payload) = sent else {
            panic!("the gateway was asked to do something else")
        };
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["op"], 37);
        assert_eq!(value["d"]["guild_id"], "3");
        assert_eq!(value["d"]["channels"]["7"], serde_json::json!([[0, 99]]));

        open_channel(&h.ops, CHANNEL).await;
        assert!(
            h.sent.try_recv().is_err(),
            "an unchanged subscription was sent again"
        );
    }

    /// Opening is loading, not reading.
    #[tokio::test]
    async fn opening_a_channel_never_acks_it() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(100, 3)))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        open_channel(&h.ops, CHANNEL).await;
    }

    #[tokio::test]
    async fn closing_a_channel_trims_it_and_lets_the_subscription_lapse() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(300, 200)))
            .mount(&server)
            .await;

        let mut h = harness(&server.uri());
        in_guild(&h.ops);
        open_channel(&h.ops, CHANNEL).await;
        let _ = h.sent.try_recv();
        assert_eq!(h.ops.state().messages(CHANNEL).unwrap().len(), 200);

        close_channel(&h.ops, CHANNEL);
        let store_len = h.ops.state().messages(CHANNEL).unwrap().len();
        assert_eq!(
            store_len,
            crate::discord::state::messages::CLOSED_CAP,
            "a closed channel kept its whole window"
        );
        assert!(
            h.sent.try_recv().is_err(),
            "closing unsubscribed immediately rather than after the grace"
        );
    }

    #[tokio::test]
    async fn loading_older_history_prepends_and_stops_at_the_beginning() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The first page is full, so there is more behind it.
        Mock::given(method("GET"))
            .and(query_param("before", "51"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(50, 3)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(100, 50)))
            .expect(1)
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        open_channel(&h.ops, CHANNEL).await;
        assert!(h.ops.state().messages(CHANNEL).unwrap().has_older());

        load_older(&h.ops, CHANNEL).await;
        {
            let state = h.ops.state();
            let store = state.messages(CHANNEL).unwrap();
            assert_eq!(store.len(), 53);
            assert_eq!(store.oldest().unwrap().id, MessageId(48));
            assert!(
                !store.has_older(),
                "a short page means the beginning of the channel"
            );
        }

        // And now it stops asking.
        load_older(&h.ops, CHANNEL).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_jump_lands_in_the_middle_and_says_so() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("around", "80"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(90, 50)))
            .expect(1)
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        jump_to(&h.ops, CHANNEL, MessageId(80)).await;

        let state = h.ops.state();
        let store = state.messages(CHANNEL).unwrap();
        assert!(store.contains(MessageId(80)));
        assert!(
            !store.at_latest(),
            "a jump into history left the store believing it was at the bottom"
        );
        assert!(store.has_older());
    }

    #[tokio::test]
    async fn a_failed_fetch_turns_the_spinner_off_and_says_why() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"message": "Missing Access", "code": 50001})),
            )
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        open_channel(&h.ops, CHANNEL).await;

        assert!(!h.ops.state().messages(CHANNEL).unwrap().loading());
        let notes: Vec<String> = h
            .events
            .try_iter()
            .filter_map(|e| match e {
                Event::Note(note) => Some(note.text),
                _ => None,
            })
            .collect();
        assert!(
            notes.iter().any(|n| n.contains("could not load")),
            "a channel that would not load said nothing: {notes:?}"
        );
    }
}
