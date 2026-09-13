//! What comes down the socket.
//!
//! Every gateway message is an envelope: an opcode, an optional payload, and —
//! for a dispatch — a sequence number and an event name. Decoding happens in
//! two steps on purpose. The envelope is read first, with the payload left as
//! [`serde_json::value::RawValue`], and the payload is only parsed when the
//! event name is one this client acts on. An account in a hundred guilds
//! receives a continuous stream of events it has no use for; parsing them into
//! structures that are immediately dropped is the difference between an idle
//! client and a warm laptop.
//!
//! An event this client does not know is not an error. It is
//! [`Dispatch::Unknown`] and a TRACE line. An event it *does* know whose
//! payload fails to parse is a WARN and [`Dispatch::Malformed`] — never a
//! disconnect, because one bad `PRESENCE_UPDATE` must not cost a session.

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::discord::model::guild::Guild;
use crate::discord::model::ready::{Ready, ReadySupplemental, Relationship};
use crate::discord::model::{
    Channel, Collection, Message, PartialEmoji, Presence, ReadState, User, UserGuildSettings,
};
use crate::discord::snowflake::{ChannelId, GuildId, MessageId, UserId};

/// Gateway opcodes, of which this client uses six.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    Dispatch,
    Heartbeat,
    Identify,
    PresenceUpdate,
    Resume,
    Reconnect,
    InvalidSession,
    Hello,
    HeartbeatAck,
    /// Member-list subscriptions. The web client sends 37; 14 is the older
    /// spelling of the same request and is kept behind a config flag because
    /// which one is live has to be confirmed against a real session.
    UpdateGuildSubscriptions,
    LazyRequest,
    Unknown(u8),
}

impl OpCode {
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => OpCode::Dispatch,
            1 => OpCode::Heartbeat,
            2 => OpCode::Identify,
            3 => OpCode::PresenceUpdate,
            6 => OpCode::Resume,
            7 => OpCode::Reconnect,
            9 => OpCode::InvalidSession,
            10 => OpCode::Hello,
            11 => OpCode::HeartbeatAck,
            14 => OpCode::LazyRequest,
            37 => OpCode::UpdateGuildSubscriptions,
            n => OpCode::Unknown(n),
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            OpCode::Dispatch => 0,
            OpCode::Heartbeat => 1,
            OpCode::Identify => 2,
            OpCode::PresenceUpdate => 3,
            OpCode::Resume => 6,
            OpCode::Reconnect => 7,
            OpCode::InvalidSession => 9,
            OpCode::Hello => 10,
            OpCode::HeartbeatAck => 11,
            OpCode::LazyRequest => 14,
            OpCode::UpdateGuildSubscriptions => 37,
            OpCode::Unknown(n) => n,
        }
    }
}

/// The outer shape of every message.
#[derive(Debug, Deserialize)]
pub struct Envelope<'a> {
    #[serde(rename = "op")]
    pub op: u8,
    #[serde(borrow, default)]
    pub d: Option<&'a RawValue>,
    /// The sequence number, present only on a dispatch. It is what RESUME
    /// replays from, so it is tracked even for events this client ignores.
    #[serde(default)]
    pub s: Option<u64>,
    #[serde(borrow, default)]
    pub t: Option<&'a str>,
}

impl Envelope<'_> {
    pub fn opcode(&self) -> OpCode {
        OpCode::from_code(self.op)
    }
}

/// HELLO's payload.
#[derive(Debug, Clone, Deserialize)]
pub struct Hello {
    pub heartbeat_interval: u64,
}

