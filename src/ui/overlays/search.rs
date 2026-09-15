//! Finding something somebody said.
//!
//! A query, a scope and a page of hits. `/` searches the channel that is open
//! and `alt+f` searches the whole server, which are the two questions anybody
//! actually asks; narrowing by author or by date is a form, and a form in a
//! terminal is a worse version of Discord's own search box.
//!
//! ## Results are not messages
//!
//! What comes back is carried by the event rather than put into `State`, and
//! it stays here. A search reaches back through a year of a channel nobody has
//! open, and inserting the answers into the message store would blow the
//! window away and make what is on screen depend on what was last searched
//! for. Choosing one is [`Command::JumpTo`], which fetches the page *around*
//! it properly and leaves the store knowing it is no longer at the bottom.
//!
//! ## Paging
//!
//! Discord answers twenty-five at a time and says how many there are
//! altogether, so paging is an offset. `ctrl+n` and `ctrl+p` step the pages —
//! not the bare letters the plan asked for, because the query is a text field
//! and `n` in it is the letter `n`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use starkit::chrome::overlay::{self, Anchor};
use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::input::{Edit, TextInput};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use crate::discord::handle::{RequestId, SearchPage, SearchQuery, SearchScope};
use crate::discord::model::Message;
use crate::discord::snowflake::{ChannelId, MessageId};
use crate::discord::Command;
use crate::ui::panels::{fit, rgb};
use crate::ui::theme::Theme;

/// How many results one page holds. Discord's own number, and what the offset
/// steps by.
pub const PAGE: u32 = 25;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1_000_000);

fn next_request() -> RequestId {
    RequestId(NEXT_REQUEST.fetch_add(1, Ordering::Relaxed))
}

/// One hit, flattened to what the row draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub channel: ChannelId,
    pub message: MessageId,
    pub author: String,
    pub when: String,
    pub snippet: String,
}

/// What a key asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Taken,
    Close,
    Jump {
        channel: ChannelId,
        message: MessageId,
    },
    Quit,
}

#[derive(Debug)]
pub struct Search {
    pub query: TextInput,
    pub scope: SearchScope,
    /// What the scope is called, for the title.
    pub where_: String,
    pub hits: Vec<Hit>,
    pub cursor: usize,
    pub scroll: usize,
    pub total: u32,
    pub offset: u32,
    pub loading: bool,
    /// The query the hits on screen answer, so `enter` knows whether it is
    /// running a search or opening one.
    ran: Option<String>,
    asked: Option<RequestId>,
    note: Option<String>,
    commands: Vec<Command>,
}

impl Search {
    pub fn new(scope: SearchScope, where_: String) -> Self {
        Self {
            query: TextInput::single(),
            scope,
            where_,
            hits: Vec::new(),
            cursor: 0,
            scroll: 0,
            total: 0,
            offset: 0,
            loading: false,
            ran: None,
            asked: None,
            note: None,
            commands: Vec::new(),
        }
    }

    pub fn selected(&self) -> Option<&Hit> {
        self.hits.get(self.cursor)
    }

    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    fn run(&mut self, offset: u32) {
        let content = self.query.text().trim().to_string();
        if content.is_empty() {
            return;
        }
        let id = next_request();
        self.asked = Some(id);
        self.loading = true;
        self.note = None;
        self.offset = offset;
        self.ran = Some(content.clone());
        self.commands.push(Command::Search {
            id,
            scope: self.scope,
            query: SearchQuery {
                content,
                channel: None,
                author: None,
                offset,
            },
        });
    }

    /// One page of results. Anything answering an older request is dropped.
    pub fn arrived(
        &mut self,
        id: RequestId,
        result: Result<SearchPage, String>,
        name_of: impl Fn(&Message) -> (String, String),
    ) -> bool {
        if self.asked != Some(id) {
            return false;
        }
        self.loading = false;
        match result {
            Ok(page) => {
                self.total = page.total;
                self.offset = page.offset;
                self.hits = page
                    .messages
                    .iter()
                    .map(|msg| hit_of(msg, &name_of))
                    .collect();
                self.note = if self.hits.is_empty() {
                    Some("nothing found".into())
                } else {
                    None
                };
            }
            Err(reason) => {
                self.hits.clear();
                // 202 is Discord saying "ask again in a moment", which is a
                // note rather than a failure: the server is being indexed.
                self.note = Some(reason);
            }
        }
        self.cursor = 0;
        self.scroll = 0;
        true
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                KeyCode::Char('n') => {
                    self.page(1);
                    Action::Taken
                }
                KeyCode::Char('p') => {
                    self.page(-1);
                    Action::Taken
                }
                KeyCode::Char('f') => Action::Close,
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
            KeyCode::PageDown => {
                self.page(1);
                return Action::Taken;
            }
            KeyCode::PageUp => {
                self.page(-1);
                return Action::Taken;
            }
            _ => {}
        }
        match self.query.handle(key) {
            Edit::Submit => {
                // The first return runs the search; the ones after it open
                // what the cursor is on. A box that re-ran the same query on
                // every return would never let anybody reach a result.
                let typed = self.query.text().trim().to_string();
                if self.ran.as_deref() != Some(typed.as_str()) {
                    self.run(0);
                    return Action::Taken;
                }
                match self.selected() {
                    Some(hit) => Action::Jump {
                        channel: hit.channel,
                        message: hit.message,
                    },
                    None => Action::Taken,
                }
            }
            Edit::Cancel => Action::Close,
            Edit::Consumed | Edit::Ignored => Action::Taken,
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.query.paste(text);
    }

