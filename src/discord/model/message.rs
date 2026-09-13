//! Messages, and everything that hangs off one.
//!
//! This is the largest wire type in the client and the one most exposed to
//! Discord's habit of adding fields on a Tuesday, so the rules from
//! [`super`](crate::discord::model) are applied harder here than anywhere else:
//! no `deny_unknown_fields`, `#[serde(default)]` on everything except the id,
//! and every id accepted as a string or a number.
//!
//! Three things are worth knowing before reading it.
//!
//! **A message's time comes from its id.** The `timestamp` field is a string
//! Discord formats, and a message whose timestamp fails to parse must still be
//! readable, so [`Message::created_at`] falls back to the snowflake — which
//! encodes the same instant and cannot be malformed.
//!
//! **`referenced_message` is a whole message.** A reply carries the message it
//! answers, inline and one level deep (Discord does not nest further). It is
//! boxed for the obvious reason: without the box the type is recursive and does
//! not compile, and with it a reply costs one pointer rather than a second copy
//! of every field.
//!
//! **`nonce` is how an echo is recognised.** A message this client sent comes
//! back through the gateway like any other, and the only thing tying it to the
//! optimistic row already on screen is the nonce that went out with the POST.
//! It is a string on the wire even when it was sent as a number.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::discord::model::user::User;
use crate::discord::snowflake::{
    ApplicationId, AttachmentId, ChannelId, EmojiId, GuildId, MessageId, RoleId, StickerId, UserId,
};

/// The `type` field.
///
/// Most of these are system messages: a join, a boost, a pin, a name change.
/// The client draws them as one dim line rather than as a message with an
/// author, which is why [`MessageKind::is_system`] exists and why an unknown
/// number is `Unknown` rather than an error — Discord adds these faster than
/// anybody documents them, and a number nobody has heard of is still a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MessageKind {
    #[default]
    Default,
    RecipientAdd,
    RecipientRemove,
    Call,
    ChannelNameChange,
    ChannelIconChange,
    ChannelPinnedMessage,
    UserJoin,
    GuildBoost,
    GuildBoostTier1,
    GuildBoostTier2,
    GuildBoostTier3,
    ChannelFollowAdd,
    GuildDiscoveryDisqualified,
    GuildDiscoveryRequalified,
    GuildDiscoveryGracePeriodInitialWarning,
    GuildDiscoveryGracePeriodFinalWarning,
    ThreadCreated,
    Reply,
    ChatInputCommand,
    ThreadStarterMessage,
    GuildInviteReminder,
    ContextMenuCommand,
    AutoModerationAction,
    RoleSubscriptionPurchase,
    InteractionPremiumUpsell,
    StageStart,
    StageEnd,
    StageSpeaker,
    StageTopic,
    GuildApplicationPremiumSubscription,
    Unknown(u8),
}

