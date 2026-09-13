//! The server rail.
//!
//! A narrow strip down the left, one row per server, with the DM home at the
//! top. Two characters of the server's name stand in for its icon until there
//! are pictures to draw; the mark to the right is the whole of the unread
//! state, because at eight columns wide there is room for nothing else.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::{empty, rgb};
use crate::config::GuildsStyle;
use crate::discord::snowflake::GuildId;
use crate::ui::theme::Theme;

/// One entry, copied out of `State` before the lock is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `None` is the direct-message home, which is always first.
    pub id: Option<GuildId>,
    pub name: String,
    pub unread: bool,
    pub mentions: u32,
    /// A Discord-side outage. The row stays, greyed.
    pub unavailable: bool,
}

impl Row {
    /// Two characters standing in for an icon.
    ///
    /// The initials of the first two words, or the first two characters of one
    /// word. `Some Long Server` is `SL`, `announcements` is `an`, and an emoji
    /// name comes out as the emoji, which is what its owner meant.
    pub fn initials(&self) -> String {
        if self.id.is_none() {
            return "@".into();
        }
        let mut words = self
            .name
            .split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric) || w.chars().count() == 1);
        match (words.next(), words.next()) {
            (Some(a), Some(b)) => {
                let mut s = String::new();
                s.extend(a.chars().next());
                s.extend(b.chars().next());
                s
            }
            (Some(a), None) => a.chars().take(2).collect(),
            _ => self.name.chars().take(2).collect(),
        }
    }
}

pub struct View<'a> {
    pub theme: &'a Theme,
    pub rows: &'a [Row],
    pub cursor: usize,
    pub scroll: usize,
    pub style: GuildsStyle,
    pub focused: bool,
}

/// Which row a body-relative `y` is on.
///
/// The renderer and the mouse both come through here, so a row that was never
/// drawn cannot be clicked.
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
        empty(body, buf, t, "—");
        return;
    }
    let width = usize::from(body.width);
    for (line, row) in v
        .rows
        .iter()
        .enumerate()
        .skip(v.scroll)
        .take(usize::from(body.height))
    {
        let (index, row) = (line, row);
        let y = body.y + (index - v.scroll) as u16;
        let selected = index == v.cursor;

        let fg = if row.unavailable {
            t.dim
        } else if row.mentions > 0 {
            t.chat.mention_fg
        } else if row.unread {
            t.chat.unread_fg
        } else {
            t.row_fg
        };
        let mut style = Style::default().fg(rgb(fg));
        if selected {
            style = style
                .fg(rgb(if v.focused {
                    t.row_selected_fg
                } else {
                    t.row_cursor_fg
                }))
                .bg(rgb(if v.focused {
                    t.row_selected_bg
                } else {
                    t.row_cursor_bg
                }));
        }
        if row.unread && !row.unavailable {
            style = style.add_modifier(Modifier::BOLD);
        }

        // `AB  •` — initials, then the one mark there is room for: a count
        // when somebody used this account's name, a dot when there is anything
        // unread at all, and nothing when there is not.
        let mark = match (row.mentions, row.unread) {
            (0, false) => String::new(),
            (0, true) => "\u{2022}".into(),
            (n, _) if n > 9 => "9+".into(),
            (n, _) => n.to_string(),
        };
        let initials = row.initials();
        let used = initials.chars().count() + mark.chars().count();
        let gap = width.saturating_sub(used).max(1);
        let text: String = format!("{initials}{:gap$}{mark}", "", gap = gap)
            .chars()
            .take(width)
            .collect();
        buf.set_string(body.x, y, format!("{text:width$}"), style);
    }

    // The list style is a later milestone; the rail is what M1 draws and
    // saying so beats drawing something that looks broken.
    let _ = v.style;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> Row {
        Row {
            id: Some(GuildId(1)),
            name: name.into(),
            unread: false,
            mentions: 0,
            unavailable: false,
        }
    }

    #[test]
    fn initials_are_the_first_letters_of_the_first_two_words() {
        assert_eq!(row("Some Long Server").initials(), "SL");
        assert_eq!(row("announcements").initials(), "an");
        assert_eq!(row("A").initials(), "A");
        assert_eq!(row("  ").initials(), "  ");
    }

    /// A server named in a script with no case, or with an emoji, still gets
    /// two cells of something rather than a blank.
    #[test]
    fn initials_survive_a_name_that_is_not_latin() {
        assert_eq!(row("日本語のサーバー").initials(), "日本");
        assert_eq!(row("🎮 Gaming").initials(), "🎮G");
    }

    #[test]
    fn the_home_row_is_not_a_server() {
        let home = Row {
            id: None,
            name: "direct messages".into(),
            unread: true,
            mentions: 0,
            unavailable: false,
        };
        assert_eq!(home.initials(), "@");
    }

    /// The rule the mouse rests on: a row that was scrolled off the top is not
    /// a row anything can click.
    #[test]
    fn row_at_answers_only_for_rows_that_were_drawn() {
        let rows: Vec<Row> = (0..5).map(|i| row(&format!("g{i}"))).collect();
        let theme = crate::ui::theme::tests_support::theme("terminal");
        let v = View {
            theme: &theme,
            rows: &rows,
            cursor: 0,
            scroll: 2,
            style: GuildsStyle::Rail,
            focused: true,
        };
        let body = Rect::new(0, 4, 6, 3);
        assert_eq!(row_at(body, &v, 3), None, "above the body");
        assert_eq!(row_at(body, &v, 4), Some(2));
        assert_eq!(row_at(body, &v, 6), Some(4));
        assert_eq!(row_at(body, &v, 7), None, "below the body");
    }
}
