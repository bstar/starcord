//! Choosing an emoji, a reaction or a GIF.
//!
//! One overlay for three jobs, because they are the same interaction with
//! different answers: a search field, a grid, a cursor, and an `Enter` that
//! does something with what the cursor is on. Three separate overlays would be
//! three copies of the grid arithmetic and three places for the key handling to
//! drift apart.
//!
//! ## What `Enter` means
//!
//! - **Emoji**: the character, or `<:name:id>`, inserted into the composer at
//!   the caret. Not sent — an emoji is part of a sentence.
//! - **Reaction**: [`Command::AddReaction`] on the message it was opened for,
//!   or [`Command::RemoveReaction`] when this account already has that one.
//!   The picker is told which reactions are already the reader's, so the
//!   toggle is decided here rather than guessed at the far end.
//! - **GIF**: [`Command::SendMessage`] with the result's page URL as the whole
//!   content, which is what makes Discord unfurl it. The animation itself is
//!   never uploaded.
//!
//! ## The GIF grid asks, and keeps asking
//!
//! Opening it asks for what is trending; typing asks for a search. Both are
//! debounced here at [`DEBOUNCE`] as well as in the core, and for different
//! reasons: the core's spacer is about what leaves the machine, and this is
//! about not building a request object per keystroke. A request carries an id
//! and an answer that does not match the last id asked for is dropped, so a
//! slow search cannot overwrite a newer one.
//!
//! ## Pictures
//!
//! The grid draws nothing itself. It produces [`Placement`]s — the same type
//! the message list produces — and the application's one picture pass draws
//! them after the overlay's chrome is down. The tile under the cursor is keyed
//! to the animated source and every other tile to the still preview, so
//! exactly one thing on the screen moves.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::input::{Edit, TextInput};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::text::{Line, Span};
use starkit::ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::discord::handle::{EmojiRef, RequestId};
use crate::discord::media::MediaKey;
use crate::discord::model::GifResult;
use crate::discord::snowflake::{ChannelId, EmojiId, MessageId};
use crate::discord::Command;
use crate::ui::panels::chat::media::{Placement, Shape};
use crate::ui::panels::{fit, rgb};
use crate::ui::theme::Theme;

/// How long the typing has to stop before a search goes out.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// How many emoji the grid holds. More than a few screenfuls of guesses is not
/// an answer, and the filter is what narrows it.
const MAX_EMOJI: usize = 512;

/// A cell of the emoji grid, in columns. Two for the picture and two of gap:
/// a grid with no gap reads as one long run of glyphs.
const EMOJI_CELL: u16 = 4;

/// How many GIF tiles the grid will squeeze into whatever width it is given.
const GIF_MIN_COLS: u16 = 3;
const GIF_MAX_COLS: u16 = 5;

/// Somewhere to put the request ids, which only have to be unique.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

fn next_request() -> RequestId {
    RequestId(NEXT_REQUEST.fetch_add(1, Ordering::Relaxed))
}

/// What the picker is picking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Emoji,
    Gif,
    /// A reaction on one message.
    Reaction(MessageId),
}

/// One thing the emoji grid offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emoji {
    /// A unicode emoji and its shortcode, with no colon.
    Unicode { ch: String, name: String },
    /// One of the account's own.
    Custom {
        name: String,
        id: EmojiId,
        animated: bool,
    },
}

impl Emoji {
    pub fn name(&self) -> &str {
        match self {
            Emoji::Unicode { name, .. } | Emoji::Custom { name, .. } => name,
        }
    }

    /// What goes into a message.
    pub fn insert(&self) -> String {
        match self {
            Emoji::Unicode { ch, .. } => ch.clone(),
            Emoji::Custom { name, id, animated } => {
                let a = if *animated { "a" } else { "" };
                format!("<{a}:{name}:{id}>")
            }
        }
    }

    /// What a reaction on it is called.
    pub fn reaction(&self) -> EmojiRef {
        match self {
            Emoji::Unicode { ch, .. } => EmojiRef::Unicode(ch.clone()),
            Emoji::Custom { name, id, animated } => EmojiRef::Custom {
                name: name.clone(),
                id: *id,
                animated: *animated,
            },
        }
    }

    /// The picture for one, when there is one.
    fn key(&self) -> Option<MediaKey> {
        match self {
            Emoji::Unicode { .. } => None,
            Emoji::Custom { id, animated, .. } => Some(MediaKey::Emoji {
                id: *id,
                animated: *animated,
                size: 32,
            }),
        }
    }
}

