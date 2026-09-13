//! The two things the UI needs from the core that the core does not offer yet.
//!
//! Both are shims, both are here rather than spread through `app.rs`, and both
//! have an owner on the core side who will delete them:
//!
//! 1. **[`friends`]** derives the friends list from the DM list. `State` knows
//!    every relationship — READY carries them and `apply` stores them — but the
//!    only way out is `State::relationship(id)`, which answers about somebody
//!    you already know the id of. What the panel needs is
//!    `State::friends() -> Vec<Arc<User>>`, or an ordered relationships query.
//!    Until there is one, this walks the DM channels and keeps the recipients
//!    who are friends, which is right for everybody you have ever messaged and
//!    silently short for everybody you have not.
//!
//! 2. **[`last_channel`]** reads `session.toml` directly. The session file is
//!    the core's, and a `session.rs` that owns it is landing in the milestone
//!    beside this one; this reads the one key the UI needs to open the channel
//!    somebody was last in, and is three lines rather than a design.
//!
//! Neither reaches into anything private. They are shims because they are
//! answering a question in the wrong place, not because they are cheating.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::discord::model::{PresenceStatus, User};
use crate::discord::snowflake::{ChannelId, GuildId, UserId};
use crate::discord::state::State;

/// Everybody this account is friends with, as far as the DM list can say.
///
/// Sorted by name so the grouping the panel does afterwards is stable, and
/// deduplicated because somebody can be in a group DM and a one-to-one at
/// once.
pub fn friends(state: &State) -> Vec<(UserId, String, PresenceStatus)> {
    let me = state.me().map(|u| u.id);
    let mut out: BTreeMap<UserId, (String, PresenceStatus)> = BTreeMap::new();

    for channel in state.dms_ordered() {
        for id in channel.recipient_ids() {
            if Some(id) == me || !state.relationship(id).can_dm() {
                continue;
            }
            out.entry(id).or_insert_with(|| {
                (
                    state
                        .user(id)
                        .as_deref()
                        .map(User::display_name)
                        .unwrap_or("unknown")
                        .to_string(),
                    state.presence(id),
                )
            });
        }
    }
    let mut v: Vec<(UserId, String, PresenceStatus)> =
        out.into_iter().map(|(id, (n, p))| (id, n, p)).collect();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

/// What a DM row should say, and who it is with.
pub fn dm_row(state: &State, channel: ChannelId) -> (String, PresenceStatus, Option<usize>) {
    let title = state.dm_title(channel);
    let Some(c) = state.channel(channel) else {
        return (title, PresenceStatus::Offline, None);
    };
    let me = state.me().map(|u| u.id);
    let others: Vec<UserId> = c
        .recipient_ids()
        .into_iter()
        .filter(|id| Some(*id) != me)
        .collect();
    // A group's dot would be a lie — there is no one presence for four people
    // — so it carries a member count instead, and the pair carries the dot.
    if others.len() > 1 {
        return (title, PresenceStatus::Offline, Some(others.len() + 1));
    }
    let presence = others
        .first()
        .map(|id| state.presence(*id))
        .unwrap_or(PresenceStatus::Offline);
    (title, presence, None)
}

/// Everybody's avatar, which the rail and the message list will both want.
/// Here so the two ask the same question rather than two similar ones.
pub fn avatar_hash(state: &State, id: UserId) -> Option<Arc<str>> {
    state.user(id).and_then(|u| u.avatar.clone()).map(Arc::from)
}

/// The member list for a guild, which the core does not keep yet.
///
/// `State` has no `member_list(guild)`: the lazy subscription that fills one
/// — op 37, then `GUILD_MEMBER_LIST_UPDATE` with SYNC, INSERT, UPDATE, DELETE
/// and INVALIDATE against a range — is core work in the milestone beside this
/// one. `None` here is "the server has not told us", which the panel draws as
/// *members not loaded*; an empty `Vec` would be "this server has nobody in
/// it", and those are not the same statement.
///
/// What the core should grow:
/// `State::member_list(&self, guild: GuildId) -> Option<&MemberList>`, with
/// `MemberList` carrying the groups Discord sent, in Discord's order.
pub fn member_rows(
    _state: &State,
    _guild: GuildId,
) -> Option<Vec<crate::ui::panels::members::Row>> {
    None
}

/// Custom emoji this account can write, by name.
///
/// `State` keeps guilds flattened to what a sidebar needs and drops the
/// `emojis` array READY carries with each one, so there is nothing to read.
/// The composer's `:` popup therefore offers unicode emoji and, once the core
/// grows `State::custom_emoji(&self) -> Vec<Arc<CustomEmoji>>`, whatever this
/// returns instead.
pub fn custom_emoji(_state: &State) -> Vec<(String, crate::discord::snowflake::EmojiId, bool)> {
    Vec::new()
}

/// The channel that was open when the program last closed.
///
/// Best effort, always: a session file that is missing, unreadable or written
/// by a version that spelled it differently costs the restore and nothing
/// else. It is a convenience, not state.
pub fn last_channel(path: &Path) -> Option<ChannelId> {
    let text = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = text.parse().ok()?;
    let raw = table.get("last_channel")?;
    match raw {
        toml::Value::Integer(n) if *n > 0 => Some(ChannelId(*n as u64)),
        toml::Value::String(s) => s.parse::<u64>().ok().filter(|n| *n > 0).map(ChannelId),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_session_file_restores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(last_channel(&dir.path().join("nothing.toml")), None);
    }

    /// Discord writes snowflakes as strings and a hand-edited file is as
    /// likely to have a number, so both are read.
    #[test]
    fn the_last_channel_is_read_either_way_round() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");

        std::fs::write(&path, "last_channel = \"123456789\"\n").unwrap();
        assert_eq!(last_channel(&path), Some(ChannelId(123456789)));

        std::fs::write(&path, "last_channel = 42\n").unwrap();
        assert_eq!(last_channel(&path), Some(ChannelId(42)));

        std::fs::write(&path, "last_channel = 0\n").unwrap();
        assert_eq!(
            last_channel(&path),
            None,
            "zero is Discord's way of saying none"
        );

        std::fs::write(&path, "not = toml [[[\n").unwrap();
        assert_eq!(last_channel(&path), None, "a broken file costs the restore");

        std::fs::write(&path, "something_else = 1\n").unwrap();
        assert_eq!(last_channel(&path), None);
    }

    #[test]
    fn an_empty_state_has_no_friends() {
        let state = State::new();
        assert!(friends(&state).is_empty());
        assert_eq!(
            dm_row(&state, ChannelId(1)),
            (String::new(), PresenceStatus::Offline, None)
        );
    }
}
