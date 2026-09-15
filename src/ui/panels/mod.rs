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
use starkit::ratatui::style::Style;

use super::keymap::Module;

// The border, corners, titles and header row are STAR/KIT's now -- see
// `starkit::chrome::frame` -- and so are the small text helpers every module
// used to keep its own copy of. Re-exported under their old names so the rest
// of this module's callers keep the imports they already have.
pub use starkit::chrome::{empty, rgb};
pub use starkit::text::fit;
pub use starkit::wrap::width_of;

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
    /// `‹` and `›`: the conversation before this one, and the one stepped
    /// back out of. On the top module, where a browser keeps them.
    Back,
    Forward,
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
            Word::Back => "\u{2039}".into(),
            Word::Forward => "\u{203a}".into(),
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
        ModuleId::Servers => vec![Word::Back, Word::Forward, Word::Settings],
        ModuleId::Channels => vec![Word::Settings],
        ModuleId::Conversation => vec![Word::Search, Word::Pins, Word::Settings],
        ModuleId::Compose => vec![Word::Attach, Word::Emoji, Word::Gif],
        ModuleId::Members => vec![Word::Settings],
    }
}

/// The application's name as the top of the window says it: letter-spaced,
/// with the slash spaced along with the rest, exactly as STAR/AMP's player
/// says `S T A R / A M P`. Pulling the slash tight against its neighbours
/// would make the seam read as a typo in one word rather than the join
/// between two.
pub const HEADING: &str = "S T A R / C O R D";

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
                    ModuleId::Servers => {
                        matches!(w, Word::Back | Word::Forward | Word::Settings)
                    }
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