/// What a key asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Taken,
    Close,
    /// Put this into the composer.
    Insert(String),
    /// Switch this picker to the GIF grid.
    ToGif,
    Quit,
}

pub struct Picker {
    pub kind: Kind,
    pub query: TextInput,
    pub cursor: usize,
    pub scroll: usize,
    /// Where the reaction or the GIF goes.
    channel: Option<ChannelId>,
    /// Everything the emoji grid could offer, before the query narrows it.
    all: Vec<Emoji>,
    shown: Vec<Emoji>,
    /// The reactions this account already has on the message, by key.
    mine: HashSet<String>,
    gifs: Vec<GifResult>,
    /// The id of the answer worth listening to.
    asked: Option<RequestId>,
    loading: bool,
    /// When the query last changed, for the debounce.
    typed: Option<Instant>,
    commands: Vec<Command>,
    /// The grid as it was last drawn, for the pointer and for the cursor keys.
    cols: u16,
    rows: u16,
    /// Cell aspect, for square GIF tiles.
    aspect: f32,
    /// What the last answer said, if it was a failure.
    note: Option<String>,
}

impl std::fmt::Debug for Picker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picker")
            .field("kind", &self.kind)
            .field("query", &self.query.text())
            .field("shown", &self.shown.len())
            .field("gifs", &self.gifs.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl Picker {
    /// A picker over the account's own emoji plus every unicode one.
    pub fn new(
        kind: Kind,
        channel: Option<ChannelId>,
        custom: Vec<(String, EmojiId, bool)>,
        aspect: f32,
    ) -> Self {
        let mut all: Vec<Emoji> = custom
            .into_iter()
            .map(|(name, id, animated)| Emoji::Custom { name, id, animated })
            .collect();
        for emoji in emojis::iter() {
            let Some(shortcode) = emoji.shortcode() else {
                continue;
            };
            all.push(Emoji::Unicode {
                ch: emoji.as_str().to_string(),
                name: shortcode.to_string(),
            });
        }
        let mut picker = Self {
            kind,
            query: TextInput::single(),
            cursor: 0,
            scroll: 0,
            channel,
            all,
            shown: Vec::new(),
            mine: HashSet::new(),
            gifs: Vec::new(),
            asked: None,
            loading: false,
            typed: None,
            commands: Vec::new(),
            cols: 1,
            rows: 1,
            aspect: if aspect > 0.0 { aspect } else { 2.0 },
            note: None,
        };
        picker.filter();
        if kind == Kind::Gif {
            picker.ask_trending();
        }
        picker
    }

    /// Which reactions the reader already has on this message, so `Enter` on
    /// one of them takes it off again.
    pub fn with_mine(mut self, mine: impl IntoIterator<Item = String>) -> Self {
        self.mine = mine.into_iter().collect();
        self
    }

    /// Turn an emoji grid into a GIF grid, keeping the box open.
    ///
    /// The query goes, because `pep` is a good way to find an emoji and a poor
    /// way to find a GIF, and what was typed to narrow one list is rarely what
    /// would narrow the other.
    pub fn switch_to_gifs(&mut self) {
        self.kind = Kind::Gif;
        self.query.clear();
        self.gifs.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.note = None;
        self.ask_trending();
    }

    pub fn is_gif(&self) -> bool {
        self.kind == Kind::Gif
    }

    pub fn shown(&self) -> &[Emoji] {
        &self.shown
    }

    pub fn gifs(&self) -> &[GifResult] {
        &self.gifs
    }

    /// How many things the grid is holding.
    fn len(&self) -> usize {
        if self.is_gif() {
            self.gifs.len()
        } else {
            self.shown.len()
        }
    }

    pub fn selected_emoji(&self) -> Option<&Emoji> {
        self.shown.get(self.cursor)
    }

    pub fn selected_gif(&self) -> Option<&GifResult> {
        self.gifs.get(self.cursor)
    }

    /// Whether this account already reacted with what the cursor is on.
    pub fn already_mine(&self) -> bool {
        self.selected_emoji()
            .map(|e| self.mine.contains(&e.reaction().key()))
            .unwrap_or(false)
    }

    fn filter(&mut self) {
        let needle = self.query.text().trim().to_lowercase();
        self.shown = self
            .all
            .iter()
            .filter(|e| needle.is_empty() || e.name().to_lowercase().contains(&needle))
            .take(MAX_EMOJI)
            .cloned()
            .collect();
        // The account's own first, then the shortest name: `:smile:` before
        // `:smiley_cat:` for a query of `smil`.
        self.shown.sort_by_key(|e| {
            (
                matches!(e, Emoji::Unicode { .. }),
                e.name().len(),
                e.name().to_string(),
            )
        });
        self.cursor = 0;
        self.scroll = 0;
    }

    fn ask_trending(&mut self) {
        let id = next_request();
        self.asked = Some(id);
        self.loading = true;
        self.commands.push(Command::GifTrending { id });
    }

    fn ask_search(&mut self, query: String) {
        let id = next_request();
        self.asked = Some(id);
        self.loading = true;
        if query.trim().is_empty() {
            self.commands.push(Command::GifTrending { id });
        } else {
            self.commands.push(Command::GifSearch { id, query });
        }
    }

    /// The debounce. Called once a frame.
    pub fn tick(&mut self, now: Instant) {
        let Some(at) = self.typed else { return };
        if now.duration_since(at) < DEBOUNCE {
            return;
        }
        self.typed = None;
        if self.is_gif() {
            let query = self.query.text().to_string();
            self.ask_search(query);
        }
    }

    /// One page of GIF results. Anything answering an older request is dropped.
    pub fn gifs_arrived(&mut self, id: RequestId, result: Result<Vec<GifResult>, String>) -> bool {
        if self.asked != Some(id) {
            return false;
        }
        self.loading = false;
        match result {
            Ok(results) => {
                self.gifs = results;
                self.note = None;
            }
            Err(reason) => {
                self.gifs.clear();
                self.note = Some(reason);
            }
        }
        self.cursor = 0;
        self.scroll = 0;
        true
    }

    /// What the application should send on this frame.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                // The key that opens the GIF grid from the composer also gets
                // there from the emoji grid, which is where somebody who
                // wanted a GIF and typed `:` ends up.
                KeyCode::Char('g') if !self.is_gif() => Action::ToGif,
                KeyCode::Char('e') | KeyCode::Char('g') => Action::Close,
                KeyCode::Char('n') => {
                    self.step(1);
                    Action::Taken
                }
                KeyCode::Char('p') => {
                    self.step(-1);
                    Action::Taken
                }
                _ => Action::Taken,
            };
        }
        match key.code {
            KeyCode::Left => {
                self.step(-1);
                return Action::Taken;
            }
            KeyCode::Right | KeyCode::Tab => {
                self.step(1);
                return Action::Taken;
            }
            KeyCode::Up => {
                self.step(-(self.cols as isize));
                return Action::Taken;
            }
            KeyCode::Down => {
                self.step(self.cols as isize);
                return Action::Taken;
            }
            KeyCode::PageUp => {
                self.step(-(self.cols as isize) * i32::from(self.rows) as isize);
                return Action::Taken;
            }
            KeyCode::PageDown => {
                self.step(self.cols as isize * i32::from(self.rows) as isize);
                return Action::Taken;
            }
            _ => {}
        }
        match self.query.handle(key) {
            Edit::Submit => self.choose(),
            Edit::Cancel => Action::Close,
            Edit::Consumed => {
                if self.is_gif() {
                    self.typed = Some(Instant::now());
                } else {
                    self.filter();
                }
                Action::Taken
            }
            Edit::Ignored => Action::Taken,
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.query.paste(text);
        if self.is_gif() {
            self.typed = Some(Instant::now());
        } else {
            self.filter();
        }
    }

    /// `Enter`, and a double-click.
    pub fn choose(&mut self) -> Action {
        match self.kind {
            Kind::Emoji => match self.selected_emoji() {
                Some(emoji) => Action::Insert(emoji.insert()),
                None => Action::Taken,
            },
            Kind::Reaction(message) => {
                let (Some(channel), Some(emoji)) = (self.channel, self.selected_emoji()) else {
                    return Action::Taken;
                };
                let mine = self.mine.contains(&emoji.reaction().key());
                let emoji = emoji.reaction();
                self.commands.push(if mine {
                    Command::RemoveReaction {
                        channel,
                        message,
                        emoji,
                    }
                } else {
                    Command::AddReaction {
                        channel,
                        message,
                        emoji,
                    }
                });
                Action::Close
            }
            Kind::Gif => {
                let (Some(channel), Some(gif)) = (self.channel, self.selected_gif()) else {
                    return Action::Taken;
                };
                // The page link, as content. Discord unfurls it into a `gifv`
                // embed; uploading the animation itself would be a twelve
                // megabyte attachment of somebody else's file.
                self.commands.push(Command::SendMessage {
                    channel,
                    content: gif.url.clone(),
                    reply_to: None,
                    mention_author: false,
                    attachments: Vec::new(),
                });
                Action::Close
            }
        }
    }

    pub fn step(&mut self, delta: isize) {
        let len = self.len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        self.cursor = next;
        self.follow();
    }

    /// Keep the cursor's row on screen.
    fn follow(&mut self) {
        let cols = usize::from(self.cols.max(1));
        let rows = usize::from(self.rows.max(1));
        let row = self.cursor / cols;
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + rows {
            self.scroll = row + 1 - rows;
        }
    }

    pub fn scroll_by(&mut self, delta: i16) {
        let cols = self.cols.max(1) as isize;
        self.step(delta.signum() as isize * cols);
    }

    /// What a click landed on, as an index into the grid.
    pub fn hit(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        let grid = self.grid_rect(area);
        if x < grid.x || y < grid.y || x >= grid.x + grid.width || y >= grid.y + grid.height {
            return None;
        }
        let (cell_w, cell_h) = self.cell();
        let col = usize::from((x - grid.x) / cell_w.max(1));
        let row = usize::from((y - grid.y) / cell_h.max(1));
        if col >= usize::from(self.cols) {
            return None;
        }
        let index = (self.scroll + row) * usize::from(self.cols.max(1)) + col;
        (index < self.len()).then_some(index)
    }

    /// One cell, in columns and rows.
    fn cell(&self) -> (u16, u16) {
        if self.is_gif() {
            let width = self.gif_tile();
            // Square on the screen rather than square in cells: a cell is
            // about twice as tall as it is wide, and a tile that ignored that
            // would be a letterbox.
            let height = ((f32::from(width) / self.aspect).round() as u16).max(2);
            (width, height + 1)
        } else {
            (EMOJI_CELL, 1)
        }
    }

    fn gif_tile(&self) -> u16 {
        12
    }

    fn grid_rect(&self, area: Rect) -> Rect {
        let inner = inner_of(area);
        Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: inner.height.saturating_sub(3),
        }
    }
}

