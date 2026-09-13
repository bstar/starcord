//! The server rail.
//!
//! A narrow strip down the left, with the DM home at the top. The mark to the
//! right of each entry is the whole of the unread state, because at eight
//! columns wide there is room for nothing else.
//!
//! An entry is one row of two characters where the terminal cannot draw a
//! picture, and two rows carrying a four-by-two icon where it can. The height
//! is the one thing everything else has to agree on: the cursor, the scroll
//! and the mouse all go through [`row_rows`] and [`row_at`], so a rail with
//! icons and a rail without behave the same way.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::{empty, fit, rgb, width_of};
use crate::config::GuildsStyle;
use crate::discord::media::MediaKey;
use crate::discord::snowflake::GuildId;
use crate::ui::theme::Theme;

/// Columns and rows an icon takes when there are pictures.
const ICON_COLS: u16 = 4;
const ICON_ROWS: u16 = 2;

/// One entry, copied out of `State` before the lock is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `None` is the direct-message home, which is always first.
    pub id: Option<GuildId>,
    pub name: String,
    /// The server's icon hash, where it has one.
    pub icon: Option<String>,
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
        // An unavailable server arrives as an id and nothing else -- no name,
        // no channels -- so there are no initials to take. Two dots rather
        // than two blanks, because a blank row reads as a drawing fault and
        // this one is a Discord outage.
        if self.name.trim().is_empty() {
            return "\u{b7}\u{b7}".into();
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
    /// Whether this terminal can draw a server's icon.
    pub pictures: bool,
}

/// Where one server's icon goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub rect: Rect,
    pub key: MediaKey,
    /// What the cells say until the picture arrives.
    pub initials: String,
}

/// How many terminal rows one entry takes.
pub fn row_rows(pictures: bool) -> u16 {
    if pictures {
        ICON_ROWS
    } else {
        1
    }
}

/// Which row a body-relative `y` is on.
///
/// The renderer and the mouse both come through here, so a row that was never
/// drawn cannot be clicked.
pub fn row_at(body: Rect, v: &View<'_>, y: u16) -> Option<usize> {
    if y < body.y || y >= body.y + body.height {
        return None;
    }
    let index = v.scroll + usize::from((y - body.y) / row_rows(v.pictures));
    (index < v.rows.len()).then_some(index)
}

