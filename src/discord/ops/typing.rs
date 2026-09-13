//! The typing indicator.
//!
//! `POST /channels/{id}/typing` tells everybody in a channel that this account
//! is writing something, and the indicator lasts about ten seconds. Discord's
//! own clients re-send it every eight to ten while the user keeps typing, so
//! this sends at most one every nine seconds per channel and drops everything
//! in between.
//!
//! Nine rather than eight: the indicator has to be renewed before it lapses, and
//! nine leaves a second of overlap while still being fewer requests than a
//! keystroke-driven client would make. The composer may call this on every
//! keystroke; that is the point of the gate being here rather than there.

use std::time::Duration;

use crate::discord::http::api;
use crate::discord::snowflake::ChannelId;

use super::Ops;

/// The shortest gap between two typing requests for one channel.
pub const EVERY: Duration = Duration::from_secs(9);

/// Whether enough time has passed to tell the channel again.
///
/// Split out so the gate can be exercised under `tokio::time::pause()` with
/// nothing on the other end: it reads tokio's clock, which the test controls,
/// and a paused clock and a real socket do not mix — the runtime auto-advances
/// whenever it parks on I/O, which moves the very thing being measured.
fn due(ops: &Ops, channel: ChannelId) -> bool {
    let now = tokio::time::Instant::now().into_std();
    ops.shared().typing.ready(channel, EVERY, now)
}

/// `Command::Typing`.
pub async fn typing(ops: &Ops, channel: ChannelId) {
    if !due(ops, channel) {
        return;
    }

    if let Err(e) = ops.rest(api::typing(&ops.http, channel)).await {
        // Not worth telling the user about: the indicator is a courtesy, and
        // the message they are typing will send regardless.
        tracing::debug!("could not send a typing indicator for {channel}: {e}");
    }
}

/// Forget a channel's gate, so reopening it starts fresh.
pub fn forget(ops: &Ops, channel: ChannelId) {
    ops.shared().typing.forget(channel);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::ChannelId;

    const CHANNEL: ChannelId = ChannelId(1);
    const OTHER: ChannelId = ChannelId(2);

    /// The gate itself, with the clock stopped. A composer calling this on
    /// every keystroke must let one through per nine seconds, not one per key.
    #[tokio::test(start_paused = true)]
    async fn the_gate_lets_one_through_every_nine_seconds() {
        let h = super::super::testing::harness("http://127.0.0.1:1");

        assert!(due(&h.ops, CHANNEL), "the first keystroke was swallowed");
        for _ in 0..40 {
            tokio::time::advance(Duration::from_millis(200)).await;
            assert!(
                !due(&h.ops, CHANNEL),
                "eight seconds of typing sent more than one indicator"
            );
        }

        tokio::time::advance(EVERY).await;
        assert!(due(&h.ops, CHANNEL), "the indicator was never renewed");
    }

    #[tokio::test(start_paused = true)]
    async fn channels_are_gated_independently() {
        let h = super::super::testing::harness("http://127.0.0.1:1");
        assert!(due(&h.ops, CHANNEL));
        assert!(due(&h.ops, OTHER), "one channel's gate held another's shut");
        assert!(!due(&h.ops, CHANNEL));
    }

    #[tokio::test(start_paused = true)]
    async fn forgetting_a_channel_opens_the_gate_again() {
        let h = super::super::testing::harness("http://127.0.0.1:1");
        assert!(due(&h.ops, CHANNEL));
        assert!(!due(&h.ops, CHANNEL));
        forget(&h.ops, CHANNEL);
        assert!(due(&h.ops, CHANNEL));
    }

    /// And the whole path, against a real server, in real time: forty calls
    /// with no waiting between them are one request.
    #[tokio::test]
    async fn a_burst_of_keystrokes_is_one_request() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/channels/1/typing"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let h = super::super::testing::harness(&server.uri());
        for _ in 0..40 {
            typing(&h.ops, CHANNEL).await;
        }
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "forty keystrokes made more than one request"
        );
    }
}
