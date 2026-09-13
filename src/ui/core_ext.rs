//! What the UI asks `State` that `State` does not answer in that shape.
//!
//! One of these is a shim; the rest are the small translations every panel
//! would otherwise write for itself, kept here so the two that ask the same
//! question ask it once.
//!
//! **[`last_channel`]** reads `session.toml` directly. The core owns that file
//! and sends `Event::SessionLoaded` with the whole of it, which is what the UI
//! uses; this is the fallback for a run where no event arrived, and is three
//! lines rather than a design. It reaches into nothing private: it is
//! answering a question in the wrong place, not cheating.
//!
//! [`friends`], [`member_rows`] and [`custom_emoji`] were shims too and are
//! not any more: the core grew `State::friends`, `State::member_list` and
//! `State::custom_emoji`, and what is left here is the turn from the core's
//! shape into the panel's.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::discord::model::member_list::ListMember;
use crate::discord::model::{PresenceStatus, Role};
use crate::discord::snowflake::{ChannelId, EmojiId, GuildId, MessageId, RoleId, UserId};
use crate::discord::state::members::MemberRow;
use crate::discord::state::State;
use crate::ui::panels::members;

/// Everybody this account is friends with, for the friends tab.
///
/// The core answers with users; the panel wants a name and a presence beside
/// each, and this is the turn from one into the other.
pub fn friends(state: &State) -> Vec<(UserId, String, PresenceStatus)> {
    state
        .friends()
        .into_iter()
        .map(|user| {
            (
                user.id,
                user.display_name().to_string(),
                state.presence(user.id),
            )
        })
        .collect()
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

/// Every picture in a channel, oldest first, for the media viewer.
///
/// The whole channel rather than one message's attachments: `h` and `l` walk a
/// conversation's photographs, and a viewer that stopped at a message boundary
/// would be one that has to be closed and reopened to see the next one.
pub fn viewer_items(state: &State, channel: ChannelId) -> Vec<crate::ui::overlays::media::Item> {
    state
        .recent(channel, 500)
        .iter()
        .flat_map(|msg| {
            msg.attachments
                .iter()
                .filter(|a| a.is_image())
                .map(|a| crate::ui::overlays::media::Item {
                    message: msg.id,
                    key: crate::discord::media::MediaKey::Attachment {
                        message: msg.id,
                        id: a.id.0,
                        url: a.url.clone(),
                    },
                    url: a.url.clone(),
                    filename: a.filename.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// What a search is searching, in words, for the title.
pub fn scope_name(state: &State, scope: crate::discord::handle::SearchScope) -> String {
    match scope {
        crate::discord::handle::SearchScope::Guild(guild) => state
            .guild(guild)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "this server".into()),
        crate::discord::handle::SearchScope::Channel(channel) => state
            .channel(channel)
            .and_then(|c| c.name().map(|n| format!("#{n}")))
            .unwrap_or_else(|| state.dm_title(channel)),
    }
}

/// The channels the unread hop walks, in the order they are listed.
///
/// A guild's text channels, or the direct messages when the rail is on the
/// message home — which is the list that is on screen in each case, and the
/// hop should visit what is on screen.
pub fn unread_stops(state: &State, guild: Option<GuildId>) -> Vec<crate::ui::unread::Stop> {
    let channels = match guild {
        Some(guild) => state.channels_ordered(guild),
        None => state.dms_ordered(),
    };
    channels
        .iter()
        .filter(|c| c.kind.is_text())
        .map(|c| {
            let unread = state.unread(c.id);
            crate::ui::unread::Stop {
                channel: c.id,
                unread: unread.notable(),
                mentions: unread.mentions,
                muted: unread.muted,
            }
        })
        .collect()
}

/// Who said what, and where, for the line a mention puts in the status bar.
pub fn mention_line(state: &State, channel: ChannelId, message: MessageId) -> (bool, String) {
    let muted = state.unread(channel).muted;
    let Some(msg) = state.message(channel, message) else {
        return (muted, String::new());
    };
    let guild = state.channel(channel).and_then(|c| c.guild_id);
    let who = state.display_name(guild, msg.author.id);
    let place = state
        .channel(channel)
        .and_then(|c| c.name().map(|n| format!("#{n}")))
        .unwrap_or_else(|| state.dm_title(channel));
    let what = crate::discord::markdown::parse(&msg.content).plain_text();
    let one: String = what.lines().next().unwrap_or("").chars().take(60).collect();
    (muted, format!("@{who} in {place}: {one}"))
}

/// The threads hanging off the messages in a window, by the message each was
/// started from.
///
/// Discord gives a thread the id of that message, which is the only link
/// between the two: the message payload says nothing about it at all.
pub fn threads_of(
    state: &State,
    guild: Option<GuildId>,
    messages: &[Arc<crate::discord::model::Message>],
) -> HashMap<MessageId, (ChannelId, String)> {
    let mut out = HashMap::new();
    let Some(guild) = guild else { return out };
    for channel in state.channels_of(guild) {
        if !channel.kind.is_thread() {
            continue;
        }
        let from = MessageId(channel.id.0);
        if messages.iter().any(|m| m.id == from) {
            out.insert(
                from,
                (channel.id, channel.name().unwrap_or("a thread").to_string()),
            );
        }
    }
    out
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
