//! "Are you sure?", and the one thing it is sure about.
//!
//! The box itself is STAR/KIT's `chrome::confirm` now -- raw keys, not the
//! key table, because while this is open the keyboard means exactly one
//! thing, and `y`/`n` rather than `Enter` for the same reason the shared
//! widget documents: a dialogue whose default key is the one a reader's
//! thumb is already resting on is a dialogue that answers a keystroke meant
//! for whatever came before it. `Esc` and `n` are the same answer and both
//! are offered, because half the people who want to say no will reach for
//! one and half for the other.
//!
//! This module keeps only what is STAR/CORD's business: the two questions
//! themselves and the [`Pending`] each answers into. [`layout`] and
//! [`render`] are thin covers over `chrome::confirm`'s own, so the box a
//! person sees and the box a click is tested against can never drift from
//! what the shared widget actually draws.

use starkit::chrome::confirm;
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;

use crate::discord::snowflake::{ChannelId, MessageId};
use crate::ui::panels::{fit, width_of};
use crate::ui::theme::Theme;

pub use starkit::chrome::confirm::{answer, hit, Answer};

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

/// A question, its consequence, and the verbs its answer is offered in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub body: String,
    /// What the consequence of `yes` actually is. A quit that said "this
    /// cannot be undone" over a draft that is about to be written to the
    /// session file would be a warning about something that is not true.
    pub consequence: &'static str,
    pub on_yes: Pending,
    pub yes: &'static str,
    pub no: &'static str,
}

impl Confirm {
    pub fn delete(channel: ChannelId, message: MessageId, preview: &str) -> Self {
        Self {
            title: "delete this message".into(),
            body: summary(preview),
            consequence: "this cannot be undone",
            on_yes: Pending::DeleteMessage { channel, message },
            yes: "delete",
            no: "keep",
        }
    }

    /// Quitting while a file is halfway up the wire.
    ///
    /// Its own question rather than the draft one, because the consequence is
    /// different: a draft is written to the session file and comes back, and
    /// an upload that is abandoned is a message nobody receives.
    pub fn quit_while_uploading(files: usize) -> Self {
        let what = if files == 1 {
            "a file is".to_string()
        } else {
            format!("{files} files are")
        };
        Self {
            title: "quit".into(),
            body: format!("{what} still being sent"),
            consequence: "what is written is kept for next time",
            on_yes: Pending::Quit,
            yes: "quit",
            no: "stay",
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
            consequence: "what is written is kept for next time",
            on_yes: Pending::Quit,
            yes: "quit",
            no: "stay",
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
        format!("{}\u{2026}", fit(first, 43).trim_end())
    }
}

/// This question, as the shared widget spells it: the consequence becomes a
/// second body line, with a blank one between it and the question so the two
/// still read as separate sentences the way they did on their own rows.
fn kit(c: &Confirm) -> confirm::Confirm {
    confirm::Confirm {
        title: c.title.clone(),
        body: vec![c.body.clone(), String::new(), c.consequence.to_string()],
        yes: c.yes,
        no: c.no,
    }
}

/// Where the box lands and where its two answers sit, so a click can be
/// tested against the same geometry [`render`] draws -- both read straight
/// through to `chrome::confirm`'s own, so the two can never disagree.
pub(super) fn layout(area: Rect, c: &Confirm) -> Option<confirm::Layout> {
    confirm::layout(area, &kit(c))
}

pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, c: &Confirm) {
    confirm::render(area, buf, theme, &kit(c));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;
    use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Both spellings of no, in both cases, plus escape. `Enter` is
    /// deliberately not among them -- see the shared widget's own doc.
    #[test]
    fn the_answers_are_the_ones_anybody_would_reach_for() {
        assert_eq!(answer(key('y')), Answer::Yes);
        assert_eq!(answer(key('Y')), Answer::Yes);
        assert_eq!(answer(key('n')), Answer::No);
        assert_eq!(answer(key('N')), Answer::No);
        assert_eq!(
            answer(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Answer::No
        );
        assert_eq!(
            answer(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Answer::Waiting,
            "Enter is deliberately not bound to yes"
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
        assert!(text.contains("DELETE THIS MESSAGE"), "{text}");
        assert!(text.contains("the thing I said"), "{text}");
        assert!(text.contains("y delete"), "{text}");
        assert!(text.contains("n keep"), "{text}");
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
