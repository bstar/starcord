//! Direct messages, and the friends who have not started one.
//!
//! Two lists behind one tab, because they are the same question asked twice:
//! the DM list is who you have been talking to, the friends list is who you
//! could be. Both are flat vectors of rows for the same reason the channel
//! list is.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::{empty, rgb, DmTab};
use crate::discord::model::PresenceStatus;
use crate::discord::snowflake::{ChannelId, UserId};
use crate::ui::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// `Online — 4`, and the like. Not selectable.
    Section { label: String },
    Dm {
        id: ChannelId,
        title: String,
        presence: PresenceStatus,
        unread: bool,
        mentions: u32,
        muted: bool,
        /// How many people are in it, for a group; `None` for a pair.
        members: Option<usize>,
    },
    Friend {
        id: UserId,
        name: String,
        presence: PresenceStatus,
    },
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(self, Row::Section { .. })
    }
}

/// The dot in front of a name.
///
/// Four shapes rather than four colours alone: a terminal in a colour scheme
/// somebody else chose, or a reader who cannot tell green from orange, still
/// gets the answer.
pub fn presence_glyph(status: PresenceStatus) -> char {
    match status {
        PresenceStatus::Online => '\u{25cf}',
        PresenceStatus::Idle => '\u{25d1}',
        PresenceStatus::Dnd => '\u{25d7}',
        PresenceStatus::Offline | PresenceStatus::Invisible => '\u{25cb}',
    }
}

pub fn presence_colour(theme: &Theme, status: PresenceStatus) -> starkit::theme::color::Rgb {
    match status {
        PresenceStatus::Online => theme.chat.presence_online,
        PresenceStatus::Idle => theme.chat.presence_idle,
        PresenceStatus::Dnd => theme.chat.presence_dnd,
        PresenceStatus::Offline | PresenceStatus::Invisible => theme.chat.presence_offline,
    }
}

pub struct View<'a> {
    pub theme: &'a Theme,
    pub rows: &'a [Row],
    pub cursor: usize,
    pub scroll: usize,
    pub focused: bool,
    pub tab: DmTab,
    pub open: Option<ChannelId>,
}

pub fn row_at(body: Rect, v: &View<'_>, y: u16) -> Option<usize> {
    if y < body.y || y >= body.y + body.height {
        return None;
    }
    let index = v.scroll + usize::from(y - body.y);
    (index < v.rows.len()).then_some(index)
}

pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) {
    let t = v.theme;
    if v.rows.is_empty() {
        empty(
            body,
            buf,
            t,
            match v.tab {
                DmTab::Dms => "no conversations",
                DmTab::Friends => "no friends yet",
            },
        );
        return;
    }
    let width = usize::from(body.width);

    for (index, row) in v
        .rows
        .iter()
        .enumerate()
        .skip(v.scroll)
        .take(usize::from(body.height))
    {
        let y = body.y + (index - v.scroll) as u16;
        let selected = index == v.cursor && row.selectable();

        match row {
            Row::Section { label } => {
                let text: String = label.to_uppercase().chars().take(width).collect();
                buf.set_string(
                    body.x,
                    y,
                    format!("{text:width$}"),
                    Style::default().fg(rgb(t.row_meta_fg)),
                );
            }
            Row::Dm {
                id,
                title,
                presence,
                unread,
                mentions,
                muted,
                members,
            } => {
                let badge = if *mentions > 0 {
                    format!(" ({mentions})")
                } else if *members.as_ref().unwrap_or(&0) > 0 {
                    format!(" [{}]", members.unwrap_or(0))
                } else {
                    String::new()
                };
                let fg = if *mentions > 0 {
                    t.chat.mention_fg
                } else if *muted {
                    t.dim
                } else if *unread {
                    t.chat.unread_fg
                } else if v.open == Some(*id) {
                    t.row_playing_fg
                } else {
                    t.row_fg
                };
                let mut style = Style::default().fg(rgb(fg));
                if *unread && !*muted {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if selected {
                    style = style
                        .fg(rgb(sel_fg(t, v.focused)))
                        .bg(rgb(sel_bg(t, v.focused)));
                }
                // The dot keeps its own colour even on a selected row: it is
                // the one thing on the line that means something by its
                // colour, and losing it to the selection bar would be losing
                // the information.
                let dot_style = Style::default()
                    .fg(rgb(presence_colour(t, *presence)))
                    .bg(style.bg.unwrap_or(rgb(t.panel_bg)));
                let rest: String = format!(" {title}{badge}")
                    .chars()
                    .take(width.saturating_sub(1))
                    .collect();
                buf.set_string(body.x, y, presence_glyph(*presence).to_string(), dot_style);
                buf.set_string(
                    body.x + 1,
                    y,
                    format!("{rest:w$}", w = width.saturating_sub(1)),
                    style,
                );
            }
            Row::Friend { name, presence, .. } => {
                let mut style = Style::default().fg(rgb(t.row_fg));
                if selected {
                    style = style
                        .fg(rgb(sel_fg(t, v.focused)))
                        .bg(rgb(sel_bg(t, v.focused)));
                }
                let dot_style = Style::default()
                    .fg(rgb(presence_colour(t, *presence)))
                    .bg(style.bg.unwrap_or(rgb(t.panel_bg)));
                let rest: String = format!(" {name}")
                    .chars()
                    .take(width.saturating_sub(1))
                    .collect();
                buf.set_string(body.x, y, presence_glyph(*presence).to_string(), dot_style);
                buf.set_string(
                    body.x + 1,
                    y,
                    format!("{rest:w$}", w = width.saturating_sub(1)),
                    style,
                );
            }
        }
    }
}

fn sel_fg(t: &Theme, focused: bool) -> starkit::theme::color::Rgb {
    if focused {
        t.row_selected_fg
    } else {
        t.row_cursor_fg
    }
}

fn sel_bg(t: &Theme, focused: bool) -> starkit::theme::color::Rgb {
    if focused {
        t.row_selected_bg
    } else {
        t.row_cursor_bg
    }
}

/// Friends, grouped by how reachable they are.
///
/// Online first, then idle, then do-not-disturb, then everybody else, each
/// group headed and counted. The order is not alphabetical on purpose: the
/// list answers "who could I talk to now", and the alphabet does not.
pub fn group_friends(mut friends: Vec<(UserId, String, PresenceStatus)>) -> Vec<Row> {
    friends.sort_by(|a, b| rank(a.2).cmp(&rank(b.2)).then_with(|| a.1.cmp(&b.1)));
    let mut out = Vec::with_capacity(friends.len() + 4);
    let mut current: Option<u8> = None;
    for (id, name, presence) in friends {
        let r = rank(presence);
        if current != Some(r) {
            let label = match presence {
                PresenceStatus::Online => "online",
                PresenceStatus::Idle => "idle",
                PresenceStatus::Dnd => "do not disturb",
                _ => "offline",
            };
            out.push(Row::Section {
                label: label.to_string(),
            });
            current = Some(r);
        }
        out.push(Row::Friend { id, name, presence });
    }
    out
}

fn rank(status: PresenceStatus) -> u8 {
    match status {
        PresenceStatus::Online => 0,
        PresenceStatus::Idle => 1,
        PresenceStatus::Dnd => 2,
        PresenceStatus::Offline | PresenceStatus::Invisible => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_heading_is_not_somewhere_the_cursor_can_land() {
        assert!(!Row::Section {
            label: "online".into()
        }
        .selectable());
        assert!(Row::Friend {
            id: UserId(1),
            name: "alex".into(),
            presence: PresenceStatus::Online,
        }
        .selectable());
    }

    /// Reachability first, then the alphabet. Somebody who is online is the
    /// answer to the question the list is asking.
    #[test]
    fn friends_are_grouped_by_presence_and_then_named() {
        let rows = group_friends(vec![
            (UserId(1), "zoe".into(), PresenceStatus::Online),
            (UserId(2), "alex".into(), PresenceStatus::Offline),
            (UserId(3), "jordan".into(), PresenceStatus::Online),
            (UserId(4), "sam".into(), PresenceStatus::Dnd),
        ]);
        let names: Vec<&str> = rows
            .iter()
            .map(|r| match r {
                Row::Section { label } => label.as_str(),
                Row::Friend { name, .. } => name.as_str(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "online",
                "jordan",
                "zoe",
                "do not disturb",
                "sam",
                "offline",
                "alex"
            ]
        );
    }

    /// Each group is headed once. A heading printed twice is a list that looks
    /// sorted and is not.
    #[test]
    fn each_group_gets_one_heading() {
        let rows = group_friends(
            (0..10)
                .map(|i| {
                    (
                        UserId(i),
                        format!("p{i}"),
                        if i % 2 == 0 {
                            PresenceStatus::Online
                        } else {
                            PresenceStatus::Offline
                        },
                    )
                })
                .collect(),
        );
        let headings = rows
            .iter()
            .filter(|r| matches!(r, Row::Section { .. }))
            .count();
        assert_eq!(headings, 2);
    }

    /// The four presences are told apart by shape as well as by colour.
    #[test]
    fn presence_is_readable_without_colour() {
        let mut seen = Vec::new();
        for s in [
            PresenceStatus::Online,
            PresenceStatus::Idle,
            PresenceStatus::Dnd,
            PresenceStatus::Offline,
        ] {
            let g = presence_glyph(s);
            assert!(!seen.contains(&g), "{s:?} draws the same shape as another");
            seen.push(g);
        }
        assert_eq!(
            presence_glyph(PresenceStatus::Invisible),
            presence_glyph(PresenceStatus::Offline),
            "invisible is offline as far as anybody else can tell"
        );
    }

    #[test]
    fn empty_lists_say_which_one_is_empty() {
        let theme = crate::ui::theme::tests_support::theme("terminal");
        for (tab, want) in [
            (DmTab::Dms, "no conversations"),
            (DmTab::Friends, "no friends yet"),
        ] {
            let area = Rect::new(0, 0, 24, 5);
            let mut buf = Buffer::empty(area);
            render(
                area,
                &mut buf,
                &View {
                    theme: &theme,
                    rows: &[],
                    cursor: 0,
                    scroll: 0,
                    focused: false,
                    tab,
                    open: None,
                },
            );
            let text: String = (0..area.width)
                .map(|x| buf[(x, 2)].symbol().to_string())
                .collect();
            assert!(text.contains(want), "{tab:?} drew {text:?}");
        }
    }
}