/// The title, which says what pressing return will do.
fn title(kind: Kind) -> &'static str {
    match kind {
        Kind::Emoji => "emoji",
        Kind::Gif => "a GIF",
        Kind::Reaction(_) => "react with",
    }
}

fn footer(kind: Kind, mine: bool) -> &'static str {
    match kind {
        Kind::Emoji => " enter insert \u{b7} ctrl+g GIFs \u{b7} esc close ",
        Kind::Gif => " enter send it \u{b7} esc close ",
        Kind::Reaction(_) if mine => " enter take it off \u{b7} esc close ",
        Kind::Reaction(_) => " enter react \u{b7} esc close ",
    }
}

/// Where the box lands.
pub fn rect(area: Rect, gif: bool) -> Rect {
    let w = if gif {
        area.width.saturating_sub(6).clamp(40, 68)
    } else {
        area.width.saturating_sub(8).clamp(30, 56)
    };
    let h = if gif {
        area.height.saturating_sub(4).clamp(10, 24)
    } else {
        area.height.saturating_sub(6).clamp(8, 18)
    };
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 3,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

fn inner_of(r: Rect) -> Rect {
    Rect {
        x: r.x + 1,
        y: r.y + 1,
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(2),
    }
}

pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, picker: &mut Picker) -> Vec<Placement> {
    let r = rect(area, picker.is_gif());
    if r.width < 12 || r.height < 6 {
        return Vec::new();
    }
    Clear.render(r, buf);

    let t = theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(rgb(t.border_focused)))
        .title(Span::styled(
            format!(
                "{}{} ",
                starkit::chrome::frame::TITLE_LEAD,
                title(picker.kind)
            ),
            Style::default()
                .fg(rgb(t.header_fg))
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                footer(picker.kind, picker.already_mine()),
                Style::default().fg(rgb(t.dim)),
            ))
            .right_aligned(),
        )
        .style(Style::default().bg(rgb(t.panel_bg)));
    let inner = block.inner(r);
    block.render(r, buf);
    starkit::chrome::frame::render_corners(r, buf, t, true);
    if inner.width < 4 || inner.height < 3 {
        return Vec::new();
    }

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
    picker
        .query
        .render(field, buf, Style::default().fg(rgb(t.fg)));

    let grid = picker.grid_rect(r);
    let (cell_w, cell_h) = picker.cell();
    let cols = if picker.is_gif() {
        (grid.width / cell_w.max(1)).clamp(GIF_MIN_COLS, GIF_MAX_COLS)
    } else {
        (grid.width / cell_w.max(1)).max(1)
    };
    picker.cols = cols.max(1);
    picker.rows = (grid.height / cell_h.max(1)).max(1);
    picker.follow();

    if let Some(note) = picker.note.clone() {
        buf.set_string(
            grid.x,
            grid.y,
            fit(&note, grid.width),
            Style::default().fg(rgb(t.error)),
        );
        return Vec::new();
    }
    if picker.len() == 0 {
        let text = if picker.loading {
            "\u{2026} asking"
        } else {
            "nothing matches"
        };
        buf.set_string(
            grid.x,
            grid.y,
            fit(text, grid.width),
            Style::default().fg(rgb(t.empty_fg)),
        );
        return Vec::new();
    }

    if picker.is_gif() {
        gif_grid(grid, buf, t, picker, cell_w, cell_h)
    } else {
        emoji_grid(grid, buf, t, picker, inner)
    }
}