impl MessageKind {
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => MessageKind::Default,
            1 => MessageKind::RecipientAdd,
            2 => MessageKind::RecipientRemove,
            3 => MessageKind::Call,
            4 => MessageKind::ChannelNameChange,
            5 => MessageKind::ChannelIconChange,
            6 => MessageKind::ChannelPinnedMessage,
            7 => MessageKind::UserJoin,
            8 => MessageKind::GuildBoost,
            9 => MessageKind::GuildBoostTier1,
            10 => MessageKind::GuildBoostTier2,
            11 => MessageKind::GuildBoostTier3,
            12 => MessageKind::ChannelFollowAdd,
            14 => MessageKind::GuildDiscoveryDisqualified,
            15 => MessageKind::GuildDiscoveryRequalified,
            16 => MessageKind::GuildDiscoveryGracePeriodInitialWarning,
            17 => MessageKind::GuildDiscoveryGracePeriodFinalWarning,
            18 => MessageKind::ThreadCreated,
            19 => MessageKind::Reply,
            20 => MessageKind::ChatInputCommand,
            21 => MessageKind::ThreadStarterMessage,
            22 => MessageKind::GuildInviteReminder,
            23 => MessageKind::ContextMenuCommand,
            24 => MessageKind::AutoModerationAction,
            25 => MessageKind::RoleSubscriptionPurchase,
            26 => MessageKind::InteractionPremiumUpsell,
            27 => MessageKind::StageStart,
            28 => MessageKind::StageEnd,
            29 => MessageKind::StageSpeaker,
            31 => MessageKind::StageTopic,
            32 => MessageKind::GuildApplicationPremiumSubscription,
            n => MessageKind::Unknown(n),
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            MessageKind::Default => 0,
            MessageKind::RecipientAdd => 1,
            MessageKind::RecipientRemove => 2,
            MessageKind::Call => 3,
            MessageKind::ChannelNameChange => 4,
            MessageKind::ChannelIconChange => 5,
            MessageKind::ChannelPinnedMessage => 6,
            MessageKind::UserJoin => 7,
            MessageKind::GuildBoost => 8,
            MessageKind::GuildBoostTier1 => 9,
            MessageKind::GuildBoostTier2 => 10,
            MessageKind::GuildBoostTier3 => 11,
            MessageKind::ChannelFollowAdd => 12,
            MessageKind::GuildDiscoveryDisqualified => 14,
            MessageKind::GuildDiscoveryRequalified => 15,
            MessageKind::GuildDiscoveryGracePeriodInitialWarning => 16,
            MessageKind::GuildDiscoveryGracePeriodFinalWarning => 17,
            MessageKind::ThreadCreated => 18,
            MessageKind::Reply => 19,
            MessageKind::ChatInputCommand => 20,
            MessageKind::ThreadStarterMessage => 21,
            MessageKind::GuildInviteReminder => 22,
            MessageKind::ContextMenuCommand => 23,
            MessageKind::AutoModerationAction => 24,
            MessageKind::RoleSubscriptionPurchase => 25,
            MessageKind::InteractionPremiumUpsell => 26,
            MessageKind::StageStart => 27,
            MessageKind::StageEnd => 28,
            MessageKind::StageSpeaker => 29,
            MessageKind::StageTopic => 31,
            MessageKind::GuildApplicationPremiumSubscription => 32,
            MessageKind::Unknown(n) => n,
        }
    }

    /// Whether this is something Discord said rather than something a person
    /// typed. A system message has an author but no content worth attributing.
    pub const fn is_system(self) -> bool {
        !matches!(
            self,
            MessageKind::Default
                | MessageKind::Reply
                | MessageKind::ChatInputCommand
                | MessageKind::ContextMenuCommand
                | MessageKind::ThreadStarterMessage
        )
    }
}

impl Serialize for MessageKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for MessageKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u8::deserialize(d).map(MessageKind::from_code)
    }
}

/// The `flags` bitfield, of which the client reads three.
pub mod flags {
    /// The author suppressed the link previews on this message.
    pub const SUPPRESS_EMBEDS: u64 = 1 << 2;
    /// Only the person who ran the command can see it.
    pub const EPHEMERAL: u64 = 1 << 6;
    /// The reply does not ping the person it answers, and an `@everyone` in it
    /// does not ping anybody either.
    pub const SUPPRESS_NOTIFICATIONS: u64 = 1 << 12;
}

/// A file on a message.
///
/// `width` and `height` are the reason this type matters before any bytes are
/// fetched: the chat panel reserves the right number of rows for a picture from
/// the declared size, so the layout does not jump when the image arrives.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size: u64,
    /// Signed, and it expires. See `MediaKey::Attachment`, which is keyed by
    /// what the file is rather than by this string.
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub proxy_url: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// A voice message's length, in seconds.
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub waveform: Option<String>,
    #[serde(default)]
    pub flags: u64,
    #[serde(default)]
    pub ephemeral: bool,
}

impl Attachment {
    /// Whether this is something the client can draw inline.
    pub fn is_image(&self) -> bool {
        match self.content_type.as_deref() {
            Some(kind) => kind.starts_with("image/"),
            // No content type is ordinary on older messages; the extension is
            // the only thing left.
            None => matches!(
                extension(&self.filename).as_deref(),
                Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
            ),
        }
    }

