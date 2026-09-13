//! Which messages belong together, and what rows the list is made of.
//!
//! Two decisions live here and nowhere else. **Grouping**: whether a message
//! is drawn with a name and a time above it or run on under the one before.
//! And **the row list**: the sequence of things the virtual list scrolls
//! through, which is the messages plus the day dividers, the unread marker,
//! the spinner at the top and the typing line at the bottom.
//!
//! Rows exist as a separate list rather than as a flag on each message
//! because a divider is a thing you can scroll onto and stop at. Folding it
//! into the message below it would mean a message whose height depends on
//! whether the day changed above it, and a cache key that has to carry its
//! neighbour.

use std::sync::Arc;

use crate::discord::model::Message;
use crate::discord::snowflake::MessageId;

/// One thing the message list can scroll onto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The spinner at the top, while older messages are being fetched, or the
    /// invitation to fetch them.
    LoadOlder,
    /// `──── Friday, 12 September ────`.
    Day(String),
    /// `──── new messages ────`, above the first one the reader has not seen.
    NewMessages,
    /// A message, by its index into the window.
    Message { index: usize, head: bool },
    /// Something sent that the gateway has not echoed back yet.
    Pending { index: usize },
    /// `alex is typing…`, always last.
    Typing,
}

impl Row {
    /// The message a row is about, for the cursor and for `r`, `e`, `d`.
    pub fn message(&self, messages: &[Arc<Message>]) -> Option<Arc<Message>> {
        match self {
            Row::Message { index, .. } => messages.get(*index).cloned(),
            _ => None,
        }
    }

    /// Whether the cursor is allowed to stop here. Dividers are landmarks
    /// rather than destinations: stopping on one would mean `r` doing nothing
    /// on every third press of `j`.
    pub fn selectable(&self) -> bool {
        matches!(self, Row::Message { .. })
    }
}

/// Whether a message starts a new group, given the one before it.
///
/// Five reasons to break, and each is a row in the table test:
///
/// - nothing before it;
/// - a different author;
/// - a different kind of message, which is what keeps a join notice out of
///   somebody's paragraph;
/// - a reply, because the `↩` line it carries has to sit under a name to say
///   whose reply it is;
/// - more than the configured window since the one before.
pub fn starts_group(previous: Option<&Message>, msg: &Message, window_secs: u64) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    // A system message never groups, in either direction: it is not somebody
    // talking and drawing it as a continuation of somebody talking is wrong.
    if msg.kind.is_system() || previous.kind.is_system() {
        return true;
    }
    if previous.author.id != msg.author.id {
        return true;
    }
    if msg.reply_target().is_some() || msg.referenced_message.is_some() {
        return true;
    }
    let (Some(a), Some(b)) = (previous.timestamp, msg.timestamp) else {
        // A message with no timestamp is a message from a payload this client
        // did not fully understand; keeping it separate is the honest answer.
        return true;
    };
    let gap = b.as_second() - a.as_second();
    gap < 0 || gap > window_secs as i64
}

/// What the day divider above a message says, or `None` when the message
/// before it was on the same day.
fn day_of(msg: &Message, tz: &jiff::tz::TimeZone) -> Option<(i32, u16, String)> {
    let at = msg.timestamp?;
    let zoned = at.to_zoned(tz.clone());
    let year = zoned.year() as i32;
    let day = zoned.day_of_year() as u16;
    Some((year, day, zoned.strftime("%A, %-d %B %Y").to_string()))
}

/// Everything the row list is built out of.
pub struct Shape<'a> {
    pub messages: &'a [Arc<Message>],
    pub pending: usize,
    pub group_window_secs: u64,
    pub has_older: bool,
    /// The oldest message the reader has not seen, from the read state.
    pub first_unread: Option<MessageId>,
    pub typing: bool,
    pub tz: jiff::tz::TimeZone,
}

