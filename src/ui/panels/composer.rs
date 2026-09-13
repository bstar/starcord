//! Writing a message.
//!
//! A text field, a mode, a draft per channel and three autocomplete popups.
//! The field itself is STAR/KIT's [`TextInput`]; what is here is everything
//! that makes it a message composer rather than a box of letters.
//!
//! ## The raw-key line
//!
//! While this has focus it eats keys, because `d` in a sentence is a letter
//! rather than "delete". Where exactly that line falls is decided next door in
//! [`keymap::composer_eats`](crate::ui::keymap::composer_eats) and asserted by
//! tests there, not here: the rule is that **every `alt+…` falls through** so
//! the panel keys keep working mid-word, and that **no plain letter is needed
//! to leave**, so nothing anybody types can strand them.
//!
//! ## Drafts outlive the channel
//!
//! Switching away from a half-written message keeps it, and switching back
//! restores it, because the alternative is losing a paragraph to a stray
//! `alt+2`. They are held here and sent to the core as `Command::SetDraft`,
//! which writes them to `session.toml` — mode 0600, because a draft is a
//! message.
//!
//! ## Escape is a chain, not a key
//!
//! `esc` undoes the innermost thing: an open autocomplete, then a reply or an
//! edit, then focus itself. One key that always means "back one step" is
//! learnable; three keys that each mean "back" in one particular state are
//! not.

use std::collections::HashMap;

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::input::{Edit, TextInput};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::{fit, rgb};
use crate::config::{Compose, SendKey};
use crate::discord::snowflake::{ChannelId, EmojiId, MessageId};
use crate::ui::theme::Theme;

/// The floor, in rows, including the border and the header word row.
///
/// Four, not three: two borders, the row the action words sit on, and one line
/// of text. At three there is nowhere to type, and the panel is a box with
/// `attach emoji gif` in it — which is what it looked like the first time it
/// was drawn.
pub const MIN_ROWS: u16 = 4;

/// The most autocomplete suggestions ever shown at once.
const MAX_SUGGESTIONS: usize = 6;

/// Discord's own limit. Refused here as well as in the core, because a message
/// that is refused after it has been typed is a message somebody has to
/// retype.
pub const MAX_CHARS: usize = 2000;

/// What the composer is doing with what is typed into it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Reply {
        to: MessageId,
        author: String,
        /// Whether the reply pings. `R` is the no-ping spelling of `r`.
        ping: bool,
    },
    Edit {
        id: MessageId,
        /// What it said before, so `esc` can put it back.
        original: String,
    },
}

impl Mode {
    pub fn is_normal(&self) -> bool {
        matches!(self, Mode::Normal)
    }
}

/// What is being completed, and from where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completing {
    Emoji,
    User,
    Channel,
}

impl Completing {
    fn trigger(self) -> char {
        match self {
            Completing::Emoji => ':',
            Completing::User => '@',
            Completing::Channel => '#',
        }
    }
}

/// One thing the popup is offering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What the row says.
    pub label: String,
    /// What replaces the trigger and the query when it is accepted.
    pub insert: String,
}

/// An open autocomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autocomplete {
    pub kind: Completing,
    /// Byte offset of the trigger character in the text.
    pub start: usize,
    pub query: String,
    pub items: Vec<Candidate>,
    pub cursor: usize,
}

/// What the composer can be asked for from the outside.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// Who can be mentioned: the channel's recent authors and its members.
    pub users: Vec<(String, u64)>,
    /// Where a `#` can point.
    pub channels: Vec<(String, u64)>,
    /// Custom emoji the account can use, by name.
    pub emoji: Vec<(String, EmojiId, bool)>,
}

