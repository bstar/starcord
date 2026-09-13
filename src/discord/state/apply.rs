//! The only path that changes [`State`].
//!
//! Every gateway dispatch ends up here, and nothing else writes. That is worth
//! the indirection for one reason: "what could have changed this" has exactly
//! one answer, and a state that looks wrong on screen is a bug in one file.
//!
//! It returns the events the change deserves. They are notifications, not data
//! — see `handle.rs` — so a coarse one is always safe and a missing one is the
//! only real bug. When in doubt, emit.

use smallvec::{smallvec, SmallVec};

use crate::discord::gateway::payload::Dispatch;
use crate::discord::handle::{Event, MessagesChange};
use crate::discord::state::{messages, SessionInfo, State};

/// How many events one dispatch usually produces. Two covers everything except
/// READY, which produces four.
pub type Events = SmallVec<[Event; 2]>;

/// Apply one dispatch and say what changed.
pub fn apply(state: &mut State, dispatch: Dispatch) -> Events {
    let events = apply_inner(state, dispatch);
    if !events.is_empty() {
        state.touch();
    }
    events
}

fn apply_inner(state: &mut State, dispatch: Dispatch) -> Events {
    match dispatch {
        Dispatch::Ready(ready) => {
            let ready = *ready;
            // A READY is the whole account. Everything derived is rebuilt from
            // it rather than merged into what a previous session left behind:
            // a merge would keep a guild the user left while disconnected.
            state.clear();
            state.set_session(Some(SessionInfo {
                id: ready.session_id,
                resume_url: ready.resume_gateway_url,
            }));
            state.set_me(ready.user);

            // Under DEDUPE_USER_OBJECTS every user in the payload is here once
            // and referenced by id everywhere else, so this has to come before
            // the channels that name them.
            for user in ready.users {
                state.upsert_user(user);
            }

            // `merged_members` is this account's own membership in each guild,
            // one list per guild in the same order as `guilds`. The roles in it
            // are the only thing that can answer "was that role mention
            // addressed to me", so they are picked out before the guilds are
            // consumed.
            let mut my_memberships = ready.merged_members.into_iter();
            for guild in ready.guilds {
                let id = guild.id;
                if let Some(members) = my_memberships.next() {
                    let me = state.me().map(|me| me.id);
                    if let Some(mine) = members
                        .into_iter()
                        .find(|m| me.is_none() || m.user_id() == me)
                    {
                        state.set_my_roles(id, mine.roles);
                    }
                }
                state.upsert_guild(guild);
            }

            for channel in ready.private_channels {
                // A DM's recipients arrive inline when the session did not ask
                // for deduped users, and are the only place those accounts
                // appear.
                for user in channel.recipients.iter().cloned() {
                    state.upsert_user(user);
                }
                state.upsert_channel(channel);
            }

            for relationship in &ready.relationships {
                state.set_relationship(relationship);
            }

            state.clear_read_states();
            for read in ready.read_state.entries() {
                state.set_read_state(read);
            }

            state.clear_settings();
            for settings in ready.user_guild_settings.entries() {
                state.set_settings(settings);
            }

            state.set_ready(true);
            smallvec![
                Event::Ready,
                Event::Guilds,
                Event::Channels(None),
                Event::Relationships,
            ]
        }

        Dispatch::ReadySupplemental(supplemental) => {
            let supplemental = *supplemental;
            for channel in supplemental.lazy_private_channels {
                for user in channel.recipients.iter().cloned() {
                    state.upsert_user(user);
                }
                state.upsert_channel(channel);
            }
            for presence in supplemental
                .merged_presences
                .friends
                .iter()
                .chain(supplemental.merged_presences.guilds.iter().flatten())
            {
                state.apply_presence(presence);
            }
            // One coarse event rather than one per friend: a supplemental
            // carries thousands of presences and the UI redraws once a frame.
            smallvec![Event::Channels(None), Event::Relationships]
        }

        // The session survived; nothing in `State` changed.
        Dispatch::Resumed => Events::new(),

        Dispatch::GuildCreate(guild) | Dispatch::GuildUpdate(guild) => {
            let id = guild.id;
            state.upsert_guild(*guild);
            smallvec![Event::Guilds, Event::Channels(Some(id))]
        }

        Dispatch::GuildDelete { id, unavailable } => {
            if unavailable {
                // An outage. Keep the row, mark it.
                let placeholder = crate::discord::model::Guild {
                    id,
                    unavailable: true,
                    ..Default::default()
                };
                state.upsert_guild(placeholder);
            } else {
                state.remove_guild(id);
            }
            smallvec![Event::Guilds, Event::Channels(Some(id))]
        }

        Dispatch::ChannelCreate(channel) | Dispatch::ChannelUpdate(channel) => {
            let guild = channel.guild_id;
            for user in channel.recipients.iter().cloned() {
                state.upsert_user(user);
            }
            state.upsert_channel(*channel);
            smallvec![Event::Channels(guild)]
        }

        Dispatch::ChannelDelete(channel) => {
            let guild = channel.guild_id;
            state.remove_channel(channel.id);
            smallvec![Event::Channels(guild)]
        }

        Dispatch::PresenceUpdate(presence) => {
            let id = presence.user.id;
            state.apply_presence(&presence);
            smallvec![Event::Presence(id)]
        }

        Dispatch::UserUpdate(user) => {
            let id = user.id;
            // A USER_UPDATE for the session's own account changes the name in
            // the status line as well as in every message it sent.
            let is_me = state.me().is_some_and(|me| me.id == id);
            if is_me {
                state.set_me(*user);
            } else {
                state.upsert_user(*user);
            }
            smallvec![Event::Presence(id)]
        }

        Dispatch::RelationshipAdd(relationship) => {
            state.set_relationship(&relationship);
            smallvec![Event::Relationships]
        }

        Dispatch::RelationshipRemove { id } => {
            state.remove_relationship(id);
            smallvec![Event::Relationships]
        }

        Dispatch::UserGuildSettingsUpdate(settings) => {
            let guild = settings.guild_id;
            state.set_settings(*settings);
            // Mute state is part of every channel row in that guild.
            smallvec![Event::Channels(guild)]
        }

        Dispatch::MessageAck {
            channel_id,
            message_id,
            mention_count,
        } => {
            // Another client of this account read something. The read state has
            // to follow or the channel stays bold here forever.
            let mut read = state.read_state(channel_id).cloned().unwrap_or_else(|| {
                crate::discord::model::ReadState {
                    id: channel_id,
                    ..Default::default()
                }
            });
            if let Some(message_id) = message_id {
                read.last_message_id = Some(message_id);
            }
            if let Some(count) = mention_count {
                read.mention_count = count;
            } else {
                // An ack with no count clears the mentions: acknowledging the
                // message that mentioned you is what clears the badge.
                read.mention_count = 0;
            }
            state.set_read_state(read);
            smallvec![Event::ReadState(channel_id)]
        }

        Dispatch::ChannelUnreadUpdate {
            guild_id,
            channel_unread_updates,
        } => {
            let mut events = Events::new();
            for read in channel_unread_updates {
                let channel = read.id;
                state.set_read_state(read);
                events.push(Event::ReadState(channel));
            }
            if events.is_empty() {
                events.push(Event::Channels(guild_id));
            }
            events
        }

        Dispatch::MessageCreate(message) => {
            let message = *message;
            let channel = message.channel_id;
            let id = message.id;
            let author = message.author.id;
            let mut events = Events::new();

            // The channel row and the DM order both follow the newest message,
            // whether or not anybody has the channel open.
            state.bump_last_message(channel, id);
            let mentioned = state.mentions_me(&message);
            let is_dm = state.channel(channel).is_some_and(|c| c.kind.is_private());

            // Somebody who just sent a message has stopped typing, and waiting
            // out the ten-second lease to admit it looks like a stuck client.
            if state.typing_mut().stopped(channel, author) {
                events.push(Event::Typing(channel));
            }

            let received = state.messages_mut(channel).receive(message);
            if received != messages::Received::Duplicate {
                events.push(Event::Messages(channel, MessagesChange::Appended(id)));
            }
            events.push(Event::ReadState(channel));
            if is_dm {
                // A DM moves to the top of its list.
                events.push(Event::Channels(None));
            }
            if mentioned {
                events.push(Event::Mention {
                    channel,
                    message: id,
                });
            }
            events
        }

        Dispatch::MessageUpdate {
            id,
            channel_id,
            payload,
        } => {
            if state.messages_mut(channel_id).update(id, &payload) {
                smallvec![Event::Messages(channel_id, MessagesChange::Updated(id))]
            } else {
                // An edit to something that scrolled out of the window changes
                // nothing here.
                Events::new()
            }
        }

        Dispatch::MessageDelete {
            id,
            channel_id,
            guild_id: _,
        } => {
            if state.messages_mut(channel_id).remove(id) {
                smallvec![Event::Messages(channel_id, MessagesChange::Removed(id))]
            } else {
                Events::new()
            }
        }

        Dispatch::MessageDeleteBulk {
            ids,
            channel_id,
            guild_id: _,
        } => {
            if state.messages_mut(channel_id).remove_many(&ids) > 0 {
                // One coarse event: a bulk delete takes out a moderation-sized
                // run of rows and every cached height above them moves.
                smallvec![Event::Messages(channel_id, MessagesChange::Replaced)]
            } else {
                Events::new()
            }
        }

        Dispatch::MessageReactionAdd {
            channel_id,
            message_id,
            user_id,
            emoji,
        } => {
            let me = state.me().is_some_and(|me| me.id == user_id);
            if state
                .messages_mut(channel_id)
                .add_reaction(message_id, &emoji, me)
            {
                smallvec![Event::Messages(
                    channel_id,
                    MessagesChange::Reactions(message_id)
                )]
            } else {
                Events::new()
            }
        }

        Dispatch::MessageReactionRemove {
            channel_id,
            message_id,
            user_id,
            emoji,
        } => {
            let me = state.me().is_some_and(|me| me.id == user_id);
            if state
                .messages_mut(channel_id)
                .remove_reaction(message_id, &emoji, me)
            {
                smallvec![Event::Messages(
                    channel_id,
                    MessagesChange::Reactions(message_id)
                )]
            } else {
                Events::new()
            }
        }

        Dispatch::MessageReactionRemoveAll {
            channel_id,
            message_id,
        } => {
            if state.messages_mut(channel_id).clear_reactions(message_id) {
                smallvec![Event::Messages(
                    channel_id,
                    MessagesChange::Reactions(message_id)
                )]
            } else {
                Events::new()
            }
        }

        Dispatch::MessageReactionRemoveEmoji {
            channel_id,
            message_id,
            emoji,
        } => {
            if state
                .messages_mut(channel_id)
                .clear_emoji(message_id, &emoji)
            {
                smallvec![Event::Messages(
                    channel_id,
                    MessagesChange::Reactions(message_id)
                )]
            } else {
                Events::new()
            }
        }

        Dispatch::TypingStart {
            channel_id,
            user_id,
        } => {
            // Only a set that changed is worth a redraw: somebody typing a long
            // message re-sends this every eight seconds.
            if state
                .typing_mut()
                .started(channel_id, user_id, std::time::Instant::now())
            {
                smallvec![Event::Typing(channel_id)]
            } else {
                Events::new()
            }
        }

        // Sent when the account's sessions change, which this client shows
        // nothing about.
        Dispatch::SessionsReplace => Events::new(),

        Dispatch::Malformed { event } => {
            tracing::debug!("dropped a malformed {event}");
            Events::new()
        }

        Dispatch::Unknown { .. } => Events::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::gateway::payload::decode;
    use crate::discord::snowflake::{ChannelId, GuildId, MessageId, UserId};
    use serde_json::value::RawValue;

    fn dispatch(event: &str, payload: &str) -> Dispatch {
        decode(
            event,
            Some(&RawValue::from_string(payload.to_string()).unwrap()),
        )
    }

    /// The hand-written READY fixture, which is documented as synthetic in
    /// `testdata/gateway/README.md`.
    fn ready_fixture() -> Dispatch {
        let text = include_str!("../../../testdata/gateway/ready.json");
        let envelope: serde_json::Value = serde_json::from_str(text).unwrap();
        let payload = serde_json::to_string(&envelope["d"]).unwrap();
        decode("READY", Some(&RawValue::from_string(payload).unwrap()))
    }

    #[test]
    fn the_ready_fixture_applies_and_orders_everything() {
        let mut state = State::new();
        let events = apply(&mut state, ready_fixture());

        assert!(state.is_ready());
        assert!(
            events.iter().any(|e| matches!(e, Event::Ready)),
            "applying a READY must announce it"
        );
        assert_eq!(state.me().unwrap().display_name(), "Sam");
        assert_eq!(state.session().unwrap().id, "0000000000000000000000000000");

        let guilds = state.guilds_ordered();
        assert_eq!(
            guilds.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["First Guild", "Second Guild"],
            "guilds must keep the order READY sent them in"
        );

        // Uncategorised first, then the category and its channels, text before
        // voice.
        let channels = state.channels_ordered(GuildId(200000000000000001));
        let names: Vec<&str> = channels.iter().filter_map(|c| c.name()).collect();
        assert_eq!(
            names,
            vec![
                "rules",
                "Text Channels",
                "general",
                "random",
                "General Voice"
            ]
        );

        // Newest conversation first; the group DM has the newer last message.
        let dms = state.dms_ordered();
        assert_eq!(dms.len(), 2);
        assert_eq!(
            dms.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![ChannelId(400000000000000002), ChannelId(400000000000000001)]
        );
        assert_eq!(state.dm_title(ChannelId(400000000000000001)), "Alex");
    }

    #[test]
    fn the_fixture_derives_the_unread_and_mute_marks() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let now: jiff::Timestamp = "2026-09-13T12:00:00Z".parse().unwrap();

        // #rules has never had a message posted in it.
        let rules = state.unread_at(ChannelId(200000000000000005), now);
        assert!(!rules.unread);

        // #general: the last message is newer than the ack, and one of them
        // said this account's name.
        let general = state.unread_at(ChannelId(200000000000000011), now);
        assert!(general.unread);
        assert_eq!(general.mentions, 1);
        assert!(!general.muted);
        assert!(general.notable());

        // #random: unread, but muted by a per-channel override in a server
        // that is not itself muted.
        let random = state.unread_at(ChannelId(200000000000000012), now);
        assert!(random.unread);
        assert!(random.muted, "the channel override did not reach the row");
        assert!(!random.notable());

        // The second guild is muted outright, so its channels are.
        let announcements = state.unread_at(ChannelId(200000000000000021), now);
        assert!(
            announcements.unread,
            "a channel with no read state is unread"
        );
        assert!(
            announcements.muted,
            "the guild mute did not reach its channel"
        );
        assert!(!announcements.notable());

        // #quiet is acknowledged up to its last message.
        let quiet = state.unread_at(ChannelId(200000000000000022), now);
        assert!(!quiet.unread, "an acknowledged channel is not unread");
    }

    #[test]
    fn a_ready_replaces_rather_than_merges() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        assert_eq!(state.guild_count(), 2);

        let second = dispatch(
            "READY",
            r#"{"user":{"id":"1","username":"me"},"session_id":"s2","guilds":[]}"#,
        );
        apply(&mut state, second);
        assert_eq!(
            state.guild_count(),
            0,
            "a guild left while disconnected would survive a merge"
        );
        assert_eq!(state.session().unwrap().id, "s2");
    }

    #[test]
    fn an_unavailable_guild_delete_keeps_the_row() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let before = state.guild_count();

        apply(
            &mut state,
            dispatch(
                "GUILD_DELETE",
                r#"{"id":"200000000000000001","unavailable":true}"#,
            ),
        );
        assert_eq!(state.guild_count(), before);
        assert!(
            state
                .guild(GuildId(200000000000000001))
                .unwrap()
                .unavailable
        );

        apply(
            &mut state,
            dispatch("GUILD_DELETE", r#"{"id":"200000000000000001"}"#),
        );
        assert_eq!(
            state.guild_count(),
            before - 1,
            "leaving must remove the row"
        );
    }

    #[test]
    fn a_channel_create_lands_in_the_right_place() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());

        let events = apply(
            &mut state,
            dispatch(
                "CHANNEL_CREATE",
                r#"{"id":"200000000000000013","type":0,"guild_id":"200000000000000001","name":"aaa","position":0,"parent_id":"200000000000000010"}"#,
            ),
        );
        assert!(matches!(
            events.as_slice(),
            [Event::Channels(Some(g))] if *g == GuildId(200000000000000001)
        ));
        let channels = state.channels_ordered(GuildId(200000000000000001));
        let names: Vec<&str> = channels.iter().filter_map(|c| c.name()).collect();
        assert_eq!(
            names,
            vec![
                "rules",
                "Text Channels",
                "aaa",
                "general",
                "random",
                "General Voice"
            ],
            "a new channel at position 0 goes to the top of its category"
        );
    }

    #[test]
    fn an_ack_from_another_client_clears_the_mark_here() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        let now: jiff::Timestamp = "2026-09-13T12:00:00Z".parse().unwrap();
        assert!(state.unread_at(channel, now).unread);

        let last = state.channel(channel).unwrap().last_message_id.unwrap();
        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_ACK",
                &format!(r#"{{"channel_id":"{channel}","message_id":"{last}"}}"#),
            ),
        );
        assert!(matches!(events.as_slice(), [Event::ReadState(c)] if *c == channel));

        let after = state.unread_at(channel, now);
        assert!(!after.unread);
        assert_eq!(after.mentions, 0, "an ack with no count clears the badge");
    }

    #[test]
    fn a_presence_update_moves_one_dot() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let friend = UserId(100000000000000002);
        assert_eq!(
            state.presence(friend),
            crate::discord::model::PresenceStatus::Offline
        );

        let events = apply(
            &mut state,
            dispatch(
                "PRESENCE_UPDATE",
                &format!(r#"{{"user":{{"id":"{friend}"}},"status":"dnd"}}"#),
            ),
        );
        assert!(matches!(events.as_slice(), [Event::Presence(u)] if *u == friend));
        assert_eq!(
            state.presence(friend),
            crate::discord::model::PresenceStatus::Dnd
        );
    }

    #[test]
    fn a_user_update_for_this_account_changes_the_name_in_the_status_line() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let me = state.me().unwrap().id;

        apply(
            &mut state,
            dispatch(
                "USER_UPDATE",
                &format!(r#"{{"id":"{me}","username":"sam","global_name":"Samantha"}}"#),
            ),
        );
        assert_eq!(state.me().unwrap().display_name(), "Samantha");
    }

    #[test]
    fn an_unknown_or_malformed_dispatch_changes_nothing() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let version = state.version();

        assert!(apply(&mut state, dispatch("CALL_CREATE", "{}")).is_empty());
        assert!(apply(
            &mut state,
            dispatch("CHANNEL_CREATE", r#"{"id":"not a snowflake"}"#)
        )
        .is_empty());
        assert_eq!(
            state.version(),
            version,
            "a dispatch that changed nothing bumped the version"
        );
    }

    #[test]
    fn a_relationship_removal_is_announced() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let friend = UserId(100000000000000002);
        assert!(state.relationship(friend).can_dm());

        apply(
            &mut state,
            dispatch("RELATIONSHIP_REMOVE", &format!(r#"{{"id":"{friend}"}}"#)),
        );
        assert!(!state.relationship(friend).can_dm());
    }

    /// The channel READY says has messages in it, with a store the reader has
    /// opened.
    fn opened(state: &mut State, channel: ChannelId) {
        let store = state.messages_mut(channel);
        store.set_open(true);
        store.set_at_latest(true);
    }

    fn message_json(id: u64, channel: u64, author: u64, extra: &str) -> String {
        format!(
            r#"{{"id":"{id}","channel_id":"{channel}","author":{{"id":"{author}","username":"a"}}{extra}}}"#
        )
    }

    #[test]
    fn a_live_message_lands_in_its_channel_and_marks_it_unread() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000999,
                    200000000000000011,
                    100000000000000002,
                    r#","content":"hello""#,
                ),
            ),
        );

        assert!(events.iter().any(|e| matches!(
            e,
            Event::Messages(c, MessagesChange::Appended(m))
                if *c == channel && *m == MessageId(500000000000000999)
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::ReadState(c) if *c == channel)));

        let store = state.messages(channel).expect("the store is there");
        assert_eq!(store.len(), 1);
        assert_eq!(
            state.channel(channel).unwrap().last_message_id,
            Some(MessageId(500000000000000999)),
            "the channel row did not follow the message"
        );
    }

    #[test]
    fn a_message_naming_this_account_is_a_mention() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000998,
                    200000000000000011,
                    100000000000000002,
                    r#","mentions":[{"id":"100000000000000001","username":"sam"}]"#,
                ),
            ),
        );
        assert!(
            events.iter().any(|e| matches!(e, Event::Mention { .. })),
            "a message that said this account's name was not a mention"
        );
    }

    #[test]
    fn a_message_this_account_sent_is_never_a_mention_of_itself() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000997,
                    200000000000000011,
                    100000000000000001,
                    r#","mention_everyone":true"#,
                ),
            ),
        );
        assert!(!events.iter().any(|e| matches!(e, Event::Mention { .. })));
    }

    #[test]
    fn everything_in_a_dm_is_addressed_to_the_reader() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let dm = ChannelId(400000000000000001);
        opened(&mut state, dm);

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000001000,
                    400000000000000001,
                    100000000000000002,
                    "",
                ),
            ),
        );
        assert!(events.iter().any(|e| matches!(e, Event::Mention { .. })));
        assert!(
            events.iter().any(|e| matches!(e, Event::Channels(None))),
            "a DM with a new message did not move in the list"
        );
    }

    #[test]
    fn a_suppressed_message_pings_nobody() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let dm = ChannelId(400000000000000001);
        opened(&mut state, dm);

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000001001,
                    400000000000000001,
                    100000000000000002,
                    r#","flags":4096"#,
                ),
            ),
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::Mention { .. })),
            "the suppress-notifications flag was ignored"
        );
    }

    #[test]
    fn an_edit_changes_the_message_and_a_deletion_removes_it() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);
        apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000996,
                    200000000000000011,
                    100000000000000002,
                    r#","content":"before""#,
                ),
            ),
        );

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_UPDATE",
                r#"{"id":"500000000000000996","channel_id":"200000000000000011","content":"after"}"#,
            ),
        );
        assert!(matches!(
            events.as_slice(),
            [Event::Messages(_, MessagesChange::Updated(_))]
        ));
        assert_eq!(
            state
                .message(channel, MessageId(500000000000000996))
                .unwrap()
                .content,
            "after"
        );

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_DELETE",
                r#"{"id":"500000000000000996","channel_id":"200000000000000011"}"#,
            ),
        );
        assert!(matches!(
            events.as_slice(),
            [Event::Messages(_, MessagesChange::Removed(_))]
        ));
        assert!(state.messages(channel).unwrap().is_empty());

        // Deleting it again changes nothing.
        assert!(apply(
            &mut state,
            dispatch(
                "MESSAGE_DELETE",
                r#"{"id":"500000000000000996","channel_id":"200000000000000011"}"#,
            ),
        )
        .is_empty());
    }

    #[test]
    fn a_bulk_delete_takes_out_what_it_holds() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);
        for id in [
            500000000000000990u64,
            500000000000000991,
            500000000000000992,
        ] {
            apply(
                &mut state,
                dispatch(
                    "MESSAGE_CREATE",
                    &message_json(id, 200000000000000011, 100000000000000002, ""),
                ),
            );
        }

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_DELETE_BULK",
                r#"{"channel_id":"200000000000000011","ids":["500000000000000990","500000000000000992"]}"#,
            ),
        );
        assert!(matches!(
            events.as_slice(),
            [Event::Messages(_, MessagesChange::Replaced)]
        ));
        assert_eq!(state.messages(channel).unwrap().len(), 1);
    }

    #[test]
    fn every_reaction_dispatch_reaches_the_chip() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);
        apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000980,
                    200000000000000011,
                    100000000000000002,
                    "",
                ),
            ),
        );
        let id = MessageId(500000000000000980);

        let add = r#"{"channel_id":"200000000000000011","message_id":"500000000000000980","user_id":"100000000000000001","emoji":{"id":null,"name":"👍"}}"#;
        let events = apply(&mut state, dispatch("MESSAGE_REACTION_ADD", add));
        assert!(matches!(
            events.as_slice(),
            [Event::Messages(_, MessagesChange::Reactions(_))]
        ));
        let message = state.message(channel, id).unwrap();
        assert_eq!(message.reactions.len(), 1);
        assert!(message.reactions[0].me, "this account's own reaction");

        apply(&mut state, dispatch("MESSAGE_REACTION_REMOVE", add));
        assert!(state.message(channel, id).unwrap().reactions.is_empty());

        apply(&mut state, dispatch("MESSAGE_REACTION_ADD", add));
        apply(
            &mut state,
            dispatch(
                "MESSAGE_REACTION_REMOVE_EMOJI",
                r#"{"channel_id":"200000000000000011","message_id":"500000000000000980","emoji":{"id":null,"name":"👍"}}"#,
            ),
        );
        assert!(state.message(channel, id).unwrap().reactions.is_empty());

        apply(&mut state, dispatch("MESSAGE_REACTION_ADD", add));
        apply(
            &mut state,
            dispatch(
                "MESSAGE_REACTION_REMOVE_ALL",
                r#"{"channel_id":"200000000000000011","message_id":"500000000000000980"}"#,
            ),
        );
        assert!(state.message(channel, id).unwrap().reactions.is_empty());
    }

    #[test]
    fn typing_starts_once_and_stops_when_the_message_arrives() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);

        let start = r#"{"channel_id":"200000000000000011","user_id":"100000000000000002"}"#;
        let events = apply(&mut state, dispatch("TYPING_START", start));
        assert!(matches!(events.as_slice(), [Event::Typing(c)] if *c == channel));
        assert_eq!(state.typing(channel), vec![UserId(100000000000000002)]);

        assert!(
            apply(&mut state, dispatch("TYPING_START", start)).is_empty(),
            "a renewed lease is a redraw a second for as long as anybody types"
        );

        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000970,
                    200000000000000011,
                    100000000000000002,
                    "",
                ),
            ),
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Typing(c) if *c == channel)));
        assert!(state.typing(channel).is_empty());
    }

    #[test]
    fn a_message_arriving_while_scrolled_up_is_counted_rather_than_appended() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);
        state.messages_mut(channel).set_at_latest(false);

        apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000960,
                    200000000000000011,
                    100000000000000002,
                    "",
                ),
            ),
        );
        let store = state.messages(channel).unwrap();
        assert!(store.is_empty());
        assert_eq!(store.newer_hidden(), 1);
    }

    /// The roles READY hands over are what decides whether a role mention was
    /// aimed at the reader.
    #[test]
    fn a_role_mention_is_only_a_mention_of_a_role_this_account_holds() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let channel = ChannelId(200000000000000011);
        opened(&mut state, channel);

        let held = state.my_roles(GuildId(200000000000000001)).to_vec();
        assert!(
            !held.is_empty(),
            "READY's merged_members did not reach the state"
        );

        let mine = format!(
            r#","mention_roles":["{}"],"guild_id":"200000000000000001""#,
            held[0]
        );
        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000950,
                    200000000000000011,
                    100000000000000002,
                    &mine,
                ),
            ),
        );
        assert!(events.iter().any(|e| matches!(e, Event::Mention { .. })));

        let theirs = r#","mention_roles":["999999999999999999"],"guild_id":"200000000000000001""#;
        let events = apply(
            &mut state,
            dispatch(
                "MESSAGE_CREATE",
                &message_json(
                    500000000000000951,
                    200000000000000011,
                    100000000000000002,
                    theirs,
                ),
            ),
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::Mention { .. })),
            "a role nobody here holds was treated as a mention"
        );
    }

    #[test]
    fn a_message_in_a_dm_moves_it_to_the_top() {
        let mut state = State::new();
        apply(&mut state, ready_fixture());
        let older = ChannelId(400000000000000001);
        assert_ne!(state.dms_ordered()[0].id, older);

        state.bump_last_message(older, MessageId(999_999_999_999_999_999));
        assert_eq!(state.dms_ordered()[0].id, older);
    }
}
