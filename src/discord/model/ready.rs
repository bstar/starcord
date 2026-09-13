//! READY, which is the whole account in one message.
//!
//! It arrives once per connection and carries every guild, every DM, every read
//! state and every relationship. It is also the largest thing this client ever
//! parses — tens of megabytes for a well-joined account — which is why the
//! gateway hands it to `spawn_blocking` rather than parsing it on a runtime
//! worker.
//!
//! READY_SUPPLEMENTAL follows it and carries what READY left out: presences and
//! the private channels the prioritised payload deferred.

use serde::Deserialize;

use crate::discord::model::channel::Channel;
use crate::discord::model::guild::Guild;
use crate::discord::model::presence::Presence;
use crate::discord::model::read_state::{ReadState, UserGuildSettings};
use crate::discord::model::user::User;
use crate::discord::model::Collection;
use crate::discord::snowflake::{GuildId, RoleId, UserId};

#[derive(Debug, Clone, Deserialize)]
pub struct Ready {
    #[serde(default)]
    pub v: u8,
    pub user: User,
    pub session_id: String,
    /// Where to reconnect to when resuming. Absent on old gateway versions, in
    /// which case the ordinary gateway URL is used.
    #[serde(default)]
    pub resume_gateway_url: Option<String>,
    #[serde(default)]
    pub guilds: Vec<Guild>,
    #[serde(default)]
    pub private_channels: Vec<Channel>,
    #[serde(default)]
    pub relationships: Vec<Relationship>,
    #[serde(default)]
    pub read_state: Collection<ReadState>,
    #[serde(default)]
    pub user_guild_settings: Collection<UserGuildSettings>,
    /// Under `DEDUPE_USER_OBJECTS`, every user mentioned anywhere in the
    /// payload appears here once and nowhere else.
    #[serde(default)]
    pub users: Vec<User>,
    /// One list per guild, in the same order as `guilds`. These are the
    /// session's own memberships, not the guild's members.
    #[serde(default)]
    pub merged_members: Vec<Vec<Member>>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub session_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReadySupplemental {
    #[serde(default)]
    pub merged_presences: MergedPresences,
    /// DMs the prioritised READY deferred.
    #[serde(default)]
    pub lazy_private_channels: Vec<Channel>,
    #[serde(default)]
    pub guilds: Vec<SupplementalGuild>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MergedPresences {
    #[serde(default)]
    pub friends: Vec<Presence>,
    /// One list per guild, in `guilds` order.
    #[serde(default)]
    pub guilds: Vec<Vec<Presence>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SupplementalGuild {
    #[serde(default)]
    pub id: Option<GuildId>,
}

/// A membership. In `merged_members` the user is a bare id, because the user
/// objects live in READY's `users`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Member {
    #[serde(default)]
    pub user_id: Option<UserId>,
    #[serde(default)]
    pub user: Option<User>,
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

impl Member {
    /// The account this membership belongs to, from whichever spelling arrived.
    pub fn user_id(&self) -> Option<UserId> {
        self.user_id.or_else(|| self.user.as_ref().map(|u| u.id))
    }
}

/// A relationship: friend, block, or a pending request in either direction.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Relationship {
    /// The other account's id. Discord reuses `id` for it rather than sending
    /// a relationship id of its own.
    pub id: UserId,
    #[serde(rename = "type", default)]
    pub kind: RelationshipKind,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub nickname: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelationshipKind {
    #[default]
    None,
    Friend,
    Blocked,
    /// They asked; the session has not answered.
    IncomingRequest,
    /// The session asked; they have not answered.
    OutgoingRequest,
    Implicit,
    Unknown(u8),
}

impl RelationshipKind {
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => RelationshipKind::None,
            1 => RelationshipKind::Friend,
            2 => RelationshipKind::Blocked,
            3 => RelationshipKind::IncomingRequest,
            4 => RelationshipKind::OutgoingRequest,
            5 => RelationshipKind::Implicit,
            n => RelationshipKind::Unknown(n),
        }
    }

    /// Whether a DM may be opened with this person.
    ///
    /// This is the only place the friends-only rule is expressed, and
    /// `Command::OpenDm` refuses everything else. Messaging a stranger out of a
    /// terminal client is exactly the automated behaviour this project promises
    /// not to have.
    pub const fn can_dm(self) -> bool {
        matches!(self, RelationshipKind::Friend)
    }
}

impl<'de> Deserialize<'de> for RelationshipKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u8::deserialize(d).map(RelationshipKind::from_code)
    }
}