/// What a key asked the program to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Handled; nothing else to do.
    Taken,
    /// Not a key the composer wants. The dispatcher gets it.
    Ignored,
    /// Send what is written.
    Send,
    /// Save the edit that is open.
    SaveEdit(MessageId),
    /// `esc` with nothing left to cancel: focus the chat.
    Leave,
    /// `up` on an empty field: open the last message this account sent.
    EditLast,
    /// The text changed, so the draft and the typing indicator both move.
    Changed,
    /// One of the things the composer offers and does not own yet: the emoji
    /// picker, the GIF picker, attaching a file, pasting a picture. Named here
    /// rather than left to fall through the key table, so that the panel and
    /// the line `keymap::composer_eats` draws say the same thing.
    Wants(Action),
}

/// What the composer can ask the application for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    EmojiPicker,
    GifPicker,
    Attach,
    PasteImage,
}

pub struct Composer {
    pub input: TextInput,
    pub mode: Mode,
    pub complete: Option<Autocomplete>,
    /// What was typed and not sent, per channel.
    drafts: HashMap<ChannelId, String>,
    channel: Option<ChannelId>,
    /// A line under the field: what the attach and picker words would do, or
    /// why a send did not go. Cleared by whatever set it.
    pub note: Option<String>,
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}

impl Composer {
    pub fn new() -> Self {
        Self {
            input: TextInput::multiline().with_max_chars(MAX_CHARS),
            mode: Mode::Normal,
            complete: None,
            drafts: HashMap::new(),
            channel: None,
            note: None,
        }
    }

    pub fn text(&self) -> &str {
        self.input.text()
    }

    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub fn channel(&self) -> Option<ChannelId> {
        self.channel
    }

    /// Channels with something unsent in them, for the quit confirmation.
    pub fn unsent(&self) -> usize {
        let mut n = self
            .drafts
            .values()
            .filter(|d| !d.trim().is_empty())
            .count();
        if let Some(channel) = self.channel {
            let live = !self.input.text().trim().is_empty();
            let stored = self
                .drafts
                .get(&channel)
                .is_some_and(|d| !d.trim().is_empty());
            if live && !stored {
                n += 1;
            } else if !live && stored {
                n -= 1;
            }
        }
        n
    }

    /// Draft text for a channel, for the session file.
    pub fn draft(&self, channel: ChannelId) -> String {
        if self.channel == Some(channel) {
            return self.input.text().to_string();
        }
        self.drafts.get(&channel).cloned().unwrap_or_default()
    }

    /// Restore everything the session file remembered.
    pub fn load_drafts(&mut self, drafts: impl IntoIterator<Item = (ChannelId, String)>) {
        for (channel, text) in drafts {
            if self.channel == Some(channel) {
                if self.input.is_empty() {
                    self.input.set_text(text);
                }
                continue;
            }
            self.drafts.insert(channel, text);
        }
    }

    /// Move to another channel, keeping what was written in this one.
    pub fn open(&mut self, channel: ChannelId) {
        if self.channel == Some(channel) {
            return;
        }
        if let Some(previous) = self.channel {
            let text = self.input.take();
            if text.trim().is_empty() {
                self.drafts.remove(&previous);
            } else {
                self.drafts.insert(previous, text);
            }
        }
        self.channel = Some(channel);
        self.mode = Mode::Normal;
        self.complete = None;
        let draft = self.drafts.get(&channel).cloned().unwrap_or_default();
        self.input.set_text(draft);
    }

    /// Take the text to send, leaving the field and the draft empty.
    pub fn take(&mut self) -> String {
        self.complete = None;
        let text = self.input.take();
        if let Some(channel) = self.channel {
            self.drafts.remove(&channel);
        }
        self.mode = Mode::Normal;
        text
    }

    pub fn reply_to(&mut self, to: MessageId, author: String, ping: bool) {
        self.mode = Mode::Reply { to, author, ping };
    }

    pub fn edit(&mut self, id: MessageId, content: String) {
        self.mode = Mode::Edit {
            id,
            original: self.input.text().to_string(),
        };
        self.input.set_text(content);
    }