    /// Whether this opens in an external player rather than in the terminal.
    pub fn is_video(&self) -> bool {
        match self.content_type.as_deref() {
            Some(kind) => kind.starts_with("video/"),
            None => matches!(
                extension(&self.filename).as_deref(),
                Some("mp4" | "webm" | "mov" | "mkv")
            ),
        }
    }
}

fn extension(name: &str) -> Option<String> {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

/// What an embed is for.
///
/// The five that matter are all here. `Gifv` is the one worth naming: a Tenor
/// or Giphy link unfurls into an embed of this type whose `video` is an mp4 and
/// whose `thumbnail` is a still, and the client draws the still with a play
/// marker rather than trying to animate an mp4 in a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EmbedKind {
    #[default]
    Rich,
    Image,
    Video,
    Gifv,
    Article,
    Link,
    /// Anything Discord invented since; kept verbatim so a log says which.
    Other(String),
}

impl EmbedKind {
    pub fn as_str(&self) -> &str {
        match self {
            EmbedKind::Rich => "rich",
            EmbedKind::Image => "image",
            EmbedKind::Video => "video",
            EmbedKind::Gifv => "gifv",
            EmbedKind::Article => "article",
            EmbedKind::Link => "link",
            EmbedKind::Other(name) => name,
        }
    }

    fn from_str(name: &str) -> Self {
        match name {
            "rich" => EmbedKind::Rich,
            "image" => EmbedKind::Image,
            "video" => EmbedKind::Video,
            "gifv" => EmbedKind::Gifv,
            "article" => EmbedKind::Article,
            "link" => EmbedKind::Link,
            other => EmbedKind::Other(other.to_string()),
        }
    }
}

impl Serialize for EmbedKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EmbedKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(|s| EmbedKind::from_str(&s))
    }
}

/// A picture, thumbnail or video inside an embed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedMedia {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedAuthor {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub proxy_icon_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedFooter {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedProvider {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub inline: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Embed {
    #[serde(rename = "type", default)]
    pub kind: EmbedKind,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Packed 0xRRGGBB, as Discord sends it.
    #[serde(default)]
    pub color: Option<u32>,
    #[serde(default)]
    pub author: Option<EmbedAuthor>,
    #[serde(default)]
    pub footer: Option<EmbedFooter>,
    #[serde(default)]
    pub provider: Option<EmbedProvider>,
    #[serde(default)]
    pub image: Option<EmbedMedia>,
    #[serde(default)]
    pub thumbnail: Option<EmbedMedia>,
    #[serde(default)]
    pub video: Option<EmbedMedia>,
    #[serde(default)]
    pub fields: Vec<EmbedField>,
}

impl Embed {
    /// Whether this embed is a moving picture the client shows as a still with
    /// a play marker: an mp4 behind a GIF-shaped link.
    pub fn is_playable(&self) -> bool {
        matches!(self.kind, EmbedKind::Gifv | EmbedKind::Video)
    }

    /// The best still to draw for it, whichever field carries one.
    pub fn still(&self) -> Option<&EmbedMedia> {
        self.image.as_ref().or(self.thumbnail.as_ref())
    }
}

/// An emoji as it appears on a reaction: unicode has a name and no id, custom
/// has both.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialEmoji {
    #[serde(default, deserialize_with = "crate::discord::model::optional_id")]
    pub id: Option<EmojiId>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub animated: bool,
}

impl PartialEmoji {
    pub fn is_custom(&self) -> bool {
        self.id.is_some()
    }

