//! `Ctrl+K`: go anywhere by typing a few of its letters.
//!
//! One list of everything that can be opened — servers, channels, direct
//! messages, friends — ranked by a fuzzy matcher rather than by a substring
//! test. The difference is not cosmetic: `gen` should find `#general` before
//! it finds `#gardening-notes-and-things`, and a subsequence match with no
//! scoring puts them in whatever order the list happened to be in.
//!
//! Types are kept apart by a sigil rather than by a section, so that one run
//! of `↑`/`↓` walks the whole answer. A switcher that groups its results makes
//! the reader choose a group before they have chosen a thing.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use starkit::chrome::overlay::{self, Anchor};
use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::input::{Edit, TextInput};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use crate::discord::snowflake::{ChannelId, GuildId, UserId};
use crate::ui::panels::{fit, rgb};
use crate::ui::theme::Theme;

/// How many hits are ever drawn. More than a screenful of guesses is not an
/// answer, and the matcher's own ordering means the tail is never what was
/// wanted.
const MAX_HITS: usize = 12;

/// Somewhere the switcher can take you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Guild(GuildId),
    Channel(ChannelId),
    Dm(ChannelId),
    Friend(UserId),
}

/// One row of the list, before anything has been typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub target: Target,
    /// What is matched against and drawn: `#general` or `@alex`.
    pub label: String,
    /// Where it is, drawn dim on the right: the server's name, or `dm`.
    pub hint: String,
}

impl Item {
    /// What the matcher sees. The hint is included so that typing a server's
    /// name narrows to its channels, which is the thing anybody with two
    /// `#general`s wants.
    fn haystack(&self) -> String {
        if self.hint.is_empty() {
            self.label.clone()
        } else {
            format!("{} {}", self.label, self.hint)
        }
    }
}

pub struct Quick {
    pub query: TextInput,
    items: Vec<Item>,
    hits: Vec<usize>,
    pub cursor: usize,
    matcher: Matcher,
}

impl std::fmt::Debug for Quick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Quick")
            .field("query", &self.query.text())
            .field("items", &self.items.len())
            .field("hits", &self.hits.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl Quick {
    pub fn new(items: Vec<Item>) -> Self {
        let mut quick = Self {
            query: TextInput::single(),
            items,
            hits: Vec::new(),
            cursor: 0,
            matcher: Matcher::new(Config::DEFAULT),
        };
        quick.rank();
        quick
    }

    pub fn hits(&self) -> Vec<&Item> {
        self.hits
            .iter()
            .filter_map(|i| self.items.get(*i))
            .collect()
    }

    pub fn selected(&self) -> Option<&Item> {
        self.hits.get(self.cursor).and_then(|i| self.items.get(*i))
    }

    /// Re-rank against whatever has been typed.
    ///
    /// An empty query is not a match of nothing: it is everything, in the
    /// order the caller supplied, which is recency for conversations and
    /// READY's order for servers.
    fn rank(&mut self) {
        let query = self.query.text().trim().to_string();
        if query.is_empty() {
            self.hits = (0..self.items.len()).take(MAX_HITS).collect();
            self.cursor = 0;
            return;
        }
        let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, usize)> = Vec::new();
        let mut buf = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let haystack = item.haystack();
            let utf32 = Utf32Str::new(&haystack, &mut buf);
            if let Some(score) = pattern.score(utf32, &mut self.matcher) {
                scored.push((score, index));
            }
        }
        // Highest score first, and ties broken by the caller's order so that
        // the list does not shuffle as somebody types.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.hits = scored.into_iter().take(MAX_HITS).map(|(_, i)| i).collect();
        self.cursor = 0;
    }

    /// What a key did.
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                KeyCode::Char('n') => {
                    self.step(1);
                    Action::Taken
                }
                KeyCode::Char('p') => {
                    self.step(-1);
                    Action::Taken
                }
                // The key that opened it closes it, which is how every other
                // toggle in the program behaves.
                KeyCode::Char('k') => Action::Close,
                _ => Action::Taken,
            };
        }
        match key.code {
            KeyCode::Up => {
                self.step(-1);
                return Action::Taken;
            }
            KeyCode::Down => {
                self.step(1);
                return Action::Taken;
            }
            _ => {}
        }
        match self.query.handle(key) {
            Edit::Submit => match self.selected() {
                Some(item) => Action::Open(item.target),
                None => Action::Taken,
            },
            Edit::Cancel => Action::Close,
            Edit::Consumed => {
                self.rank();
                Action::Taken
            }
            Edit::Ignored => Action::Taken,
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.query.paste(text);
        self.rank();
    }

    fn step(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len() as isize;
        self.cursor = ((self.cursor as isize + delta).rem_euclid(n)) as usize;
    }

    pub fn scroll(&mut self, delta: i16) {
        self.step(delta.signum() as isize);
    }
}

/// What the switcher wants done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Taken,
    Close,
    Open(Target),
    Quit,
}

