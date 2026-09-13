//! What this client sends up the socket.
//!
//! IDENTIFY is the one payload whose exact contents matter beyond correctness.
//! It is what Discord's own heuristics read, so it carries the same properties
//! object as the `X-Super-Properties` header and nothing that a browser would
//! not send: presence `online`, no activities, no Rich Presence, and a
//! capability set chosen for what this client actually reads rather than for
//! everything it is allowed to ask for.
//!
//! `capabilities` is the part worth reviewing. It changes the *shape* of READY,
//! which is why `model/` supports both shapes of several fields: turning a
//! capability on here changes what arrives, and the first recorded READY from a
//! real account is what settles which branch is live.

use std::fmt;

use serde::{Serialize, Serializer};

use crate::discord::auth::Token;
use crate::discord::model::PresenceStatus;
use crate::discord::props::SuperProperties;
use crate::discord::snowflake::{ChannelId, GuildId};

bitflags::bitflags! {
    /// The `capabilities` bitfield.
    ///
    /// Named rather than written as a literal because the number is meaningless
    /// on its own and because a review of "why is READY this shape" starts here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Capabilities: u64 {
        /// User notes arrive on demand rather than all of them in READY.
        const LAZY_USER_NOTES = 1 << 0;
        const NO_AFFINE_USER_IDS = 1 << 1;
        /// `read_state` arrives as `{version, partial, entries}` rather than a
        /// bare array, and can then be sent as a delta.
        const VERSIONED_READ_STATES = 1 << 2;
        /// The same for `user_guild_settings`.
        const VERSIONED_USER_GUILD_SETTINGS = 1 << 3;
        /// User objects appear once in READY's `users` and are referenced by id
        /// everywhere else. On an account with many DMs this is most of the
        /// payload.
        const DEDUPE_USER_OBJECTS = 1 << 4;
        /// READY carries what is needed to draw a first frame and
        /// READY_SUPPLEMENTAL carries the rest.
        const PRIORITIZED_READY_PAYLOAD = 1 << 5;
        const MULTIPLE_GUILD_EXPERIMENT_POPULATIONS = 1 << 6;
        const NON_CHANNEL_READ_STATES = 1 << 7;
        const AUTH_TOKEN_REFRESH = 1 << 8;
        const USER_SETTINGS_PROTO = 1 << 9;
        const CLIENT_STATE_V2 = 1 << 10;
        const PASSIVE_GUILD_UPDATE = 1 << 11;
        const AUTO_CALL_CONNECT = 1 << 12;
        const DEBOUNCE_MESSAGE_REACTIONS = 1 << 13;
        const PASSIVE_GUILD_UPDATE_V2 = 1 << 14;
    }
}

impl Capabilities {
    /// What this client asks for.
    ///
    /// Four, and each one is here because something reads the result:
    /// deduplicated users because DMs are most of the payload, prioritised
    /// READY because the first frame should not wait on the whole account,
    /// versioned read states because unread marks are derived from them, and
    /// lazy notes because notes are never shown.
    ///
    /// Everything else is off. A capability that changes a payload nothing
    /// parses is a shape to support for no benefit.
    pub fn client() -> Self {
        Capabilities::LAZY_USER_NOTES
            | Capabilities::VERSIONED_READ_STATES
            | Capabilities::DEDUPE_USER_OBJECTS
            | Capabilities::PRIORITIZED_READY_PAYLOAD
    }
}

impl Serialize for Capabilities {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.bits())
    }
}

/// The presence IDENTIFY announces.
#[derive(Debug, Clone, Serialize)]
pub struct IdentifyPresence {
    pub status: &'static str,
    /// Always empty. This client does not publish what anybody is doing.
    pub activities: Vec<()>,
    /// Milliseconds since the client went idle; 0 means "not idle".
    pub since: u64,
    pub afk: bool,
}

impl IdentifyPresence {
    pub fn new(status: PresenceStatus) -> Self {
        Self {
            status: status.as_str(),
            activities: Vec::new(),
            since: 0,
            afk: false,
        }
    }
}