    /// The form a reaction route wants before percent-encoding: the character
    /// itself for unicode, `name:id` for a custom emoji.
    pub fn reaction_key(&self) -> String {
        match self.id {
            Some(id) => format!("{}:{}", self.name.as_deref().unwrap_or("_"), id),
            None => self.name.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reaction {
    #[serde(default)]
    pub count: u32,
    /// Whether this account is one of the people who reacted. It is the only
    /// part of a reaction that is per-viewer, and it is what decides whether
    /// clicking the chip adds or removes.
    #[serde(default)]
    pub me: bool,
    #[serde(default)]
    pub emoji: PartialEmoji,
    /// Super-reaction bookkeeping, kept so the count reads correctly.
    #[serde(default)]
    pub burst_count: u32,
    #[serde(default)]
    pub me_burst: bool,
}

/// Where a reply points.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageReference {
    /// 0 is a reply, 1 is a forward.
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default, deserialize_with = "crate::discord::model::optional_id")]
    pub message_id: Option<MessageId>,
    #[serde(default, deserialize_with = "crate::discord::model::optional_id")]
    pub channel_id: Option<ChannelId>,
    #[serde(default, deserialize_with = "crate::discord::model::optional_id")]
    pub guild_id: Option<GuildId>,
    #[serde(default)]
    pub fail_if_not_exists: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StickerItem {
    pub id: StickerId,
    #[serde(default)]
    pub name: String,
    /// 1 png, 2 apng, 3 lottie, 4 gif. Only the first is drawn.
    #[serde(default)]
    pub format_type: u8,
}

/// The author's membership in the guild the message was posted in.
///
/// Carries the nickname and the roles, which is what a name is coloured by. The
/// user is not in here: on a message it is in `author`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialMember {
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub roles: Vec<RoleId>,
    #[serde(default)]
    pub joined_at: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub premium_since: Option<String>,
}

/// One message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    #[serde(default)]
    pub channel_id: ChannelId,
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    #[serde(default)]
    pub author: User,
    #[serde(default)]
    pub member: Option<PartialMember>,
    #[serde(default)]
    pub content: String,
    #[serde(default, deserialize_with = "lenient_timestamp")]
    pub timestamp: Option<jiff::Timestamp>,
    #[serde(default, deserialize_with = "lenient_timestamp")]
    pub edited_timestamp: Option<jiff::Timestamp>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub embeds: Vec<Embed>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    /// The message a reply answers, one level deep. `None` on a reply whose
    /// target was deleted, which is why the reference is kept separately.
    #[serde(default)]
    pub referenced_message: Option<Box<Message>>,
    #[serde(default)]
    pub message_reference: Option<MessageReference>,
    #[serde(rename = "type", default)]
    pub kind: MessageKind,
    #[serde(default)]
    pub flags: u64,
    #[serde(default)]
    pub sticker_items: Vec<StickerItem>,
    /// What this client sent with the POST, echoed back. A string here even
    /// when it went out as a number, because that is how Discord returns it.
    #[serde(default, deserialize_with = "nonce_as_string")]
    pub nonce: Option<String>,
    #[serde(default)]
    pub mentions: Vec<User>,
    #[serde(default)]
    pub mention_roles: Vec<RoleId>,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub tts: bool,
    #[serde(default)]
    pub webhook_id: Option<UserId>,
    #[serde(default)]
    pub application_id: Option<ApplicationId>,
}

impl Message {
    /// When it was posted.
    ///
    /// The snowflake rather than the string whenever the string is missing or
    /// unparseable: both encode the same instant, and only one of them can be
    /// malformed.
    pub fn created_at(&self) -> jiff::Timestamp {
        self.timestamp.unwrap_or_else(|| self.id.timestamp())
    }

    pub fn is_edited(&self) -> bool {
        self.edited_timestamp.is_some()
    }

    /// Whether the author asked for no link previews.
    pub fn embeds_suppressed(&self) -> bool {
        self.flags & flags::SUPPRESS_EMBEDS != 0
    }

    /// Whether this message pings nobody regardless of what it says.
    pub fn notifications_suppressed(&self) -> bool {
        self.flags & flags::SUPPRESS_NOTIFICATIONS != 0
    }

    /// The message this one replies to, if it is still there.
    pub fn reply_target(&self) -> Option<MessageId> {
        self.message_reference
            .as_ref()
            .filter(|r| r.kind == 0)
            .and_then(|r| r.message_id)
    }

    pub fn mentions_user(&self, user: UserId) -> bool {
        self.mentions.iter().any(|u| u.id == user)
    }

    /// Whether any of the roles this message pinged is one the reader holds.
    pub fn mentions_any_role(&self, roles: &[RoleId]) -> bool {
        self.mention_roles.iter().any(|r| roles.contains(r))
    }