    pub fn step(&mut self, delta: isize) {
        if self.hits.is_empty() {
            self.cursor = 0;
            return;
        }
        let n = self.hits.len() as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, n - 1) as usize;
    }

    pub fn scroll_by(&mut self, delta: i16) {
        self.step(delta.signum() as isize);
    }

    /// Forward or back a page, within what the total says exists.
    pub fn page(&mut self, delta: i32) {
        if self.ran.is_none() {
            return;
        }
        let next = self.offset as i64 + i64::from(delta) * i64::from(PAGE);
        if next < 0 || next >= i64::from(self.total.max(1)) {
            return;
        }
        self.run(next as u32);
    }

    /// What a click landed on.
    pub fn hit_at(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        let list = list_rect(rect(area));
        if x < list.x || x >= list.x + list.width || y < list.y || y >= list.y + list.height {
            return None;
        }
        let index = self.scroll + usize::from(y - list.y);
        (index < self.hits.len()).then_some(index)
    }
}

fn hit_of(msg: &Arc<Message>, name_of: &impl Fn(&Message) -> (String, String)) -> Hit {
    let (author, when) = name_of(msg);
    Hit {
        channel: msg.channel_id,
        message: msg.id,
        author,
        when,
        snippet: snippet(&msg.content),
    }
}

/// One line of what was said, with the markup taken out.
fn snippet(content: &str) -> String {
    let plain = crate::discord::markdown::parse(content).plain_text();
    let one: String = plain
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if one.is_empty() {
        "(no text)".into()
    } else {
        one
    }
}

/// Where the box lands: the same shape every overlay opens in, upper-anchored
/// where the typing boxes sit.
pub fn rect(area: Rect) -> Rect {
    overlay::rect(area, (40, 88), 22, 8, Anchor::Upper)
}

