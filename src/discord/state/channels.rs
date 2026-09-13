//! Putting channels and DMs in the order Discord's own client shows them.
//!
//! Neither order is sent. Both are derived, and both are easy to get subtly
//! wrong in ways that only show up on somebody else's account.
//!
//! **Guild channels.** Uncategorised channels come first, then each category
//! with its own channels under it. Within any one group the sort is `position`,
//! and ties break on the id — which is not arbitrary: two channels created in
//! the same drag-and-drop reorder share a position, and without the id the
//! list would shuffle on every reconnect.
//!
//! Voice channels sort *after* text channels within the same category, which is
//! how Discord draws them and is not implied by their positions: Discord keeps
//! two independent position sequences per category.
//!
//! **Threads sit under the channel they were started in**, immediately after
//! it, newest conversation first. Only active ones: a thread is archived rather
//! than deleted, Discord keeps sending the archived ones, and a year-old server
//! has thousands. A forum channel is a parent in exactly the same way — its
//! posts *are* threads — so the same rule draws it without a special case.
//!
//! **DMs.** Newest conversation first, by the last message in it. A DM with no
//! messages falls back to its own id, which is its creation time, so a freshly
//! opened conversation appears at the top rather than at the bottom.

use std::collections::HashMap;
use std::sync::Arc;

use crate::discord::model::{Channel, ChannelKind};
use crate::discord::snowflake::ChannelId;

/// The sort key within one category.
fn rank(channel: &Channel) -> (u8, i32, u64) {
    // Voice after text, then position, then id.
    let group = if channel.kind.is_voice() { 1 } else { 0 };
    (group, channel.position, channel.id.get())
}

/// Guild channels in display order, categories included as their own rows and
/// active threads under the channel they belong to.
pub fn order_guild_channels(channels: &[Arc<Channel>]) -> Vec<ChannelId> {
    let mut categories: Vec<&Arc<Channel>> = Vec::new();
    let mut uncategorised: Vec<&Arc<Channel>> = Vec::new();
    let mut by_category: HashMap<ChannelId, Vec<&Arc<Channel>>> = HashMap::new();
    let mut threads: HashMap<ChannelId, Vec<&Arc<Channel>>> = HashMap::new();

    for channel in channels {
        if channel.kind == ChannelKind::GuildCategory {
            categories.push(channel);
        } else if channel.kind.is_thread() {
            // Archived threads are dropped outright rather than sorted and
            // hidden: there are thousands of them in an old server and none of
            // them is a row.
            if !channel.is_active_thread() {
                continue;
            }
            // A thread with no parent has nowhere to go but the end, which is
            // what the orphan sweep below does with it.
            if let Some(parent) = channel.parent_id {
                threads.entry(parent).or_default().push(channel);
            }
        } else if let Some(parent) = channel.parent_id {
            by_category.entry(parent).or_default().push(channel);
        } else {
            uncategorised.push(channel);
        }
    }

    // Newest conversation first, as the DM list is ordered and for the same
    // reason: a thread's position is its activity, not a number somebody set.
    for children in threads.values_mut() {
        children.sort_by_key(|c| std::cmp::Reverse(dm_recency(c)));
    }

    categories.sort_by_key(|c| rank(c));
    uncategorised.sort_by_key(|c| rank(c));

    let mut out: Vec<ChannelId> = Vec::with_capacity(channels.len());
    let push = |channel: &Arc<Channel>, out: &mut Vec<ChannelId>| {
        out.push(channel.id);
        if let Some(children) = threads.get(&channel.id) {
            out.extend(children.iter().map(|c| c.id));
        }
    };

    for channel in &uncategorised {
        push(channel, &mut out);
    }
    for category in categories {
        out.push(category.id);
        if let Some(children) = by_category.get_mut(&category.id) {
            children.sort_by_key(|c| rank(c));
            for channel in children.iter() {
                push(channel, &mut out);
            }
        }
    }

    // A channel whose parent is not in this guild's list — a category that
    // arrived in a later GUILD_UPDATE, or a thread whose channel has not — would
    // otherwise vanish. Appending is not where it belongs, but it is visible,
    // and invisible is the worse failure.
    let placed: std::collections::HashSet<ChannelId> = out.iter().copied().collect();
    let mut orphans: Vec<&Arc<Channel>> = channels
        .iter()
        .filter(|c| !placed.contains(&c.id))
        .filter(|c| !c.kind.is_thread() || c.is_active_thread())
        .collect();
    orphans.sort_by_key(|c| rank(c));
    out.extend(orphans.iter().map(|c| c.id));

    out
}

/// The recency key for a DM.
pub fn dm_recency(channel: &Channel) -> u64 {
    channel
        .last_message_id
        .map(|id| id.get())
        .unwrap_or(0)
        .max(channel.id.get())
}