    /// `esc`, one step at a time. Returns whether anything was cancelled.
    pub fn cancel(&mut self) -> bool {
        if self.complete.take().is_some() {
            return true;
        }
        match std::mem::take(&mut self.mode) {
            Mode::Normal => false,
            Mode::Reply { .. } => true,
            Mode::Edit { original, .. } => {
                self.input.set_text(original);
                true
            }
        }
    }

    /// Rows the panel wants from the dock, including its own chrome.
    pub fn rows(&self, cfg: &Compose, width: u16) -> u16 {
        let text_width = width.saturating_sub(2).max(1);
        let text = self.input.height(text_width).max(1);
        let banner = u16::from(!self.mode.is_normal());
        let popup = self
            .complete
            .as_ref()
            .map(|c| c.items.len().min(MAX_SUGGESTIONS) as u16)
            .unwrap_or(0);
        // Three of chrome: two borders and the header row the words sit on.
        (text + banner + popup + 3).clamp(MIN_ROWS, cfg.max_rows.max(MIN_ROWS))
    }

    /// One key, of the ones the key table hands to the composer.
    ///
    /// Which those are is [`keymap::composer_eats`](crate::ui::keymap::composer_eats)
    /// and the dispatcher asks it before calling this, so `esc` and `tab` never
    /// arrive here: they are the ways out, and the way out of a text field
    /// cannot be a key the text field might want.
    pub fn handle(&mut self, key: KeyEvent, cfg: &Compose, sources: &Sources) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // The popup first: while it is open, the keys that move a list belong
        // to it and nothing else.
        if self.complete.is_some() {
            match key.code {
                KeyCode::Up | KeyCode::Down if !ctrl && !alt => {
                    let delta = if key.code == KeyCode::Down { 1 } else { -1 };
                    self.step_complete(delta);
                    return Outcome::Taken;
                }
                // `enter` accepts, and only `enter`. `tab` is one of the two
                // ways out of the composer and the key table never lets it
                // this far, which is the rule that keeps a text field from
                // owning the key somebody uses to leave it.
                KeyCode::Enter if !shift && !alt => {
                    self.accept_complete();
                    return Outcome::Changed;
                }
                _ => {}
            }
        }

        // Sending, which depends on `[compose] send_key`.
        if key.code == KeyCode::Enter && !shift && !alt {
            let wants_ctrl = cfg.send_key == SendKey::CtrlEnter;
            if wants_ctrl == ctrl {
                return self.submit();
            }
            // The other spelling of the pair is always the newline, whichever
            // way round `[compose] send_key` has them.
            self.input.paste("\n");
            return Outcome::Changed;
        }

        // `up` on an empty field opens the last message for editing, which is
        // the one gesture every chat client shares.
        if key.code == KeyCode::Up
            && !ctrl
            && !alt
            && self.input.is_empty()
            && self.mode.is_normal()
        {
            return Outcome::EditLast;
        }

        if key.code == KeyCode::Esc {
            return if self.cancel() {
                Outcome::Taken
            } else {
                Outcome::Leave
            };
        }

        // `ctrl+u` clears rather than killing to the start of the line, which
        // is what somebody who wants the box empty means by it.
        if ctrl && key.code == KeyCode::Char('u') {
            self.input.clear();
            self.complete = None;
            return Outcome::Changed;
        }

        // The four the composer offers and the pickers milestone will fill in.
        // `ctrl+e` is the emoji picker rather than end-of-line: the key table
        // says so, the help prints it, and a panel that quietly meant
        // something else would make the table a lie.
        if ctrl {
            match key.code {
                KeyCode::Char('e') => return Outcome::Wants(Action::EmojiPicker),
                KeyCode::Char('g') => return Outcome::Wants(Action::GifPicker),
                KeyCode::Char('v') => return Outcome::Wants(Action::PasteImage),
                _ => {}
            }
        }
        if alt && key.code == KeyCode::Char('a') {
            return Outcome::Wants(Action::Attach);
        }

