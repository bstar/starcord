//! The one row at the bottom.
//!
//! Three fields with different jobs. The left is a fixed reminder that the
//! help exists. The middle is transient — a note for three seconds, then back
//! to where you are. The right is **stable**: the connection, the unread
//! count, the mode. Stability there is the whole design: it is the part
//! somebody glances at without stopping what they are doing, and a field that
//! moves is a field that has to be read rather than glanced at.
//!
//! Its geometry is computed once, by [`fields`], and both the renderer and the
//! mouse come through it.

use std::time::{Duration, Instant};

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::panels::rgb;
use super::theme::Theme;
use crate::discord::handle::{Connection, NoteLevel};

/// How long a note holds the middle field before the location comes back.
pub const NOTE_FOR: Duration = Duration::from_secs(3);

/// What a click on the status line lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Help,
    /// The channel name: opens the quick switcher, because that is what
    /// somebody pointing at where they are wants to change.
    Location,
    /// The connection word: reconnects now rather than waiting out the
    /// backoff.
    Connection,
}

pub struct View<'a> {
    pub theme: &'a Theme,
    pub connection: &'a Connection,
    /// `#general · Some Guild`, or empty before a channel is open.
    pub location: &'a str,
    /// A transient message and when it arrived.
    pub note: Option<&'a (String, NoteLevel, Instant)>,
    pub unread: u32,
    pub mentions: u32,
    /// What the terminal can draw pictures with, for the right-hand end.
    pub graphics: &'a str,
    pub now: Instant,
}

impl View<'_> {
    /// The middle field: the note while it is fresh, else where you are.
    fn middle(&self) -> (String, Option<NoteLevel>) {
        if let Some((text, level, at)) = self.note {
            if self.now.duration_since(*at) < NOTE_FOR {
                return (text.clone(), Some(*level));
            }
        }
        (self.location.to_string(), None)
    }

    /// The right field. Every part of it is a fixed shape, so nothing to its
    /// left moves when a number changes.
    fn right(&self) -> String {
        let mut parts = Vec::new();
        if self.mentions > 0 {
            parts.push(format!("{} unread · @{}", self.unread, self.mentions));
        } else if self.unread > 0 {
            parts.push(format!("{} unread", self.unread));
        }
        parts.push(connection_word(self.connection));
        if !self.graphics.is_empty() {
            parts.push(self.graphics.to_string());
        }
        parts.join("  ")
    }
}

/// The connection, as one glyph and one word.
///
/// The glyph carries it for anybody who is not reading: a full triangle is
/// connected, a hollow one is trying, a cross has given up.
pub fn connection_word(c: &Connection) -> String {
    match c {
        // A resume and a fresh identify both end up online, and the
        // difference between them is not the reader's business: it is worth a
        // note when it happens and nothing at all afterwards.
        Connection::Ready { .. } => "\u{25b2} online".into(),
        Connection::Connecting => "\u{25bd} connecting".into(),
        Connection::Identifying => "\u{25bd} identifying".into(),
        Connection::Resuming => "\u{25bd} resuming".into(),
        Connection::Reconnecting { next_in, .. } => {
            format!("\u{25bd} reconnecting {}s", next_in.as_secs().max(1))
        }
        Connection::AuthFailed(_) => "\u{2715} rejected".into(),
        Connection::LoggedOut => "\u{2715} logged out".into(),
        Connection::Offline => "\u{2715} offline".into(),
    }
}

const HELP: &str = "? help";

/// Where each field sits. The renderer draws from this and the mouse tests
/// against it, so a word that was not drawn cannot be clicked.
pub fn fields(area: Rect, v: &View<'_>) -> Vec<(Hit, Rect)> {
    if area.height == 0 || area.width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let help_w = HELP.chars().count() as u16;
    if area.width < help_w {
        return out;
    }
    out.push((
        Hit::Help,
        Rect {
            x: area.x,
            y: area.y,
            width: help_w,
            height: 1,
        },
    ));

    let right = v.right();
    let right_w = (right.chars().count() as u16).min(area.width.saturating_sub(help_w + 2));
    if right_w > 0 {
        // Only the connection half of the right field is clickable: the unread
        // count is a fact rather than a button.
        let word = connection_word(v.connection);
        let word_w = (word.chars().count() as u16).min(right_w);
        out.push((
            Hit::Connection,
            Rect {
                x: area.x + area.width - right_w + (right_w - word_w).min(right_w),
                y: area.y,
                width: word_w,
                height: 1,
            },
        ));
    }

    let middle_x = area.x + help_w + 2;
    let middle_w = area
        .width
        .saturating_sub(help_w + 2)
        .saturating_sub(right_w + 2);
    if middle_w > 0 {
        out.push((
            Hit::Location,
            Rect {
                x: middle_x,
                y: area.y,
                width: middle_w,
                height: 1,
            },
        ));
    }
    out
}

pub fn hit(area: Rect, v: &View<'_>, x: u16, y: u16) -> Option<Hit> {
    fields(area, v)
        .into_iter()
        .find(|(_, r)| y == r.y && x >= r.x && x < r.x + r.width)
        .map(|(h, _)| h)
}