/// DMs and group DMs, newest conversation first.
pub fn order_dms(channels: &[Arc<Channel>]) -> Vec<ChannelId> {
    let mut dms: Vec<&Arc<Channel>> = channels.iter().filter(|c| c.kind.is_private()).collect();
    dms.sort_by(|a, b| {
        dm_recency(b)
            .cmp(&dm_recency(a))
            .then_with(|| b.id.get().cmp(&a.id.get()))
    });
    dms.iter().map(|c| c.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::MessageId;

    /// A thread hanging off `parent`, with its own activity.
    fn thread(id: u64, parent: u64, last: Option<u64>, archived: bool) -> Arc<Channel> {
        Arc::new(Channel {
            id: ChannelId(id),
            kind: ChannelKind::PublicThread,
            parent_id: Some(ChannelId(parent)),
            last_message_id: last.map(MessageId),
            thread_metadata: Some(crate::discord::model::channel::ThreadMetadata {
                archived,
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    fn channel(id: u64, kind: u8, position: i32, parent: Option<u64>) -> Arc<Channel> {
        Arc::new(Channel {
            id: ChannelId(id),
            kind: ChannelKind::from_code(kind),
            position,
            parent_id: parent.map(ChannelId),
            ..Default::default()
        })
    }

    #[test]
    fn uncategorised_channels_come_before_every_category() {
        let channels = vec![
            channel(10, 4, 0, None),     // category "first"
            channel(11, 0, 5, Some(10)), // inside it
            channel(1, 0, 0, None),      // uncategorised
        ];
        assert_eq!(
            order_guild_channels(&channels),
            vec![ChannelId(1), ChannelId(10), ChannelId(11)]
        );
    }

    #[test]
    fn a_tie_on_position_breaks_on_the_id_so_the_list_does_not_shuffle() {
        let channels = vec![
            channel(3, 0, 1, None),
            channel(2, 0, 1, None),
            channel(1, 0, 1, None),
        ];
        let once = order_guild_channels(&channels);
        let reversed: Vec<Arc<Channel>> = channels.iter().rev().cloned().collect();
        let twice = order_guild_channels(&reversed);
        assert_eq!(once, twice, "the order depended on the input order");
        assert_eq!(once, vec![ChannelId(1), ChannelId(2), ChannelId(3)]);
    }

    #[test]
    fn voice_channels_sort_after_text_in_the_same_category() {
        let channels = vec![
            channel(10, 4, 0, None),
            channel(20, 2, 0, Some(10)), // voice, position 0
            channel(21, 0, 9, Some(10)), // text, position 9
        ];
        assert_eq!(
            order_guild_channels(&channels),
            vec![ChannelId(10), ChannelId(21), ChannelId(20)],
            "Discord keeps separate position sequences for text and voice"
        );
    }

    #[test]
    fn an_active_thread_sits_under_the_channel_it_belongs_to() {
        let channels = vec![
            channel(1, 0, 0, None),
            channel(2, 0, 1, None),
            thread(11, 1, Some(500), false),
        ];
        assert_eq!(
            order_guild_channels(&channels),
            vec![ChannelId(1), ChannelId(11), ChannelId(2)],
            "the thread belongs to #1, not between the two channels"
        );
    }

    #[test]
    fn an_archived_thread_is_not_a_row_at_all() {
        let channels = vec![channel(1, 0, 0, None), thread(11, 1, Some(500), true)];
        assert_eq!(
            order_guild_channels(&channels),
            vec![ChannelId(1)],
            "an old server has thousands of these"
        );
    }

    /// A thread's place is its activity, not a position somebody set: it has
    /// none.
    #[test]
    fn threads_under_one_channel_are_newest_first() {
        let channels = vec![
            channel(1, 0, 0, None),
            thread(11, 1, Some(100), false),
            thread(12, 1, Some(900), false),
            thread(950, 1, None, false),
        ];
        assert_eq!(
            order_guild_channels(&channels),
            vec![
                ChannelId(1),
                // 950 has no messages, so it falls back to its own id — its
                // creation time, and the newest thing about it.
                ChannelId(950),
                ChannelId(12),
                ChannelId(11)
            ]
        );
    }

    /// A forum's posts are threads, so the same rule draws it with no special
    /// case at all.
    #[test]
    fn a_forums_posts_hang_off_the_forum() {
        let channels = vec![
            channel(10, 4, 0, None),      // a category
            channel(20, 15, 0, Some(10)), // the forum inside it
            thread(21, 20, Some(700), false),
            thread(22, 20, Some(800), false),
        ];
        assert_eq!(
            order_guild_channels(&channels),
            vec![ChannelId(10), ChannelId(20), ChannelId(22), ChannelId(21)]
        );
    }

    #[test]
    fn a_thread_whose_channel_is_missing_is_still_shown() {
        let channels = vec![channel(1, 0, 0, None), thread(11, 999, Some(1), false)];
        let order = order_guild_channels(&channels);
        assert!(order.contains(&ChannelId(11)), "{order:?}");
    }

    #[test]
    fn a_channel_whose_category_is_missing_is_still_shown() {
        let channels = vec![channel(1, 0, 0, None), channel(2, 0, 0, Some(999))];
        let order = order_guild_channels(&channels);
        assert!(order.contains(&ChannelId(2)), "{order:?}");
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn dms_are_newest_first_and_an_empty_one_uses_its_own_age() {
        let mut with_message = Channel {
            id: ChannelId(100),
            kind: ChannelKind::Dm,
            ..Default::default()
        };
        with_message.last_message_id = Some(MessageId(500));

        let brand_new = Channel {
            id: ChannelId(900),
            kind: ChannelKind::Dm,
            ..Default::default()
        };
        let stale = Channel {
            id: ChannelId(200),
            kind: ChannelKind::GroupDm,
            ..Default::default()
        };

        let channels = vec![Arc::new(stale), Arc::new(with_message), Arc::new(brand_new)];
        assert_eq!(
            order_dms(&channels),
            vec![ChannelId(900), ChannelId(100), ChannelId(200)]
        );
    }

    #[test]
    fn a_guild_channel_is_not_a_dm() {
        let channels = vec![channel(1, 0, 0, None)];
        assert!(order_dms(&channels).is_empty());
    }
}