/// `client_state`, which tells the gateway what this client already knows.
///
/// Empty on a fresh IDENTIFY: nothing is cached across connections, so asking
/// for deltas against a cache that does not exist would be asking for a payload
/// that cannot be applied.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientState {
    pub guild_versions: std::collections::BTreeMap<String, u64>,
}

/// op 2.
#[derive(Serialize)]
pub struct Identify<'a> {
    #[serde(serialize_with = "expose_token")]
    pub token: &'a Token,
    pub capabilities: Capabilities,
    pub properties: &'a SuperProperties,
    pub presence: IdentifyPresence,
    /// Always false: the *connection* is compressed with `zlib-stream`, and
    /// asking for per-message compression on top of that compresses nothing
    /// twice.
    pub compress: bool,
    pub client_state: ClientState,
}

/// The only place a token is written into a payload, and it does not go through
/// `Debug`.
fn expose_token<S: Serializer>(token: &&Token, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(token.expose())
}

/// Redacted by hand, because deriving `Debug` here would put a live session
/// token in the log the first time somebody traced an IDENTIFY.
impl fmt::Debug for Identify<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identify")
            .field("token", &self.token)
            .field("capabilities", &self.capabilities)
            .field("properties", &self.properties)
            .field("presence", &self.presence)
            .field("compress", &self.compress)
            .finish()
    }
}

/// op 6.
#[derive(Serialize)]
pub struct Resume<'a> {
    #[serde(serialize_with = "expose_token")]
    pub token: &'a Token,
    pub session_id: &'a str,
    pub seq: u64,
}

impl fmt::Debug for Resume<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resume")
            .field("token", &self.token)
            .field("session_id", &self.session_id)
            .field("seq", &self.seq)
            .finish()
    }
}

/// op 37, the member-list subscription the web client sends today.
///
/// op 14 is the older spelling of the same request. Which one a user-account
/// session is expected to send has to be confirmed against a live connection,
/// so both are reachable and the choice is one boolean rather than an edit.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateGuildSubscriptions {
    pub guild_id: GuildId,
    pub typing: bool,
    pub threads: bool,
    pub activities: bool,
    /// Channel id → the ranges of the member list to receive, as `[start, end]`
    /// pairs. Three ranges of a hundred is the documented maximum.
    pub channels: std::collections::BTreeMap<String, Vec<[u32; 2]>>,
}

impl UpdateGuildSubscriptions {
    /// Subscribe to one channel's member list.
    pub fn for_channel(guild: GuildId, channel: ChannelId, ranges: &[(u32, u32)]) -> Self {
        let mut channels = std::collections::BTreeMap::new();
        channels.insert(
            channel.to_string(),
            ranges.iter().map(|&(a, b)| [a, b]).collect(),
        );
        Self {
            guild_id: guild,
            typing: true,
            threads: false,
            activities: true,
            channels,
        }
    }
}

/// Wrap a payload in its opcode.
#[derive(Debug, Serialize)]
pub struct Outgoing<T> {
    pub op: u8,
    pub d: T,
}

impl<T> Outgoing<T> {
    pub fn new(op: super::payload::OpCode, d: T) -> Self {
        Self { op: op.code(), d }
    }
}

