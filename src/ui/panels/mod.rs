//! The docked panels, and the words on their headers.
//!
//! A panel is a pure widget over a small view struct. It reads what it draws
//! and owns none of it, and it gets its geometry from
//! [`layout::regions`](super::layout::regions) rather than working any out —
//! which is the rule that keeps a click and a drawn row talking about the same
//! cell.

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

/// The six things that can be docked.
///
/// The status line is not one of them. It is always drawn, never focused and
/// never closed, so giving it a `PanelId` would mean writing "except status"
/// at every use of this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelId {
    Guilds,
    Channels,
    Dms,
    Chat,
    Composer,
    Members,
}

/// Tab order. Left to right, top to bottom, which is the order they are drawn
/// in and the order somebody looking at the screen would guess.
pub const FOCUS_ORDER: [PanelId; 6] = [
    PanelId::Guilds,
    PanelId::Channels,
    PanelId::Dms,
    PanelId::Chat,
    PanelId::Composer,
    PanelId::Members,
];

impl PanelId {
    /// What the border says.
    pub fn title(self) -> &'static str {
        match self {
            PanelId::Guilds => "servers",
            PanelId::Channels => "channels",
            PanelId::Dms => "messages",
            PanelId::Chat => "chat",
            PanelId::Composer => "compose",
            PanelId::Members => "members",
        }
    }

    /// Which half of the key table this panel gets first refusal on.
    pub fn module(self) -> Module {
        match self {
            PanelId::Guilds => Module::Guilds,
            PanelId::Channels => Module::Channels,
            PanelId::Dms => Module::Dms,
            PanelId::Chat => Module::Chat,
            PanelId::Composer => Module::Composer,
            PanelId::Members => Module::Members,
        }
    }

    /// Whether the dock is allowed to take this one away.
    ///
    /// Chat and the composer are the application. A layout that can close them
    /// is a layout with a state in which there is nothing to do.
    pub fn closable(self) -> bool {
        !matches!(self, PanelId::Chat | PanelId::Composer)
    }
}

/// A word on a panel's header row.
///
/// One enum for every panel rather than one each: they are drawn by the same
/// code, hit-tested by the same code, and an application-wide list is what lets
/// the mouse handler answer "which word was that" without knowing which panel
/// it was over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Close,
    Settings,
    /// The DM panel's tab toggle, which says where it would go rather than
    /// where it is.
    ShowFriends,
    ShowDms,
    Search,
    Pins,
    Zen,
    Attach,
    Emoji,
    Gif,
}

impl header::Word for Word {
    fn word(self) -> Cow<'static, str> {
        match self {
            Word::Close => "close".into(),
            Word::Settings => "settings".into(),
            Word::ShowFriends => "friends".into(),
            Word::ShowDms => "dms".into(),
            Word::Search => "search".into(),
            Word::Pins => "pins".into(),
            Word::Zen => "zen".into(),
            Word::Attach => "attach".into(),
            Word::Emoji => "emoji".into(),
            Word::Gif => "gif".into(),
        }
    }
}

/// What each panel offers, right-aligned, dropped from the left as it narrows.
///
/// Chat has no `close`: it is the one panel that cannot be closed, and offering
/// a word that does nothing is worse than offering none.
pub fn words(panel: PanelId, dm_tab: DmTab) -> Vec<Word> {
    match panel {
        PanelId::Guilds => vec![Word::Close],
        PanelId::Channels => vec![Word::Settings, Word::Close],
        PanelId::Dms => vec![
            match dm_tab {
                DmTab::Dms => Word::ShowFriends,
                DmTab::Friends => Word::ShowDms,
            },
            Word::Close,
        ],
        PanelId::Chat => vec![Word::Search, Word::Pins, Word::Zen, Word::Settings],
        PanelId::Composer => vec![Word::Attach, Word::Emoji, Word::Gif],
        PanelId::Members => vec![Word::Settings, Word::Close],
    }
}

/// Which list the message panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DmTab {
    #[default]
    Dms,
    Friends,
}

/// Everything a panel needs that is not its own contents.
pub struct Frame<'a> {
    pub theme: &'a Theme,
    pub focused: bool,
    /// The title on the border, which is not always the panel's own name: the
    /// channel list says which server it is listing.
    pub title: &'a str,
    pub words: &'a [Word],
}

/// Draw a panel's border, corners, title and header row, and hand back what is
/// left for its contents.
///
/// Every panel starts with this and nothing else knows how a panel is framed,
/// so a change to the chrome is a change in one place. The body rect comes from
/// [`header::body`], which is also what the mouse tests against.
pub fn frame(area: Rect, buf: &mut Buffer, f: &Frame<'_>) -> Rect {
    let t = f.theme;
    let border = if f.focused { t.border_focused } else { t.border };
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
        // Clipped rather than wrapped: a title is a label, and a panel narrow
        // enough to cut one is narrow enough that the cut says so.
        let room = usize::from(area.width.saturating_sub(2));
        let title: String = title.chars().take(room).collect();
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

/// A ratatui colour from a theme one. Every panel wants it; it is here so that
/// none of them writes it again.
pub fn rgb(c: starkit::theme::color::Rgb) -> starkit::ratatui::style::Color {
    starkit::ratatui::style::Color::Rgb(c.r, c.g, c.b)
}

/// One dim line in the middle of an empty panel.
pub fn empty(area: Rect, buf: &mut Buffer, theme: &Theme, text: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let y = area.y + area.height / 2;
    let width = usize::from(area.width);
    let text: String = text.chars().take(width).collect();
    let x = area.x + (area.width.saturating_sub(text.chars().count() as u16)) / 2;
    buf.set_string(x, y, text, Style::default().fg(rgb(theme.empty_fg)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_panel_is_in_the_focus_order_once() {
        for p in [
            PanelId::Guilds,
            PanelId::Channels,
            PanelId::Dms,
            PanelId::Chat,
            PanelId::Composer,
            PanelId::Members,
        ] {
            assert_eq!(
                FOCUS_ORDER.iter().filter(|&&q| q == p).count(),
                1,
                "{p:?} is not in the tab order exactly once"
            );
        }
    }

    /// A panel id goes into `config.toml` and into the session file, so its
    /// spelling is a compatibility surface rather than a detail.
    #[test]
    fn a_panel_serialises_as_its_lowercase_name() {
        let json = serde_json::to_string(&PanelId::Composer).unwrap();
        assert_eq!(json, "\"composer\"");
        let back: PanelId = serde_json::from_str("\"members\"").unwrap();
        assert_eq!(back, PanelId::Members);
    }

    /// The two that carry the conversation cannot be closed, and the header
    /// must not offer a word that does nothing.
    #[test]
    fn the_chat_and_composer_offer_no_close() {
        for p in FOCUS_ORDER {
            let has_close = words(p, DmTab::Dms).contains(&Word::Close);
            assert_eq!(
                has_close,
                p.closable(),
                "{p:?} offers close: {has_close}, closable: {}",
                p.closable()
            );
        }
    }

    /// The DM panel's tab word says where it would take you, not where you
    /// already are.
    #[test]
    fn the_message_tab_word_names_the_other_tab() {
        assert!(words(PanelId::Dms, DmTab::Dms).contains(&Word::ShowFriends));
        assert!(words(PanelId::Dms, DmTab::Friends).contains(&Word::ShowDms));
    }

    #[test]
    fn every_panel_maps_to_its_own_key_module() {
        let mut seen = Vec::new();
        for p in FOCUS_ORDER {
            let m = p.module();
            assert!(!seen.contains(&m), "{m:?} is claimed by two panels");
            seen.push(m);
        }
    }
}
