//! "Are you sure?", and the one thing it is sure about.
//!
//! Raw keys, not the key table. `y` and `n` are letters everywhere else in the
//! program and here they are the whole dialogue, which is exactly the case the
//! table's layering exists to keep apart: a modal asks a question, and while
//! it is up the answer is the only thing the keyboard means.
//!
//! `Esc` and `n` are the same answer and both are offered, because half the
//! people who want to say no will reach for one and half for the other. There
//! is no default on `Enter`: it confirms, and the title says what it confirms,
//! because a dialogue whose default is "yes" is a dialogue that deletes
//! somebody's message when they pressed return on the message below.

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::text::{Line, Span};
use starkit::ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::discord::snowflake::{ChannelId, MessageId};
use crate::ui::panels::{fit, rgb, width_of};
use crate::ui::theme::Theme;

/// What will happen if the answer is yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    DeleteMessage {
        channel: ChannelId,
        message: MessageId,
    },
    /// Quitting with something half-written in the composer.
    Quit,
}

/// A question and its consequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub body: String,
    pub on_yes: Pending,
}

impl Confirm {
    pub fn delete(channel: ChannelId, message: MessageId, preview: &str) -> Self {
        Self {
            title: "delete this message".into(),
            body: summary(preview),
            on_yes: Pending::DeleteMessage { channel, message },
        }
    }

    pub fn quit_with_draft(channels: usize) -> Self {
        let what = if channels == 1 {
            "one unsent message".to_string()
        } else {
            format!("{channels} unsent messages")
        };
        Self {
            title: "quit".into(),
            body: format!("there is {what} waiting"),
            on_yes: Pending::Quit,
        }
    }
}

fn summary(text: &str) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if first.trim().is_empty() {
        return "(no text)".into();
    }
    let first = first.trim();
    if width_of(first) <= 44 {
        first.to_string()
    } else {
        format!("{}\u{2026}", crate::ui::panels::fit(first, 43).trim_end())
    }
}

/// What a key did to the dialogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// Anything else. A modal takes every key; it just does not act on most.
    Waiting,
    /// Quitting works from here as it does from everywhere.
    Quit,
}

/// Read one key as an answer.
pub fn answer(key: KeyEvent) -> Answer {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Answer::Quit,
            _ => Answer::Waiting,
        };
    }
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Answer::Yes,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Answer::No,
        _ => Answer::Waiting,
    }
}

/// Where the box lands, so a click can be tested against it.
pub fn rect(area: Rect) -> Rect {
    let w = area.width.saturating_sub(4).clamp(24, 52);
    let h = 6.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, confirm: &Confirm) {
    let r = rect(area);
    if r.width < 8 || r.height < 4 {
        return;
    }
    Clear.render(r, buf);

    let t = theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(rgb(t.border_focused)))
        .title(Span::styled(
            format!("{}{} ", starkit::chrome::frame::TITLE_LEAD, confirm.title),
            Style::default()
                .fg(rgb(t.header_fg))
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " y yes \u{b7} n no ",
                Style::default().fg(rgb(t.dim)),
            ))
            .right_aligned(),
        )
        .style(Style::default().bg(rgb(t.panel_bg)));
    let inner = block.inner(r);
    block.render(r, buf);
    starkit::chrome::frame::render_corners(r, buf, t, true);

    if inner.height == 0 || inner.width == 0 {
        return;
    }
    buf.set_string(
        inner.x,
        inner.y,
        fit(&confirm.body, inner.width),
        Style::default().fg(rgb(t.fg)),
    );
    if inner.height > 2 {
        // What the consequence actually is. A quit that says "this cannot be
        // undone" over a draft that is about to be written to the session file
        // is a warning about something that is not true.
        let consequence = match confirm.on_yes {
            Pending::DeleteMessage { .. } => "this cannot be undone",
            Pending::Quit => "what is written is kept for next time",
        };
        buf.set_string(
            inner.x,
            inner.y + 2,
            fit(consequence, inner.width),
            Style::default().fg(rgb(t.dim)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Both spellings of each answer, in both cases.
    #[test]
    fn the_answers_are_the_ones_anybody_would_reach_for() {
        assert_eq!(answer(key('y')), Answer::Yes);
        assert_eq!(answer(key('Y')), Answer::Yes);
        assert_eq!(
            answer(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Answer::Yes
        );
        assert_eq!(answer(key('n')), Answer::No);
        assert_eq!(answer(key('N')), Answer::No);
        assert_eq!(
            answer(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Answer::No
        );
    }

    /// Every other key is taken and does nothing, which is what modal means.
    #[test]
    fn nothing_else_falls_through() {
        for c in ['t', 'q', 'j', 'd', 'r', '1'] {
            assert_eq!(answer(key(c)), Answer::Waiting, "{c:?}");
        }
        assert_eq!(
            answer(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Answer::Quit,
            "except quitting"
        );
    }

    #[test]
    fn the_box_says_what_is_being_deleted() {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        let confirm = Confirm::delete(ChannelId(1), MessageId(2), "the thing I said");
        render(area, &mut buf, &t, &confirm);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("delete this message"), "{text}");
        assert!(text.contains("the thing I said"), "{text}");
    }

    /// A long message is cut rather than wrapped out of the box.
    #[test]
    fn a_long_message_is_summarised() {
        let long = "x".repeat(200);
        let confirm = Confirm::delete(ChannelId(1), MessageId(2), &long);
        assert!(width_of(&confirm.body) <= 45, "{}", confirm.body);
        assert!(confirm.body.ends_with('\u{2026}'));
    }

    #[test]
    fn quitting_with_drafts_counts_them() {
        assert!(Confirm::quit_with_draft(1).body.contains("one unsent"));
        assert!(Confirm::quit_with_draft(3).body.contains("3 unsent"));
    }
}