/// op 1. The sequence number, or `null` before the first dispatch.
#[derive(Debug, Serialize)]
pub struct Heartbeat(pub Option<u64>);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::gateway::payload::OpCode;
    use crate::discord::props::ClientProps;

    fn token() -> Token {
        Token::new("mfa.aVeryRealLookingSecret").unwrap()
    }

    #[test]
    fn the_capability_set_is_the_four_that_are_read() {
        let caps = Capabilities::client();
        assert!(caps.contains(Capabilities::DEDUPE_USER_OBJECTS));
        assert!(caps.contains(Capabilities::VERSIONED_READ_STATES));
        assert!(caps.contains(Capabilities::PRIORITIZED_READY_PAYLOAD));
        assert!(caps.contains(Capabilities::LAZY_USER_NOTES));
        assert_eq!(
            caps.bits(),
            53,
            "the capability bits changed, so the READY shape changed; re-record \
             testdata/gateway/ready.json before assuming otherwise"
        );
    }

    #[test]
    fn identify_carries_the_same_properties_as_the_header() {
        let props = ClientProps::new("en-US", 611_316);
        let token = token();
        let identify = Identify {
            token: &token,
            capabilities: Capabilities::client(),
            properties: props.identify_properties(),
            presence: IdentifyPresence::new(PresenceStatus::Online),
            compress: false,
            client_state: ClientState::default(),
        };

        let value = serde_json::to_value(Outgoing::new(OpCode::Identify, &identify)).unwrap();
        assert_eq!(value["op"], 2);
        assert_eq!(value["d"]["capabilities"], 53);
        assert_eq!(value["d"]["compress"], false);
        assert_eq!(value["d"]["presence"]["status"], "online");
        assert_eq!(
            value["d"]["presence"]["activities"],
            serde_json::json!([]),
            "this client never publishes an activity"
        );
        assert_eq!(
            value["d"]["properties"]["browser_user_agent"],
            props.user_agent()
        );
        assert_eq!(value["d"]["properties"]["client_build_number"], 611_316);
        assert_eq!(
            value["d"]["client_state"]["guild_versions"],
            serde_json::json!({})
        );
    }

    /// The token is in the payload, on purpose, and nowhere else.
    #[test]
    fn identify_serialises_the_token_but_never_prints_it() {
        let token = token();
        let props = ClientProps::default();
        let identify = Identify {
            token: &token,
            capabilities: Capabilities::client(),
            properties: props.identify_properties(),
            presence: IdentifyPresence::new(PresenceStatus::Online),
            compress: false,
            client_state: ClientState::default(),
        };
        let json = serde_json::to_string(&identify).unwrap();
        assert!(json.contains("mfa.aVeryRealLookingSecret"));

        let printed = format!("{identify:?}");
        assert!(
            !printed.contains("aVeryRealLookingSecret"),
            "IDENTIFY printed a live token: {printed}"
        );
        assert!(printed.contains("Token(<redacted>)"));
    }

    #[test]
    fn resume_is_the_session_and_the_sequence_and_nothing_else() {
        let token = token();
        let resume = Resume {
            token: &token,
            session_id: "abc",
            seq: 42,
        };
        let value = serde_json::to_value(Outgoing::new(OpCode::Resume, &resume)).unwrap();
        assert_eq!(value["op"], 6);
        assert_eq!(value["d"]["session_id"], "abc");
        assert_eq!(value["d"]["seq"], 42);
        assert!(!format!("{resume:?}").contains("aVeryRealLookingSecret"));
    }

    #[test]
    fn a_heartbeat_is_the_sequence_or_null() {
        let first =
            serde_json::to_value(Outgoing::new(OpCode::Heartbeat, Heartbeat(None))).unwrap();
        assert_eq!(first, serde_json::json!({"op": 1, "d": null}));
        let later =
            serde_json::to_value(Outgoing::new(OpCode::Heartbeat, Heartbeat(Some(7)))).unwrap();
        assert_eq!(later, serde_json::json!({"op": 1, "d": 7}));
    }

    #[test]
    fn a_member_list_subscription_is_op_37() {
        let sub =
            UpdateGuildSubscriptions::for_channel(GuildId(1), ChannelId(2), &[(0, 99), (100, 199)]);
        let value =
            serde_json::to_value(Outgoing::new(OpCode::UpdateGuildSubscriptions, &sub)).unwrap();
        assert_eq!(value["op"], 37);
        assert_eq!(value["d"]["guild_id"], "1");
        assert_eq!(
            value["d"]["channels"]["2"],
            serde_json::json!([[0, 99], [100, 199]])
        );
    }
}
