//! Channels: guild text, categories, threads, DMs and group DMs, which Discord
//! models as one type discriminated by a number.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::discord::model::{optional_id, User};
use crate::discord::snowflake::{ChannelId, GuildId, MessageId, UserId};

/// The `type` field.
///
/// `Unknown` is not a defensive nicety: Discord has added forum, media, stage
/// and directory channels since this design was drawn, and a client that
/// refuses to deserialise a channel kind it has not heard of loses the whole
/// guild rather than one row in a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChannelKind {
    #[default]
    GuildText,
    Dm,
    GuildVoice,
    GroupDm,
    GuildCategory,
    GuildAnnouncement,
    AnnouncementThread,
    PublicThread,
    PrivateThread,
    GuildStageVoice,
    GuildDirectory,
    GuildForum,
    GuildMedia,
    Unknown(u8),
}

impl ChannelKind {
    pub const fn code(self) -> u8 {
        match self {
            ChannelKind::GuildText => 0,
            ChannelKind::Dm => 1,
            ChannelKind::GuildVoice => 2,
            ChannelKind::GroupDm => 3,
            ChannelKind::GuildCategory => 4,
            ChannelKind::GuildAnnouncement => 5,
            ChannelKind::AnnouncementThread => 10,
            ChannelKind::PublicThread => 11,
            ChannelKind::PrivateThread => 12,
            ChannelKind::GuildStageVoice => 13,
            ChannelKind::GuildDirectory => 14,
            ChannelKind::GuildForum => 15,
            ChannelKind::GuildMedia => 16,
            ChannelKind::Unknown(n) => n,
        }
    }

    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => ChannelKind::GuildText,
            1 => ChannelKind::Dm,
            2 => ChannelKind::GuildVoice,
            3 => ChannelKind::GroupDm,
            4 => ChannelKind::GuildCategory,
            5 => ChannelKind::GuildAnnouncement,
            10 => ChannelKind::AnnouncementThread,
            11 => ChannelKind::PublicThread,
            12 => ChannelKind::PrivateThread,
            13 => ChannelKind::GuildStageVoice,
            14 => ChannelKind::GuildDirectory,
            15 => ChannelKind::GuildForum,
            16 => ChannelKind::GuildMedia,
            n => ChannelKind::Unknown(n),
        }
    }

    /// Whether messages can be read and written here.
    pub const fn is_text(self) -> bool {
        matches!(
            self,
            ChannelKind::GuildText
                | ChannelKind::Dm
                | ChannelKind::GroupDm
                | ChannelKind::GuildAnnouncement
                | ChannelKind::AnnouncementThread
                | ChannelKind::PublicThread
                | ChannelKind::PrivateThread
        )
    }

    pub const fn is_thread(self) -> bool {
        matches!(
            self,
            ChannelKind::AnnouncementThread
                | ChannelKind::PublicThread
                | ChannelKind::PrivateThread
        )
    }

    pub const fn is_private(self) -> bool {
        matches!(self, ChannelKind::Dm | ChannelKind::GroupDm)
    }

    /// Voice and stage channels, which this client deliberately does not join.
    pub const fn is_voice(self) -> bool {
        matches!(self, ChannelKind::GuildVoice | ChannelKind::GuildStageVoice)
    }
}

impl Serialize for ChannelKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for ChannelKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u8::deserialize(d).map(ChannelKind::from_code)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    #[serde(rename = "type", default)]
    pub kind: ChannelKind,
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    #[serde(default)]
    pub name: Option<String>,
    /// Guild channels are ordered by this within their category. Absent on DMs.
    #[serde(default)]
    pub position: i32,
    /// The category for a guild channel, or the parent channel for a thread.
    #[serde(default)]
    pub parent_id: Option<ChannelId>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub nsfw: bool,
    #[serde(default, deserialize_with = "optional_id")]
    pub last_message_id: Option<MessageId>,
    /// Present on DMs and group DMs when the session did not ask for deduped
    /// users.
    #[serde(default)]
    pub recipients: Vec<User>,
    /// The same list under `DEDUPE_USER_OBJECTS`, where the user objects
    /// themselves arrive once in READY's `users`.
    #[serde(default)]
    pub recipient_ids: Vec<UserId>,
    /// The group DM's creator.
    #[serde(default)]
    pub owner_id: Option<UserId>,
    /// A group DM's picture hash.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub flags: u64,
}

impl Channel {
    /// Everyone in a DM, however the payload spelled it.
    pub fn recipient_ids(&self) -> Vec<UserId> {
        if !self.recipient_ids.is_empty() {
            return self.recipient_ids.clone();
        }
        self.recipients.iter().map(|u| u.id).collect()
    }

    /// A guild channel's `#name`, or a DM's own title when it has one.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref().filter(|n| !n.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_nobody_has_heard_of_survives() {
        let channel: Channel = serde_json::from_str(r#"{"id":"1","type":99}"#).unwrap();
        assert_eq!(channel.kind, ChannelKind::Unknown(99));
        assert!(!channel.kind.is_text());
        assert_eq!(
            serde_json::to_value(channel.kind).unwrap(),
            serde_json::json!(99),
            "an unknown kind goes back out as the number it came in as"
        );
    }

    #[test]
    fn every_known_code_round_trips() {
        for code in 0u8..=20 {
            assert_eq!(ChannelKind::from_code(code).code(), code);
        }
    }

    #[test]
    fn recipients_are_read_from_either_spelling() {
        let inline: Channel =
            serde_json::from_str(r#"{"id":"1","type":1,"recipients":[{"id":"7","username":"a"}]}"#)
                .unwrap();
        let deduped: Channel =
            serde_json::from_str(r#"{"id":"1","type":1,"recipient_ids":["7"]}"#).unwrap();
        assert_eq!(inline.recipient_ids(), deduped.recipient_ids());
        assert_eq!(inline.recipient_ids(), vec![UserId(7)]);
    }

    #[test]
    fn a_channel_nobody_has_posted_in_has_no_last_message() {
        let channel: Channel =
            serde_json::from_str(r#"{"id":"1","type":0,"last_message_id":null}"#).unwrap();
        assert_eq!(channel.last_message_id, None);
    }

    #[test]
    fn a_thread_knows_it_is_one() {
        let thread: Channel =
            serde_json::from_str(r#"{"id":"1","type":11,"parent_id":"2"}"#).unwrap();
        assert!(thread.kind.is_thread());
        assert!(thread.kind.is_text());
        assert_eq!(thread.parent_id, Some(ChannelId(2)));
    }
}
