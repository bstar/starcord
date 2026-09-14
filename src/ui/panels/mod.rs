//! The modules of the column, and the words on their headers.
//!
//! A module is a pure widget over a small view struct. It reads what it draws
//! and owns none of it, and it gets its geometry from
//! [`layout::regions`](super::layout::LayoutState::regions) rather than working
//! any out — which is the rule that keeps a click and a drawn row talking about
//! the same cell.

pub mod channels;
pub mod chat;
pub mod composer;
pub mod dms;
pub mod guilds;
pub mod members;

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use starkit::chrome::header;
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::widgets::{Block, BorderType, Borders, Widget};

use super::keymap::Module;
use super::theme::Theme;

/// The five modules the window is made of.
///
/// The status line is not one of them. It is always drawn, never focused and
/// never folded, so giving it a `ModuleId` would mean writing "except status"
/// at every use of this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
    Servers,
    Channels,
    Conversation,
    Compose,
    Members,
}

/// The column, top to bottom.
///
/// Draw order, tab order and the order somebody drills through them, which are
/// deliberately the same order: the window is the sequence of questions you
/// answer to get to a conversation.
pub const COLUMN: [ModuleId; 5] = [
    ModuleId::Servers,
    ModuleId::Channels,
    ModuleId::Conversation,
    ModuleId::Compose,
    ModuleId::Members,
];

impl ModuleId {
    /// Where it is in [`COLUMN`], which is also how `Regions` indexes it.
    pub fn index(self) -> usize {
        match self {
            ModuleId::Servers => 0,
            ModuleId::Channels => 1,
            ModuleId::Conversation => 2,
            ModuleId::Compose => 3,
            ModuleId::Members => 4,
        }
    }

    /// Whether this module is a list that folds to one row.
    pub fn is_list(self) -> bool {
        matches!(
            self,
            ModuleId::Servers | ModuleId::Channels | ModuleId::Members
        )
    }

    /// What the border says, before the app makes it more specific.
    pub fn title(self) -> &'static str {
        match self {
            ModuleId::Servers => "servers",
            ModuleId::Channels => "channels",
            ModuleId::Conversation => "conversation",
            ModuleId::Compose => "compose",
            ModuleId::Members => "members",
        }
    }

    /// Which half of the key table this module gets first refusal on.
    pub fn module(self) -> Module {
        match self {
            ModuleId::Servers => Module::Servers,
            ModuleId::Channels => Module::Channels,
            ModuleId::Conversation => Module::Conversation,
            ModuleId::Compose => Module::Compose,
            ModuleId::Members => Module::Members,
        }
    }
}

/// A word on a module's header row.
///
/// One enum for every module rather than one each: they are drawn by the same
/// code, hit-tested by the same code, and an application-wide list is what lets
/// the mouse handler answer "which word was that" without knowing which module
/// it was over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Settings,
    Search,
    Pins,
    Attach,
    Emoji,
    Gif,
}

impl header::Word for Word {
    fn word(self) -> Cow<'static, str> {
        match self {
            Word::Settings => "settings".into(),
            Word::Search => "search".into(),
            Word::Pins => "pins".into(),
            Word::Attach => "attach".into(),
            Word::Emoji => "emoji".into(),
            Word::Gif => "gif".into(),
        }
    }
}

/// What each module offers, right-aligned, dropped from the left as it
/// narrows.
///
/// There is no `close`: nothing in the column can be closed any more. A module
/// you are not using is one row of itself, which is cheaper than a way of
/// making it disappear and a way of finding it again.
pub fn words(module: ModuleId) -> Vec<Word> {
    match module {
        ModuleId::Servers => vec![Word::Settings],
        ModuleId::Channels => vec![Word::Settings],
        ModuleId::Conversation => vec![Word::Search, Word::Pins, Word::Settings],
        ModuleId::Compose => vec![Word::Attach, Word::Emoji, Word::Gif],
        ModuleId::Members => vec![Word::Settings],
    }
}

/// Everything a module needs that is not its own contents.
pub struct Frame<'a> {
    pub theme: &'a Theme,
    pub focused: bool,
    /// The title on the border, which is not always the module's own name: the
    /// channel list says which server it is listing.
    pub title: &'a str,
    pub words: &'a [Word],
}

/// Draw a module's border, corners, title and header row, and hand back what is
/// left for its contents.
///
/// Every module starts with this and nothing else knows how one is framed, so a
/// change to the chrome is a change in one place. The body rect comes from
/// [`header::body`], which is also what the mouse tests against.
pub fn frame(area: Rect, buf: &mut Buffer, f: &Frame<'_>) -> Rect {
    let t = f.theme;
    let border = if f.focused {
        t.border_focused
    } else {
        t.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(rgb(border)))
        .style(Style::default().bg(rgb(t.panel_bg)));
    block.render(area, buf);

    if area.width >= 2 && area.height >= 2 {
        starkit::chrome::frame::render_corners(area, buf, t, f.focused);
        let title = format!(
            "{}{}{}",
            starkit::chrome::frame::TITLE_LEAD,
            f.title,
            starkit::chrome::frame::TITLE_TRAIL
        );
        // All of it or none of it. A clipped title is a word cut off mid-way
        // that reads as a fault rather than as a label: `= channels · Some Long
        // Serv` is not the name of anything.
        let room = area.width.saturating_sub(2);
        let title = if width_of(&title) <= room {
            title
        } else {
            String::new()
        };
        let style = if f.focused {
            Style::default()
                .fg(rgb(t.titlebar_active_fg))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(rgb(t.titlebar_inactive_fg))
        };
        buf.set_string(area.x + 1, area.y, title, style);
    }

    header::render(area, f.words, buf, t);
    header::body(area)
}

