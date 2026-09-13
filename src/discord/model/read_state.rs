//! What has been read, and what has been muted.
//!
//! Unread state is derived, not sent. Discord says "the last message you
//! acknowledged in this channel was id X" and the client compares that against
//! the channel's `last_message_id`; there is no boolean anywhere. Mentions are
//! the exception and arrive as a count, because a client cannot work out which
//! of the messages it has not fetched mentioned it.
//!
//! Muting is the other half and it is a resolution problem rather than a flag:
//! a channel override wins over the guild setting, and a mute can carry an end
//! time that has already passed.

use serde::Deserialize;

use crate::discord::model::optional_id;
use crate::discord::snowflake::{ChannelId, GuildId, MessageId};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReadState {
    /// The channel. Spelled `id` rather than `channel_id`, which is the one
    /// thing about this payload that catches everybody.
    pub id: ChannelId,
    #[serde(default, deserialize_with = "optional_id")]
    pub last_message_id: Option<MessageId>,
    #[serde(default)]
    pub mention_count: u32,
    #[serde(default)]
    pub last_pin_timestamp: Option<String>,
    #[serde(default)]
    pub flags: u64,
}

/// Per-guild notification settings, including the mute state.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserGuildSettings {
    /// `null` for the entry that covers every DM.
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    #[serde(default)]
    pub suppress_everyone: bool,
    #[serde(default)]
    pub suppress_roles: bool,
    /// 0 all messages, 1 only mentions, 2 nothing, 3 inherit.
    #[serde(default)]
    pub message_notifications: u8,
    #[serde(default)]
    pub channel_overrides: Vec<ChannelOverride>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChannelOverride {
    pub channel_id: ChannelId,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    #[serde(default)]
    pub message_notifications: u8,
    #[serde(default)]
    pub collapsed: bool,
}

/// A mute that expires.
///
/// "Mute for 8 hours" is stored as an end time, and nothing tells the client
/// when it passes — no gateway event, no refresh. A mute is therefore always a
/// question asked now rather than a flag read once.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MuteConfig {
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub selected_time_window: Option<i64>,
}

impl MuteConfig {
    /// Whether the mute this configures is still in force.
    pub fn active_at(&self, now: jiff::Timestamp) -> bool {
        let Some(end) = self.end_time.as_deref() else {
            // No end time is an indefinite mute, which is the ordinary case.
            return true;
        };
        match end.parse::<jiff::Timestamp>() {
            Ok(end) => end > now,
            // An end time that cannot be parsed is treated as indefinite. The
            // alternative is silently unmuting a channel the user muted, which
            // is the louder of the two failures.
            Err(_) => true,
        }
    }
}

impl UserGuildSettings {
    /// Whether the guild as a whole is muted right now.
    pub fn muted_at(&self, now: jiff::Timestamp) -> bool {
        self.muted && self.mute_config.as_ref().is_none_or(|c| c.active_at(now))
    }

    pub fn channel_override(&self, channel: ChannelId) -> Option<&ChannelOverride> {
        self.channel_overrides
            .iter()
            .find(|o| o.channel_id == channel)
    }
}

impl ChannelOverride {
    pub fn muted_at(&self, now: jiff::Timestamp) -> bool {
        self.muted && self.mute_config.as_ref().is_none_or(|c| c.active_at(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::model::Collection;

    #[test]
    fn a_read_state_arrives_bare_or_versioned() {
        let bare: Collection<ReadState> =
            serde_json::from_str(r#"[{"id":"1","last_message_id":"9","mention_count":2}]"#)
                .unwrap();
        let versioned: Collection<ReadState> = serde_json::from_str(
            r#"{"version":1188,"partial":false,"entries":[{"id":"1","last_message_id":"9","mention_count":2}]}"#,
        )
        .unwrap();

        let bare = bare.entries();
        let versioned = versioned.entries();
        assert_eq!(bare.len(), versioned.len());
        assert_eq!(bare[0].id, versioned[0].id);
        assert_eq!(bare[0].last_message_id, Some(MessageId(9)));
        assert_eq!(versioned[0].mention_count, 2);
    }

    #[test]
    fn an_unacknowledged_channel_has_no_last_message() {
        let state: ReadState = serde_json::from_str(r#"{"id":"1","last_message_id":0}"#).unwrap();
        assert_eq!(
            state.last_message_id, None,
            "zero is Discord's spelling of never acknowledged"
        );
    }

    #[test]
    fn an_expired_mute_is_not_a_mute() {
        let now: jiff::Timestamp = "2026-09-13T12:00:00Z".parse().unwrap();
        let expired = MuteConfig {
            end_time: Some("2026-09-13T11:00:00Z".into()),
            selected_time_window: Some(3600),
        };
        let running = MuteConfig {
            end_time: Some("2026-09-13T13:00:00Z".into()),
            selected_time_window: Some(3600),
        };
        let forever = MuteConfig::default();

        assert!(!expired.active_at(now));
        assert!(running.active_at(now));
        assert!(forever.active_at(now), "no end time means indefinite");
    }

    #[test]
    fn an_unparseable_end_time_keeps_the_channel_muted() {
        let now = jiff::Timestamp::now();
        let odd = MuteConfig {
            end_time: Some("whenever".into()),
            selected_time_window: None,
        };
        assert!(
            odd.active_at(now),
            "unmuting on a parse failure is the louder failure"
        );
    }

    #[test]
    fn the_dm_settings_entry_has_no_guild() {
        let settings: UserGuildSettings =
            serde_json::from_str(r#"{"guild_id":null,"muted":false,"channel_overrides":[]}"#)
                .unwrap();
        assert!(settings.guild_id.is_none());
    }

    #[test]
    fn a_channel_override_is_found_by_id() {
        let settings: UserGuildSettings = serde_json::from_str(
            r#"{"guild_id":"1","muted":false,"channel_overrides":[{"channel_id":"5","muted":true}]}"#,
        )
        .unwrap();
        let now = jiff::Timestamp::now();
        assert!(settings
            .channel_override(ChannelId(5))
            .is_some_and(|o| o.muted_at(now)));
        assert!(settings.channel_override(ChannelId(6)).is_none());
        assert!(!settings.muted_at(now));
    }
}
