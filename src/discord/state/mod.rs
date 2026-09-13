//! The authoritative picture of the account.
//!
//! Everything the UI draws comes from here, behind one `RwLock`. `Event`s are
//! notifications rather than data: they say "the channel list changed", not
//! what it changed to. That is what makes a bounded event channel safe — a UI
//! that drops half of them still renders the truth on the next frame, and
//! `Event::Refresh` is the only thing a full channel has to guarantee.
//!
//! Nothing here is `async` and nothing here does I/O. [`apply`](super::state::apply)
//! is the only path that mutates it, so "what could have changed this" has one
//! answer.

pub mod apply;
pub mod channels;
pub mod messages;
pub mod read;
pub mod typing;

use std::collections::HashMap;
use std::sync::Arc;

use crate::discord::model::{
    Channel, ChannelKind, Message, Presence, PresenceStatus, ReadState, Relationship,
    RelationshipKind, Role, User, UserGuildSettings,
};
use crate::discord::snowflake::{ChannelId, GuildId, MessageId, RoleId, UserId};

pub use messages::MessageStore;
pub use read::Unread;
pub use typing::Typing;

/// One guild, flattened into what a sidebar needs.
#[derive(Debug, Clone, Default)]
pub struct GuildState {
    pub id: GuildId,
    pub name: String,
    pub icon: Option<String>,
    pub owner_id: Option<UserId>,
    /// Display order, computed once per change rather than once per frame.
    pub channels: Vec<ChannelId>,
    pub roles: HashMap<RoleId, Arc<Role>>,
    pub member_count: Option<u64>,
    /// A Discord-side outage. The row stays, greyed.
    pub unavailable: bool,
}

/// The live gateway session, kept for RESUME. In memory only: writing a session
/// id to disk buys nothing, because a resume window is a minute or two and the
/// token is what actually persists.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub resume_url: Option<String>,
}

#[derive(Default)]
pub struct State {
    version: u64,
    me: Option<Arc<User>>,
    session: Option<SessionInfo>,
    ready: bool,

    users: HashMap<UserId, Arc<User>>,
    guilds: HashMap<GuildId, GuildState>,
    guild_order: Vec<GuildId>,
    channels: HashMap<ChannelId, Arc<Channel>>,
    dm_order: Vec<ChannelId>,

    presences: HashMap<UserId, PresenceStatus>,
    relationships: HashMap<UserId, RelationshipKind>,
    read_states: HashMap<ChannelId, ReadState>,
    /// Keyed by guild, with `None` for the entry that covers every DM.
    settings: HashMap<Option<GuildId>, Arc<UserGuildSettings>>,

