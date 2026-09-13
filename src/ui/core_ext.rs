//! What the UI asks `State` that `State` does not answer in that shape.
//!
//! Two of these are shims with an owner on the core side who will delete them;
//! the rest are the small translations every panel would otherwise write for
//! itself, kept here so the two that ask the same question ask it once.
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
//! 2. **[`last_channel`]** reads `session.toml` directly. The core owns that
//!    file and sends `Event::SessionLoaded` with the whole of it, which is
//!    what the UI uses; this is the fallback for a run where no event arrived,
//!    and is three lines rather than a design.
//!
//! Neither reaches into anything private. They are shims because they are
//! answering a question in the wrong place, not because they are cheating.
//!
//! [`member_rows`] and [`custom_emoji`] were shims too and are not any more:
//! the core grew `State::member_list` and `State::custom_emoji`, and what is
//! left here is the turn from the core's shape into the panel's.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::discord::model::member_list::ListMember;
use crate::discord::model::{PresenceStatus, Role, User};
use crate::discord::snowflake::{ChannelId, EmojiId, GuildId, RoleId, UserId};
use crate::discord::state::members::MemberRow;
use crate::discord::state::State;
use crate::ui::panels::members;

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

/// The member list for a guild, as rows the panel draws.
///
/// `None` is "the server has not told us", which the panel draws as *members
/// not loaded*; an empty `Vec` would be "this server has nobody in it", and
/// those are not the same statement. The subscription that fills the list is
/// the core's; what is here is the turn from its shape into the panel's.
///
/// Group headings arrive as a role id or the words `online` and `offline`, so
/// the role name is resolved against the guild — which is the one thing the
/// payload cannot carry and the panel should not have to look up.
pub fn member_rows(state: &State, guild: GuildId) -> Option<Vec<members::Row>> {
    let list = state.member_list(guild)?;
    let roles = state.guild(guild).map(|g| &g.roles);
    Some(
        list.rows()
            .iter()
            .map(|row| match row {
                MemberRow::Group(group) => members::Row::Group {
                    label: group
                        .role()
                        .and_then(|id| roles.and_then(|r| r.get(&id)))
                        .map(|role| role.name.clone())
                        .unwrap_or_else(|| group.label().to_string()),
                    count: group.count as usize,
                },
                MemberRow::Member(member) => members::Row::Member {
                    name: member.display_name().to_string(),
                    presence: member
                        .presence
                        .as_ref()
                        .map(|p| p.status)
                        .unwrap_or(PresenceStatus::Offline),
                    colour: top_colour(member, roles),
                    bot: member.user.bot,
                },
            })
            .collect(),
    )
}

/// The colour of somebody's highest coloured role, packed `0xRRGGBB`.
///
/// Zero means "no colour", not black: Discord uses it for a role that has not
/// been given one, and a member whose roles are all uncoloured is drawn in the
/// ordinary text colour.
fn top_colour(member: &ListMember, roles: Option<&HashMap<RoleId, Arc<Role>>>) -> u32 {
    let Some(roles) = roles else { return 0 };
    member
        .roles
        .iter()
        .filter_map(|id| roles.get(id))
        .filter(|role| role.color != 0)
        .max_by_key(|role| role.position)
        .map(|role| role.color)
        .unwrap_or(0)
}

/// Custom emoji this account can write, by name, for the composer's `:` popup.
pub fn custom_emoji(state: &State) -> Vec<(String, EmojiId, bool)> {
    state
        .custom_emoji()
        .into_iter()
        .filter_map(|emoji| Some((emoji.name.clone()?, emoji.id?, emoji.animated)))
        .collect()
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