/// Build the rows for one channel's window.
pub fn rows(shape: &Shape<'_>) -> Vec<Row> {
    let mut out = Vec::with_capacity(shape.messages.len() + 8);
    if shape.has_older {
        out.push(Row::LoadOlder);
    }

    let mut last_day: Option<(i32, u16)> = None;
    let mut unread_drawn = false;
    for (index, msg) in shape.messages.iter().enumerate() {
        if let Some((year, day, label)) = day_of(msg, &shape.tz) {
            if last_day != Some((year, day)) {
                out.push(Row::Day(label));
                last_day = Some((year, day));
            }
        }
        if !unread_drawn && shape.first_unread == Some(msg.id) {
            out.push(Row::NewMessages);
            unread_drawn = true;
        }
        let previous = index.checked_sub(1).and_then(|i| shape.messages.get(i));
        // A divider always breaks the group: a name that appears under a date
        // heading reads as the start of the day's conversation, which it is.
        let broken = matches!(out.last(), Some(Row::Day(_)) | Some(Row::NewMessages));
        let head = broken || starts_group(previous.map(Arc::as_ref), msg, shape.group_window_secs);
        out.push(Row::Message { index, head });
    }

    for index in 0..shape.pending {
        out.push(Row::Pending { index });
    }
    if shape.typing {
        out.push(Row::Typing);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::model::{MessageKind, MessageReference, User};
    use crate::discord::snowflake::{ChannelId, UserId};

    fn msg(id: u64, author: u64, at: i64) -> Message {
        Message {
            id: MessageId(id),
            channel_id: ChannelId(1),
            author: User {
                id: UserId(author),
                username: format!("u{author}"),
                ..User::default()
            },
            timestamp: jiff::Timestamp::from_second(at).ok(),
            ..Message::default()
        }
    }

    /// The grouping table. Each row is a reason a block breaks, and the last
    /// is the only case where it does not.
    #[test]
    fn the_grouping_rules() {
        let window = 420;
        let base = msg(1, 10, 1_000_000);

        assert!(starts_group(None, &base, window), "nothing before it");

        let other_author = msg(2, 11, 1_000_010);
        assert!(
            starts_group(Some(&base), &other_author, window),
            "a different author"
        );

        let mut system = msg(3, 10, 1_000_010);
        system.kind = MessageKind::UserJoin;
        assert!(
            starts_group(Some(&base), &system, window),
            "a system message never joins a block"
        );
        assert!(
            starts_group(Some(&system), &msg(4, 10, 1_000_020), window),
            "and nothing joins one"
        );

        let mut reply = msg(5, 10, 1_000_010);
        reply.message_reference = Some(MessageReference {
            message_id: Some(MessageId(1)),
            ..MessageReference::default()
        });
        assert!(
            starts_group(Some(&base), &reply, window),
            "a reply breaks it"
        );

        let late = msg(6, 10, 1_000_000 + window as i64 + 1);
        assert!(starts_group(Some(&base), &late, window), "past the window");

        let edge = msg(7, 10, 1_000_000 + window as i64);
        assert!(
            !starts_group(Some(&base), &edge, window),
            "exactly the window still groups"
        );

        let soon = msg(8, 10, 1_000_030);
        assert!(
            !starts_group(Some(&base), &soon, window),
            "the same person, a moment later"
        );
    }

    /// A clock that went backwards is not a group: out-of-order timestamps
    /// happen on a resume, and running them together would hide it.
    #[test]
    fn a_message_from_before_the_one_above_it_starts_a_block() {
        let base = msg(1, 10, 1_000_000);
        let earlier = msg(2, 10, 999_000);
        assert!(starts_group(Some(&base), &earlier, 420));
    }

    fn window(ids: &[(u64, u64, i64)]) -> Vec<Arc<Message>> {
        ids.iter()
            .map(|(id, author, at)| Arc::new(msg(*id, *author, *at)))
            .collect()
    }

    #[test]
    fn the_rows_carry_the_dividers_and_the_spinner() {
        // Two days apart, so a day divider falls between them.
        let messages = window(&[(1, 10, 0), (2, 10, 30), (3, 11, 200_000)]);
        let rows = rows(&Shape {
            messages: &messages,
            pending: 1,
            group_window_secs: 420,
            has_older: true,
            first_unread: Some(MessageId(3)),
            typing: true,
            tz: jiff::tz::TimeZone::UTC,
        });

        assert_eq!(rows.first(), Some(&Row::LoadOlder));
        assert_eq!(rows.last(), Some(&Row::Typing));
        assert!(rows.contains(&Row::NewMessages));
        assert_eq!(
            rows.iter().filter(|r| matches!(r, Row::Day(_))).count(),
            2,
            "one divider per day, and no more: {rows:?}"
        );
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, Row::Pending { .. }))
                .count(),
            1
        );

        // The second message runs on under the first; the third does not.
        let heads: Vec<bool> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Message { head, .. } => Some(*head),
                _ => None,
            })
            .collect();
        assert_eq!(heads, vec![true, false, true]);
    }

    /// A divider above a message always gives it its own name row, or the day
    /// heading would sit over a paragraph with nobody's name on it.
    #[test]
    fn a_divider_starts_a_new_block() {
        let messages = window(&[(1, 10, 0), (2, 10, 30)]);
        let rows = rows(&Shape {
            messages: &messages,
            pending: 0,
            group_window_secs: 420,
            has_older: false,
            first_unread: Some(MessageId(2)),
            typing: false,
            tz: jiff::tz::TimeZone::UTC,
        });
        let heads: Vec<bool> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Message { head, .. } => Some(*head),
                _ => None,
            })
            .collect();
        assert_eq!(heads, vec![true, true]);
    }

    #[test]
    fn only_messages_take_the_cursor() {
        assert!(Row::Message {
            index: 0,
            head: true
        }
        .selectable());
        assert!(!Row::Day("x".into()).selectable());
        assert!(!Row::NewMessages.selectable());
        assert!(!Row::LoadOlder.selectable());
        assert!(!Row::Typing.selectable());
    }
}