    /// What to call the author here: the per-guild nickname when there is one.
    pub fn author_name(&self) -> &str {
        match self.member.as_ref().and_then(|m| m.nick.as_deref()) {
            Some(nick) if !nick.is_empty() => nick,
            _ => self.author.display_name(),
        }
    }

    /// Whether the message shows nothing at all, which is what an attachment-
    /// only or embed-only message looks like before its media loads.
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
            && self.attachments.is_empty()
            && self.embeds.is_empty()
            && self.sticker_items.is_empty()
    }

    /// Take the fields a MESSAGE_UPDATE carries and leave the rest alone.
    ///
    /// An update is a partial message: Discord sends the id, the channel, and
    /// whichever fields changed. Overwriting wholesale would blank the author
    /// and the timestamp of every edited message, which is the classic bug in
    /// every client that treats an update as a create.
    pub fn merge_update(&self, update: &serde_json::Value) -> Message {
        let mut merged = self.clone();
        let Some(object) = update.as_object() else {
            return merged;
        };

        if let Some(content) = object.get("content").and_then(|v| v.as_str()) {
            merged.content = content.to_string();
        }
        if object.contains_key("edited_timestamp") {
            merged.edited_timestamp = object
                .get("edited_timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok());
        }
        if let Some(embeds) = object.get("embeds") {
            if let Ok(embeds) = serde_json::from_value::<Vec<Embed>>(embeds.clone()) {
                merged.embeds = embeds;
            }
        }
        if let Some(attachments) = object.get("attachments") {
            if let Ok(attachments) = serde_json::from_value::<Vec<Attachment>>(attachments.clone())
            {
                merged.attachments = attachments;
            }
        }
        if let Some(flags) = object.get("flags").and_then(|v| v.as_u64()) {
            merged.flags = flags;
        }
        if let Some(pinned) = object.get("pinned").and_then(|v| v.as_bool()) {
            merged.pinned = pinned;
        }
        if let Some(mentions) = object.get("mentions") {
            if let Ok(mentions) = serde_json::from_value::<Vec<User>>(mentions.clone()) {
                merged.mentions = mentions;
            }
        }
        if let Some(roles) = object.get("mention_roles") {
            if let Ok(roles) = serde_json::from_value::<Vec<RoleId>>(roles.clone()) {
                merged.mention_roles = roles;
            }
        }
        if let Some(everyone) = object.get("mention_everyone").and_then(|v| v.as_bool()) {
            merged.mention_everyone = everyone;
        }
        merged
    }
}

/// A timestamp that refuses to fail the message it is on.
///
/// Discord's format is RFC 3339 with a numeric offset, which `jiff` parses. A
/// value it cannot parse becomes `None` rather than an error: the id carries
/// the same instant, and losing a message because its clock string changed
/// shape is the kind of failure that takes a whole channel with it.
fn lenient_timestamp<'de, D: Deserializer<'de>>(d: D) -> Result<Option<jiff::Timestamp>, D::Error> {
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw.and_then(|s| s.parse().ok()))
}

