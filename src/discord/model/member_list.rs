//! The member list, which Discord sends as edits to a list nobody has.
//!
//! This is the strangest payload in the protocol and it is worth saying why
//! before reading the types. A server's member list is not fetched; it is
//! *subscribed to*, by index range — "send me rows 0 to 99 of the list as
//! somebody with my permissions would see it" — and what comes back is a stream
//! of splice operations against that window: SYNC replaces a range, INSERT and
//! DELETE shift everything below them, UPDATE replaces one row, INVALIDATE says
//! a range is no longer being sent.
//!
//! The rows are not all members. A member list is grouped — by hoisted role,
//! then online, then offline — and the group headers occupy indices of their
//! own, which is why `online` at index 4 may be the fifth row and the first
//! person. Everything here therefore works in *rows*, and a row is either a
//! header or somebody.
//!
//! `id` is the list's identity: `"everyone"` when the channel is visible to
//! everybody, and otherwise a hash of the permission overwrites that decide who
//! is in it. Two channels with the same permissions share a list, which is why
//! it is worth keeping: a SYNC for a different `id` is a different list and must
//! not be spliced into this one.

use serde::Deserialize;

use crate::discord::model::presence::{Presence, PresenceStatus};
use crate::discord::model::user::User;
use crate::discord::snowflake::{GuildId, RoleId, UserId};

/// One heading in the list, and how many people are under it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MemberGroup {
    /// A role id, or `online` or `offline`.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub count: u32,
}

impl MemberGroup {
    /// The role this group is for, if it is one rather than a status.
    pub fn role(&self) -> Option<RoleId> {
        self.id.parse().ok().map(RoleId)
    }

    /// A heading to draw. The role name is not in this payload, so a caller
    /// with a guild to hand resolves `role()` instead.
    pub fn label(&self) -> &str {
        &self.id
    }
}

/// Somebody in the list.
///
/// Their presence arrives here rather than through PRESENCE_UPDATE, because a
/// member list is the one place Discord sends presences for people this account
/// has no other reason to know about.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListMember {
    #[serde(default)]
    pub user: User,
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub roles: Vec<RoleId>,
    #[serde(default)]
    pub presence: Option<Presence>,
    #[serde(default)]
    pub premium_since: Option<String>,
}

impl ListMember {
    pub fn id(&self) -> UserId {
        self.user.id
    }

    /// What to call them here: the per-server nickname if there is one.
    pub fn display_name(&self) -> &str {
        match self.nick.as_deref().filter(|n| !n.is_empty()) {
            Some(nick) => nick,
            None => self.user.display_name(),
        }
    }

    pub fn status(&self) -> PresenceStatus {
        self.presence.as_ref().map(|p| p.status).unwrap_or_default()
    }
}

/// One row: a heading, or a person.
#[derive(Debug, Clone, Deserialize)]
pub enum MemberListItem {
    #[serde(rename = "group")]
    Group(MemberGroup),
    #[serde(rename = "member")]
    Member(Box<ListMember>),
}

/// One splice against the subscribed window.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op")]
pub enum MemberListOp {
    /// Replace this inclusive range of rows with these.
    #[serde(rename = "SYNC")]
    Sync {
        #[serde(default)]
        range: [u32; 2],
        #[serde(default)]
        items: Vec<MemberListItem>,
    },
    /// Put a row at this index; everything below it moves down.
    #[serde(rename = "INSERT")]
    Insert {
        #[serde(default)]
        index: u32,
        item: MemberListItem,
    },
    /// Replace the row at this index.
    #[serde(rename = "UPDATE")]
    Update {
        #[serde(default)]
        index: u32,
        item: MemberListItem,
    },
    /// Take the row at this index out; everything below it moves up.
    #[serde(rename = "DELETE")]
    Delete {
        #[serde(default)]
        index: u32,
    },
    /// This range is no longer being sent. Whatever is held for it is stale.
    #[serde(rename = "INVALIDATE")]
    Invalidate {
        #[serde(default)]
        range: [u32; 2],
    },
}

impl MemberListOp {
    pub fn name(&self) -> &'static str {
        match self {
            MemberListOp::Sync { .. } => "SYNC",
            MemberListOp::Insert { .. } => "INSERT",
            MemberListOp::Update { .. } => "UPDATE",
            MemberListOp::Delete { .. } => "DELETE",
            MemberListOp::Invalidate { .. } => "INVALIDATE",
        }
    }
}

