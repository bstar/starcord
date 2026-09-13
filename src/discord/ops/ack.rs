//! Marking things read.
//!
//! An ack is the most easily abused request this client makes. It happens on
//! scroll, which means it happens continuously, and each one is a write against
//! the account. Three rules, all of them enforced here rather than at the call
//! site:
//!
//! - **At most one per channel per second**, coalesced to the highest id. A
//!   reader scrolling through a hundred messages generates one ack, not a
//!   hundred, and the one it generates is for the newest message they reached.
//! - **Never for a channel other than the one `SetFocus` names.** Reading is
//!   something a person does by looking at a channel; a client that acks a
//!   channel nobody is looking at is a client marking messages read that nobody
//!   read.
//! - **Never for something already read.** The read state is checked first, so
//!   reopening a channel that is up to date makes no request at all.
//!
//! The coalescing is written as a decision rather than a timer: [`submit`]
//! returns whether to send now or how long to wait, and the caller either sends
//! or arranges to come back. That is what makes it testable under
//! `tokio::time::pause()` with no sleeping at all.
//!
//! [`submit`]: AckCoalescer::submit

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::discord::http::api;
use crate::discord::snowflake::{ChannelId, MessageId};

use super::Ops;

/// The shortest gap between two acks for one channel.
pub const EVERY: Duration = Duration::from_secs(1);

/// What to do with an ack that was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Send it now.
    Send(MessageId),
    /// Too soon. Come back in this long and send whatever is highest then.
    Wait(Duration),
    /// Somebody is already waiting to send for this channel; it will pick up
    /// the higher id.
    Folded,
    /// Nothing to do: not newer than what was already asked for.
    Nothing,
}

#[derive(Debug, Default)]
pub struct AckCoalescer {
    /// The highest id asked for per channel, not yet sent.
    wanted: HashMap<ChannelId, MessageId>,
    /// When the last one went out.
    last: HashMap<ChannelId, Instant>,
    /// Channels with a flush already scheduled.
    waiting: HashSet<ChannelId>,
}

impl AckCoalescer {
    /// Ask to mark a channel read up to a message.
    pub fn submit(&mut self, channel: ChannelId, up_to: MessageId, now: Instant) -> Decision {
        match self.wanted.get(&channel) {
            // A lower id than one already queued is not news: acks are
            // cumulative, and the higher one covers it.
            Some(held) if *held >= up_to => {
                if self.waiting.contains(&channel) {
                    return Decision::Folded;
                }
                return Decision::Nothing;
            }
            _ => {}
        }
        self.wanted.insert(channel, up_to);

        if self.waiting.contains(&channel) {
            // A flush is already scheduled and will read the higher id.
            return Decision::Folded;
        }

        match self.last.get(&channel) {
            Some(last) if now.duration_since(*last) < EVERY => {
                self.waiting.insert(channel);
                Decision::Wait(EVERY - now.duration_since(*last))
            }
            _ => {
                self.last.insert(channel, now);
                self.wanted.remove(&channel);
                Decision::Send(up_to)
            }
        }
    }

    /// What a scheduled flush should send, now that its wait is over.
    pub fn flush(&mut self, channel: ChannelId, now: Instant) -> Option<MessageId> {
        self.waiting.remove(&channel);
        let up_to = self.wanted.remove(&channel)?;
        self.last.insert(channel, now);
        Some(up_to)
    }

    /// Forget a channel: it was closed, or focus moved away.
    pub fn forget(&mut self, channel: ChannelId) {
        self.wanted.remove(&channel);
        self.waiting.remove(&channel);
        self.last.remove(&channel);
    }
}