/// Where the box lands: the same shape every overlay opens in, upper-anchored
/// where the typing boxes sit, sized to the hits it has to show.
pub fn rect(area: Rect, hits: usize) -> Rect {
    overlay::rect(area, (30, 64), hits as u16 + 4, 5, Anchor::Upper)
}

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    quick: &mut Quick,
) -> Option<(u16, u16)> {
    let hits: Vec<Item> = quick.hits().into_iter().cloned().collect();
    let r = rect(area, hits.len());
    if r.width < 10 || r.height < 4 {
        return None;
    }

    let t = theme;
    // The core theme type -- a struct literal is not a coercion site, so the
    // deref from this crate's own `Theme` is spelled out here.
    let core: &starkit::theme::Theme = t;
    let inner = overlay::render(
        r,
        buf,
        &overlay::Overlay {
            theme: core,
            title: "jump to",
            detail: None,
            footer: Some("enter go \u{b7} esc close"),
        },
    );
    if inner.height == 0 || inner.width == 0 {
        return None;
    }

    // The query line, with a real caret: this is a text field and looking like
    // one is most of what says so.
    buf.set_string(
        inner.x,
        inner.y,
        "\u{203a} ",
        Style::default().fg(rgb(t.accent)),
    );
    let field = Rect {
        x: inner.x + 2,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: 1,
    };
    let caret = quick
        .query
        .render(field, buf, Style::default().fg(rgb(t.fg)));

    if hits.is_empty() {
        buf.set_string(
            inner.x,
            inner.y + 2,
            fit("nothing matches", inner.width),
            Style::default().fg(rgb(t.empty_fg)),
        );
        return caret;
    }

    for (n, item) in hits.iter().enumerate() {
        let y = inner.y + 2 + n as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = n == quick.cursor;
        let style = if selected {
            Style::default()
                .fg(rgb(t.row_cursor_fg))
                .bg(rgb(t.row_cursor_bg))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(rgb(t.row_fg))
        };
        let hint_w = if item.hint.is_empty() {
            0
        } else {
            (starkit::wrap::width_of(&item.hint) + 2).min(inner.width / 2)
        };
        let label_w = inner.width.saturating_sub(hint_w);
        buf.set_string(inner.x, y, fit(&item.label, label_w), style);
        if hint_w > 0 {
            buf.set_string(
                inner.x + label_w,
                y,
                fit(&format!("  {}", item.hint), hint_w),
                Style::default().fg(rgb(t.dim)),
            );
        }
    }
    caret
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn items() -> Vec<Item> {
        vec![
            Item {
                target: Target::Channel(ChannelId(1)),
                label: "#general".into(),
                hint: "First Guild".into(),
            },
            Item {
                target: Target::Channel(ChannelId(2)),
                label: "#gardening-notes".into(),
                hint: "Second Guild".into(),
            },
            Item {
                target: Target::Dm(ChannelId(3)),
                label: "@alex".into(),
                hint: "dm".into(),
            },
            Item {
                target: Target::Guild(GuildId(4)),
                label: "First Guild".into(),
                hint: "server".into(),
            },
        ]
    }

    fn typed(quick: &mut Quick, text: &str) {
        for c in text.chars() {
            quick.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// Nothing typed is everything, in the order it was given.
    #[test]
    fn an_empty_query_lists_everything() {
        let quick = Quick::new(items());
        assert_eq!(quick.hits().len(), 4);
        assert_eq!(
            quick.selected().map(|i| i.target),
            Some(Target::Channel(ChannelId(1)))
        );
    }

    /// The whole reason a matcher is here rather than `contains`.
    #[test]
    fn the_closer_match_comes_first() {
        let mut quick = Quick::new(items());
        typed(&mut quick, "gen");
        let hits = quick.hits();
        assert_eq!(hits[0].label, "#general", "{:?}", hits);
    }

    #[test]
    fn a_query_that_matches_nothing_selects_nothing() {
        let mut quick = Quick::new(items());
        typed(&mut quick, "zzzzq");
        assert!(quick.hits().is_empty());
        assert_eq!(
            quick.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Taken,
            "enter on nothing does nothing rather than opening the first row"
        );
    }

    #[test]
    fn enter_opens_what_is_selected() {
        let mut quick = Quick::new(items());
        typed(&mut quick, "alex");
        assert_eq!(
            quick.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Open(Target::Dm(ChannelId(3)))
        );
    }

    /// Both the arrows and the readline pair, because both are muscle memory
    /// for somebody and neither is a letter that could be part of a query.
    #[test]
    fn the_cursor_moves_and_wraps() {
        let mut quick = Quick::new(items());
        quick.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(quick.cursor, 1);
        quick.handle(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(quick.cursor, 2);
        quick.handle(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(quick.cursor, 1);
        quick.handle(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        quick.handle(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(quick.cursor, 3, "up from the top wraps to the end");
    }

    /// Letters are always typing. A switcher you cannot type `q` into is a
    /// switcher that cannot find `#questions`.
    #[test]
    fn every_letter_is_a_query_and_not_a_command() {
        let mut quick = Quick::new(items());
        typed(&mut quick, "q");
        assert_eq!(quick.query.text(), "q");
        assert_eq!(
            quick.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Close
        );
    }

    #[test]
    fn it_draws_the_query_and_the_hits() {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        let mut quick = Quick::new(items());
        typed(&mut quick, "gen");
        render(area, &mut buf, &t, &mut quick);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("JUMP TO"), "{text}");
        assert!(text.contains("#general"), "{text}");
        assert!(text.contains("First Guild"), "{text}");
    }
}
