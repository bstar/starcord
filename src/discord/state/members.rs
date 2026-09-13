//! The member list a server sends, and the splices it sends against it.
//!
//! See [`crate::discord::model::member_list`] for what the payload is. What
//! this file adds is the one rule that keeps it honest: **the rows held here
//! are only ever the window that was subscribed to**, starting at index zero,
//! and every operation is applied against that window's own indices.
//!
//! Two things follow from that and neither is obvious.
//!
//! An index past the end of what is held is not an error and not a gap to fill
//! with blanks. It is a row in a part of the list this client did not ask for,
//! and the right answer is to ignore it: Discord sends operations for the whole
//! list, not for the window.
//!
//! A SYNC for a different list `id` replaces everything rather than splicing.
//! The `id` is the permission set the list was computed for, so a SYNC carrying
//! a different one is a different list that happens to have arrived on the same
//! guild.

use std::sync::Arc;

use crate::discord::model::member_list::{
    ListMember, MemberGroup, MemberListItem, MemberListOp, MemberListUpdate,
};
use crate::discord::snowflake::UserId;

/// One row of the list as it is drawn.
#[derive(Debug, Clone)]
pub enum MemberRow {
    Group(MemberGroup),
    Member(Arc<ListMember>),
}

impl MemberRow {
    fn from_item(item: MemberListItem) -> Self {
        match item {
            MemberListItem::Group(group) => MemberRow::Group(group),
            MemberListItem::Member(member) => MemberRow::Member(Arc::from(member)),
        }
    }

    pub fn member(&self) -> Option<&Arc<ListMember>> {
        match self {
            MemberRow::Member(member) => Some(member),
            MemberRow::Group(_) => None,
        }
    }
}

/// One guild's member list, as far as this client has been told about it.
#[derive(Debug, Clone, Default)]
pub struct MemberList {
    /// Which list this is: `everyone`, or a hash of the permission overwrites.
    /// A SYNC carrying a different one is a different list.
    pub id: String,
    pub member_count: u32,
    pub online_count: u32,
    /// Every heading in the whole list, including the ones below the window.
    pub groups: Vec<MemberGroup>,
    rows: Vec<MemberRow>,
}

impl MemberList {
    pub fn rows(&self) -> &[MemberRow] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Everybody in the window, headings dropped.
    pub fn members(&self) -> Vec<Arc<ListMember>> {
        self.rows
            .iter()
            .filter_map(MemberRow::member)
            .cloned()
            .collect()
    }

    pub fn member(&self, id: UserId) -> Option<Arc<ListMember>> {
        self.rows
            .iter()
            .filter_map(MemberRow::member)
            .find(|m| m.id() == id)
            .cloned()
    }

    /// Apply one payload, and say whether anything changed.
    pub fn apply(&mut self, update: MemberListUpdate) -> bool {
        let mut changed = false;

        if self.id != update.id {
            // A different permission set is a different list. Splicing one into
            // the other would interleave two servers' worth of people.
            self.id = update.id.clone();
            self.rows.clear();
            changed = true;
        }
        if self.member_count != update.member_count || self.online_count != update.online_count {
            self.member_count = update.member_count;
            self.online_count = update.online_count;
            changed = true;
        }
        if self.groups != update.groups {
            self.groups = update.groups;
            changed = true;
        }

        for op in update.ops {
            changed |= self.apply_op(op);
        }
        changed
    }

    fn apply_op(&mut self, op: MemberListOp) -> bool {
        match op {
            MemberListOp::Sync { range, items } => {
                let start = range[0] as usize;
                if start > self.rows.len() {
                    // A window this client did not subscribe to. Filling the
                    // gap with blanks would put holes in the list.
                    tracing::debug!(
                        "ignoring a SYNC of {range:?} against {} rows",
                        self.rows.len()
                    );
                    return false;
                }
                let end = (range[1] as usize).saturating_add(1).min(self.rows.len());
                let replacement: Vec<MemberRow> =
                    items.into_iter().map(MemberRow::from_item).collect();
                self.rows.splice(start..end.max(start), replacement);
                true
            }
            MemberListOp::Insert { index, item } => {
                let at = index as usize;
                if at > self.rows.len() {
                    return false;
                }
                self.rows.insert(at, MemberRow::from_item(item));
                true
            }
            MemberListOp::Update { index, item } => {
                let at = index as usize;
                let Some(row) = self.rows.get_mut(at) else {
                    return false;
                };
                *row = MemberRow::from_item(item);
                true
            }
            MemberListOp::Delete { index } => {
                let at = index as usize;
                if at >= self.rows.len() {
                    return false;
                }
                self.rows.remove(at);
                true
            }
            MemberListOp::Invalidate { range } => {
                let start = range[0] as usize;
                if start >= self.rows.len() {
                    return false;
                }
                let end = (range[1] as usize).saturating_add(1).min(self.rows.len());
                self.rows.drain(start..end);
                true
            }
        }
    }
}