/// GUILD_MEMBER_LIST_UPDATE.
#[derive(Debug, Clone, Deserialize)]
pub struct MemberListUpdate {
    pub guild_id: GuildId,
    /// Which list this is: `everyone`, or a hash of the permission overwrites.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub member_count: u32,
    #[serde(default)]
    pub online_count: u32,
    /// Every group in the whole list, not just the subscribed window, which is
    /// what makes "N others" under a heading possible.
    #[serde(default)]
    pub groups: Vec<MemberGroup>,
    #[serde(default)]
    pub ops: Vec<MemberListOp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hand-written fixture, which `testdata/gateway/README.md` records as
    /// synthetic.
    fn fixture() -> MemberListUpdate {
        let text = include_str!("../../../testdata/gateway/guild_member_list_update.json");
        let envelope: serde_json::Value = serde_json::from_str(text).unwrap();
        serde_json::from_value(envelope["d"].clone()).expect("the fixture parses")
    }

    #[test]
    fn the_fixture_carries_a_sync_of_groups_and_people() {
        let update = fixture();
        assert_eq!(update.guild_id, GuildId(200000000000000001));
        assert_eq!(update.id, "everyone");
        assert_eq!(update.member_count, 42);
        assert_eq!(update.online_count, 7);
        assert_eq!(update.groups.len(), 3);

        let Some(MemberListOp::Sync { range, items }) = update.ops.first() else {
            panic!("{:?}", update.ops.first().map(MemberListOp::name));
        };
        assert_eq!(*range, [0, 99]);
        assert_eq!(items.len(), 6);

        match &items[0] {
            MemberListItem::Group(group) => {
                assert_eq!(group.label(), "300000000000000001");
                assert_eq!(group.role(), Some(RoleId(300000000000000001)));
                assert_eq!(group.count, 2);
            }
            other => panic!("{other:?}"),
        }
        match &items[1] {
            MemberListItem::Member(member) => {
                assert_eq!(member.display_name(), "Mod Alex");
                assert_eq!(member.id(), UserId(100000000000000002));
                assert_eq!(member.status(), PresenceStatus::Online);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A group whose id is `online` is a status heading, not a role.
    #[test]
    fn a_status_heading_is_not_a_role() {
        let online = MemberGroup {
            id: "online".into(),
            count: 5,
        };
        assert_eq!(online.role(), None);
        assert_eq!(online.label(), "online");

        let role = MemberGroup {
            id: "300000000000000001".into(),
            count: 2,
        };
        assert_eq!(role.role(), Some(RoleId(300000000000000001)));
    }

    #[test]
    fn every_op_is_recognised_by_its_name() {
        let ops: Vec<MemberListOp> = serde_json::from_str(
            r#"[
                {"op":"SYNC","range":[0,99],"items":[]},
                {"op":"INSERT","index":3,"item":{"member":{"user":{"id":"1"}}}},
                {"op":"UPDATE","index":3,"item":{"member":{"user":{"id":"1"}}}},
                {"op":"DELETE","index":3},
                {"op":"INVALIDATE","range":[100,199]}
            ]"#,
        )
        .unwrap();
        let names: Vec<&str> = ops.iter().map(MemberListOp::name).collect();
        assert_eq!(names, ["SYNC", "INSERT", "UPDATE", "DELETE", "INVALIDATE"]);
    }

    /// A member with no nickname is called whatever their account is called.
    #[test]
    fn a_nickname_wins_and_an_empty_one_does_not() {
        let member: ListMember = serde_json::from_str(
            r#"{"user":{"id":"1","username":"alex","global_name":"Alex"},"nick":""}"#,
        )
        .unwrap();
        assert_eq!(member.display_name(), "Alex");

        let nicked: ListMember = serde_json::from_str(
            r#"{"user":{"id":"1","username":"alex","global_name":"Alex"},"nick":"Mod Alex"}"#,
        )
        .unwrap();
        assert_eq!(nicked.display_name(), "Mod Alex");
    }

    proptest::proptest! {
        /// An op this client has not heard of, or one with the wrong shape,
        /// must be an `Err` rather than a panic.
        #[test]
        fn a_mangled_op_is_refused_rather_than_a_panic(
            body in r#"\{"op":"(SYNC|INSERT|BOGUS)"(,"(range|index|items)":(null|1|"x"|\[\]|\{\})){0,3}\}"#,
        ) {
            let _ = serde_json::from_str::<MemberListOp>(&body);
        }
    }
}