fn emoji_grid(
    grid: Rect,
    buf: &mut Buffer,
    t: &Theme,
    picker: &Picker,
    inner: Rect,
) -> Vec<Placement> {
    let mut places = Vec::new();
    let cols = usize::from(picker.cols);
    let first = picker.scroll * cols;
    for (n, emoji) in picker.shown.iter().enumerate().skip(first) {
        let cell = n - first;
        let row = (cell / cols) as u16;
        let col = (cell % cols) as u16;
        if row >= grid.height {
            break;
        }
        let x = grid.x + col * EMOJI_CELL;
        let y = grid.y + row;
        let selected = n == picker.cursor;
        let style = if selected {
            Style::default()
                .fg(rgb(t.row_cursor_fg))
                .bg(rgb(t.row_cursor_bg))
        } else {
            Style::default().fg(rgb(t.row_fg))
        };
        match emoji {
            Emoji::Unicode { ch, .. } => {
                buf.set_string(x, y, fit(ch, EMOJI_CELL), style);
            }
            Emoji::Custom { name, .. } => {
                // Two cells, which is what a custom emoji is drawn in
                // everywhere else in the program, and the first two letters of
                // its name while the picture is on its way.
                let short: String = name.chars().take(2).collect();
                buf.set_string(x, y, fit(&short, EMOJI_CELL), style);
                if let Some(key) = emoji.key() {
                    places.push(Placement {
                        rect: Rect {
                            x,
                            y,
                            width: 2,
                            height: 1,
                        },
                        clip: grid,
                        clipped: false,
                        key,
                        shape: Shape::Emoji { name: name.clone() },
                        alt: String::new(),
                    });
                }
            }
        }
    }

    // What the cursor is on, spelled out, because two cells is not a name.
    if let Some(emoji) = picker.selected_emoji() {
        let y = inner.y + inner.height - 1;
        buf.set_string(
            inner.x,
            y,
            fit(&format!(":{}:", emoji.name()), inner.width),
            Style::default().fg(rgb(t.dim)),
        );
    }
    places
}