fn list_rect(r: Rect) -> Rect {
    Rect {
        x: r.x + 1,
        y: r.y + 3,
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(4),
    }
}

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    search: &mut Search,
) -> Option<(u16, u16)> {
    let r = rect(area);
    if r.width < 16 || r.height < 6 {
        return None;
    }

    let t = theme;
    let footer = if search.total > 0 {
        format!(
            "{}\u{2013}{} of {} \u{b7} enter go \u{b7} ctrl+n page \u{b7} esc close",
            search.offset + 1,
            search.offset + search.hits.len() as u32,
            search.total
        )
    } else {
        "enter go \u{b7} ctrl+n page \u{b7} esc close".to_string()
    };
    // The core theme type -- a struct literal is not a coercion site, so the
    // deref from this crate's own `Theme` is spelled out here.
    let core: &starkit::theme::Theme = t;
    let inner = overlay::render(
        r,
        buf,
        &overlay::Overlay {
            theme: core,
            title: "search",
            detail: Some(&search.where_),
            footer: Some(&footer),
        },
    );
    if inner.width < 4 || inner.height < 2 {
        return None;
    }

    buf.set_string(
        inner.x,
        inner.y,
        "\u{203a} ",
        Style::default().fg(rgb(t.accent)),
    );
    let caret = search.query.render(
        Rect {
            x: inner.x + 2,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: 1,
        },
        buf,
        Style::default().fg(rgb(t.fg)),
    );

    let list = list_rect(r);
    if list.height == 0 {
        return caret;
    }
    if search.hits.is_empty() {
        let text = if search.loading {
            "\u{2026} searching".to_string()
        } else if let Some(note) = &search.note {
            note.clone()
        } else {
            "type, then press return".to_string()
        };
        buf.set_string(
            list.x,
            list.y,
            fit(&text, list.width),
            Style::default().fg(rgb(t.empty_fg)),
        );
        return caret;
    }

    // Keep the cursor on screen.
    let rows = usize::from(list.height);
    search.scroll = starkit::list::clamp_scroll(search.cursor, search.scroll, rows);

    for (n, hit) in search.hits.iter().enumerate().skip(search.scroll) {
        let row = (n - search.scroll) as u16;
        if row >= list.height {
            break;
        }
        let y = list.y + row;
        let selected = n == search.cursor;
        let style = if selected {
            Style::default()
                .fg(rgb(t.row_cursor_fg))
                .bg(rgb(t.row_cursor_bg))
        } else {
            Style::default().fg(rgb(t.row_fg))
        };
        let head = format!("{}  {}  ", hit.author, hit.when);
        let head_w = starkit::wrap::width_of(&head).min(list.width);
        buf.set_string(
            list.x,
            y,
            fit(&head, head_w),
            style.add_modifier(Modifier::BOLD),
        );
        buf.set_string(
            list.x + head_w,
            y,
            fit(&hit.snippet, list.width.saturating_sub(head_w)),
            style,
        );
    }
    caret
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::GuildId;
    use crate::ui::theme::tests_support::theme;

    fn message(id: u64, text: &str) -> Arc<Message> {
        Arc::new(Message {
            id: MessageId(id),
            channel_id: ChannelId(11),
            content: text.into(),
            ..Message::default()
        })
    }

    fn names(_: &Message) -> (String, String) {
        ("alex".into(), "14:32".into())
    }

    fn typed(s: &mut Search, text: &str) {
        for c in text.chars() {
            s.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn ran(s: &mut Search) -> RequestId {
        match s.take_commands().first() {
            Some(Command::Search { id, .. }) => *id,
            other => panic!("{other:?}"),
        }
    }

    /// The first return runs; the second opens what the cursor is on.
    #[test]
    fn return_runs_the_search_then_opens_a_hit() {
        let mut s = Search::new(SearchScope::Channel(ChannelId(11)), "#general".into());
        typed(&mut s, "hello");
        assert_eq!(
            s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Taken
        );
        let id = ran(&mut s);
        assert!(s.loading);

        s.arrived(
            id,
            Ok(SearchPage {
                total: 2,
                messages: vec![message(101, "hello there"), message(102, "hello again")],
                offset: 0,
            }),
            names,
        );
        assert_eq!(s.hits.len(), 2);
        assert_eq!(s.hits[0].snippet, "hello there");
        assert_eq!(s.hits[0].author, "alex");

        s.step(1);
        assert_eq!(
            s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Jump {
                channel: ChannelId(11),
                message: MessageId(102)
            }
        );
    }

    /// Changing the query makes the next return a search again rather than a
    /// jump into results that answer something else.
    #[test]
    fn editing_the_query_makes_return_search_again() {
        let mut s = Search::new(SearchScope::Guild(GuildId(1)), "the server".into());
        typed(&mut s, "one");
        s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let id = ran(&mut s);
        s.arrived(
            id,
            Ok(SearchPage {
                total: 1,
                messages: vec![message(101, "one")],
                offset: 0,
            }),
            names,
        );
        typed(&mut s, "two");
        assert_eq!(
            s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Taken
        );
        assert!(!s.take_commands().is_empty(), "it searched again");
    }

    /// Paging steps the offset and stops at both ends.
    #[test]
    fn paging_walks_the_offsets_and_no_further() {
        let mut s = Search::new(SearchScope::Guild(GuildId(1)), "the server".into());
        typed(&mut s, "hello");
        s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let id = ran(&mut s);
        s.arrived(
            id,
            Ok(SearchPage {
                total: 60,
                messages: vec![message(101, "a")],
                offset: 0,
            }),
            names,
        );

        s.page(-1);
        assert!(
            s.take_commands().is_empty(),
            "there is no page before the first"
        );
        s.page(1);
        match s.take_commands().first() {
            Some(Command::Search { query, .. }) => assert_eq!(query.offset, PAGE),
            other => panic!("{other:?}"),
        }
        s.offset = 50;
        s.page(1);
        assert!(s.take_commands().is_empty(), "and none past the last");
    }

    /// An answer to an older request is dropped.
    #[test]
    fn a_stale_page_is_ignored() {
        let mut s = Search::new(SearchScope::Channel(ChannelId(11)), "#general".into());
        typed(&mut s, "hello");
        s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let id = ran(&mut s);
        assert!(!s.arrived(RequestId(id.0 + 500), Ok(SearchPage::default()), names));
        assert!(s.loading, "and it is still waiting for its own");
    }

    /// Discord saying "still indexing" is a note, not an empty result.
    #[test]
    fn a_server_still_being_indexed_says_so() {
        let mut s = Search::new(SearchScope::Guild(GuildId(1)), "the server".into());
        typed(&mut s, "hello");
        s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let id = ran(&mut s);
        s.arrived(id, Err("still indexing, try again".into()), names);
        assert!(s.hits.is_empty());
        assert_eq!(s.note.as_deref(), Some("still indexing, try again"));

        let t = theme("terminal");
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &t, &mut s);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("indexing"), "{text}");
    }

    /// Every letter is the query; the box is not a command line.
    #[test]
    fn letters_are_typed_rather_than_obeyed() {
        let mut s = Search::new(SearchScope::Channel(ChannelId(11)), "#general".into());
        typed(&mut s, "npq");
        assert_eq!(s.query.text(), "npq");
    }

    #[test]
    fn it_draws_the_hits() {
        let mut s = Search::new(SearchScope::Channel(ChannelId(11)), "#general".into());
        typed(&mut s, "hello");
        s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let id = ran(&mut s);
        s.arrived(
            id,
            Ok(SearchPage {
                total: 1,
                messages: vec![message(101, "**hello** there")],
                offset: 0,
            }),
            names,
        );
        let t = theme("terminal");
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &t, &mut s);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("SEARCH \u{2014} #general"), "{text}");
        assert!(
            text.contains("hello there"),
            "the markup is taken out: {text}"
        );
        assert!(text.contains("alex"), "{text}");
    }
}