/// A nonce, whichever way it was spelled.
///
/// Sent as a string, echoed as a string — usually. A number has been seen, and
/// a nonce that fails to parse is a message that never matches its optimistic
/// row, so both are accepted and both come out as the decimal string.
fn nonce_as_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match raw {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full message, as the documented field list describes one. Used by the
    /// mutation proptest below as well as by the table tests.
    pub(crate) const FIXTURE: &str = include_str!("../../../testdata/gateway/message.json");

    fn fixture() -> Message {
        serde_json::from_str(FIXTURE).expect("the fixture parses")
    }

    #[test]
    fn the_fixture_carries_everything_the_chat_panel_draws() {
        let message = fixture();
        assert_eq!(message.id, MessageId(500000000000000110));
        assert_eq!(message.channel_id, ChannelId(200000000000000011));
        assert_eq!(message.author.display_name(), "Alex");
        assert_eq!(message.author_name(), "Al", "a nickname wins over a name");
        assert!(message.is_edited());
        assert_eq!(message.kind, MessageKind::Reply);
        assert_eq!(message.reply_target(), Some(MessageId(500000000000000109)));
        assert!(message.referenced_message.is_some());
        assert_eq!(message.attachments.len(), 1);
        assert!(message.attachments[0].is_image());
        assert_eq!(message.attachments[0].width, Some(1024));
        assert_eq!(message.embeds.len(), 2);
        assert_eq!(message.embeds[0].kind, EmbedKind::Link);
        assert_eq!(message.embeds[1].kind, EmbedKind::Gifv);
        assert!(message.embeds[1].is_playable());
        assert!(message.embeds[1].still().is_some());
        assert_eq!(message.reactions.len(), 2);
        assert!(message.reactions[0].me);
        assert!(!message.reactions[1].emoji.is_custom());
        assert_eq!(message.sticker_items.len(), 1);
        assert_eq!(message.mention_roles.len(), 1);
        assert!(!message.mention_everyone);
        assert_eq!(message.nonce.as_deref(), Some("81237712343"));
    }

    #[test]
    fn a_message_with_nothing_but_an_id_still_parses() {
        let message: Message = serde_json::from_str(r#"{"id":"1"}"#).unwrap();
        assert_eq!(message.id, MessageId(1));
        assert_eq!(message.kind, MessageKind::Default);
        assert!(message.is_empty());
    }

    #[test]
    fn the_time_falls_back_to_the_snowflake() {
        let good: Message =
            serde_json::from_str(r#"{"id":"1","timestamp":"2026-09-13T12:00:00.000000+00:00"}"#)
                .unwrap();
        assert_eq!(
            good.created_at().to_string(),
            "2026-09-13T12:00:00Z",
            "a parseable timestamp is used as sent"
        );

        let broken: Message =
            serde_json::from_str(r#"{"id":"175928847299117063","timestamp":"tuesday"}"#).unwrap();
        assert!(broken.timestamp.is_none());
        assert_eq!(broken.created_at().as_millisecond(), 1_462_015_105_796);
    }

    #[test]
    fn a_nonce_arrives_as_a_string_or_a_number() {
        let string: Message = serde_json::from_str(r#"{"id":"1","nonce":"42"}"#).unwrap();
        let number: Message = serde_json::from_str(r#"{"id":"1","nonce":42}"#).unwrap();
        let none: Message = serde_json::from_str(r#"{"id":"1","nonce":null}"#).unwrap();
        assert_eq!(string.nonce.as_deref(), Some("42"));
        assert_eq!(number.nonce.as_deref(), Some("42"));
        assert_eq!(none.nonce, None);
    }

    #[test]
    fn a_message_kind_nobody_has_heard_of_is_a_row_not_an_error() {
        let message: Message = serde_json::from_str(r#"{"id":"1","type":200}"#).unwrap();
        assert_eq!(message.kind, MessageKind::Unknown(200));
        assert!(message.kind.is_system());
        assert_eq!(
            serde_json::to_value(message.kind).unwrap(),
            serde_json::json!(200)
        );
    }

    #[test]
    fn every_known_message_kind_round_trips() {
        for code in 0u8..=40 {
            assert_eq!(MessageKind::from_code(code).code(), code);
        }
        assert!(!MessageKind::Default.is_system());
        assert!(!MessageKind::Reply.is_system());
        assert!(MessageKind::UserJoin.is_system());
    }

    #[test]
    fn an_embed_type_nobody_has_heard_of_keeps_its_name() {
        let embed: Embed = serde_json::from_str(r#"{"type":"poll_result"}"#).unwrap();
        assert_eq!(embed.kind, EmbedKind::Other("poll_result".into()));
        assert_eq!(embed.kind.as_str(), "poll_result");
        assert!(!embed.is_playable());
    }

    #[test]
    fn an_attachment_without_a_content_type_is_judged_by_its_name() {
        let png: Attachment =
            serde_json::from_str(r#"{"id":"1","filename":"Screenshot.PNG"}"#).unwrap();
        let video: Attachment =
            serde_json::from_str(r#"{"id":"2","filename":"clip.mp4"}"#).unwrap();
        let other: Attachment =
            serde_json::from_str(r#"{"id":"3","filename":"notes.txt"}"#).unwrap();
        assert!(png.is_image());
        assert!(video.is_video());
        assert!(!other.is_image() && !other.is_video());
    }

    #[test]
    fn a_reaction_key_distinguishes_unicode_from_custom() {
        let unicode = PartialEmoji {
            id: None,
            name: Some("👍".into()),
            animated: false,
        };
        let custom = PartialEmoji {
            id: Some(EmojiId(12345)),
            name: Some("pepe".into()),
            animated: true,
        };
        assert_eq!(unicode.reaction_key(), "👍");
        assert_eq!(custom.reaction_key(), "pepe:12345");
        assert!(custom.is_custom());
    }

    #[test]
    fn the_suppression_flags_are_read_off_the_bitfield() {
        let quiet: Message = serde_json::from_str(r#"{"id":"1","flags":4100}"#).unwrap();
        assert!(quiet.embeds_suppressed());
        assert!(quiet.notifications_suppressed());
        let loud: Message = serde_json::from_str(r#"{"id":"1"}"#).unwrap();
        assert!(!loud.embeds_suppressed());
        assert!(!loud.notifications_suppressed());
    }

    /// An update is a partial message, and treating one as a create is how a
    /// client blanks the author of every message anybody edits.
    #[test]
    fn an_update_changes_only_what_it_names() {
        let original = fixture();
        let update = serde_json::json!({
            "id": "500000000000000110",
            "channel_id": "200000000000000011",
            "content": "edited text",
            "edited_timestamp": "2026-09-13T12:05:00.000000+00:00",
            "embeds": []
        });
        let merged = original.merge_update(&update);

        assert_eq!(merged.content, "edited text");
        assert!(merged.embeds.is_empty());
        assert_eq!(
            merged.author.id, original.author.id,
            "an update blanked the author"
        );
        assert_eq!(merged.attachments.len(), 1, "an update dropped a file");
        assert_eq!(merged.reactions.len(), 2);
        assert!(merged.is_edited());
    }

    /// A MESSAGE_UPDATE for an embed that finished unfurling carries nothing
    /// but the embeds, and it must not clear the content.
    #[test]
    fn an_embed_only_update_keeps_the_text() {
        let original = fixture();
        let update = serde_json::json!({
            "id": "500000000000000110",
            "channel_id": "200000000000000011",
            "embeds": []
        });
        let merged = original.merge_update(&update);
        assert_eq!(merged.content, original.content);
        assert!(merged.embeds.is_empty());
    }

    #[test]
    fn mentions_are_answered_by_id_and_by_role() {
        let message = fixture();
        assert!(message.mentions_user(UserId(100000000000000001)));
        assert!(!message.mentions_user(UserId(999)));
        assert!(message.mentions_any_role(&[RoleId(200000000000000002)]));
        assert!(!message.mentions_any_role(&[RoleId(1)]));
        assert!(!message.mentions_any_role(&[]));
    }

    proptest::proptest! {
        /// The rule for every wire type: bytes somebody else wrote produce an
        /// `Ok` or an `Err` and never a panic. The generator mutates the real
        /// fixture — deleting keys and swapping types — because random JSON
        /// exercises the error path and almost never reaches the parse.
        #[test]
        fn a_mutated_message_is_never_a_panic(
            drop_key in 0usize..24,
            swap_key in 0usize..24,
            replacement in proptest::prop_oneof![
                proptest::strategy::Just(serde_json::Value::Null),
                proptest::strategy::Just(serde_json::json!(0)),
                proptest::strategy::Just(serde_json::json!("")),
                proptest::strategy::Just(serde_json::json!([])),
                proptest::strategy::Just(serde_json::json!({})),
                proptest::strategy::Just(serde_json::json!(true)),
            ],
        ) {
            let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
            let keys: Vec<String> = value
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            if keys.is_empty() {
                return Ok(());
            }
            if let Some(object) = value.as_object_mut() {
                object.remove(&keys[drop_key % keys.len()]);
                object.insert(keys[swap_key % keys.len()].clone(), replacement);
            }
            let _ = serde_json::from_value::<Message>(value);
        }

        #[test]
        fn arbitrary_json_is_refused_rather_than_fatal(text in ".{0,300}") {
            let _ = serde_json::from_str::<Message>(&text);
        }
    }
}