    /// One store per channel that has ever been opened. Channels are dropped
    /// only on a logout: a closed one costs fifty messages, and keeping them is
    /// what makes reopening draw a frame before the fetch returns.
    messages: HashMap<ChannelId, MessageStore>,
    typing: Typing,
    /// This account's own roles, per guild, out of READY's `merged_members`.
    /// The only thing they are read for is deciding whether a role mention was
    /// addressed to the reader.
    my_roles: HashMap<GuildId, Vec<RoleId>>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bumped by every mutation. A UI that caches derived layout can compare
    /// this instead of diffing.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub(crate) fn touch(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// Whether a READY has been applied. Before that, every list is empty and
    /// showing "no servers" would be a lie.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn me(&self) -> Option<Arc<User>> {
        self.me.clone()
    }

    pub fn session(&self) -> Option<SessionInfo> {
        self.session.clone()
    }

    pub fn user(&self, id: UserId) -> Option<Arc<User>> {
        self.users.get(&id).cloned()
    }

    pub fn presence(&self, id: UserId) -> PresenceStatus {
        self.presences.get(&id).copied().unwrap_or_default()
    }

    pub fn relationship(&self, id: UserId) -> RelationshipKind {
        self.relationships
            .get(&id)
            .copied()
            .unwrap_or(RelationshipKind::None)
    }

    /// Guilds in the order Discord sent them in READY.
    ///
    /// Not the order shown in the official client, which comes from guild
    /// folders in the user's settings — a payload this client does not fetch
    /// yet. READY's order is stable across reconnects and is the closest thing
    /// available; when folders arrive, this is the single place that changes.
    pub fn guilds_ordered(&self) -> Vec<&GuildState> {
        self.guild_order
            .iter()
            .filter_map(|id| self.guilds.get(id))
            .collect()
    }

    pub fn guild(&self, id: GuildId) -> Option<&GuildState> {
        self.guilds.get(&id)
    }

    pub fn guild_count(&self) -> usize {
        self.guilds.len()
    }

    pub fn channel(&self, id: ChannelId) -> Option<Arc<Channel>> {
        self.channels.get(&id).cloned()
    }

    /// One guild's channels, categories included, in display order.
    pub fn channels_ordered(&self, guild: GuildId) -> Vec<Arc<Channel>> {
        self.guilds
            .get(&guild)
            .map(|g| {
                g.channels
                    .iter()
                    .filter_map(|id| self.channels.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// DMs and group DMs, newest conversation first.
    pub fn dms_ordered(&self) -> Vec<Arc<Channel>> {
        self.dm_order
            .iter()
            .filter_map(|id| self.channels.get(id).cloned())
            .collect()
    }

    pub fn dm_count(&self) -> usize {
        self.dm_order.len()
    }

    pub fn read_state(&self, channel: ChannelId) -> Option<&ReadState> {
        self.read_states.get(&channel)
    }

    /// The settings entry that governs a channel: its guild's, or the DM entry.
    pub fn settings_for(&self, channel: ChannelId) -> Option<&UserGuildSettings> {
        let guild = self.channels.get(&channel).and_then(|c| c.guild_id);
        self.settings.get(&guild).map(|s| s.as_ref())
    }

    /// Unread and mute state for one channel, resolved now.
    pub fn unread(&self, channel: ChannelId) -> Unread {
        self.unread_at(channel, jiff::Timestamp::now())
    }

    /// The same, at a stated moment, so a test does not depend on the clock.
    pub fn unread_at(&self, channel: ChannelId, now: jiff::Timestamp) -> Unread {
        let last_message = self.channels.get(&channel).and_then(|c| c.last_message_id);
        read::unread_for(
            channel,
            last_message,
            self.read_states.get(&channel),
            self.settings_for(channel),
            now,
        )
    }

    /// What to call somebody, in a guild or out of one.
    ///
    /// Per-guild nicknames are a member field and members are not loaded in
    /// M1, so this resolves the account's own display name. The signature
    /// carries the guild already so that adding nicknames is a body change.
    pub fn display_name(&self, _guild: Option<GuildId>, id: UserId) -> String {
        self.users
            .get(&id)
            .map(|u| u.display_name().to_string())
            .unwrap_or_else(|| id.to_string())
    }

    /// The other side of a DM, for its title.
    pub fn dm_title(&self, channel: ChannelId) -> String {
        let Some(channel) = self.channels.get(&channel) else {
            return String::new();
        };
        if let Some(name) = channel.name() {
            return name.to_string();
        }
        let me = self.me.as_ref().map(|u| u.id);
        let mut names: Vec<String> = channel
            .recipient_ids()
            .into_iter()
            .filter(|id| Some(*id) != me)
            .map(|id| self.display_name(None, id))
            .collect();
        names.sort();
        match channel.kind {
            ChannelKind::GroupDm if names.is_empty() => "Unnamed group".to_string(),
            _ if names.is_empty() => "Unknown".to_string(),
            _ => names.join(", "),
        }
    }

    // -- messages ----------------------------------------------------------

    pub fn messages(&self, channel: ChannelId) -> Option<&MessageStore> {
        self.messages.get(&channel)
    }

    /// The store for a channel, created if this is the first time it is asked
    /// for. Only `apply` and `ops` reach this.
    pub(crate) fn messages_mut(&mut self, channel: ChannelId) -> &mut MessageStore {
        self.messages.entry(channel).or_default()
    }

    /// The newest `count` messages, which is what a channel opens on.
    pub fn recent(&self, channel: ChannelId, count: usize) -> Vec<Arc<Message>> {
        self.messages
            .get(&channel)
            .map(|store| store.latest(count))
            .unwrap_or_default()
    }

    pub fn message(&self, channel: ChannelId, id: MessageId) -> Option<Arc<Message>> {
        self.messages.get(&channel).and_then(|store| store.get(id))
    }

    /// Who is typing in a channel, right now.
    pub fn typing(&self, channel: ChannelId) -> Vec<UserId> {
        self.typing.users(channel, std::time::Instant::now())
    }

    pub(crate) fn typing_mut(&mut self) -> &mut Typing {
        &mut self.typing
    }

    /// This account's roles in a guild.
    pub fn my_roles(&self, guild: GuildId) -> &[RoleId] {
        self.my_roles.get(&guild).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether a message is addressed to the reader.
    ///
    /// This is the whole of the notification rule, in one place, because it is
    /// the sort of thing that is easy to write twice and get subtly different.
    /// A direct mention, a mention of a role the reader holds, an `@everyone`,
    /// or any message in a DM — minus the two suppressions and minus anything
    /// the reader wrote themselves.
    pub fn mentions_me(&self, message: &Message) -> bool {
        let Some(me) = self.me.as_ref() else {
            return false;
        };
        if message.author.id == me.id {
            return false;
        }
        // The flag an author sets to send something without pinging anybody.
        if message.notifications_suppressed() {
            return false;
        }

        let channel = self.channels.get(&message.channel_id);
        // Everything in a DM is addressed to the reader; that is what a DM is.
        if channel.is_some_and(|c| c.kind.is_private()) {
            return true;
        }
        if message.mentions_user(me.id) {
            return true;
        }

        let guild = message
            .guild_id
            .or_else(|| channel.and_then(|c| c.guild_id));
        let settings = self.settings.get(&guild);

        if let Some(guild) = guild {
            let suppressed = settings.is_some_and(|s| s.suppress_roles);
            if !suppressed && message.mentions_any_role(self.my_roles(guild)) {
                return true;
            }
        }

        message.mention_everyone && !settings.is_some_and(|s| s.suppress_everyone)
    }

    // -- mutation, used only by `apply` ------------------------------------

    pub(crate) fn set_me(&mut self, user: User) {
        let user = Arc::new(user);
        self.users.insert(user.id, Arc::clone(&user));
        self.me = Some(user);
    }

    pub(crate) fn set_session(&mut self, session: Option<SessionInfo>) {
        self.session = session;
    }

    pub(crate) fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }

    pub(crate) fn upsert_user(&mut self, user: User) {
        self.users.insert(user.id, Arc::new(user));
    }

    pub(crate) fn apply_presence(&mut self, presence: &Presence) {
        self.presences.insert(presence.user.id, presence.status);
    }

    /// Record this account's own memberships, which READY sends as one list
    /// per guild in the same order as `guilds`.
    pub(crate) fn set_my_roles(&mut self, guild: GuildId, roles: Vec<RoleId>) {
        self.my_roles.insert(guild, roles);
    }

    pub(crate) fn set_relationship(&mut self, relationship: &Relationship) {
        self.relationships
            .insert(relationship.id, relationship.kind);
        if let Some(user) = relationship.user.clone() {
            self.upsert_user(user);
        }
    }

    pub(crate) fn remove_relationship(&mut self, id: UserId) {
        self.relationships.remove(&id);
    }

    pub(crate) fn set_read_state(&mut self, state: ReadState) {
        self.read_states.insert(state.id, state);
    }

    pub(crate) fn set_settings(&mut self, settings: UserGuildSettings) {
        self.settings.insert(settings.guild_id, Arc::new(settings));
    }

    pub(crate) fn clear_read_states(&mut self) {
        self.read_states.clear();
    }

    pub(crate) fn clear_settings(&mut self) {
        self.settings.clear();
    }

    /// Insert or replace a channel and re-sort whatever list it belongs to.
    pub(crate) fn upsert_channel(&mut self, channel: Channel) {
        let guild = channel.guild_id;
        let id = channel.id;
        self.channels.insert(id, Arc::new(channel));
        match guild {
            Some(guild) => self.resort_guild(guild),
            None => self.resort_dms(),
        }
    }

    pub(crate) fn remove_channel(&mut self, id: ChannelId) {
        self.messages.remove(&id);
        self.typing.forget(id);
        let guild = self.channels.remove(&id).and_then(|c| c.guild_id);
        match guild {
            Some(guild) => self.resort_guild(guild),
            None => self.resort_dms(),
        }
    }

    pub(crate) fn upsert_guild(&mut self, guild: crate::discord::model::Guild) {
        let id = guild.id;
        let existing = self.guilds.get(&id);

        // An unavailable guild is an outage, not a departure: it arrives with
        // nothing but an id, and overwriting the name and channels with empties
        // would blank the sidebar for the duration.
        if guild.unavailable {
            if let Some(existing) = existing {
                let mut kept = existing.clone();
                kept.unavailable = true;
                self.guilds.insert(id, kept);
            } else {
                self.guilds.insert(
                    id,
                    GuildState {
                        id,
                        unavailable: true,
                        ..Default::default()
                    },
                );
            }
            if !self.guild_order.contains(&id) {
                self.guild_order.push(id);
            }
            return;
        }

        for channel in guild.channels.iter().chain(guild.threads.iter()) {
            self.channels.insert(channel.id, Arc::new(channel.clone()));
        }

        let roles = guild
            .roles
            .iter()
            .map(|r| (r.id, Arc::new(r.clone())))
            .collect();

        self.guilds.insert(
            id,
            GuildState {
                id,
                name: guild.name,
                icon: guild.icon,
                owner_id: guild.owner_id,
                channels: Vec::new(),
                roles,
                member_count: guild.member_count,
                unavailable: false,
            },
        );
        if !self.guild_order.contains(&id) {
            self.guild_order.push(id);
        }
        self.resort_guild(id);
    }

    pub(crate) fn remove_guild(&mut self, id: GuildId) {
        if let Some(guild) = self.guilds.remove(&id) {
            for channel in guild.channels {
                self.channels.remove(&channel);
            }
        }
        self.channels.retain(|_, c| c.guild_id != Some(id));
        self.guild_order.retain(|g| *g != id);
        self.my_roles.remove(&id);
    }

    pub(crate) fn resort_guild(&mut self, guild: GuildId) {
        let members: Vec<Arc<Channel>> = self
            .channels
            .values()
            .filter(|c| c.guild_id == Some(guild))
            .cloned()
            .collect();
        let order = channels::order_guild_channels(&members);
        if let Some(state) = self.guilds.get_mut(&guild) {
            state.channels = order;
        }
    }

    pub(crate) fn resort_dms(&mut self) {
        let privates: Vec<Arc<Channel>> = self
            .channels
            .values()
            .filter(|c| c.guild_id.is_none() && c.kind.is_private())
            .cloned()
            .collect();
        self.dm_order = channels::order_dms(&privates);
    }

    /// Note the newest message in a channel without fetching it, so the DM
    /// order and the unread mark follow a MESSAGE_CREATE for a channel nobody
    /// has opened.
    pub(crate) fn bump_last_message(&mut self, channel: ChannelId, message: MessageId) {
        let Some(existing) = self.channels.get(&channel) else {
            return;
        };
        if existing.last_message_id.is_some_and(|id| id >= message) {
            return;
        }
        let mut updated = (**existing).clone();
        updated.last_message_id = Some(message);
        let guild = updated.guild_id;
        self.channels.insert(channel, Arc::new(updated));
        match guild {
            Some(guild) => self.resort_guild(guild),
            None => self.resort_dms(),
        }
    }

    /// Everything goes, except nothing: a logout builds a new `State`.
    pub(crate) fn clear(&mut self) {
        let version = self.version;
        *self = State::new();
        self.version = version;
        self.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::model::Guild;

    fn guild(id: u64, name: &str) -> Guild {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","name":"{name}","channels":[
                {{"id":"{id}01","type":4,"name":"cat","position":0}},
                {{"id":"{id}02","type":0,"name":"general","position":0,"parent_id":"{id}01"}}
            ]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_guild_brings_its_channels_in_order() {
        let mut state = State::new();
        state.upsert_guild(guild(1, "One"));
        assert_eq!(state.guild_count(), 1);
        let ordered = state.channels_ordered(GuildId(1));
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].kind, ChannelKind::GuildCategory);
        assert_eq!(ordered[1].name(), Some("general"));
    }

    #[test]
    fn an_outage_keeps_the_guild_and_its_name() {
        let mut state = State::new();
        state.upsert_guild(guild(1, "One"));
        let outage: Guild = serde_json::from_str(r#"{"id":"1","unavailable":true}"#).unwrap();
        state.upsert_guild(outage);

        let kept = state.guild(GuildId(1)).unwrap();
        assert!(kept.unavailable);
        assert_eq!(kept.name, "One", "an outage blanked the sidebar");
        assert_eq!(state.channels_ordered(GuildId(1)).len(), 2);
    }

    #[test]
    fn leaving_a_guild_takes_its_channels_with_it() {
        let mut state = State::new();
        state.upsert_guild(guild(1, "One"));
        state.upsert_guild(guild(2, "Two"));
        state.remove_guild(GuildId(1));

        assert_eq!(state.guild_count(), 1);
        assert!(state.channel(ChannelId(102)).is_none());
        assert!(state.channel(ChannelId(202)).is_some());
        assert_eq!(state.guilds_ordered().len(), 1);
    }

    #[test]
    fn a_dm_title_is_the_other_person() {
        let mut state = State::new();
        state.set_me(serde_json::from_str(r#"{"id":"1","username":"me"}"#).unwrap());
        state.upsert_user(
            serde_json::from_str(r#"{"id":"2","username":"alex","global_name":"Alex"}"#).unwrap(),
        );
        state.upsert_channel(
            serde_json::from_str(r#"{"id":"300","type":1,"recipient_ids":["1","2"]}"#).unwrap(),
        );

        assert_eq!(state.dm_title(ChannelId(300)), "Alex");
        assert_eq!(state.dm_count(), 1);
    }

    #[test]
    fn a_new_message_reorders_the_dms() {
        let mut state = State::new();
        for id in [301u64, 302] {
            state.upsert_channel(
                serde_json::from_str(&format!(r#"{{"id":"{id}","type":1}}"#)).unwrap(),
            );
        }
        assert_eq!(
            state.dms_ordered().first().map(|c| c.id),
            Some(ChannelId(302))
        );

        state.bump_last_message(ChannelId(301), MessageId(999_999));
        assert_eq!(
            state.dms_ordered().first().map(|c| c.id),
            Some(ChannelId(301)),
            "a message did not move its DM to the top"
        );
    }

    #[test]
    fn an_older_message_does_not_move_anything() {
        let mut state = State::new();
        state.upsert_channel(
            serde_json::from_str(r#"{"id":"301","type":1,"last_message_id":"500"}"#).unwrap(),
        );
        state.bump_last_message(ChannelId(301), MessageId(400));
        assert_eq!(
            state.channel(ChannelId(301)).unwrap().last_message_id,
            Some(MessageId(500))
        );
    }

    #[test]
    fn the_version_moves_on_every_change() {
        let mut state = State::new();
        let before = state.version();
        state.touch();
        assert_ne!(state.version(), before);
    }
}
