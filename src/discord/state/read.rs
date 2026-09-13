//! Working out what is unread, and what is muted.
//!
//! Discord sends neither. A channel is unread when its newest message is newer
//! than the last one this account acknowledged, which is a snowflake comparison
//! and nothing else — there is no boolean anywhere in the protocol. Mentions
//! are the exception and arrive as a count, because a client cannot know which
//! of the messages it has never fetched said its name.
//!
//! Muting is a resolution: a per-channel override wins over the guild's
//! setting, and either can be a timed mute that has already expired. It is
//! therefore always a question asked *now*, never a flag read once and cached —
//! nothing tells a client when a timed mute lapses.

use crate::discord::model::{ReadState, UserGuildSettings};
use crate::discord::snowflake::{ChannelId, MessageId};

/// What the channel list needs to draw one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unread {
    /// There is at least one message this account has not acknowledged.
    pub unread: bool,
    /// How many of them named this account, directly or by a role.
    pub mentions: u32,
    /// Muted right now. A muted channel with mentions still shows the count:
    /// muting suppresses the bold, not the badge.
    pub muted: bool,
}

impl Unread {
    /// Whether this row should draw attention at all.
    pub fn notable(&self) -> bool {
        self.mentions > 0 || (self.unread && !self.muted)
    }
}

/// Is there anything newer than what was acknowledged?
pub fn is_unread(last_message: Option<MessageId>, read: Option<&ReadState>) -> bool {
    let Some(last_message) = last_message else {
        // Nothing has ever been posted.
        return false;
    };
    match read.and_then(|r| r.last_message_id) {
        Some(acknowledged) => last_message > acknowledged,
        // A channel with messages and no read state has never been opened.
        None => true,
    }
}

/// Resolve the mute for one channel.
///
/// `settings` is the guild's entry, or the one whose `guild_id` is null for a
/// DM. A channel is muted when the guild is muted *or* the channel has its own
/// mute: an override adds a mute, it does not remove one. That is not a
/// simplification — Discord's own client does not offer unmuting a channel
/// inside a muted server, and a per-channel row with `muted: false` exists for
/// every category anybody has ever collapsed. Treating those as unmutes would
/// quietly unmute most of a muted server.
///
/// Either mute can be a timed one that has already lapsed, and nothing tells a
/// client when that happens, so both are asked at `now` rather than cached.
pub fn is_muted(
    channel: ChannelId,
    settings: Option<&UserGuildSettings>,
    now: jiff::Timestamp,
) -> bool {
    let Some(settings) = settings else {
        return false;
    };
    settings.muted_at(now)
        || settings
            .channel_override(channel)
            .is_some_and(|over| over.muted_at(now))
}

/// Everything a channel row needs.
pub fn unread_for(
    channel: ChannelId,
    last_message: Option<MessageId>,
    read: Option<&ReadState>,
    settings: Option<&UserGuildSettings>,
    now: jiff::Timestamp,
) -> Unread {
    Unread {
        unread: is_unread(last_message, read),
        mentions: read.map(|r| r.mention_count).unwrap_or(0),
        muted: is_muted(channel, settings, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::model::ChannelOverride;
    use crate::discord::model::MuteConfig;

    fn read(last: Option<u64>, mentions: u32) -> ReadState {
        ReadState {
            id: ChannelId(1),
            last_message_id: last.map(MessageId),
            mention_count: mentions,
            ..Default::default()
        }
    }

    #[test]
    fn unread_is_a_snowflake_comparison() {
        assert!(is_unread(Some(MessageId(10)), Some(&read(Some(9), 0))));
        assert!(!is_unread(Some(MessageId(10)), Some(&read(Some(10), 0))));
        assert!(
            !is_unread(Some(MessageId(10)), Some(&read(Some(11), 0))),
            "acknowledging past the last message is ordinary after a bulk ack"
        );
    }

    #[test]
    fn a_channel_nobody_has_opened_is_unread_and_an_empty_one_is_not() {
        assert!(is_unread(Some(MessageId(10)), None));
        assert!(!is_unread(None, None));
        assert!(!is_unread(None, Some(&read(None, 0))));
    }

    #[test]
    fn a_channel_override_can_add_a_mute_but_not_remove_one() {
        let now = jiff::Timestamp::now();

        let muted_channel = UserGuildSettings {
            muted: false,
            channel_overrides: vec![ChannelOverride {
                channel_id: ChannelId(5),
                muted: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(is_muted(ChannelId(5), Some(&muted_channel), now));
        assert!(!is_muted(ChannelId(6), Some(&muted_channel), now));

        // The row that exists because somebody collapsed a category. It must
        // not unmute the server.
        let muted_guild = UserGuildSettings {
            muted: true,
            channel_overrides: vec![ChannelOverride {
                channel_id: ChannelId(5),
                muted: false,
                collapsed: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(
            is_muted(ChannelId(5), Some(&muted_guild), now),
            "a collapsed category unmuted a muted server"
        );
    }

    #[test]
    fn a_timed_mute_that_has_passed_is_not_a_mute() {
        let now: jiff::Timestamp = "2026-09-13T12:00:00Z".parse().unwrap();
        let settings = UserGuildSettings {
            muted: true,
            mute_config: Some(MuteConfig {
                end_time: Some("2026-09-13T11:00:00Z".into()),
                selected_time_window: Some(3600),
            }),
            ..Default::default()
        };
        assert!(!is_muted(ChannelId(1), Some(&settings), now));

        let later: jiff::Timestamp = "2026-09-13T10:00:00Z".parse().unwrap();
        assert!(is_muted(ChannelId(1), Some(&settings), later));
    }

    #[test]
    fn a_muted_channel_with_a_mention_is_still_notable() {
        let now = jiff::Timestamp::now();
        let settings = UserGuildSettings {
            muted: true,
            ..Default::default()
        };
        let with_mention = unread_for(
            ChannelId(1),
            Some(MessageId(10)),
            Some(&read(Some(1), 3)),
            Some(&settings),
            now,
        );
        assert_eq!(
            with_mention,
            Unread {
                unread: true,
                mentions: 3,
                muted: true
            }
        );
        assert!(
            with_mention.notable(),
            "muting hides the bold, not the badge"
        );

        let without = unread_for(
            ChannelId(1),
            Some(MessageId(10)),
            Some(&read(Some(1), 0)),
            Some(&settings),
            now,
        );
        assert!(!without.notable());
    }

    #[test]
    fn no_settings_means_not_muted() {
        let now = jiff::Timestamp::now();
        assert!(!is_muted(ChannelId(1), None, now));
    }
}
