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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::time::{Duration, Instant};

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
    pub guild_versions: BTreeMap<String, u64>,
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
/// op 14 is the older spelling of the same request with the same body. Which
/// one a user-account session is expected to send has to be confirmed against a
/// live connection, so both are reachable and the choice is one boolean rather
/// than an edit.
///
/// The flags are what this client actually reads. `typing` is on because the
/// chat panel shows who is typing. `threads` is on because a thread created
/// under an open channel should appear without a reconnect. `activities` is
/// **off**: it is a presence firehose for a whole guild, and nothing here draws
/// what anybody is playing.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateGuildSubscriptions {
    pub guild_id: GuildId,
    pub typing: bool,
    pub threads: bool,
    pub activities: bool,
    /// Channel id → the ranges of the member list to receive, as `[start, end]`
    /// pairs. Three ranges of a hundred is the documented maximum. An empty map
    /// is how a subscription is dropped.
    pub channels: BTreeMap<String, Vec<[u32; 2]>>,
}

impl UpdateGuildSubscriptions {
    /// Subscribe to one channel's member list.
    pub fn for_channel(guild: GuildId, channel: ChannelId, ranges: &[(u32, u32)]) -> Self {
        let mut channels = BTreeMap::new();
        channels.insert(
            channel.to_string(),
            ranges.iter().map(|&(a, b)| [a, b]).collect(),
        );
        Self::for_channels(guild, channels)
    }

    pub fn for_channels(guild: GuildId, channels: BTreeMap<String, Vec<[u32; 2]>>) -> Self {
        Self {
            guild_id: guild,
            typing: true,
            threads: true,
            activities: false,
            channels,
        }
    }

    /// Stop receiving anything for a guild.
    pub fn none(guild: GuildId) -> Self {
        Self {
            guild_id: guild,
            typing: false,
            threads: false,
            activities: false,
            channels: BTreeMap::new(),
        }
    }
}

/// The first window of a member list, which is what one open channel asks for.
pub const FIRST_RANGE: [u32; 2] = [0, 99];

/// How long a guild keeps its subscription after the last channel in it closes.
///
/// Thirty seconds, because clicking between two channels in the same server is
/// the common case and unsubscribing and resubscribing across it would be two
/// payloads for nothing. The grace is also why leaving is scheduled rather than
/// sent: coming back inside it costs nothing at all.
pub const UNSUBSCRIBE_GRACE: Duration = Duration::from_secs(30);

/// What this client is subscribed to, and what it has actually told the gateway.
///
/// The two are separate on purpose. Discord's member-list subscriptions are
/// cheap to hold and expensive to churn, and a client that re-sends the same
/// ranges every time the user clicks a channel is a client generating traffic
/// that says nothing. Every method here returns a payload only when what the
/// gateway believes differs from what is wanted.
pub struct Subscriptions {
    /// Channels open per guild, which is what the ranges are derived from.
    open: HashMap<GuildId, BTreeSet<ChannelId>>,
    /// The member-list windows asked for, per channel. A channel with no entry
    /// gets [`FIRST_RANGE`], which is what one open channel needs before
    /// anybody has scrolled the list.
    ranges: HashMap<ChannelId, Vec<[u32; 2]>>,
    /// What was last sent, per guild, so an unchanged request is not re-sent.
    sent: HashMap<GuildId, BTreeMap<String, Vec<[u32; 2]>>>,
    /// Guilds with nothing open, and when their grace runs out.
    leaving: HashMap<GuildId, Instant>,
    legacy: bool,
}

impl Subscriptions {
    /// `legacy` sends op 14 rather than op 37, for a session where the newer
    /// opcode turns out not to be accepted.
    pub fn new(legacy: bool) -> Self {
        Self {
            open: HashMap::new(),
            ranges: HashMap::new(),
            sent: HashMap::new(),
            leaving: HashMap::new(),
            legacy,
        }
    }

    /// The opcode in use, which `starcord probe` prints so that a live session
    /// can settle which one Discord accepts.
    pub fn opcode(&self) -> super::payload::OpCode {
        if self.legacy {
            super::payload::OpCode::LazyRequest
        } else {
            super::payload::OpCode::UpdateGuildSubscriptions
        }
    }