        match self.input.handle(key) {
            Edit::Consumed => {
                self.retrigger(sources);
                Outcome::Changed
            }
            Edit::Submit => self.submit(),
            Edit::Cancel => {
                if self.cancel() {
                    Outcome::Taken
                } else {
                    Outcome::Leave
                }
            }
            Edit::Ignored => Outcome::Ignored,
        }
    }

    pub fn paste(&mut self, text: &str, sources: &Sources) {
        self.input.paste(text);
        self.retrigger(sources);
    }

    fn submit(&mut self) -> Outcome {
        if let Mode::Edit { id, .. } = self.mode {
            return Outcome::SaveEdit(id);
        }
        if self.input.text().trim().is_empty() {
            return Outcome::Taken;
        }
        Outcome::Send
    }

    // -- autocomplete ------------------------------------------------------

    /// Work out whether the caret is inside something completable, and if so
    /// what the candidates are.
    ///
    /// Recomputed on every change rather than tracked incrementally: the state
    /// that would have to be kept in step is "where the trigger was and
    /// whether it is still there", and the text is at most two thousand
    /// characters.
    pub fn retrigger(&mut self, sources: &Sources) {
        self.complete = None;
        let text = self.input.text();
        let caret = self.input.cursor().min(text.len());
        let before = &text[..caret];

        let Some((start, kind)) = trigger_at(before) else {
            return;
        };
        let query = &before[start + 1..];
        // `:` needs two characters before it means anything: a colon is
        // punctuation far more often than it is the start of an emoji.
        if kind == Completing::Emoji && query.chars().count() < 2 {
            return;
        }
        let items = candidates(kind, query, sources);
        if items.is_empty() {
            return;
        }
        self.complete = Some(Autocomplete {
            kind,
            start,
            query: query.to_string(),
            items,
            cursor: 0,
        });
    }

    fn step_complete(&mut self, delta: isize) {
        let Some(complete) = &mut self.complete else {
            return;
        };
        let n = complete.items.len() as isize;
        if n == 0 {
            return;
        }
        complete.cursor = ((complete.cursor as isize + delta).rem_euclid(n)) as usize;
    }

    fn accept_complete(&mut self) {
        let Some(complete) = self.complete.take() else {
            return;
        };
        let Some(item) = complete.items.get(complete.cursor) else {
            return;
        };
        let text = self.input.text();
        let caret = self.input.cursor().min(text.len());
        let tail = text[caret..].to_string();
        let mut next = String::with_capacity(text.len() + item.insert.len());
        next.push_str(&text[..complete.start]);
        next.push_str(&item.insert);
        next.push_str(&tail);
        self.input.set_text(next);
        // Just past what was inserted, so typing carries on where the name
        // ended rather than at the end of a message somebody was in the
        // middle of.
        self.input.set_cursor(complete.start + item.insert.len());
    }

    // -- drawing -----------------------------------------------------------

    pub fn render(&mut self, body: Rect, buf: &mut Buffer, v: &View<'_>) -> Option<(u16, u16)> {
        if body.width == 0 || body.height == 0 {
            return None;
        }
        let t = v.theme;
        let mut y = body.y;
        let bottom = body.y + body.height;

        if let Some(banner) = self.banner() {
            buf.set_string(
                body.x,
                y,
                fit(&banner, body.width),
                Style::default()
                    .fg(rgb(t.accent))
                    .add_modifier(Modifier::BOLD),
            );
            y += 1;
        }

        let popup_rows = self
            .complete
            .as_ref()
            .map(|c| c.items.len().min(MAX_SUGGESTIONS) as u16)
            .unwrap_or(0);
        let text_height = bottom.saturating_sub(y).saturating_sub(popup_rows).max(1);
        let field = Rect {
            x: body.x,
            y,
            width: body.width,
            height: text_height,
        };

        let cursor = if self.input.is_empty() && !v.focused {
            let prompt = if v.channel.is_empty() {
                "open a channel to write in it".to_string()
            } else {
                format!("message {}", v.channel)
            };
            buf.set_string(
                field.x,
                field.y,
                fit(&prompt, field.width),
                Style::default().fg(rgb(t.empty_fg)),
            );
            None
        } else {
            self.input
                .render(field, buf, Style::default().fg(rgb(t.fg)))
        };

        if let Some(note) = &self.note {
            let y = bottom.saturating_sub(1);
            if y >= field.y + field.height {
                buf.set_string(
                    body.x,
                    y,
                    fit(note, body.width),
                    Style::default().fg(rgb(t.dim)),
                );
            }
        }

        self.render_popup(
            Rect {
                x: body.x,
                y: field.y + field.height,
                width: body.width,
                height: popup_rows.min(bottom.saturating_sub(field.y + field.height)),
            },
            buf,
            t,
        );

        if v.focused {
            cursor
        } else {
            None
        }
    }

    fn banner(&self) -> Option<String> {
        match &self.mode {
            Mode::Normal => None,
            Mode::Reply { author, ping, .. } => Some(format!(
                "\u{21a9} replying to @{author}{}  \u{b7} esc to cancel",
                if *ping { "" } else { " (no ping)" }
            )),
            Mode::Edit { .. } => {
                Some("\u{270e} editing  \u{b7} enter to save \u{b7} esc to cancel".into())
            }
        }
    }

    fn render_popup(&self, area: Rect, buf: &mut Buffer, t: &Theme) {
        let Some(complete) = &self.complete else {
            return;
        };
        if area.height == 0 || area.width == 0 {
            return;
        }
        for (n, item) in complete
            .items
            .iter()
            .take(usize::from(area.height))
            .enumerate()
        {
            let y = area.y + n as u16;
            let selected = n == complete.cursor;
            let style = if selected {
                Style::default()
                    .fg(rgb(t.row_cursor_fg))
                    .bg(rgb(t.row_cursor_bg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(rgb(t.row_fg)).bg(rgb(t.panel_bg))
            };
            let lead = complete.kind.trigger();
            buf.set_string(
                area.x,
                y,
                fit(&format!("{lead} {}", item.label), area.width),
                style,
            );
        }
    }
}

/// Everything the panel needs that is not its own state.
pub struct View<'a> {
    pub theme: &'a Theme,
    pub focused: bool,
    /// Where a message would go, for the prompt.
    pub channel: &'a str,
}