/// An event this client acts on.
#[derive(Debug)]
pub enum Dispatch {
    Ready(Box<Ready>),
    ReadySupplemental(Box<ReadySupplemental>),
    Resumed,
    GuildCreate(Box<Guild>),
    GuildUpdate(Box<Guild>),
    GuildDelete {
        id: GuildId,
        /// An outage rather than a departure: the guild keeps its place.
        unavailable: bool,
    },
    ChannelCreate(Box<Channel>),
    ChannelUpdate(Box<Channel>),
    ChannelDelete(Box<Channel>),
    PresenceUpdate(Box<Presence>),
    UserUpdate(Box<User>),
    RelationshipAdd(Box<Relationship>),
    RelationshipRemove {
        id: UserId,
    },
    UserGuildSettingsUpdate(Box<UserGuildSettings>),
    /// A read state changed, usually because another client of the same account
    /// read something.
    MessageAck {
        channel_id: ChannelId,
        message_id: Option<MessageId>,
        /// Present when the ack cleared mentions.
        mention_count: Option<u32>,
    },
    ChannelUnreadUpdate {
        guild_id: Option<GuildId>,
        channel_unread_updates: Vec<ReadState>,
    },

    MessageCreate(Box<Message>),
    /// A partial message: the id, the channel, and whichever fields changed.
    /// The payload is kept as JSON rather than parsed into a `Message`, because
    /// what matters is *which keys are present* -- an absent `content` means
    /// unchanged, and a `Message` with `#[serde(default)]` everywhere cannot
    /// tell that from an empty one.
    MessageUpdate {
        id: MessageId,
        channel_id: ChannelId,
        payload: serde_json::Value,
    },
    MessageDelete {
        id: MessageId,
        channel_id: ChannelId,
        guild_id: Option<GuildId>,
    },
    MessageDeleteBulk {
        ids: Vec<MessageId>,
        channel_id: ChannelId,
        guild_id: Option<GuildId>,
    },
    MessageReactionAdd {
        channel_id: ChannelId,
        message_id: MessageId,
        user_id: UserId,
        emoji: PartialEmoji,
    },
    MessageReactionRemove {
        channel_id: ChannelId,
        message_id: MessageId,
        user_id: UserId,
        emoji: PartialEmoji,
    },
    MessageReactionRemoveAll {
        channel_id: ChannelId,
        message_id: MessageId,
    },
    MessageReactionRemoveEmoji {
        channel_id: ChannelId,
        message_id: MessageId,
        emoji: PartialEmoji,
    },
    TypingStart {
        channel_id: ChannelId,
        user_id: UserId,
    },
    /// A wholesale replacement of the read states, sent after a bulk ack.
    SessionsReplace,
    /// Known name, unparseable payload. Logged and dropped.
    Malformed {
        event: String,
    },
    /// A name this client has no use for. Its sequence number still counts.
    Unknown {
        event: String,
    },
}

