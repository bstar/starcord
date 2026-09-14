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

/// The six modules the window is made of.
///
/// The status line is not one of them. It is always drawn, never focused and
/// never closed, so giving it a `ModuleId` would mean writing "except status"
/// at every use of this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
    Servers,
    Channels,
    Dms,
    Conversation,
    Compose,
    Members,
}

/// Tab order. Left to right, top to bottom, which is the order they are drawn
/// in and the order somebody looking at the screen would guess.
pub const FOCUS_ORDER: [ModuleId; 6] = [
    ModuleId::Servers,
    ModuleId::Channels,
    ModuleId::Dms,
    ModuleId::Conversation,
    ModuleId::Compose,
    ModuleId::Members,
];

impl ModuleId {
    /// What the border says.
    pub fn title(self) -> &'static str {
        match self {
            ModuleId::Servers => "servers",
            ModuleId::Channels => "channels",
            ModuleId::Dms => "messages",
            ModuleId::Conversation => "chat",
            ModuleId::Compose => "compose",
            ModuleId::Members => "members",
        }
    }

    /// Which half of the key table this panel gets first refusal on.
    pub fn module(self) -> Module {
        match self {
            ModuleId::Servers => Module::Servers,
            ModuleId::Channels => Module::Channels,
            ModuleId::Dms => Module::Dms,
            ModuleId::Conversation => Module::Conversation,
            ModuleId::Compose => Module::Compose,
            ModuleId::Members => Module::Members,
        }
    }

    /// Whether the dock is allowed to take this one away.
    ///
    /// Chat and the composer are the application. A layout that can close them
    /// is a layout with a state in which there is nothing to do.
    pub fn closable(self) -> bool {
        !matches!(self, ModuleId::Conversation | ModuleId::Compose)
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
    /// The channel panel's tab, when the DM list has folded into it.
    ShowMessages,
    ShowChannels,
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
            Word::ShowMessages => "messages".into(),
            Word::ShowChannels => "channels".into(),
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
pub fn words(panel: ModuleId, dm_tab: DmTab, folded: Option<Fold>) -> Vec<Word> {
    match panel {
        ModuleId::Servers => vec![Word::Close],
        // When the DM list has folded in here the panel is carrying two lists,
        // so it grows the word that swaps them. The word names the list you
        // would go to, as the DM panel's own tab does.
        //
        // It sits to the right of `settings` because the header drops words
        // from the left as it narrows, and a folded panel is a narrow one by
        // definition: the tab has to outlive the settings, or the only way to
        // the DM list disappears exactly when it is the only way there is.
        ModuleId::Channels => match folded {
            Some(Fold::Channels) => vec![Word::Settings, Word::ShowMessages, Word::Close],
            Some(Fold::Dms) => vec![Word::Settings, Word::ShowChannels, Word::Close],
            None => vec![Word::Settings, Word::Close],
        },
        ModuleId::Dms => vec![
            match dm_tab {
                DmTab::Dms => Word::ShowFriends,
                DmTab::Friends => Word::ShowDms,
            },
            Word::Close,
        ],
        ModuleId::Conversation => vec![Word::Search, Word::Pins, Word::Zen, Word::Settings],
        ModuleId::Compose => vec![Word::Attach, Word::Emoji, Word::Gif],
        ModuleId::Members => vec![Word::Settings, Word::Close],
    }
}

/// Which list the message panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DmTab {
    #[default]
    Dms,
    Friends,
}

/// Which list the channel panel is showing, while it is carrying both.
///
/// A narrow terminal cannot give the DM list a panel of its own, and closing
/// it outright would mean losing the way to a conversation because the window
/// got smaller. So it folds in here behind a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fold {
    #[default]
    Channels,
    Dms,
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
        // that reads as a fault rather than as a label -- the guild rail is
        // eight columns wide and `= serv` is not the name of anything.
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

/// A ratatui colour from a theme one. Every panel wants it; it is here so that
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
/// row measured in characters is a row one cell wider than the panel it is in,
/// which writes over the border and leaves it there until something else
/// redraws it. That is the artefact this function exists to prevent, and it is
/// why no panel formats a row with `{:width$}`.
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

/// One dim line in the middle of an empty panel.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_panel_is_in_the_focus_order_once() {
        for p in [
            ModuleId::Servers,
            ModuleId::Channels,
            ModuleId::Dms,
            ModuleId::Conversation,
            ModuleId::Compose,
            ModuleId::Members,
        ] {
            assert_eq!(
                FOCUS_ORDER.iter().filter(|&&q| q == p).count(),
                1,
                "{p:?} is not in the tab order exactly once"
            );
        }
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

    /// The two that carry the conversation cannot be closed, and the header
    /// must not offer a word that does nothing.
    #[test]
    fn the_chat_and_composer_offer_no_close() {
        for p in FOCUS_ORDER {
            let has_close = words(p, DmTab::Dms, None).contains(&Word::Close);
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
        assert!(words(ModuleId::Dms, DmTab::Dms, None).contains(&Word::ShowFriends));
        assert!(words(ModuleId::Dms, DmTab::Friends, None).contains(&Word::ShowDms));
    }

    /// The tab survives a narrow header; the settings word does not.
    ///
    /// The header drops words from the left, and a folded panel is narrow by
    /// definition. Losing the only route to the DM list because the panel got
    /// small is the failure this order prevents.
    #[test]
    fn the_folded_tab_outlives_the_settings_word() {
        use starkit::ratatui::layout::Rect;
        let words = words(ModuleId::Channels, DmTab::Dms, Some(Fold::Channels));
        let kept: Vec<Word> = starkit::chrome::header::slots(Rect::new(0, 0, 20, 6), &words)
            .into_iter()
            .map(|(w, _)| w)
            .collect();
        assert_eq!(kept, vec![Word::ShowMessages, Word::Close], "{kept:?}");
    }

    /// The folded channel panel grows a word that names the other list, and
    /// loses it again when the DM list has a panel of its own.
    #[test]
    fn the_folded_channel_panel_grows_a_tab() {
        assert!(words(ModuleId::Channels, DmTab::Dms, Some(Fold::Channels))
            .contains(&Word::ShowMessages));
        assert!(
            words(ModuleId::Channels, DmTab::Dms, Some(Fold::Dms)).contains(&Word::ShowChannels)
        );
        let plain = words(ModuleId::Channels, DmTab::Dms, None);
        assert!(!plain.contains(&Word::ShowMessages));
        assert!(!plain.contains(&Word::ShowChannels));
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