/// Tokio's clock, which `tokio::time::pause()` controls, as a plain `Instant`.
///
/// Used everywhere a coalescing decision is made, so that the tests assert the
/// arithmetic rather than waiting out the seconds.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// `Command::MarkRead`.
pub async fn mark_read(ops: &Ops, channel: ChannelId, up_to: MessageId) {
    if !ackable(ops, channel, up_to) {
        return;
    }

    let decision = ops.shared().acks.submit(channel, up_to, now());
    match decision {
        Decision::Send(id) => post(ops, channel, id).await,
        Decision::Wait(wait) => {
            let ops = ops.clone();
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                let owed = ops.shared().acks.flush(channel, now());
                if let Some(id) = owed {
                    if ackable(&ops, channel, id) {
                        post(&ops, channel, id).await;
                    }
                }
            });
        }
        Decision::Folded | Decision::Nothing => {}
    }
}

/// Whether this ack should exist at all.
fn ackable(ops: &Ops, channel: ChannelId, up_to: MessageId) -> bool {
    let shared = ops.shared();
    if shared.focus != Some(channel) {
        tracing::debug!("refusing to ack {channel}: the user is not looking at it");
        return false;
    }
    drop(shared);

    let state = ops.state();
    let already = state
        .read_state(channel)
        .and_then(|read| read.last_message_id)
        .is_some_and(|read| read >= up_to);
    if already {
        tracing::trace!("nothing to ack in {channel}");
    }
    !already
}