pub fn render(area: Rect, buf: &mut Buffer, v: &View<'_>) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let t = v.theme;
    let base = Style::default().fg(rgb(t.status_fg)).bg(rgb(t.status_bg));
    buf.set_style(area, base);

    let (middle, level) = v.middle();
    let right = v.right();

    for (what, rect) in fields(area, v) {
        match what {
            Hit::Help => {
                buf.set_string(
                    rect.x,
                    rect.y,
                    "?",
                    Style::default()
                        .fg(rgb(t.hint_key_fg))
                        .bg(rgb(t.hint_key_bg))
                        .add_modifier(Modifier::BOLD),
                );
                buf.set_string(rect.x + 1, rect.y, " help", base.fg(rgb(t.hint_desc_fg)));
            }
            Hit::Location => {
                let style = match level {
                    Some(NoteLevel::Error) => base.fg(rgb(t.error)),
                    Some(NoteLevel::Warning) => base.fg(rgb(t.warn)),
                    Some(NoteLevel::Info) => base.fg(rgb(t.accent)),
                    None => base,
                };
                let text: String = middle.chars().take(usize::from(rect.width)).collect();
                buf.set_string(rect.x, rect.y, text, style);
            }
            Hit::Connection => {
                // The whole right-hand field is drawn from its own left edge,
                // which is where the unread count lives; the clickable part is
                // only the connection word inside it.
                let x = area.x + area.width - right.chars().count() as u16;
                let style = match v.connection {
                    Connection::Ready { .. } => base.fg(rgb(t.ok)),
                    Connection::AuthFailed(_) | Connection::Offline => base.fg(rgb(t.error)),
                    Connection::LoggedOut => base,
                    _ => base.fg(rgb(t.warn)),
                };
                buf.set_string(x.max(area.x), rect.y, &right, style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn view<'a>(
        t: &'a Theme,
        c: &'a Connection,
        note: Option<&'a (String, NoteLevel, Instant)>,
        now: Instant,
    ) -> View<'a> {
        View {
            theme: t,
            connection: c,
            location: "#general · Some Guild",
            note,
            unread: 3,
            mentions: 1,
            graphics: "kitty",
            now,
        }
    }

    fn line(area: Rect, buf: &Buffer) -> String {
        (0..area.width)
            .map(|x| buf[(area.x + x, area.y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn a_note_holds_the_middle_and_then_gives_it_back() {
        let t = theme("terminal");
        let c = Connection::Offline;
        let at = Instant::now();
        let note = ("sent".to_string(), NoteLevel::Info, at);

        let fresh = view(&t, &c, Some(&note), at + Duration::from_secs(1));
        assert_eq!(fresh.middle().0, "sent");
        let stale = view(
            &t,
            &c,
            Some(&note),
            at + NOTE_FOR + Duration::from_millis(1),
        );
        assert_eq!(stale.middle().0, "#general · Some Guild");
    }

    /// The right-hand field is the one somebody glances at, so it says the
    /// same things in the same order however the numbers move.
    #[test]
    fn the_right_hand_field_keeps_its_shape() {
        let t = theme("terminal");
        let c = Connection::Ready {
            since: Instant::now(),
            resumed: false,
        };
        let mut v = view(&t, &c, None, Instant::now());
        assert_eq!(v.right(), "3 unread · @1  \u{25b2} online  kitty");
        v.mentions = 0;
        assert_eq!(v.right(), "3 unread  \u{25b2} online  kitty");
        v.unread = 0;
        assert_eq!(v.right(), "\u{25b2} online  kitty");
    }

    /// Every connection has a word, and the three states are told apart by a
    /// glyph as well as by a colour.
    #[test]
    fn every_connection_says_something_without_colour() {
        let states = [
            Connection::LoggedOut,
            Connection::Connecting,
            Connection::Identifying,
            Connection::Ready {
                since: Instant::now(),
                resumed: true,
            },
            Connection::Resuming,
            Connection::Reconnecting {
                attempt: 2,
                next_in: Duration::from_secs(4),
                reason: "closed".into(),
            },
            Connection::AuthFailed("no".into()),
            Connection::Offline,
        ];
        for s in &states {
            let word = connection_word(s);
            assert!(!word.is_empty(), "{s:?}");
            assert!(
                word.starts_with(['\u{25b2}', '\u{25bd}', '\u{2715}']),
                "{word:?} has no glyph"
            );
        }
        assert!(connection_word(&states[5]).contains("4s"));
    }

    /// The three fields never overlap, at any width the line is drawn at.
    #[test]
    fn the_fields_never_overlap() {
        let t = theme("terminal");
        let c = Connection::Offline;
        for width in 1u16..200 {
            let area = Rect::new(0, 9, width, 1);
            let v = view(&t, &c, None, Instant::now());
            let mut cells = vec![0u8; usize::from(width)];
            for (what, r) in fields(area, &v) {
                assert!(
                    r.x >= area.x && r.x + r.width <= area.x + area.width,
                    "{what:?} runs off the line at {width}"
                );
                for x in r.x..r.x + r.width {
                    assert_eq!(cells[usize::from(x)], 0, "{what:?} overlaps at {width}");
                    cells[usize::from(x)] = 1;
                }
            }
        }
    }

    /// The mouse and the renderer share one answer.
    #[test]
    fn a_click_lands_on_what_was_drawn() {
        let t = theme("terminal");
        let c = Connection::Offline;
        let area = Rect::new(0, 0, 100, 1);
        let v = view(&t, &c, None, Instant::now());
        assert_eq!(hit(area, &v, 0, 0), Some(Hit::Help));
        assert_eq!(hit(area, &v, 10, 0), Some(Hit::Location));
        assert_eq!(hit(area, &v, 99, 0), Some(Hit::Connection));
        assert_eq!(hit(area, &v, 0, 1), None, "another row is not the status");
    }

    #[test]
    fn a_narrow_line_still_offers_the_help() {
        let t = theme("terminal");
        let c = Connection::Offline;
        let area = Rect::new(0, 0, 8, 1);
        let v = view(&t, &c, None, Instant::now());
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &v);
        assert!(line(area, &buf).starts_with("? help"));
    }
}
