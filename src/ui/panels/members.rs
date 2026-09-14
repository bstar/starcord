//! Who is in this server.
//!
//! The list Discord keeps is a *lazy* one: the client subscribes to a range of
//! rows and the server sends inserts, updates and deletes against it, so the
//! panel is a view onto a window rather than onto a set. That subscription is
//! core work and lands with the core milestone beside this one; what is here
//! is the whole of the drawing, fed by [`core_ext::members`], which answers
//! `None` until `State` grows a `member_list(guild)`.
//!
//! Groups are the server's own, not this panel's: Discord hoists roles into
//! headings and puts everybody else under `online` and `offline`, and a client
//! that sorted them differently would show a different list from the one the
//! reader knows.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::{empty, fit, rgb, width_of};
use crate::discord::model::PresenceStatus;
use crate::ui::theme::Theme;

/// The dot beside a name.
const DOT: &str = "\u{25cf}";

/// One line of the member list.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// `ONLINE — 12`, or a hoisted role's name.
    Group { label: String, count: usize },
    Member {
        name: String,
        presence: PresenceStatus,
        /// The colour of the member's top coloured role, packed `0xRRGGBB`.
        /// Zero means "no colour", which is the ordinary text colour.
        colour: u32,
        bot: bool,
    },
}

impl Row {
    pub fn selectable(&self) -> bool {
        matches!(self, Row::Member { .. })
    }
}

pub struct View<'a> {
    pub theme: &'a Theme,
    pub focused: bool,
    pub rows: &'a [Row],
    pub cursor: usize,
    pub scroll: usize,
    /// `None` when the server has not sent a list yet, which is different from
    /// a server with nobody in it.
    pub loaded: bool,
}

/// The one line the folded module draws: how many people are about.
///
/// Counted from the rows rather than from the group headings, because a
/// hoisted role is a group of its own and adding up the headings would count
/// `REGULARS` as neither online nor offline. Everybody Discord has not put
/// under the offline heading is here now, whatever presence their member entry
/// carries, which is what that heading means.
pub fn summary(rows: &[Row]) -> String {
    let mut under_offline = false;
    let (mut online, mut idle, mut offline) = (0usize, 0usize, 0usize);
    for row in rows {
        match row {
            Row::Group { label, .. } => under_offline = label.eq_ignore_ascii_case("offline"),
            Row::Member { presence, .. } => match presence {
                PresenceStatus::Idle => idle += 1,
                PresenceStatus::Offline | PresenceStatus::Invisible if under_offline => {
                    offline += 1
                }
                _ => online += 1,
            },
        }
    }
    if online + idle + offline == 0 {
        return "nobody here".into();
    }
    let mut parts = Vec::with_capacity(3);
    for (n, word) in [(online, "online"), (idle, "idle"), (offline, "offline")] {
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    parts.join(" \u{b7} ")
}

/// Which row a click landed on.
pub fn row_at(body: Rect, v: &View<'_>, y: u16) -> Option<usize> {
    if y < body.y || y >= body.y + body.height {
        return None;
    }
    let index = v.scroll + usize::from(y - body.y);
    (index < v.rows.len()).then_some(index)
}

pub fn presence_colour(theme: &Theme, presence: PresenceStatus) -> starkit::theme::color::Rgb {
    match presence {
        PresenceStatus::Online => theme.chat.presence_online,
        PresenceStatus::Idle => theme.chat.presence_idle,
        PresenceStatus::Dnd => theme.chat.presence_dnd,
        PresenceStatus::Offline | PresenceStatus::Invisible => theme.chat.presence_offline,
    }
}

pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) {
    if body.width == 0 || body.height == 0 {
        return;
    }
    if !v.loaded {
        empty(body, buf, v.theme, "members not loaded");
        return;
    }
    if v.rows.is_empty() {
        empty(body, buf, v.theme, "nobody here");
        return;
    }

    let t = v.theme;
    for (line, index) in (v.scroll..v.rows.len())
        .take(usize::from(body.height))
        .enumerate()
    {
        let y = body.y + line as u16;
        match &v.rows[index] {
            Row::Group { label, count } => {
                let text = format!("{} \u{2014} {count}", label.to_uppercase());
                buf.set_string(
                    body.x,
                    y,
                    fit(&text, body.width),
                    Style::default()
                        .fg(rgb(t.header_fg))
                        .add_modifier(Modifier::BOLD),
                );
            }
            Row::Member {
                name,
                presence,
                colour,
                bot,
            } => {
                let selected = index == v.cursor && v.focused;
                let dot = presence_colour(t, *presence);
                buf.set_string(body.x, y, DOT, Style::default().fg(rgb(dot)));

                let name_style = Style::default().fg(match *colour {
                    0 => rgb(t.row_fg),
                    packed => starkit::ratatui::style::Color::Rgb(
                        ((packed >> 16) & 0xff) as u8,
                        ((packed >> 8) & 0xff) as u8,
                        (packed & 0xff) as u8,
                    ),
                });
                let name_style = if selected {
                    name_style
                        .bg(rgb(t.row_cursor_bg))
                        .add_modifier(Modifier::BOLD)
                } else {
                    name_style
                };
                let label = if *bot {
                    format!("{name} [bot]")
                } else {
                    name.clone()
                };
                let room = body.width.saturating_sub(width_of(DOT) + 1);
                buf.set_string(body.x + width_of(DOT) + 1, y, fit(&label, room), name_style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn drawn(v: &View<'_>, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, v);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A server whose list has not arrived says so. "Nobody here" would be a
    /// statement about the server rather than about this client.
    #[test]
    fn an_unloaded_list_is_not_an_empty_one() {
        let t = theme("terminal");
        let empty_rows: [Row; 0] = [];
        let text = drawn(
            &View {
                theme: &t,
                focused: false,
                rows: &empty_rows,
                cursor: 0,
                scroll: 0,
                loaded: false,
            },
            24,
            6,
        );
        assert!(text.contains("not loaded"), "{text}");

        let text = drawn(
            &View {
                theme: &t,
                focused: false,
                rows: &empty_rows,
                cursor: 0,
                scroll: 0,
                loaded: true,
            },
            24,
            6,
        );
        assert!(text.contains("nobody here"), "{text}");
    }

    #[test]
    fn the_groups_and_the_names_are_drawn() {
        let t = theme("terminal");
        let rows = vec![
            Row::Group {
                label: "Regulars".into(),
                count: 2,
            },
            Row::Member {
                name: "alex".into(),
                presence: PresenceStatus::Online,
                colour: 0x3498db,
                bot: false,
            },
            Row::Member {
                name: "helper".into(),
                presence: PresenceStatus::Offline,
                colour: 0,
                bot: true,
            },
        ];
        let text = drawn(
            &View {
                theme: &t,
                focused: true,
                rows: &rows,
                cursor: 1,
                scroll: 0,
                loaded: true,
            },
            24,
            6,
        );
        assert!(text.contains("REGULARS \u{2014} 2"), "{text}");
        assert!(text.contains("alex"), "{text}");
        assert!(text.contains("helper [bot]"), "{text}");
        assert!(text.contains(DOT), "{text}");
    }

    /// A heading is not somewhere the cursor can stop.
    #[test]
    fn only_members_take_the_cursor() {
        assert!(!Row::Group {
            label: "x".into(),
            count: 1
        }
        .selectable());
        assert!(Row::Member {
            name: "a".into(),
            presence: PresenceStatus::Online,
            colour: 0,
            bot: false
        }
        .selectable());
    }
}