/// Draw the rail, and say where the icons go.
///
/// The icons are placed rather than drawn: a protocol image is one escape
/// sequence over a region, and every one of them on the screen goes down in a
/// single pass after the text. What comes back is what that pass needs.
pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) -> Vec<Icon> {
    let t = v.theme;
    let mut icons = Vec::new();
    if v.rows.is_empty() {
        empty(body, buf, t, "—");
        return icons;
    }
    let width = body.width;
    let step = row_rows(v.pictures);
    let visible = usize::from(body.height / step);
    for (index, row) in v.rows.iter().enumerate().skip(v.scroll).take(visible) {
        let y = body.y + (index - v.scroll) as u16 * step;
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

        if v.pictures {
            // Two rows: the icon's four columns, the mark beside it, and the
            // selection carried across both so the entry reads as one thing.
            for line in 0..step {
                buf.set_string(body.x, y + line, " ".repeat(usize::from(width)), style);
            }
            let gap = usize::from(width.saturating_sub(ICON_COLS + width_of(&mark)));
            buf.set_string(
                body.x + ICON_COLS,
                y,
                fit(&format!("{:gap$}{mark}", ""), width - ICON_COLS),
                style,
            );
            match (row.id, row.icon.clone()) {
                (Some(guild), Some(hash)) => icons.push(Icon {
                    rect: Rect {
                        x: body.x,
                        y,
                        width: ICON_COLS.min(width),
                        height: step,
                    },
                    key: MediaKey::GuildIcon {
                        guild,
                        hash,
                        size: 32,
                    },
                    initials,
                }),
                // No icon to fetch: the initials are the icon, centred on the
                // two rows so that a rail of mixed entries lines up.
                _ => buf.set_string(body.x + 1, y, fit(&initials, ICON_COLS), style),
            }
            continue;
        }

        let used = width_of(&initials) + width_of(&mark);
        let gap = usize::from(width.saturating_sub(used).max(1));
        let text = format!("{initials}{:gap$}{mark}", "", gap = gap);
        buf.set_string(body.x, y, fit(&text, width), style);
    }

    // The list style is a later milestone; the rail is what M1 draws and
    // saying so beats drawing something that looks broken.
    let _ = v.style;
    icons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> Row {
        Row {
            id: Some(GuildId(1)),
            name: name.into(),
            icon: None,
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
    }

    /// A server named in a script with no case, or with an emoji, still gets
    /// two cells of something rather than a blank.
    #[test]
    fn initials_survive_a_name_that_is_not_latin() {
        assert_eq!(row("日本語のサーバー").initials(), "日本");
        assert_eq!(row("🎮 Gaming").initials(), "🎮G");
    }

    /// A server Discord is having an outage in arrives as an id and nothing
    /// else, so there is no name to take initials from. A blank row would read
    /// as a drawing fault rather than as an outage.
    #[test]
    fn an_unnamed_server_still_gets_two_cells() {
        assert_eq!(row("").initials(), "\u{b7}\u{b7}");
        assert_eq!(row("   ").initials(), "\u{b7}\u{b7}");
    }

    #[test]
    fn the_home_row_is_not_a_server() {
        let home = Row {
            id: None,
            name: "direct messages".into(),
            icon: None,
            unread: true,
            mentions: 0,
            unavailable: false,
        };
        assert_eq!(home.initials(), "@");
    }

    fn view<'a>(
        rows: &'a [Row],
        theme: &'a crate::ui::theme::Theme,
        scroll: usize,
        pictures: bool,
    ) -> View<'a> {
        View {
            theme,
            rows,
            cursor: 0,
            scroll,
            style: GuildsStyle::Rail,
            focused: true,
            pictures,
        }
    }

    /// The rule the mouse rests on: a row that was scrolled off the top is not
    /// a row anything can click.
    #[test]
    fn row_at_answers_only_for_rows_that_were_drawn() {
        let rows: Vec<Row> = (0..5).map(|i| row(&format!("g{i}"))).collect();
        let theme = crate::ui::theme::tests_support::theme("terminal");
        let v = view(&rows, &theme, 2, false);
        let body = Rect::new(0, 4, 6, 3);
        assert_eq!(row_at(body, &v, 3), None, "above the body");
        assert_eq!(row_at(body, &v, 4), Some(2));
        assert_eq!(row_at(body, &v, 6), Some(4));
        assert_eq!(row_at(body, &v, 7), None, "below the body");
    }

    /// With icons an entry is two rows tall, and a click anywhere on either of
    /// them is a click on that server. The renderer and the mouse share
    /// `row_rows`, which is what makes that true rather than nearly true.
    #[test]
    fn an_entry_with_an_icon_is_two_rows_the_mouse_agrees_about() {
        let rows: Vec<Row> = (0..4).map(|i| row(&format!("g{i}"))).collect();
        let theme = crate::ui::theme::tests_support::theme("terminal");
        let v = view(&rows, &theme, 1, true);
        let body = Rect::new(0, 0, 6, 6);
        assert_eq!(row_rows(true), 2);
        assert_eq!(row_at(body, &v, 0), Some(1));
        assert_eq!(row_at(body, &v, 1), Some(1), "the second row of the same");
        assert_eq!(row_at(body, &v, 2), Some(2));
        assert_eq!(row_at(body, &v, 5), Some(3));
    }

    /// A server with an icon hands back a four-by-two rectangle to draw it in;
    /// one without keeps its initials, in the same place.
    #[test]
    fn an_icon_is_placed_rather_than_drawn() {
        let theme = crate::ui::theme::tests_support::theme("terminal");
        let mut rows = vec![row("First Guild"), row("Second Guild")];
        rows[0].icon = Some("abc123".into());
        let v = view(&rows, &theme, 0, true);
        let body = Rect::new(0, 0, 6, 6);
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 6));
        let icons = render(body, &mut buf, &v);

        assert_eq!(icons.len(), 1, "only the one with a hash");
        assert_eq!(icons[0].rect, Rect::new(0, 0, 4, 2));
        assert_eq!(icons[0].initials, "FG");
        assert!(
            matches!(&icons[0].key, MediaKey::GuildIcon { hash, .. } if hash == "abc123"),
            "{:?}",
            icons[0].key
        );

        // The second entry has nothing to fetch, so its initials are on the
        // screen where its icon would have been.
        let second: String = (0..6).map(|x| buf[(x, 2)].symbol().to_string()).collect();
        assert!(second.contains("SG"), "{second:?}");
    }

    /// And with no pictures nothing is placed at all: the rail is what it was.
    #[test]
    fn a_terminal_without_pictures_places_nothing() {
        let theme = crate::ui::theme::tests_support::theme("terminal");
        let mut rows = vec![row("First Guild")];
        rows[0].icon = Some("abc123".into());
        let v = view(&rows, &theme, 0, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 6));
        assert!(render(Rect::new(0, 0, 6, 6), &mut buf, &v).is_empty());
        let first: String = (0..6).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(first.starts_with("FG"), "{first:?}");
    }
}