fn gif_grid(
    grid: Rect,
    buf: &mut Buffer,
    t: &Theme,
    picker: &Picker,
    cell_w: u16,
    cell_h: u16,
) -> Vec<Placement> {
    let mut places = Vec::new();
    let cols = usize::from(picker.cols);
    let first = picker.scroll * cols;
    for (n, gif) in picker.gifs.iter().enumerate().skip(first) {
        let cell = n - first;
        let row = (cell / cols) as u16;
        let col = (cell % cols) as u16;
        if (row + 1) * cell_h > grid.height {
            break;
        }
        let x = grid.x + col * cell_w;
        let y = grid.y + row * cell_h;
        let selected = n == picker.cursor;
        let tile = Rect {
            x,
            y,
            width: cell_w.saturating_sub(1).max(1),
            height: cell_h.saturating_sub(1).max(1),
        };
        // The one under the cursor is the animated source; every other tile is
        // the still preview, so exactly one picture on the screen moves.
        let url = if selected {
            gif.gif_src.clone()
        } else {
            String::new()
        };
        let url = if url.is_empty() {
            gif.tile().unwrap_or_default().to_string()
        } else {
            url
        };
        if !url.is_empty() {
            places.push(Placement {
                rect: tile,
                clip: grid,
                clipped: false,
                key: MediaKey::Gif { url },
                shape: Shape::Picture,
                alt: String::new(),
            });
        }
        let caption = Rect {
            x,
            y: y + tile.height,
            width: tile.width,
            height: 1,
        };
        if caption.y < grid.y + grid.height {
            let style = if selected {
                Style::default()
                    .fg(rgb(t.row_cursor_fg))
                    .bg(rgb(t.row_cursor_bg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(rgb(t.dim))
            };
            buf.set_string(caption.x, caption.y, fit(&gif.title, caption.width), style);
        }
    }
    places
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn custom() -> Vec<(String, EmojiId, bool)> {
        vec![
            ("pepe".into(), EmojiId(1), false),
            ("blobwave".into(), EmojiId(2), true),
        ]
    }

    fn emoji_picker() -> Picker {
        Picker::new(Kind::Emoji, Some(ChannelId(7)), custom(), 2.0)
    }

    fn typed(p: &mut Picker, text: &str) {
        for c in text.chars() {
            p.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// Typing narrows the grid, and the account's own emoji come first.
    #[test]
    fn the_query_filters_and_the_servers_own_lead() {
        let mut p = emoji_picker();
        assert!(p.shown().len() > 100, "every unicode emoji is offered");
        typed(&mut p, "pep");
        assert!(!p.shown().is_empty());
        assert_eq!(p.shown()[0].name(), "pepe");
        assert!(matches!(p.shown()[0], Emoji::Custom { .. }));

        typed(&mut p, "zzzz");
        assert!(p.shown().is_empty());
        assert_eq!(p.choose(), Action::Taken, "nothing chosen is nothing done");
    }

    /// The three things `Enter` can mean, spelled out.
    #[test]
    fn what_enter_inserts_or_sends() {
        let mut p = emoji_picker();
        typed(&mut p, "pepe");
        assert_eq!(p.choose(), Action::Insert("<:pepe:1>".into()));

        let mut p = emoji_picker();
        typed(&mut p, "blobwave");
        assert_eq!(
            p.choose(),
            Action::Insert("<a:blobwave:2>".into()),
            "an animated one carries its a"
        );

        let mut p = Picker::new(Kind::Emoji, None, Vec::new(), 2.0);
        typed(&mut p, "smile");
        let inserted = match p.choose() {
            Action::Insert(text) => text,
            other => panic!("{other:?}"),
        };
        assert!(!inserted.contains(':'), "a unicode emoji is the character");
    }

    /// A chip the reader is already on comes off rather than going on twice.
    /// This is the bug from the milestone before: a click on a reaction always
    /// added one.
    #[test]
    fn reacting_with_one_i_already_have_takes_it_off() {
        let mine = ["pepe:1".to_string()];
        let mut p = Picker::new(
            Kind::Reaction(MessageId(5)),
            Some(ChannelId(7)),
            custom(),
            2.0,
        )
        .with_mine(mine);
        typed(&mut p, "pepe");
        assert!(p.already_mine());
        assert_eq!(p.choose(), Action::Close);
        let sent = p.take_commands();
        assert!(
            matches!(sent.first(), Some(Command::RemoveReaction { .. })),
            "{sent:?}"
        );

        // And one that is not already there is added.
        let mut p = Picker::new(
            Kind::Reaction(MessageId(5)),
            Some(ChannelId(7)),
            custom(),
            2.0,
        );
        typed(&mut p, "pepe");
        assert!(!p.already_mine());
        p.choose();
        assert!(matches!(
            p.take_commands().first(),
            Some(Command::AddReaction { .. })
        ));
    }

    /// Opening the GIF grid asks for what is trending, and typing asks for a
    /// search once the typing stops.
    #[test]
    fn the_gif_grid_asks_once_it_has_been_left_alone() {
        let mut p = Picker::new(Kind::Gif, Some(ChannelId(7)), Vec::new(), 2.0);
        let first = p.take_commands();
        assert!(matches!(first.first(), Some(Command::GifTrending { .. })));

        typed(&mut p, "cat");
        assert!(p.take_commands().is_empty(), "not on every keystroke");
        let now = Instant::now();
        p.tick(now);
        assert!(p.take_commands().is_empty(), "not before the debounce");
        p.tick(now + DEBOUNCE + Duration::from_millis(1));
        let sent = p.take_commands();
        match sent.first() {
            Some(Command::GifSearch { query, .. }) => assert_eq!(query, "cat"),
            other => panic!("{other:?}"),
        }
    }

    /// An answer to a question nobody asked is dropped, so a slow search
    /// cannot overwrite a newer one.
    #[test]
    fn a_stale_answer_is_ignored() {
        let mut p = Picker::new(Kind::Gif, Some(ChannelId(7)), Vec::new(), 2.0);
        let id = match p.take_commands().first() {
            Some(Command::GifTrending { id }) => *id,
            other => panic!("{other:?}"),
        };
        assert!(!p.gifs_arrived(RequestId(id.0 + 999), Ok(vec![gif("a")])));
        assert!(p.gifs().is_empty());
        assert!(p.gifs_arrived(id, Ok(vec![gif("a"), gif("b")])));
        assert_eq!(p.gifs().len(), 2);
    }

    fn gif(name: &str) -> GifResult {
        GifResult {
            id: name.into(),
            title: name.into(),
            url: format!("https://tenor.invalid/view/{name}"),
            src: String::new(),
            gif_src: format!("https://media.invalid/{name}.gif"),
            width: 200,
            height: 200,
            preview: format!("https://media.invalid/{name}.png"),
        }
    }

    /// Choosing a GIF sends its page link as the whole message.
    #[test]
    fn choosing_a_gif_posts_its_link() {
        let mut p = Picker::new(Kind::Gif, Some(ChannelId(7)), Vec::new(), 2.0);
        let id = match p.take_commands().first() {
            Some(Command::GifTrending { id }) => *id,
            other => panic!("{other:?}"),
        };
        p.gifs_arrived(id, Ok(vec![gif("cat")]));
        assert_eq!(p.choose(), Action::Close);
        match p.take_commands().first() {
            Some(Command::SendMessage {
                content,
                attachments,
                ..
            }) => {
                assert_eq!(content, "https://tenor.invalid/view/cat");
                assert!(attachments.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    /// The grid walks in two dimensions and never leaves the list.
    #[test]
    fn the_cursor_moves_across_the_grid_and_stops_at_the_ends() {
        let mut p = emoji_picker();
        let area = Rect::new(0, 0, 100, 30);
        let t = theme("terminal");
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &t, &mut p);
        assert!(p.cols > 1, "the grid has more than one column");

        p.handle(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(p.cursor, 1);
        p.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(p.cursor, 1 + usize::from(p.cols));
        p.handle(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(p.cursor, 1);
        p.handle(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        p.handle(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(p.cursor, 0, "and it does not go negative");
    }

    /// Letters are always the query. A picker you cannot type `q` into cannot
    /// find `:question:`.
    #[test]
    fn every_letter_is_the_query() {
        let mut p = emoji_picker();
        typed(&mut p, "q");
        assert_eq!(p.query.text(), "q");
        assert_eq!(
            p.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Close
        );
    }

    /// `ctrl+g` from the emoji grid is the GIF grid.
    #[test]
    fn the_emoji_grid_hands_over_to_the_gifs() {
        let mut p = emoji_picker();
        assert_eq!(
            p.handle(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Action::ToGif
        );
    }

    /// A click lands on the cell it is over.
    #[test]
    fn a_click_finds_the_cell_under_it() {
        let mut p = emoji_picker();
        let area = Rect::new(0, 0, 100, 30);
        let t = theme("terminal");
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &t, &mut p);
        let grid = p.grid_rect(rect(area, false));
        assert_eq!(p.hit(rect(area, false), grid.x, grid.y), Some(0));
        assert_eq!(
            p.hit(rect(area, false), grid.x + EMOJI_CELL, grid.y + 1),
            Some(usize::from(p.cols) + 1)
        );
        assert_eq!(p.hit(rect(area, false), 0, 0), None, "outside the grid");
    }

    #[test]
    fn it_draws_the_grid_and_says_what_is_chosen() {
        let mut p = emoji_picker();
        typed(&mut p, "pepe");
        let t = theme("terminal");
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let places = render(area, &mut buf, &t, &mut p);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("emoji"), "{text}");
        assert!(text.contains(":pepe:"), "{text}");
        assert_eq!(places.len(), 1, "one custom emoji, one picture");
    }
}