async fn post(ops: &Ops, channel: ChannelId, up_to: MessageId) {
    let result = ops.rest(api::ack(&ops.http, channel, up_to)).await;
    match result {
        Ok(_) => {
            // Discord answers the ack with a MESSAGE_ACK of its own, which is
            // what actually clears the mark. Writing the read state here as
            // well would be a second source of truth for the same fact.
            tracing::debug!("acked {channel} up to {up_to}");
        }
        Err(e) => tracing::debug!("could not ack {channel}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: ChannelId = ChannelId(1);
    const OTHER: ChannelId = ChannelId(2);

    #[test]
    fn the_first_ack_goes_straight_out() {
        let mut acks = AckCoalescer::default();
        let now = Instant::now();
        assert_eq!(
            acks.submit(CHANNEL, MessageId(10), now),
            Decision::Send(MessageId(10))
        );
    }

    /// A reader scrolling through a hundred messages generates one ack, and it
    /// is for the newest message they reached.
    #[test]
    fn a_burst_becomes_one_ack_for_the_highest_id() {
        let mut acks = AckCoalescer::default();
        let now = Instant::now();
        assert_eq!(
            acks.submit(CHANNEL, MessageId(10), now),
            Decision::Send(MessageId(10))
        );

        let next = now + Duration::from_millis(100);
        assert!(matches!(
            acks.submit(CHANNEL, MessageId(11), next),
            Decision::Wait(_)
        ));
        assert_eq!(acks.submit(CHANNEL, MessageId(12), next), Decision::Folded);
        assert_eq!(acks.submit(CHANNEL, MessageId(20), next), Decision::Folded);

        assert_eq!(
            acks.flush(CHANNEL, now + EVERY),
            Some(MessageId(20)),
            "the flush sent an id that was not the highest"
        );
        assert_eq!(acks.flush(CHANNEL, now + EVERY), None, "sent twice");
    }

    #[test]
    fn an_older_id_than_one_already_queued_is_not_news() {
        let mut acks = AckCoalescer::default();
        let now = Instant::now();
        acks.submit(CHANNEL, MessageId(10), now);

        let next = now + Duration::from_millis(10);
        assert!(matches!(
            acks.submit(CHANNEL, MessageId(20), next),
            Decision::Wait(_)
        ));
        assert_eq!(acks.submit(CHANNEL, MessageId(15), next), Decision::Folded);
        assert_eq!(acks.flush(CHANNEL, now + EVERY), Some(MessageId(20)));
    }

    #[test]
    fn channels_are_held_independently() {
        let mut acks = AckCoalescer::default();
        let now = Instant::now();
        acks.submit(CHANNEL, MessageId(10), now);
        assert_eq!(
            acks.submit(OTHER, MessageId(10), now),
            Decision::Send(MessageId(10)),
            "one channel's ack held another's up"
        );
    }

    #[test]
    fn a_second_ack_after_the_gap_goes_straight_out() {
        let mut acks = AckCoalescer::default();
        let now = Instant::now();
        acks.submit(CHANNEL, MessageId(10), now);
        assert_eq!(
            acks.submit(
                CHANNEL,
                MessageId(11),
                now + EVERY + Duration::from_millis(1)
            ),
            Decision::Send(MessageId(11))
        );
    }

    #[test]
    fn forgetting_a_channel_clears_its_history() {
        let mut acks = AckCoalescer::default();
        let now = Instant::now();
        acks.submit(CHANNEL, MessageId(10), now);
        acks.forget(CHANNEL);
        assert_eq!(
            acks.submit(CHANNEL, MessageId(11), now),
            Decision::Send(MessageId(11))
        );
    }

    /// The same arithmetic through the real clock source, with `pause()`
    /// driving it. A paused clock and a real socket do not mix -- the runtime
    /// auto-advances whenever it parks on I/O -- so this exercises the
    /// decisions and the wiremock test below exercises the requests.
    #[tokio::test(start_paused = true)]
    async fn the_decisions_follow_tokios_clock() {
        let mut acks = AckCoalescer::default();

        assert_eq!(
            acks.submit(CHANNEL, MessageId(10), now()),
            Decision::Send(MessageId(10))
        );

        tokio::time::advance(Duration::from_millis(100)).await;
        let Decision::Wait(wait) = acks.submit(CHANNEL, MessageId(11), now()) else {
            panic!("a second ack a tenth of a second later went straight out")
        };
        assert_eq!(wait, Duration::from_millis(900));
        assert_eq!(acks.submit(CHANNEL, MessageId(30), now()), Decision::Folded);

        tokio::time::advance(wait).await;
        assert_eq!(acks.flush(CHANNEL, now()), Some(MessageId(30)));

        tokio::time::advance(EVERY).await;
        assert_eq!(
            acks.submit(CHANNEL, MessageId(31), now()),
            Decision::Send(MessageId(31)),
            "the gate never reopened"
        );
    }

    /// Three acks in quick succession are two requests: the first goes out, and
    /// the rest fold into one that names the highest message.
    #[tokio::test]
    async fn a_burst_of_acks_is_coalesced_into_two_requests() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/channels/1/messages/20/ack"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/channels/1/messages/10/ack"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let harness = super::super::testing::harness(&server.uri());
        harness.ops.set_focus(Some(CHANNEL), true);

        mark_read(&harness.ops, CHANNEL, MessageId(10)).await;
        mark_read(&harness.ops, CHANNEL, MessageId(15)).await;
        mark_read(&harness.ops, CHANNEL, MessageId(20)).await;

        // Wait out the one-second gate so the folded ack goes.
        tokio::time::sleep(EVERY + Duration::from_millis(400)).await;

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            2,
            "three acks a fraction of a second apart made {} requests",
            requests.len()
        );
        assert!(requests[1].url.path().ends_with("/20/ack"));
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, serde_json::json!({"token": null}));
    }

    /// The rule that matters most: reading is something a person does by
    /// looking at a channel.
    #[tokio::test]
    async fn a_channel_nobody_is_looking_at_is_never_acked() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let harness = super::super::testing::harness(&server.uri());
        harness.ops.set_focus(Some(OTHER), true);
        mark_read(&harness.ops, CHANNEL, MessageId(10)).await;

        harness.ops.set_focus(None, true);
        mark_read(&harness.ops, CHANNEL, MessageId(10)).await;

        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_channel_that_is_already_read_makes_no_request() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let harness = super::super::testing::harness(&server.uri());
        harness.ops.set_focus(Some(CHANNEL), true);
        {
            let mut state = harness.ops.state_mut();
            state.set_read_state(crate::discord::model::ReadState {
                id: CHANNEL,
                last_message_id: Some(MessageId(20)),
                ..Default::default()
            });
        }

        mark_read(&harness.ops, CHANNEL, MessageId(20)).await;
        mark_read(&harness.ops, CHANNEL, MessageId(19)).await;
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