impl Dispatch {
    /// The event name, for logs.
    pub fn name(&self) -> &str {
        match self {
            Dispatch::Ready(_) => "READY",
            Dispatch::ReadySupplemental(_) => "READY_SUPPLEMENTAL",
            Dispatch::Resumed => "RESUMED",
            Dispatch::GuildCreate(_) => "GUILD_CREATE",
            Dispatch::GuildUpdate(_) => "GUILD_UPDATE",
            Dispatch::GuildDelete { .. } => "GUILD_DELETE",
            Dispatch::ChannelCreate(_) => "CHANNEL_CREATE",
            Dispatch::ChannelUpdate(_) => "CHANNEL_UPDATE",
            Dispatch::ChannelDelete(_) => "CHANNEL_DELETE",
            Dispatch::PresenceUpdate(_) => "PRESENCE_UPDATE",
            Dispatch::UserUpdate(_) => "USER_UPDATE",
            Dispatch::RelationshipAdd(_) => "RELATIONSHIP_ADD",
            Dispatch::RelationshipRemove { .. } => "RELATIONSHIP_REMOVE",
            Dispatch::UserGuildSettingsUpdate(_) => "USER_GUILD_SETTINGS_UPDATE",
            Dispatch::MessageAck { .. } => "MESSAGE_ACK",
            Dispatch::ChannelUnreadUpdate { .. } => "CHANNEL_UNREAD_UPDATE",
            Dispatch::MessageCreate(_) => "MESSAGE_CREATE",
            Dispatch::MessageUpdate { .. } => "MESSAGE_UPDATE",
            Dispatch::MessageDelete { .. } => "MESSAGE_DELETE",
            Dispatch::MessageDeleteBulk { .. } => "MESSAGE_DELETE_BULK",
            Dispatch::MessageReactionAdd { .. } => "MESSAGE_REACTION_ADD",
            Dispatch::MessageReactionRemove { .. } => "MESSAGE_REACTION_REMOVE",
            Dispatch::MessageReactionRemoveAll { .. } => "MESSAGE_REACTION_REMOVE_ALL",
            Dispatch::MessageReactionRemoveEmoji { .. } => "MESSAGE_REACTION_REMOVE_EMOJI",
            Dispatch::TypingStart { .. } => "TYPING_START",
            Dispatch::SessionsReplace => "SESSIONS_REPLACE",
            Dispatch::Malformed { event } | Dispatch::Unknown { event } => event,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GuildDeletePayload {
    id: GuildId,
    #[serde(default)]
    unavailable: bool,
}

#[derive(Debug, Deserialize)]
struct RelationshipRemovePayload {
    id: UserId,
}

#[derive(Debug, Deserialize)]
struct MessageAckPayload {
    channel_id: ChannelId,
    #[serde(default, deserialize_with = "crate::discord::model::optional_id")]
    message_id: Option<MessageId>,
    #[serde(default)]
    mention_count: Option<u32>,
}

/// MESSAGE_UPDATE's envelope, read for its ids while the body stays JSON.
#[derive(Debug, Deserialize)]
struct MessageUpdatePayload {
    id: MessageId,
    #[serde(default)]
    channel_id: ChannelId,
}

#[derive(Debug, Deserialize)]
struct MessageDeletePayload {
    id: MessageId,
    channel_id: ChannelId,
    #[serde(default)]
    guild_id: Option<GuildId>,
}

#[derive(Debug, Deserialize)]
struct MessageDeleteBulkPayload {
    #[serde(default)]
    ids: Vec<MessageId>,
    channel_id: ChannelId,
    #[serde(default)]
    guild_id: Option<GuildId>,
}

#[derive(Debug, Deserialize)]
struct ReactionPayload {
    channel_id: ChannelId,
    message_id: MessageId,
    #[serde(default)]
    user_id: UserId,
    #[serde(default)]
    emoji: PartialEmoji,
}

#[derive(Debug, Deserialize)]
struct ReactionClearPayload {
    channel_id: ChannelId,
    message_id: MessageId,
    #[serde(default)]
    emoji: PartialEmoji,
}

#[derive(Debug, Deserialize)]
struct TypingStartPayload {
    channel_id: ChannelId,
    #[serde(default)]
    user_id: UserId,
}

#[derive(Debug, Deserialize)]
struct ChannelUnreadUpdatePayload {
    #[serde(default)]
    guild_id: Option<GuildId>,
    #[serde(default)]
    channel_unread_updates: Vec<ReadState>,
}

/// The READY_SUPPLEMENTAL read-state shape, which is the same `Collection` the
/// main READY uses. Declared so the type is referenced somewhere and a change
/// to it is a compile error rather than a silent divergence.
pub type ReadStates = Collection<ReadState>;

/// Decode a dispatch payload by its event name.
///
/// The `Box` on every variant is not ceremony: `Ready` alone is several hundred
/// bytes of struct before it points at megabytes of heap, and an enum is as
/// large as its largest variant. Every `Dispatch` that crosses a channel would
/// otherwise carry that.
pub fn decode(event: &str, payload: Option<&RawValue>) -> Dispatch {
    /// Parse, or say so and carry on.
    macro_rules! parse {
        ($ty:ty, $wrap:expr) => {
            match payload.map(|p| serde_json::from_str::<$ty>(p.get())) {
                Some(Ok(value)) => $wrap(value),
                Some(Err(e)) => {
                    tracing::warn!("{event} did not parse: {e}");
                    Dispatch::Malformed {
                        event: event.to_string(),
                    }
                }
                None => {
                    tracing::warn!("{event} arrived with no payload");
                    Dispatch::Malformed {
                        event: event.to_string(),
                    }
                }
            }
        };
    }

    match event {
        "READY" => parse!(Ready, |v| Dispatch::Ready(Box::new(v))),
        "READY_SUPPLEMENTAL" => {
            parse!(ReadySupplemental, |v| Dispatch::ReadySupplemental(
                Box::new(v)
            ))
        }
        "RESUMED" => Dispatch::Resumed,
        "GUILD_CREATE" => parse!(Guild, |v| Dispatch::GuildCreate(Box::new(v))),
        "GUILD_UPDATE" => parse!(Guild, |v| Dispatch::GuildUpdate(Box::new(v))),
        "GUILD_DELETE" => parse!(GuildDeletePayload, |v: GuildDeletePayload| {
            Dispatch::GuildDelete {
                id: v.id,
                unavailable: v.unavailable,
            }
        }),
        "CHANNEL_CREATE" => parse!(Channel, |v| Dispatch::ChannelCreate(Box::new(v))),
        "CHANNEL_UPDATE" => parse!(Channel, |v| Dispatch::ChannelUpdate(Box::new(v))),
        "CHANNEL_DELETE" => parse!(Channel, |v| Dispatch::ChannelDelete(Box::new(v))),
        "PRESENCE_UPDATE" => parse!(Presence, |v| Dispatch::PresenceUpdate(Box::new(v))),
        "USER_UPDATE" => parse!(User, |v| Dispatch::UserUpdate(Box::new(v))),
        "RELATIONSHIP_ADD" => parse!(Relationship, |v| Dispatch::RelationshipAdd(Box::new(v))),
        "RELATIONSHIP_UPDATE" => {
            parse!(Relationship, |v| Dispatch::RelationshipAdd(Box::new(v)))
        }
        "RELATIONSHIP_REMOVE" => {
            parse!(RelationshipRemovePayload, |v: RelationshipRemovePayload| {
                Dispatch::RelationshipRemove { id: v.id }
            })
        }
        "USER_GUILD_SETTINGS_UPDATE" => parse!(UserGuildSettings, |v| {
            Dispatch::UserGuildSettingsUpdate(Box::new(v))
        }),
        "MESSAGE_ACK" => parse!(MessageAckPayload, |v: MessageAckPayload| {
            Dispatch::MessageAck {
                channel_id: v.channel_id,
                message_id: v.message_id,
                mention_count: v.mention_count,
            }
        }),
        "CHANNEL_UNREAD_UPDATE" => {
            parse!(
                ChannelUnreadUpdatePayload,
                |v: ChannelUnreadUpdatePayload| {
                    Dispatch::ChannelUnreadUpdate {
                        guild_id: v.guild_id,
                        channel_unread_updates: v.channel_unread_updates,
                    }
                }
            )
        }
        "MESSAGE_CREATE" => parse!(Message, |v| Dispatch::MessageCreate(Box::new(v))),
        "MESSAGE_UPDATE" => {
            // Parsed twice on purpose: once for the ids, once as a value the
            // store merges field by field.
            match payload.map(|p| {
                serde_json::from_str::<MessageUpdatePayload>(p.get())
                    .and_then(|ids| serde_json::from_str(p.get()).map(|body| (ids, body)))
            }) {
                Some(Ok((ids, payload))) => Dispatch::MessageUpdate {
                    id: ids.id,
                    channel_id: ids.channel_id,
                    payload,
                },
                Some(Err(e)) => {
                    tracing::warn!("MESSAGE_UPDATE did not parse: {e}");
                    Dispatch::Malformed {
                        event: event.to_string(),
                    }
                }
                None => Dispatch::Malformed {
                    event: event.to_string(),
                },
            }
        }
        "MESSAGE_DELETE" => parse!(MessageDeletePayload, |v: MessageDeletePayload| {
            Dispatch::MessageDelete {
                id: v.id,
                channel_id: v.channel_id,
                guild_id: v.guild_id,
            }
        }),
        "MESSAGE_DELETE_BULK" => {
            parse!(MessageDeleteBulkPayload, |v: MessageDeleteBulkPayload| {
                Dispatch::MessageDeleteBulk {
                    ids: v.ids,
                    channel_id: v.channel_id,
                    guild_id: v.guild_id,
                }
            })
        }
        "MESSAGE_REACTION_ADD" => parse!(ReactionPayload, |v: ReactionPayload| {
            Dispatch::MessageReactionAdd {
                channel_id: v.channel_id,
                message_id: v.message_id,
                user_id: v.user_id,
                emoji: v.emoji,
            }
        }),
        "MESSAGE_REACTION_REMOVE" => parse!(ReactionPayload, |v: ReactionPayload| {
            Dispatch::MessageReactionRemove {
                channel_id: v.channel_id,
                message_id: v.message_id,
                user_id: v.user_id,
                emoji: v.emoji,
            }
        }),
        "MESSAGE_REACTION_REMOVE_ALL" => {
            parse!(ReactionClearPayload, |v: ReactionClearPayload| {
                Dispatch::MessageReactionRemoveAll {
                    channel_id: v.channel_id,
                    message_id: v.message_id,
                }
            })
        }
        "MESSAGE_REACTION_REMOVE_EMOJI" => {
            parse!(ReactionClearPayload, |v: ReactionClearPayload| {
                Dispatch::MessageReactionRemoveEmoji {
                    channel_id: v.channel_id,
                    message_id: v.message_id,
                    emoji: v.emoji,
                }
            })
        }
        "TYPING_START" => parse!(TypingStartPayload, |v: TypingStartPayload| {
            Dispatch::TypingStart {
                channel_id: v.channel_id,
                user_id: v.user_id,
            }
        }),
        "SESSIONS_REPLACE" => Dispatch::SessionsReplace,
        other => {
            tracing::trace!("ignoring {other}");
            Dispatch::Unknown {
                event: other.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(text: &str) -> Envelope<'_> {
        serde_json::from_str(text).expect("envelope")
    }

    #[test]
    fn an_envelope_leaves_its_payload_unparsed() {
        let text = r#"{"t":"READY","s":1,"op":0,"d":{"anything":[1,2,3]}}"#;
        let env = envelope(text);
        assert_eq!(env.opcode(), OpCode::Dispatch);
        assert_eq!(env.s, Some(1));
        assert_eq!(env.t, Some("READY"));
        assert_eq!(env.d.unwrap().get(), r#"{"anything":[1,2,3]}"#);
    }

    #[test]
    fn hello_and_the_control_opcodes_have_no_event_name() {
        let hello = envelope(r#"{"t":null,"s":null,"op":10,"d":{"heartbeat_interval":41250}}"#);
        assert_eq!(hello.opcode(), OpCode::Hello);
        assert!(hello.t.is_none());
        let payload: Hello = serde_json::from_str(hello.d.unwrap().get()).unwrap();
        assert_eq!(payload.heartbeat_interval, 41250);

        assert_eq!(envelope(r#"{"op":11}"#).opcode(), OpCode::HeartbeatAck);
        assert_eq!(envelope(r#"{"op":7,"d":null}"#).opcode(), OpCode::Reconnect);
        assert_eq!(
            envelope(r#"{"op":9,"d":false}"#).opcode(),
            OpCode::InvalidSession
        );
    }

    #[test]
    fn every_opcode_round_trips() {
        for code in [0u8, 1, 2, 3, 6, 7, 9, 10, 11, 14, 37, 99] {
            assert_eq!(OpCode::from_code(code).code(), code);
        }
        assert_eq!(OpCode::from_code(99), OpCode::Unknown(99));
    }

    #[test]
    fn an_unknown_event_is_not_an_error() {
        let d: Dispatch = decode("CALL_CREATE", Some(&raw(r#"{"channel_id":"1"}"#)));
        assert!(matches!(d, Dispatch::Unknown { .. }));
        assert_eq!(d.name(), "CALL_CREATE");
    }

    #[test]
    fn a_known_event_with_a_broken_payload_is_dropped_not_fatal() {
        let d = decode("CHANNEL_CREATE", Some(&raw(r#"{"id":"not a number"}"#)));
        assert!(matches!(d, Dispatch::Malformed { .. }), "{}", d.name());

        let missing = decode("GUILD_CREATE", None);
        assert!(matches!(missing, Dispatch::Malformed { .. }));
    }

    #[test]
    fn the_events_m1_acts_on_decode() {
        let ready = decode(
            "READY",
            Some(&raw(
                r#"{"user":{"id":"1","username":"me"},"session_id":"s"}"#,
            )),
        );
        match ready {
            Dispatch::Ready(r) => assert_eq!(r.session_id, "s"),
            other => panic!("{}", other.name()),
        }

        let guild = decode("GUILD_CREATE", Some(&raw(r#"{"id":"2","name":"G"}"#)));
        assert!(matches!(guild, Dispatch::GuildCreate(_)));

        let gone = decode(
            "GUILD_DELETE",
            Some(&raw(r#"{"id":"2","unavailable":true}"#)),
        );
        match gone {
            Dispatch::GuildDelete { id, unavailable } => {
                assert_eq!(id, GuildId(2));
                assert!(unavailable, "an outage is not a departure");
            }
            other => panic!("{}", other.name()),
        }

        let channel = decode("CHANNEL_UPDATE", Some(&raw(r#"{"id":"3","type":0}"#)));
        assert!(matches!(channel, Dispatch::ChannelUpdate(_)));

        let presence = decode(
            "PRESENCE_UPDATE",
            Some(&raw(r#"{"user":{"id":"4"},"status":"idle"}"#)),
        );
        assert!(matches!(presence, Dispatch::PresenceUpdate(_)));

        let user = decode("USER_UPDATE", Some(&raw(r#"{"id":"1","username":"me2"}"#)));
        assert!(matches!(user, Dispatch::UserUpdate(_)));

        let rel = decode("RELATIONSHIP_ADD", Some(&raw(r#"{"id":"5","type":1}"#)));
        assert!(matches!(rel, Dispatch::RelationshipAdd(_)));

        let unrel = decode("RELATIONSHIP_REMOVE", Some(&raw(r#"{"id":"5","type":1}"#)));
        assert!(matches!(
            unrel,
            Dispatch::RelationshipRemove { id } if id == UserId(5)
        ));

        let settings = decode(
            "USER_GUILD_SETTINGS_UPDATE",
            Some(&raw(r#"{"guild_id":"2","muted":true}"#)),
        );
        assert!(matches!(settings, Dispatch::UserGuildSettingsUpdate(_)));

        let ack = decode(
            "MESSAGE_ACK",
            Some(&raw(
                r#"{"channel_id":"6","message_id":"7","mention_count":0}"#,
            )),
        );
        match ack {
            Dispatch::MessageAck {
                channel_id,
                message_id,
                mention_count,
            } => {
                assert_eq!(channel_id, ChannelId(6));
                assert_eq!(message_id, Some(MessageId(7)));
                assert_eq!(mention_count, Some(0));
            }
            other => panic!("{}", other.name()),
        }

        let unread = decode(
            "CHANNEL_UNREAD_UPDATE",
            Some(&raw(
                r#"{"guild_id":"2","channel_unread_updates":[{"id":"6","last_message_id":"7"}]}"#,
            )),
        );
        match unread {
            Dispatch::ChannelUnreadUpdate {
                guild_id,
                channel_unread_updates,
            } => {
                assert_eq!(guild_id, Some(GuildId(2)));
                assert_eq!(channel_unread_updates.len(), 1);
            }
            other => panic!("{}", other.name()),
        }

        assert!(matches!(decode("RESUMED", None), Dispatch::Resumed));
    }

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_string()).unwrap()
    }

    proptest::proptest! {
        /// Bytes off a socket, again. Any string is a possible event name and
        /// any JSON is a possible payload.
        #[test]
        fn decoding_never_panics(
            event in "[A-Z_]{0,32}",
            payload in r#"\{("[a-z]{1,4}":(null|1|"x"|\[\]|\{\})){0,3}\}"#,
        ) {
            if let Ok(value) = RawValue::from_string(payload) {
                let _ = decode(&event, Some(&value));
            }
        }

        #[test]
        fn an_envelope_is_parsed_or_refused_but_never_panics(text in ".{0,200}") {
            let _ = serde_json::from_str::<Envelope<'_>>(&text);
        }
    }
}