/// Cut the ranges the UI asked for down to what may be sent.
///
/// Discord takes at most three ranges of at most a hundred rows each. A request
/// for more is not refused, it is *ignored* — the subscription silently never
/// arrives — which is the worst way for a limit to be enforced and the reason
/// this is a function rather than a comment.
///
/// Ranges are inclusive, so a hundred rows is `[0, 99]`.
pub fn clamp_ranges(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    /// Discord's own maximum.
    const MAX_RANGES: usize = 3;
    const MAX_ROWS: u32 = 100;

    let mut out: Vec<(u32, u32)> = ranges
        .iter()
        .map(|&(start, end)| {
            let end = end.max(start);
            (start, end.min(start.saturating_add(MAX_ROWS - 1)))
        })
        .take(MAX_RANGES)
        .collect();
    out.sort_by_key(|&(start, _)| start);
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::model::presence::PresenceStatus;

    fn fixture() -> MemberListUpdate {
        let text = include_str!("../../../testdata/gateway/guild_member_list_update.json");
        let envelope: serde_json::Value = serde_json::from_str(text).unwrap();
        serde_json::from_value(envelope["d"].clone()).unwrap()
    }

    fn labels(list: &MemberList) -> Vec<String> {
        list.rows()
            .iter()
            .map(|row| match row {
                MemberRow::Group(group) => format!("[{}]", group.label()),
                MemberRow::Member(member) => member.display_name().to_string(),
            })
            .collect()
    }

    fn member(id: u64, name: &str) -> MemberListItem {
        serde_json::from_value(serde_json::json!({
            "member": {"user": {"id": id.to_string(), "username": name}}
        }))
        .unwrap()
    }

    #[test]
    fn a_sync_from_the_fixture_becomes_headings_and_people() {
        let mut list = MemberList::default();
        assert!(list.apply(fixture()));

        assert_eq!(
            labels(&list),
            vec![
                "[300000000000000001]",
                "Mod Alex",
                "Jordan",
                "[online]",
                "Sam",
                "[offline]"
            ]
        );
        assert_eq!(list.member_count, 42);
        assert_eq!(list.online_count, 7);
        assert_eq!(list.groups.len(), 3);
        assert_eq!(list.members().len(), 3, "a heading is not a person");

        let alex = list.member(UserId(100000000000000002)).unwrap();
        assert_eq!(alex.status(), PresenceStatus::Online);
        assert!(
            list.member(UserId(999)).is_none(),
            "somebody who is not in the window is not in the list"
        );
    }

    #[test]
    fn an_insert_moves_everything_below_it_down() {
        let mut list = MemberList::default();
        list.apply(fixture());

        list.apply(MemberListUpdate {
            guild_id: crate::discord::snowflake::GuildId(200000000000000001),
            id: "everyone".into(),
            member_count: 43,
            online_count: 8,
            groups: fixture().groups,
            ops: vec![MemberListOp::Insert {
                index: 4,
                item: member(100000000000000009, "robin"),
            }],
        });

        assert_eq!(
            labels(&list),
            vec![
                "[300000000000000001]",
                "Mod Alex",
                "Jordan",
                "[online]",
                "robin",
                "Sam",
                "[offline]"
            ]
        );
        assert_eq!(list.member_count, 43);
    }

    #[test]
    fn an_update_replaces_one_row_and_a_delete_closes_the_gap() {
        let mut list = MemberList::default();
        list.apply(fixture());

        // Everything but the ops is what the list already holds, so a `true`
        // from `apply` can only have come from the splice.
        let update = |ops: Vec<MemberListOp>| MemberListUpdate {
            guild_id: crate::discord::snowflake::GuildId(200000000000000001),
            id: "everyone".into(),
            member_count: 42,
            online_count: 7,
            groups: fixture().groups,
            ops,
        };

        list.apply(update(vec![MemberListOp::Update {
            index: 2,
            item: member(100000000000000003, "jordan-renamed"),
        }]));
        assert_eq!(labels(&list)[2], "jordan-renamed");

        list.apply(update(vec![MemberListOp::Delete { index: 1 }]));
        assert_eq!(
            labels(&list),
            vec![
                "[300000000000000001]",
                "jordan-renamed",
                "[online]",
                "Sam",
                "[offline]"
            ]
        );
    }

    #[test]
    fn an_invalidate_takes_the_range_away() {
        let mut list = MemberList::default();
        list.apply(fixture());

        list.apply(MemberListUpdate {
            guild_id: crate::discord::snowflake::GuildId(200000000000000001),
            id: "everyone".into(),
            member_count: 42,
            online_count: 7,
            groups: fixture().groups,
            ops: vec![MemberListOp::Invalidate { range: [0, 99] }],
        });
        assert!(list.is_empty(), "{:?}", labels(&list));
    }

    /// An operation against a window nobody subscribed to is ignored rather
    /// than filling the list with blanks to reach it.
    #[test]
    fn an_index_past_the_window_changes_nothing() {
        let mut list = MemberList::default();
        list.apply(fixture());
        let before = labels(&list);

        // Everything but the ops is what the list already holds, so a `true`
        // from `apply` can only have come from the splice.
        let update = |ops: Vec<MemberListOp>| MemberListUpdate {
            guild_id: crate::discord::snowflake::GuildId(200000000000000001),
            id: "everyone".into(),
            member_count: 42,
            online_count: 7,
            groups: fixture().groups,
            ops,
        };

        assert!(!list.apply(update(vec![MemberListOp::Delete { index: 500 }])));
        assert!(!list.apply(update(vec![MemberListOp::Update {
            index: 500,
            item: member(1, "nobody")
        }])));
        assert!(!list.apply(update(vec![MemberListOp::Insert {
            index: 500,
            item: member(1, "nobody")
        }])));
        assert_eq!(labels(&list), before);
    }

    /// The `id` is the permission set the list was computed for. A different
    /// one is a different list, not more of this one.
    #[test]
    fn a_sync_for_a_different_list_replaces_rather_than_splices() {
        let mut list = MemberList::default();
        list.apply(fixture());

        list.apply(MemberListUpdate {
            guild_id: crate::discord::snowflake::GuildId(200000000000000001),
            id: "a9f3c1".into(),
            member_count: 3,
            online_count: 1,
            groups: vec![MemberGroup {
                id: "online".into(),
                count: 1,
            }],
            ops: vec![MemberListOp::Sync {
                range: [0, 99],
                items: vec![member(100000000000000004, "robin")],
            }],
        });

        assert_eq!(labels(&list), vec!["robin"]);
        assert_eq!(list.id, "a9f3c1");
    }

    /// Discord ignores a subscription asking for too much rather than refusing
    /// it, so the list simply never arrives. Clamping is the only defence.
    #[test]
    fn ranges_are_cut_to_three_windows_of_a_hundred() {
        assert_eq!(clamp_ranges(&[(0, 99)]), vec![(0, 99)]);
        assert_eq!(
            clamp_ranges(&[(0, 5000)]),
            vec![(0, 99)],
            "a hundred rows is [0, 99], inclusive"
        );
        assert_eq!(
            clamp_ranges(&[(200, 299), (0, 99), (100, 199), (300, 399)]),
            vec![(0, 99), (100, 199), (200, 299)],
            "the fourth range is dropped and the rest are ordered"
        );
        assert_eq!(
            clamp_ranges(&[(50, 10)]),
            vec![(50, 50)],
            "a backwards range is one row, not a panic"
        );
        assert_eq!(clamp_ranges(&[]), Vec::new());
        assert_eq!(
            clamp_ranges(&[(u32::MAX - 5, u32::MAX)]),
            vec![(u32::MAX - 5, u32::MAX)],
            "the arithmetic must not wrap"
        );
    }
}