/// The trigger the caret is inside, if any.
///
/// Scans back from the caret to the first trigger character, giving up at a
/// space: `hello @al` is completing `al`, and `hello @alex how are` is not
/// completing anything, because the query ended at the space.
fn trigger_at(before: &str) -> Option<(usize, Completing)> {
    let mut start = None;
    for (i, c) in before.char_indices().rev() {
        match c {
            ':' => {
                start = Some((i, Completing::Emoji));
                break;
            }
            '@' => {
                start = Some((i, Completing::User));
                break;
            }
            '#' => {
                start = Some((i, Completing::Channel));
                break;
            }
            ' ' | '\t' | '\n' => return None,
            _ => {}
        }
    }
    let (at, kind) = start?;
    // A trigger has to start a word. `a@b` is an email address, not a mention.
    let ok = before[..at]
        .chars()
        .next_back()
        .map(|c| c.is_whitespace())
        .unwrap_or(true);
    ok.then_some((at, kind))
}

/// What the popup offers, for a kind and a query.
fn candidates(kind: Completing, query: &str, sources: &Sources) -> Vec<Candidate> {
    let needle = query.to_lowercase();
    let mut out: Vec<Candidate> = Vec::new();
    match kind {
        Completing::User => {
            for (name, id) in &sources.users {
                if name.to_lowercase().contains(&needle) {
                    out.push(Candidate {
                        label: name.clone(),
                        insert: format!("<@{id}> "),
                    });
                }
            }
        }
        Completing::Channel => {
            for (name, id) in &sources.channels {
                if name.to_lowercase().contains(&needle) {
                    out.push(Candidate {
                        label: format!("#{name}"),
                        insert: format!("<#{id}> "),
                    });
                }
            }
        }
        Completing::Emoji => {
            // The account's own emoji first: somebody typing `:pe` in a server
            // that has `:pepe:` means that one.
            for (name, id, animated) in &sources.emoji {
                if name.to_lowercase().contains(&needle) {
                    let a = if *animated { "a" } else { "" };
                    out.push(Candidate {
                        label: format!(":{name}:"),
                        insert: format!("<{a}:{name}:{id}> "),
                    });
                }
            }
            for emoji in emojis::iter() {
                let Some(shortcode) = emoji.shortcode() else {
                    continue;
                };
                if shortcode.contains(&needle) {
                    out.push(Candidate {
                        label: format!("{} :{shortcode}:", emoji.as_str()),
                        insert: format!("{} ", emoji.as_str()),
                    });
                }
                if out.len() >= MAX_SUGGESTIONS * 2 {
                    break;
                }
            }
        }
    }
    // Shorter names first: an exact match is never further down the list than
    // something that merely contains it.
    out.sort_by_key(|c| (c.label.len(), c.label.clone()));
    out.truncate(MAX_SUGGESTIONS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn cfg() -> Compose {
        Compose::default()
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn typed(c: &mut Composer, text: &str, sources: &Sources) {
        for ch in text.chars() {
            c.handle(key(ch), &cfg(), sources);
        }
    }

    fn sources() -> Sources {
        Sources {
            users: vec![("alex".into(), 2), ("alexandra".into(), 7)],
            channels: vec![("general".into(), 11), ("random".into(), 12)],
            emoji: vec![("pepe".into(), EmojiId(99), false)],
        }
    }

    /// A draft follows the channel it was written in, and comes back.
    #[test]
    fn drafts_survive_switching_channels() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, "half a thought", &Sources::default());
        c.open(ChannelId(2));
        assert_eq!(c.text(), "", "the other channel starts empty");
        typed(&mut c, "something else", &Sources::default());
        c.open(ChannelId(1));
        assert_eq!(c.text(), "half a thought");
        assert_eq!(c.unsent(), 2);
    }

    /// Sending empties both the field and the draft, so coming back to the
    /// channel does not offer the message again.
    #[test]
    fn sending_clears_the_draft() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, "hello", &Sources::default());
        assert_eq!(
            c.handle(code(KeyCode::Enter), &cfg(), &Sources::default()),
            Outcome::Send
        );
        assert_eq!(c.take(), "hello");
        c.open(ChannelId(2));
        c.open(ChannelId(1));
        assert_eq!(c.text(), "");
        assert_eq!(c.unsent(), 0);
    }

    /// An empty message is not a message.
    #[test]
    fn enter_on_nothing_sends_nothing() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        assert_eq!(
            c.handle(code(KeyCode::Enter), &cfg(), &Sources::default()),
            Outcome::Taken
        );
        typed(&mut c, "   ", &Sources::default());
        assert_eq!(
            c.handle(code(KeyCode::Enter), &cfg(), &Sources::default()),
            Outcome::Taken
        );
    }

    /// `shift+enter` and `alt+enter` are both a newline, whichever way round
    /// the send key is set.
    #[test]
    fn the_send_key_and_its_opposite() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, "a", &Sources::default());
        c.handle(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            &cfg(),
            &Sources::default(),
        );
        assert_eq!(c.text(), "a\n");
        c.handle(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            &cfg(),
            &Sources::default(),
        );
        assert_eq!(c.text(), "a\n\n");

        let ctrl_cfg = Compose {
            send_key: SendKey::CtrlEnter,
            ..Compose::default()
        };
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, "b", &Sources::default());
        assert_eq!(
            c.handle(code(KeyCode::Enter), &ctrl_cfg, &Sources::default()),
            Outcome::Changed,
            "plain enter is a newline when ctrl+enter sends"
        );
        assert_eq!(
            c.handle(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
                &ctrl_cfg,
                &Sources::default()
            ),
            Outcome::Send
        );
    }

    /// `up` on an empty field, and only on an empty field.
    #[test]
    fn up_edits_the_last_message_only_when_there_is_nothing_written() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        assert_eq!(
            c.handle(code(KeyCode::Up), &cfg(), &Sources::default()),
            Outcome::EditLast
        );
        typed(&mut c, "x", &Sources::default());
        assert_ne!(
            c.handle(code(KeyCode::Up), &cfg(), &Sources::default()),
            Outcome::EditLast
        );
    }

    /// The escape chain: popup, then mode, then focus.
    #[test]
    fn escape_undoes_one_thing_at_a_time() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        c.reply_to(MessageId(5), "alex".into(), true);
        typed(&mut c, "@ale", &sources());
        assert!(c.complete.is_some(), "the popup should be open");

        assert_eq!(
            c.handle(code(KeyCode::Esc), &cfg(), &sources()),
            Outcome::Taken
        );
        assert!(c.complete.is_none(), "first escape closed the popup");
        assert!(!c.mode.is_normal(), "and left the reply alone");

        assert_eq!(
            c.handle(code(KeyCode::Esc), &cfg(), &sources()),
            Outcome::Taken
        );
        assert!(c.mode.is_normal(), "second escape cancelled the reply");

        assert_eq!(
            c.handle(code(KeyCode::Esc), &cfg(), &sources()),
            Outcome::Leave,
            "third escape leaves the panel"
        );
    }

    /// Cancelling an edit puts back what was in the box before it started.
    #[test]
    fn cancelling_an_edit_restores_what_was_being_written() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, "a draft", &Sources::default());
        c.edit(MessageId(9), "the old message".into());
        assert_eq!(c.text(), "the old message");
        assert!(c.cancel());
        assert_eq!(c.text(), "a draft");
    }

    /// Editing sends an edit rather than a message.
    #[test]
    fn enter_while_editing_saves_the_edit() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        c.edit(MessageId(9), "was".into());
        assert_eq!(
            c.handle(code(KeyCode::Enter), &cfg(), &Sources::default()),
            Outcome::SaveEdit(MessageId(9))
        );
    }

    /// `@` completes people, `#` channels, `:` emoji, and each inserts the
    /// form Discord actually sends.
    #[test]
    fn the_three_autocompletes() {
        let s = sources();
        let mut c = Composer::new();
        c.open(ChannelId(1));

        typed(&mut c, "@ale", &s);
        assert_eq!(c.complete.as_ref().map(|a| a.kind), Some(Completing::User));
        c.handle(code(KeyCode::Enter), &cfg(), &s);
        assert_eq!(c.text(), "<@2> ");

        c.input.clear();
        typed(&mut c, "#gen", &s);
        assert_eq!(
            c.complete.as_ref().map(|a| a.kind),
            Some(Completing::Channel)
        );
        c.handle(code(KeyCode::Enter), &cfg(), &s);
        assert_eq!(c.text(), "<#11> ");

        c.input.clear();
        typed(&mut c, ":pep", &s);
        assert_eq!(c.complete.as_ref().map(|a| a.kind), Some(Completing::Emoji));
        c.handle(code(KeyCode::Enter), &cfg(), &s);
        assert_eq!(c.text(), "<:pepe:99> ");
    }

    /// A unicode shortcode becomes the character, not the code.
    #[test]
    fn a_unicode_shortcode_expands_to_the_character() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, ":thinking", &Sources::default());
        let complete = c.complete.as_ref().expect("something should match");
        assert!(
            complete
                .items
                .iter()
                .any(|i| i.insert.trim() == "\u{1f914}"),
            "{:?}",
            complete.items
        );
        c.handle(code(KeyCode::Enter), &cfg(), &Sources::default());
        assert!(
            c.text().starts_with('\u{1f914}'),
            "the character, not the code: {:?}",
            c.text()
        );
    }

    /// A colon in the middle of a sentence is punctuation.
    #[test]
    fn one_character_is_not_enough_to_open_the_emoji_popup() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, "so: ", &Sources::default());
        assert!(c.complete.is_none());
        typed(&mut c, "a@b", &sources());
        assert!(c.complete.is_none(), "an email address is not a mention");
    }

    /// A trigger whose word has ended is not a trigger any more.
    #[test]
    fn the_popup_closes_when_the_word_ends() {
        let s = sources();
        let mut c = Composer::new();
        c.open(ChannelId(1));
        typed(&mut c, "@ale", &s);
        assert!(c.complete.is_some());
        typed(&mut c, " and", &s);
        assert!(c.complete.is_none());
    }

    /// The height the dock is told about is the height that gets drawn.
    #[test]
    fn the_panel_grows_with_what_is_written() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        assert_eq!(c.rows(&cfg(), 40), MIN_ROWS);
        typed(&mut c, &"x".repeat(120), &Sources::default());
        let rows = c.rows(&cfg(), 40);
        assert!(rows > MIN_ROWS, "{rows}");
        assert!(rows <= cfg().max_rows, "{rows} is past [compose] max_rows");
    }

    #[test]
    fn the_banner_says_what_mode_it_is_in() {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 60, 4);
        let mut c = Composer::new();
        c.open(ChannelId(1));
        c.reply_to(MessageId(5), "alex".into(), false);
        let mut buf = Buffer::empty(area);
        c.render(
            area,
            &mut buf,
            &View {
                theme: &t,
                focused: true,
                channel: "#general",
            },
        );
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("replying to @alex"), "{text}");
        assert!(text.contains("no ping"), "{text}");
    }

    /// Two thousand characters is Discord's limit and the field's.
    #[test]
    fn the_field_refuses_more_than_discord_accepts() {
        let mut c = Composer::new();
        c.open(ChannelId(1));
        c.paste(&"y".repeat(MAX_CHARS + 100), &Sources::default());
        assert_eq!(c.text().chars().count(), MAX_CHARS);
    }

    /// Every key the table hands to the composer is one the composer uses.
    ///
    /// The two are written apart on purpose -- the key table compiles with no
    /// reference to the panels -- and the cost of that is that they could
    /// disagree. A key eaten here and handled by nobody is worse than an
    /// unbound key: it does nothing, it cannot be rebound to anything that
    /// would, and there is no way to tell from the outside which it is. The
    /// other direction is asserted in `app.rs`, where the dispatch order that
    /// makes it true actually lives.
    #[test]
    fn the_composer_takes_exactly_what_the_key_table_gives_it() {
        use crate::ui::keymap::composer_eats;

        let mut keys: Vec<KeyEvent> = Vec::new();
        for c in ('a'..='z')
            .chain('A'..='Z')
            .chain('0'..='9')
            .chain([' ', '.', ':', '@', '#'])
        {
            keys.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            keys.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
            keys.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
        }
        for code in [
            KeyCode::Enter,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Tab,
            KeyCode::Esc,
            KeyCode::PageUp,
            KeyCode::F(1),
        ] {
            for mods in [KeyModifiers::NONE, KeyModifiers::CONTROL, KeyModifiers::ALT] {
                keys.push(KeyEvent::new(code, mods));
            }
        }

        for key in keys {
            // The dispatcher only ever calls `handle` for the keys the table
            // hands over, so those are the ones this is about.
            if !composer_eats(key) {
                continue;
            }
            let mut c = Composer::new();
            c.open(ChannelId(1));
            c.input.set_text("some words");
            assert_ne!(
                c.handle(key, &cfg(), &Sources::default()),
                Outcome::Ignored,
                "{key:?} is given to the composer and the composer does nothing \
                 with it, so it is a key that is swallowed and lost"
            );
        }
    }
}