/// A ratatui colour from a theme one. Every module wants it; it is here so that
/// none of them writes it again.
pub fn rgb(c: starkit::theme::color::Rgb) -> starkit::ratatui::style::Color {
    starkit::ratatui::style::Color::Rgb(c.r, c.g, c.b)
}

/// Columns a string takes on screen.
pub fn width_of(text: &str) -> u16 {
    starkit::wrap::width_of(text)
}

/// Cut and pad a row to exactly `width` columns.
///
/// By display width, never by character count. Channel names, servers and the
/// people in a DM all routinely contain emoji, and an emoji is two columns; a
/// row measured in characters is a row one cell wider than the module it is in,
/// which writes over the border and leaves it there until something else
/// redraws it. That is the artefact this function exists to prevent, and it is
/// why no module formats a row with `{:width$}`.
pub fn fit(text: &str, width: u16) -> String {
    let mut out = String::with_capacity(usize::from(width) + 4);
    let mut used = 0u16;
    for (_, cluster) in starkit::wrap::clusters(text) {
        let w = width_of(cluster);
        if used + w > width {
            break;
        }
        out.push_str(cluster);
        used += w;
    }
    // A double-width cluster at the edge leaves one column over; a space is
    // what fills it, because a half-drawn emoji is not a thing a terminal can
    // show.
    for _ in used..width {
        out.push(' ');
    }
    out
}

/// One dim line in the middle of an empty module.
pub fn empty(area: Rect, buf: &mut Buffer, theme: &Theme, text: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let y = area.y + area.height / 2;
    let text = fit(text, area.width);
    let text = text.trim_end();
    let x = area.x + area.width.saturating_sub(width_of(text)) / 2;
    buf.set_string(x, y, text, Style::default().fg(rgb(theme.empty_fg)));
}

/// The one line a folded list draws: what is currently chosen in it.
///
/// Left-aligned and fitted rather than centred like [`empty`], because it is a
/// row of the list rather than a message about the list, and it should sit
/// where the rows above it would have been.
pub fn summary_row(body: Rect, buf: &mut Buffer, text: &str, style: Style) {
    if body.height == 0 || body.width == 0 {
        return;
    }
    buf.set_string(body.x, body.y, fit(text, body.width), style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_module_is_in_the_column_once() {
        for m in [
            ModuleId::Servers,
            ModuleId::Channels,
            ModuleId::Conversation,
            ModuleId::Compose,
            ModuleId::Members,
        ] {
            assert_eq!(
                COLUMN.iter().filter(|&&q| q == m).count(),
                1,
                "{m:?} is not in the column exactly once"
            );
            assert_eq!(COLUMN[m.index()], m, "{m:?} indexes somebody else's rect");
        }
    }

    /// Three lists and two that are not. The layout's arithmetic counts on it,
    /// and so does the accordion.
    #[test]
    fn three_of_the_five_are_lists() {
        let lists: Vec<ModuleId> = COLUMN.into_iter().filter(|m| m.is_list()).collect();
        assert_eq!(
            lists,
            vec![ModuleId::Servers, ModuleId::Channels, ModuleId::Members]
        );
    }

    /// Nothing persists a module id today, but it is serialisable and the
    /// spelling it would be written with is the lowercase name.
    #[test]
    fn a_module_serialises_as_its_lowercase_name() {
        let json = serde_json::to_string(&ModuleId::Compose).unwrap();
        assert_eq!(json, "\"compose\"");
        let back: ModuleId = serde_json::from_str("\"members\"").unwrap();
        assert_eq!(back, ModuleId::Members);
    }

    /// Every word a header offers has to do something, and what it does is a
    /// `match` in `App::word_click`. A word offered by a module that cannot
    /// honour it is a click that looks broken.
    #[test]
    fn no_module_offers_a_word_it_cannot_honour() {
        for m in COLUMN {
            for w in words(m) {
                let allowed = match m {
                    ModuleId::Conversation => {
                        matches!(w, Word::Search | Word::Pins | Word::Settings)
                    }
                    ModuleId::Compose => matches!(w, Word::Attach | Word::Emoji | Word::Gif),
                    _ => matches!(w, Word::Settings),
                };
                assert!(allowed, "{m:?} offers {w:?}");
            }
        }
    }

    #[test]
    fn every_module_maps_to_its_own_key_module() {
        let mut seen = Vec::new();
        for m in COLUMN {
            let k = m.module();
            assert!(!seen.contains(&k), "{k:?} is claimed by two modules");
            seen.push(k);
        }
    }

    /// A folded list draws its summary at the top left of its body, cut to fit
    /// rather than spilling over the border.
    #[test]
    fn a_summary_row_is_one_fitted_line() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        summary_row(area, &mut buf, "a rather long summary", Style::default());
        let drawn: String = (0..10).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(drawn, "a rather l");
    }
}