    /// A word for a report.
    pub fn describe(&self) -> &'static str {
        if self.legacy {
            "op 14 (legacy lazy request)"
        } else {
            "op 37 (update guild subscriptions)"
        }
    }

    fn wanted(&self, guild: GuildId) -> BTreeMap<String, Vec<[u32; 2]>> {
        self.open
            .get(&guild)
            .map(|channels| {
                channels
                    .iter()
                    .map(|c| {
                        let ranges = self
                            .ranges
                            .get(c)
                            .cloned()
                            .unwrap_or_else(|| vec![FIRST_RANGE]);
                        (c.to_string(), ranges)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ask for a different window of a channel's member list.
    ///
    /// `Command::RequestMembers`, which the members panel sends as it scrolls.
    /// Nothing goes out unless the ranges actually changed: the panel may call
    /// it on every frame, and re-sending the same subscription is traffic that
    /// says nothing.
    ///
    /// The ranges are clamped before they get here — three windows of a
    /// hundred, which is Discord's limit — because asking for more is not
    /// refused, it is ignored, and an ignored subscription looks exactly like
    /// one that was accepted and then never delivered.
    pub fn set_ranges(
        &mut self,
        guild: GuildId,
        channel: ChannelId,
        ranges: &[(u32, u32)],
    ) -> Option<String> {
        let wanted: Vec<[u32; 2]> = if ranges.is_empty() {
            vec![FIRST_RANGE]
        } else {
            ranges.iter().map(|&(a, b)| [a, b]).collect()
        };

        if self.ranges.get(&channel) == Some(&wanted) {
            return None;
        }
        self.ranges.insert(channel, wanted);
        // Asking for a member list is also opening the channel as far as the
        // subscription is concerned: there is no window without one.
        self.leaving.remove(&guild);
        self.open.entry(guild).or_default().insert(channel);
        self.sync(guild)
    }

    /// What was last asked for, for a test and for a report.
    pub fn ranges_of(&self, channel: ChannelId) -> Option<&[[u32; 2]]> {
        self.ranges.get(&channel).map(Vec::as_slice)
    }

    /// Send the guild's current subscription, if it differs from the last one.
    fn sync(&mut self, guild: GuildId) -> Option<String> {
        let wanted = self.wanted(guild);
        if self.sent.get(&guild) == Some(&wanted) {
            return None;
        }
        let payload = serde_json::to_string(&Outgoing::new(
            self.opcode(),
            UpdateGuildSubscriptions::for_channels(guild, wanted.clone()),
        ))
        .ok()?;
        self.sent.insert(guild, wanted);
        Some(payload)
    }

    /// A channel was opened. Returns the payload to send, if anything changed.
    pub fn open(&mut self, guild: GuildId, channel: ChannelId) -> Option<String> {
        // Coming back inside the grace period cancels the departure.
        self.leaving.remove(&guild);
        self.open.entry(guild).or_default().insert(channel);
        self.sync(guild)
    }

    /// A channel was closed.
    ///
    /// Closing the last channel in a guild schedules the unsubscribe rather
    /// than sending it; see [`UNSUBSCRIBE_GRACE`].
    pub fn close(&mut self, guild: GuildId, channel: ChannelId, now: Instant) -> Option<String> {
        let channels = self.open.get_mut(&guild)?;
        channels.remove(&channel);
        if !channels.is_empty() {
            return self.sync(guild);
        }
        self.open.remove(&guild);
        self.leaving.insert(guild, now + UNSUBSCRIBE_GRACE);
        self.ranges.remove(&channel);
        None
    }

    /// Unsubscribe payloads whose grace has run out.
    pub fn due(&mut self, now: Instant) -> Vec<String> {
        let expired: Vec<GuildId> = self
            .leaving
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(guild, _)| *guild)
            .collect();

        let mut payloads = Vec::new();
        for guild in expired {
            self.leaving.remove(&guild);
            if self.open.contains_key(&guild) {
                // Something reopened while the timer was running.
                continue;
            }
            if self.sent.remove(&guild).is_none() {
                continue;
            }
            if let Ok(payload) = serde_json::to_string(&Outgoing::new(
                self.opcode(),
                UpdateGuildSubscriptions::none(guild),
            )) {
                payloads.push(payload);
            }
        }
        payloads
    }

    /// Everything goes: the socket was lost, so the gateway remembers nothing
    /// and the next open must re-send.
    pub fn forget_connection(&mut self) {
        self.sent.clear();
    }

    /// Re-send every live subscription, for after a reconnect.
    pub fn resend_all(&mut self) -> Vec<String> {
        self.forget_connection();
        let guilds: Vec<GuildId> = self.open.keys().copied().collect();
        guilds.into_iter().filter_map(|g| self.sync(g)).collect()
    }

    /// How many guilds are subscribed, for a report.
    pub fn guilds(&self) -> usize {
        self.sent.len()
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
        assert_eq!(value["d"]["typing"], true);
        assert_eq!(value["d"]["threads"], true);
        assert_eq!(
            value["d"]["activities"], false,
            "activities is a presence firehose for a whole guild"
        );
    }

    fn body(payload: &str) -> serde_json::Value {
        serde_json::from_str(payload).expect("a subscription payload is json")
    }

    #[test]
    fn opening_a_channel_subscribes_and_opening_it_again_does_not() {
        let mut subs = Subscriptions::new(false);
        let payload = subs
            .open(GuildId(1), ChannelId(2))
            .expect("the first open sends");
        let value = body(&payload);
        assert_eq!(value["op"], 37);
        assert_eq!(value["d"]["guild_id"], "1");
        assert_eq!(value["d"]["channels"]["2"], serde_json::json!([[0, 99]]));

        assert!(
            subs.open(GuildId(1), ChannelId(2)).is_none(),
            "the same ranges were sent twice"
        );
        assert_eq!(subs.guilds(), 1);
    }

    #[test]
    fn a_second_channel_in_the_same_guild_is_one_merged_subscription() {
        let mut subs = Subscriptions::new(false);
        subs.open(GuildId(1), ChannelId(2));
        let payload = subs
            .open(GuildId(1), ChannelId(3))
            .expect("the ranges changed");
        let value = body(&payload);
        assert_eq!(value["d"]["channels"]["2"], serde_json::json!([[0, 99]]));
        assert_eq!(value["d"]["channels"]["3"], serde_json::json!([[0, 99]]));
        assert_eq!(
            subs.guilds(),
            1,
            "two channels in one guild are one subscription"
        );
    }

    #[test]
    fn closing_the_last_channel_waits_out_the_grace_before_unsubscribing() {
        let mut subs = Subscriptions::new(false);
        let now = Instant::now();
        subs.open(GuildId(1), ChannelId(2));

        assert!(
            subs.close(GuildId(1), ChannelId(2), now).is_none(),
            "leaving a guild unsubscribed immediately"
        );
        assert!(subs.due(now + Duration::from_secs(5)).is_empty());

        let due = subs.due(now + UNSUBSCRIBE_GRACE + Duration::from_secs(1));
        assert_eq!(due.len(), 1);
        let value = body(&due[0]);
        assert_eq!(value["d"]["guild_id"], "1");
        assert_eq!(value["d"]["channels"], serde_json::json!({}));
        assert_eq!(value["d"]["typing"], false);
        assert_eq!(subs.guilds(), 0);

        // And nothing is owed twice.
        assert!(subs
            .due(now + UNSUBSCRIBE_GRACE + Duration::from_secs(60))
            .is_empty());
    }

    /// Clicking between two channels in the same server is the common case;
    /// unsubscribing and resubscribing across it would be two payloads for
    /// nothing.
    #[test]
    fn coming_back_inside_the_grace_costs_nothing() {
        let mut subs = Subscriptions::new(false);
        let now = Instant::now();
        subs.open(GuildId(1), ChannelId(2));
        subs.close(GuildId(1), ChannelId(2), now);

        assert!(
            subs.open(GuildId(1), ChannelId(2)).is_none(),
            "reopening inside the grace re-sent an unchanged subscription"
        );
        assert!(
            subs.due(now + UNSUBSCRIBE_GRACE + Duration::from_secs(1))
                .is_empty(),
            "the cancelled departure still happened"
        );
    }

    #[test]
    fn closing_one_of_two_channels_re_sends_the_rest() {
        let mut subs = Subscriptions::new(false);
        let now = Instant::now();
        subs.open(GuildId(1), ChannelId(2));
        subs.open(GuildId(1), ChannelId(3));

        let payload = subs
            .close(GuildId(1), ChannelId(3), now)
            .expect("the ranges changed");
        let value = body(&payload);
        assert_eq!(value["d"]["channels"]["2"], serde_json::json!([[0, 99]]));
        assert!(value["d"]["channels"]["3"].is_null());
    }

    #[test]
    fn a_lost_socket_means_the_next_open_re_sends() {
        let mut subs = Subscriptions::new(false);
        subs.open(GuildId(1), ChannelId(2));
        assert!(subs.open(GuildId(1), ChannelId(2)).is_none());

        subs.forget_connection();
        assert!(
            subs.open(GuildId(1), ChannelId(2)).is_some(),
            "the gateway remembers nothing across a reconnect"
        );
    }

    #[test]
    fn the_legacy_flag_changes_the_opcode_and_nothing_else() {
        let mut modern = Subscriptions::new(false);
        let mut legacy = Subscriptions::new(true);
        let a = body(&modern.open(GuildId(1), ChannelId(2)).unwrap());
        let b = body(&legacy.open(GuildId(1), ChannelId(2)).unwrap());

        assert_eq!(a["op"], 37);
        assert_eq!(b["op"], 14);
        assert_eq!(a["d"], b["d"], "the body is the same request either way");
        assert_eq!(legacy.opcode(), OpCode::LazyRequest);
        assert!(legacy.describe().contains("14"));
        assert!(modern.describe().contains("37"));
    }
}
