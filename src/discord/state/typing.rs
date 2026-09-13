//! Who is typing, and for how long.
//!
//! Discord sends TYPING_START and never sends a stop. The indicator is
//! therefore a lease: ten seconds from the last TYPING_START, renewed by the
//! next one, and gone when nothing renews it. That is what the web client does
//! and it is why a client that only reacts to events shows somebody typing
//! forever after they close the tab.
//!
//! Two things follow from that, and both are here rather than in the caller:
//!
//! - **A sweep is needed.** Nothing arrives when a lease lapses, so something
//!   has to look. `core.rs` runs [`Typing::sweep`] on a timer and emits an
//!   event for each channel whose set actually changed.
//! - **An event is only worth sending when the set changed.** A person typing
//!   a long message sends TYPING_START every eight or nine seconds; renewing a
//!   lease is not news, and forwarding it as one is a redraw a second for as
//!   long as anybody is typing.
//!
//! A MESSAGE_CREATE ends the lease early, because somebody who just sent a
//! message has stopped typing and waiting ten seconds to admit it looks like a
//! stuck client.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::discord::snowflake::{ChannelId, UserId};

/// How long one TYPING_START is good for.
///
/// Discord's own clients re-send every eight to ten seconds, so ten is the
/// shortest lease that does not flicker.
pub const LEASE: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct Typing {
    by_channel: HashMap<ChannelId, HashMap<UserId, Instant>>,
}

impl Typing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a TYPING_START. Returns whether the *set* changed, which is the
    /// only case worth telling anybody about.
    pub fn started(&mut self, channel: ChannelId, user: UserId, now: Instant) -> bool {
        let users = self.by_channel.entry(channel).or_default();
        let was_typing = users.get(&user).is_some_and(|until| *until > now);
        users.insert(user, now + LEASE);
        !was_typing
    }

    /// Somebody stopped, because they sent the message they were typing.
    pub fn stopped(&mut self, channel: ChannelId, user: UserId) -> bool {
        let Some(users) = self.by_channel.get_mut(&channel) else {
            return false;
        };
        let removed = users.remove(&user).is_some();
        if users.is_empty() {
            self.by_channel.remove(&channel);
        }
        removed
    }

    /// Drop every lapsed lease, and say which channels changed.
    pub fn sweep(&mut self, now: Instant) -> Vec<ChannelId> {
        let mut changed = Vec::new();
        self.by_channel.retain(|channel, users| {
            let before = users.len();
            users.retain(|_, until| *until > now);
            if users.len() != before {
                changed.push(*channel);
            }
            !users.is_empty()
        });
        changed
    }

    /// Who is typing in a channel right now, oldest lease first so the order
    /// does not shuffle between frames.
    pub fn users(&self, channel: ChannelId, now: Instant) -> Vec<UserId> {
        let Some(users) = self.by_channel.get(&channel) else {
            return Vec::new();
        };
        let mut live: Vec<(Instant, UserId)> = users
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(user, until)| (*until, *user))
            .collect();
        live.sort();
        live.into_iter().map(|(_, user)| user).collect()
    }

    /// Whether anything at all is happening in a channel.
    pub fn any(&self, channel: ChannelId, now: Instant) -> bool {
        self.by_channel
            .get(&channel)
            .is_some_and(|users| users.values().any(|until| *until > now))
    }

    /// Forget a channel outright, for a logout or a channel that went away.
    pub fn forget(&mut self, channel: ChannelId) {
        self.by_channel.remove(&channel);
    }

    pub fn clear(&mut self) {
        self.by_channel.clear();
    }

    /// How many channels are being tracked, for the sweeper's own logging.
    pub fn channels(&self) -> usize {
        self.by_channel.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: ChannelId = ChannelId(1);
    const OTHER: ChannelId = ChannelId(2);
    const ALEX: UserId = UserId(10);
    const JORDAN: UserId = UserId(11);

    #[test]
    fn a_first_notice_is_news_and_a_renewal_is_not() {
        let mut typing = Typing::new();
        let now = Instant::now();

        assert!(typing.started(CHANNEL, ALEX, now));
        assert!(
            !typing.started(CHANNEL, ALEX, now + Duration::from_secs(8)),
            "renewing a lease is a redraw a second for as long as anybody types"
        );
        assert!(typing.started(CHANNEL, JORDAN, now + Duration::from_secs(1)));
    }

    #[test]
    fn a_lapsed_lease_is_news_again() {
        let mut typing = Typing::new();
        let now = Instant::now();
        typing.started(CHANNEL, ALEX, now);

        let later = now + LEASE + Duration::from_secs(1);
        assert!(
            typing.started(CHANNEL, ALEX, later),
            "somebody who stopped and started again is starting again"
        );
    }

    #[test]
    fn the_sweep_reports_only_the_channels_that_changed() {
        let mut typing = Typing::new();
        let now = Instant::now();
        typing.started(CHANNEL, ALEX, now);
        typing.started(OTHER, JORDAN, now + Duration::from_secs(5));

        assert!(typing.sweep(now + Duration::from_secs(1)).is_empty());

        let changed = typing.sweep(now + LEASE + Duration::from_millis(1));
        assert_eq!(changed, vec![CHANNEL], "the wrong channel was swept");
        assert!(!typing.any(CHANNEL, now + LEASE + Duration::from_millis(1)));
        assert!(typing.any(OTHER, now + LEASE + Duration::from_millis(1)));

        // And the second one, five seconds later.
        let changed = typing.sweep(now + LEASE + Duration::from_secs(6));
        assert_eq!(changed, vec![OTHER]);
        assert_eq!(typing.channels(), 0, "an empty channel was kept");
    }

    #[test]
    fn sending_a_message_ends_the_lease_early() {
        let mut typing = Typing::new();
        let now = Instant::now();
        typing.started(CHANNEL, ALEX, now);
        typing.started(CHANNEL, JORDAN, now);

        assert!(typing.stopped(CHANNEL, ALEX));
        assert_eq!(typing.users(CHANNEL, now), vec![JORDAN]);
        assert!(!typing.stopped(CHANNEL, ALEX), "stopping twice is not news");
        assert!(!typing.stopped(OTHER, ALEX));

        typing.stopped(CHANNEL, JORDAN);
        assert_eq!(typing.channels(), 0);
    }

    #[test]
    fn the_order_of_typists_does_not_shuffle() {
        let mut typing = Typing::new();
        let now = Instant::now();
        typing.started(CHANNEL, JORDAN, now);
        typing.started(CHANNEL, ALEX, now + Duration::from_millis(1));

        assert_eq!(typing.users(CHANNEL, now), vec![JORDAN, ALEX]);
        assert_eq!(
            typing.users(CHANNEL, now),
            vec![JORDAN, ALEX],
            "two reads gave two orders"
        );
    }

    #[test]
    fn an_expired_typist_is_not_reported_before_the_sweep_runs() {
        let mut typing = Typing::new();
        let now = Instant::now();
        typing.started(CHANNEL, ALEX, now);

        let later = now + LEASE + Duration::from_secs(1);
        assert!(
            typing.users(CHANNEL, later).is_empty(),
            "a read must not depend on the sweeper having run"
        );
        assert!(!typing.any(CHANNEL, later));
    }

    #[test]
    fn forgetting_a_channel_takes_everything_with_it() {
        let mut typing = Typing::new();
        let now = Instant::now();
        typing.started(CHANNEL, ALEX, now);
        typing.forget(CHANNEL);
        assert!(!typing.any(CHANNEL, now));

        typing.started(OTHER, ALEX, now);
        typing.clear();
        assert_eq!(typing.channels(), 0);
    }
}
