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

/// Guild channels in display order, categories included as their own rows.
pub fn order_guild_channels(channels: &[Arc<Channel>]) -> Vec<ChannelId> {
    let mut categories: Vec<&Arc<Channel>> = Vec::new();
    let mut uncategorised: Vec<&Arc<Channel>> = Vec::new();
    let mut by_category: HashMap<ChannelId, Vec<&Arc<Channel>>> = HashMap::new();

    for channel in channels {
        if channel.kind == ChannelKind::GuildCategory {
            categories.push(channel);
        } else if channel.kind.is_thread() {
            // Threads hang off their parent channel and are not rows in the
            // channel list; the chat panel shows them under the message that
            // started them.
            continue;
        } else if let Some(parent) = channel.parent_id {
            by_category.entry(parent).or_default().push(channel);
        } else {
            uncategorised.push(channel);
        }
    }

    categories.sort_by_key(|c| (c.position, c.id.get()));
    uncategorised.sort_by_key(|c| rank(c));

    let mut out: Vec<ChannelId> = uncategorised.iter().map(|c| c.id).collect();
    for category in categories {
        out.push(category.id);
        if let Some(children) = by_category.get_mut(&category.id) {
            children.sort_by_key(|c| rank(c));
            out.extend(children.iter().map(|c| c.id));
        }
    }

    // A channel whose parent is not in this guild's list — a category that
    // arrived in a later GUILD_UPDATE, most likely — would otherwise vanish.
    // Appending is not where it belongs, but it is visible, and invisible is
    // the worse failure.
    let placed: std::collections::HashSet<ChannelId> = out.iter().copied().collect();
    let mut orphans: Vec<&Arc<Channel>> = channels
        .iter()
        .filter(|c| !placed.contains(&c.id) && !c.kind.is_thread())
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
    fn threads_are_not_rows_in_the_channel_list() {
        let channels = vec![channel(1, 0, 0, None), channel(2, 11, 0, Some(1))];
        assert_eq!(order_guild_channels(&channels), vec![ChannelId(1)]);
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
