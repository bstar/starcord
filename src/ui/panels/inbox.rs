//! What is waiting: the conversations with something unread in them, each
//! with its last message, and then the rest.
//!
//! Drawn in the conversation module when nothing is open, which is what the
//! window shows before the first conversation and what `alt+i` gets back to.
//! The DM list in the column says which conversations are unread; this says
//! what is in them, which is the question somebody with a few people waiting
//! on a reply is actually asking. Two lines a conversation: who and when on
//! the first, what they said on the second, so a screenful can be read in
//! one pass rather than opened one at a time.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::dms::{presence_colour, presence_glyph};
use super::{empty, fit, rgb};
use crate::discord::model::PresenceStatus;
use crate::discord::snowflake::ChannelId;
use crate::ui::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// `waiting — 3`, `the rest`. Not selectable.
    Section { label: String },
    Conversation {
        id: ChannelId,
        title: String,
        presence: PresenceStatus,
        unread: bool,
        mentions: u32,
        muted: bool,
        /// How many people are in it, for a group; `None` for a pair.
        members: Option<usize>,
        /// The last message, once it is known: who said it, when, and the
        /// text with the markdown taken out and collapsed to one line.
        last: Option<Last>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Last {
    pub author: String,
    pub when: String,
    pub text: String,
}

impl Row {
    pub fn selectable(&self) -> bool {
        matches!(self, Row::Conversation { .. })
    }

    /// The lines it takes: a heading is one, a conversation is two when its
    /// last message is known and one until then.
    pub fn height(&self) -> u16 {
        match self {
            Row::Section { .. } => 1,
            Row::Conversation { last, .. } => 1 + u16::from(last.is_some()),
        }
    }
}

pub struct View<'a> {
    pub theme: &'a Theme,
    pub rows: &'a [Row],
    pub cursor: usize,
    /// The first row drawn, as an index into `rows`.
    pub scroll: usize,
    pub focused: bool,
}

/// The row under a line of the body, from the same layout `render` draws.
pub fn row_at(body: Rect, v: &View<'_>, y: u16) -> Option<usize> {
    if y < body.y || y >= body.y + body.height {
        return None;
    }
    let mut line = body.y;
    for (index, row) in v.rows.iter().enumerate().skip(v.scroll) {
        let next = line.saturating_add(row.height());
        if y < next {
            return Some(index);
        }
        line = next;
    }
    None
}

/// The scroll that keeps `cursor` on a body this tall, moving only as far as
/// it must. Rows have two heights, so this walks them rather than dividing.
pub fn clamp_scroll(rows: &[Row], cursor: usize, scroll: usize, height: u16) -> usize {
    let mut scroll = scroll.min(cursor);
    loop {
        let used: u16 = rows[scroll..=cursor.min(rows.len().saturating_sub(1))]
            .iter()
            .map(Row::height)
            .sum();
        if used <= height || scroll >= cursor {
            return scroll;
        }
        scroll += 1;
    }
}

pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) {
    let t = v.theme;
    if v.rows.is_empty() {
        empty(body, buf, t, "no conversations");
        return;
    }
    let width = body.width;
    let mut y = body.y;
    let bottom = body.y + body.height;

    for (index, row) in v.rows.iter().enumerate().skip(v.scroll) {
        if y >= bottom {
            break;
        }
        let selected = index == v.cursor && row.selectable() && v.focused;
        match row {
            Row::Section { label } => {
                buf.set_string(
                    body.x,
                    y,
                    fit(&label.to_uppercase(), width),
                    Style::default().fg(rgb(t.row_meta_fg)),
                );
                y += 1;
            }
            Row::Conversation {
                title,
                presence,
                unread,
                mentions,
                muted,
                members,
                last,
                ..
            } => {
                let badge = if *mentions > 0 {
                    format!(" ({mentions})")
                } else if let Some(n) = members.filter(|n| *n > 0) {
                    format!(" [{n}]")
                } else {
                    String::new()
                };
                let fg = if *mentions > 0 {
                    t.chat.mention_fg
                } else if *muted {
                    t.dim
                } else if *unread {
                    t.chat.unread_fg
                } else {
                    t.row_fg
                };
                let mut style = Style::default().fg(rgb(fg));
                if *unread && !*muted {
                    style = style.add_modifier(Modifier::BOLD);
                }
                let bg = if selected {
                    style = style.fg(rgb(t.row_selected_fg)).bg(rgb(t.row_selected_bg));
                    t.row_selected_bg
                } else {
                    t.panel_bg
                };
                // The whole width, so a selected conversation reads as one
                // block across both of its lines.
                if selected {
                    for line in 0..row.height() {
                        if y + line < bottom {
                            buf.set_string(
                                body.x,
                                y + line,
                                " ".repeat(usize::from(width)),
                                Style::default().bg(rgb(bg)),
                            );
                        }
                    }
                }
                // The dot keeps its own colour, as it does in the column: it
                // is the one thing on the line that means something by it.
                let dot = Style::default().fg(rgb(presence_colour(t, *presence))).bg(rgb(bg));
                buf.set_string(body.x, y, presence_glyph(*presence).to_string(), dot);
                let when = last.as_ref().map(|l| l.when.as_str()).unwrap_or("");
                let name_width = width.saturating_sub(1 + when.len() as u16 + 1);
                buf.set_string(
                    body.x + 1,
                    y,
                    fit(&format!(" {title}{badge}"), name_width),
                    style,
                );
                if !when.is_empty() && width > when.len() as u16 + 4 {
                    buf.set_string(
                        body.x + width - when.len() as u16,
                        y,
                        when,
                        Style::default().fg(rgb(if selected { t.row_selected_fg } else { t.dim })).bg(rgb(bg)),
                    );
                }
                y += 1;
                if let Some(last) = last {
                    if y < bottom {
                        let text = if last.author.is_empty() {
                            format!("  {}", last.text)
                        } else {
                            format!("  {}: {}", last.author, last.text)
                        };
                        let fg = if selected {
                            t.row_selected_fg
                        } else if *unread && !*muted {
                            t.fg
                        } else {
                            t.dim
                        };
                        buf.set_string(
                            body.x,
                            y,
                            fit(&text, width),
                            Style::default().fg(rgb(fg)).bg(rgb(bg)),
                        );
                        y += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(id: u64, last: bool) -> Row {
        Row::Conversation {
            id: ChannelId(id),
            title: format!("person {id}"),
            presence: PresenceStatus::Online,
            unread: true,
            mentions: 0,
            muted: false,
            members: None,
            last: last.then(|| Last {
                author: "them".into(),
                when: "09:41".into(),
                text: "are we still on for tonight?".into(),
            }),
        }
    }

    /// Rows are two lines when the last message is known, and the hit test
    /// and the scroll both count them that way.
    #[test]
    fn a_conversation_with_a_preview_is_two_lines() {
        let rows = vec![
            Row::Section {
                label: "waiting".into(),
            },
            conversation(1, true),
            conversation(2, false),
            conversation(3, true),
        ];
        let t = crate::ui::theme::tests_support::theme("terminal");
        let v = View {
            theme: &t,
            rows: &rows,
            cursor: 1,
            scroll: 0,
            focused: true,
        };
        let body = Rect::new(0, 0, 40, 10);
        assert_eq!(row_at(body, &v, 0), Some(0), "the heading");
        assert_eq!(row_at(body, &v, 1), Some(1));
        assert_eq!(row_at(body, &v, 2), Some(1), "its second line is still it");
        assert_eq!(row_at(body, &v, 3), Some(2));
        assert_eq!(row_at(body, &v, 4), Some(3));
        assert_eq!(row_at(body, &v, 6), None);

        // Four lines of body: the heading and the first conversation fit;
        // moving to the third has to scroll past both.
        assert_eq!(clamp_scroll(&rows, 1, 0, 4), 0);
        assert_eq!(clamp_scroll(&rows, 3, 0, 4), 2);
        // And back up never scrolls further down than the cursor.
        assert_eq!(clamp_scroll(&rows, 1, 2, 4), 1);

        let mut buf = Buffer::empty(body);
        render(body, &mut buf, &v);
        let line = |y: u16| -> String {
            (0..40).map(|x| buf[(x, y)].symbol().to_string()).collect()
        };
        assert_eq!(line(0).trim_end(), "WAITING");
        assert!(line(1).contains("person 1"));
        assert!(line(1).ends_with("09:41"), "{:?}", line(1));
        assert!(line(2).contains("them: are we still on"));
        assert!(line(3).contains("person 2"));
    }
}
