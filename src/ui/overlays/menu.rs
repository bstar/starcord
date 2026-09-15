//! What can be done to a message, listed.
//!
//! The right-click menu, and the only part of the interface that exists purely
//! for discovery: every row here is a key that already works. Somebody who has
//! read `?` never needs it; somebody who has not would otherwise have to guess
//! that `r` replies.
//!
//! It is STAR/KIT's settings list rather than a menu widget of its own, for
//! the same reason the panel settings are: the rows, the cursor, the scroll and
//! the hit test are already written, and a second list that looked almost the
//! same would be a second place for them to drift.
//!
//! Editing and deleting are only offered on this account's own messages —
//! not greyed out, absent. A row that cannot do anything is a row that has to
//! explain itself.

use starkit::chrome::settings::{self, SettingsView};
use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::widgets::Widget;

use crate::discord::snowflake::MessageId;
use crate::ui::theme::Theme;

/// One thing the menu can ask for. Each is a key in the table as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Reply,
    ReplyNoPing,
    React,
    Edit,
    Delete,
    Yank,
    Open,
    CopyLink,
}

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Choice::Reply => "reply  r",
            Choice::ReplyNoPing => "reply, no ping  R",
            Choice::React => "react  +",
            Choice::Edit => "edit  e",
            Choice::Delete => "delete  d",
            Choice::Yank => "copy the text  y",
            Choice::Open => "open elsewhere  o",
            Choice::CopyLink => "copy a link  ctrl+y",
        }
    }
}

/// The open menu.
#[derive(Debug, Clone)]
pub struct Menu {
    pub message: MessageId,
    pub cursor: usize,
    pub scroll: usize,
    choices: Vec<Choice>,
}

impl Menu {
    /// A menu for one message. `mine` decides whether editing and deleting are
    /// offered at all.
    pub fn new(message: MessageId, mine: bool) -> Self {
        let mut choices = vec![Choice::Reply, Choice::ReplyNoPing, Choice::React];
        if mine {
            choices.push(Choice::Edit);
            choices.push(Choice::Delete);
        }
        choices.extend([Choice::Yank, Choice::Open, Choice::CopyLink]);
        Self {
            message,
            cursor: 0,
            scroll: 0,
            choices,
        }
    }

    pub fn choices(&self) -> &[Choice] {
        &self.choices
    }

    pub fn selected(&self) -> Choice {
        self.choices[self.cursor.min(self.choices.len() - 1)]
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                _ => Action::Taken,
            };
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Action::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.step(-1);
                Action::Taken
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.step(1);
                Action::Taken
            }
            KeyCode::Enter | KeyCode::Char(' ') => Action::Chose(self.selected()),
            _ => Action::Taken,
        }
    }

    fn step(&mut self, delta: isize) {
        let n = self.choices.len() as isize;
        self.cursor = ((self.cursor as isize + delta).rem_euclid(n)) as usize;
        self.scroll = settings::clamp_scroll(self.cursor, self.scroll, self.choices.len());
    }

    pub fn scroll_by(&mut self, delta: i16) {
        self.step(delta.signum() as isize);
    }

    /// A click on a row does it; a click off the list closes, which is what a
    /// click outside a menu has always meant.
    pub fn click(&mut self, area: Rect, x: u16, y: u16) -> Action {
        match settings::hit(area, self.choices.len(), self.scroll, x, y) {
            Some(index) => {
                self.cursor = index;
                Action::Chose(self.selected())
            }
            None => Action::Close,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let rows: Vec<settings::Row> = self
            .choices
            .iter()
            .map(|c| settings::Row::action(c.label()))
            .collect();
        SettingsView {
            theme,
            heading: "message",
            title: "what can be done",
            rows: &rows,
            cursor: self.cursor,
            scroll: self.scroll,
            footer: "enter choose \u{b7} esc close",
        }
        .render(area, buf);
    }
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Taken,
    Close,
    Chose(Choice),
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Somebody else's message cannot be edited or deleted, so those rows are
    /// not there to be pressed.
    #[test]
    fn only_my_own_message_offers_editing_and_deleting() {
        let mine = Menu::new(MessageId(1), true);
        assert!(mine.choices().contains(&Choice::Edit));
        assert!(mine.choices().contains(&Choice::Delete));

        let theirs = Menu::new(MessageId(1), false);
        assert!(!theirs.choices().contains(&Choice::Edit));
        assert!(!theirs.choices().contains(&Choice::Delete));
        assert!(theirs.choices().contains(&Choice::Reply));
    }

    #[test]
    fn the_cursor_wraps_and_return_chooses() {
        let mut m = Menu::new(MessageId(1), true);
        assert_eq!(m.selected(), Choice::Reply);
        m.handle(key(KeyCode::Up));
        assert_eq!(m.selected(), Choice::CopyLink, "up from the top wraps");
        m.handle(key(KeyCode::Down));
        assert_eq!(m.handle(key(KeyCode::Enter)), Action::Chose(Choice::Reply));
        assert_eq!(m.handle(key(KeyCode::Esc)), Action::Close);
    }

    /// Every row names the key that does the same thing, so the menu teaches
    /// itself out of a job.
    #[test]
    fn every_row_names_its_key() {
        let m = Menu::new(MessageId(1), true);
        for choice in m.choices() {
            let label = choice.label();
            assert!(
                label.contains("  "),
                "{label:?} does not name the key beside it"
            );
        }
    }

    #[test]
    fn a_click_on_a_row_chooses_it() {
        let m = Menu::new(MessageId(1), true);
        let area = Rect::new(0, 0, 60, 20);
        let rows = m.choices().len();
        let mut found = None;
        for y in area.y..area.y + area.height {
            if settings::hit(area, rows, 0, area.x + area.width / 2, y) == Some(1) {
                found = Some(y);
                break;
            }
        }
        let y = found.expect("the widget draws the second row somewhere");
        let mut m = m;
        assert_eq!(
            m.click(area, area.x + area.width / 2, y),
            Action::Chose(Choice::ReplyNoPing)
        );
        assert_eq!(m.click(area, area.x, area.y), Action::Close);
    }

    #[test]
    fn it_draws_the_rows() {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        Menu::new(MessageId(1), true).render(area, &mut buf, &t);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("reply"), "{text}");
        assert!(text.contains("delete"), "{text}");
    }
}