/// One of the account's live sessions, including this one.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub active: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::ChannelId;

    const MINIMAL: &str = r#"{
        "v": 9,
        "user": {"id":"1","username":"me","global_name":"Me","discriminator":"0"},
        "session_id": "abc",
        "resume_gateway_url": "wss://gateway-us-east1-b.discord.gg",
        "guilds": [{"id":"100","name":"G","channels":[{"id":"200","type":0,"name":"general"}]}],
        "private_channels": [{"id":"300","type":1,"recipient_ids":["2"]}],
        "relationships": [{"id":"2","type":1}],
        "read_state": {"version":1,"partial":false,"entries":[{"id":"200","last_message_id":"9"}]},
        "user_guild_settings": {"version":1,"partial":false,"entries":[{"guild_id":"100","muted":false}]},
        "users": [{"id":"2","username":"friend","discriminator":"0"}],
        "merged_members": [[{"user_id":"1","roles":[]}]],
        "sessions": []
    }"#;

    #[test]
    fn a_minimal_ready_parses() {
        let ready: Ready = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(ready.user.display_name(), "Me");
        assert_eq!(ready.session_id, "abc");
        assert_eq!(ready.guilds.len(), 1);
        assert_eq!(ready.private_channels[0].id, ChannelId(300));
        assert_eq!(ready.read_state.entries().len(), 1);
        assert_eq!(ready.merged_members[0][0].user_id(), Some(UserId(1)));
        assert_eq!(ready.relationships[0].kind, RelationshipKind::Friend);
        assert!(ready.relationships[0].kind.can_dm());
    }

    /// Only the user and the session id are load-bearing; everything else is a
    /// list this client can render as empty.
    #[test]
    fn the_smallest_ready_that_is_still_a_ready() {
        let ready: Ready = serde_json::from_str(r#"{"user":{"id":"1"},"session_id":"s"}"#).unwrap();
        assert!(ready.guilds.is_empty());
        assert!(ready.resume_gateway_url.is_none());
    }

    #[test]
    fn a_ready_without_a_session_id_is_refused() {
        assert!(serde_json::from_str::<Ready>(r#"{"user":{"id":"1"}}"#).is_err());
        assert!(serde_json::from_str::<Ready>(r#"{"session_id":"s"}"#).is_err());
    }

    #[test]
    fn a_relationship_kind_nobody_has_heard_of_cannot_be_dmed() {
        let r: Relationship = serde_json::from_str(r#"{"id":"2","type":99}"#).unwrap();
        assert_eq!(r.kind, RelationshipKind::Unknown(99));
        assert!(!r.kind.can_dm());
        assert!(!RelationshipKind::Blocked.can_dm());
        assert!(!RelationshipKind::IncomingRequest.can_dm());
    }

    #[test]
    fn a_supplemental_carries_presences_per_guild() {
        let supp: ReadySupplemental = serde_json::from_str(
            r#"{"merged_presences":{"friends":[{"user":{"id":"2"},"status":"online"}],"guilds":[[]]},"lazy_private_channels":[],"guilds":[{"id":"100"}]}"#,
        )
        .unwrap();
        assert_eq!(supp.merged_presences.friends.len(), 1);
        assert_eq!(supp.merged_presences.guilds.len(), 1);
        assert_eq!(supp.guilds[0].id, Some(GuildId(100)));
    }

    proptest::proptest! {
        /// Delete a key or swap a type anywhere in a real payload: the result
        /// is a parsed READY or a serde error, never a panic and never a hang.
        #[test]
        fn a_mutated_ready_is_never_a_panic(
            index in 0usize..64,
            mutation in 0u8..3,
        ) {
            let mut value: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
            mutate(&mut value, index, mutation);
            let _ = serde_json::from_value::<Ready>(value);
        }
    }

    /// Walk to the `index`-th leaf in document order and break it.
    #[cfg(test)]
    fn mutate(value: &mut serde_json::Value, index: usize, mutation: u8) {
        let mut seen = 0usize;
        fn walk(v: &mut serde_json::Value, seen: &mut usize, target: usize, mutation: u8) -> bool {
            match v {
                serde_json::Value::Object(map) => {
                    let keys: Vec<String> = map.keys().cloned().collect();
                    for key in keys {
                        if *seen == target {
                            match mutation {
                                0 => {
                                    map.remove(&key);
                                }
                                1 => {
                                    map.insert(key, serde_json::Value::Null);
                                }
                                _ => {
                                    map.insert(key, serde_json::json!({"unexpected": true}));
                                }
                            }
                            return true;
                        }
                        *seen += 1;
                        if let Some(child) = map.get_mut(&key) {
                            if walk(child, seen, target, mutation) {
                                return true;
                            }
                        }
                    }
                    false
                }
                serde_json::Value::Array(items) => {
                    for item in items.iter_mut() {
                        if walk(item, seen, target, mutation) {
                            return true;
                        }
                    }
                    false
                }
                _ => false,
            }
        }
        walk(value, &mut seen, index, mutation);
    }
}
