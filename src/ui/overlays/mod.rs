//! Modal things drawn over everything else.
//!
//! One rule holds them together and is the reason they are gathered in a
//! module rather than scattered: **[`Overlays::open`] is the first check in
//! both `handle` and `handle_mouse`.** An overlay that is drawn over a panel
//! and does not take that panel's keys is a dialogue you can type through,
//! which is the bug this arrangement makes impossible to write.
//!
//! The help overlay is the only one this milestone has. The confirm, media,
//! picker, search, quick-switch and settings overlays land with the milestones
//! that give them something to do, and each is a field on this struct and an
//! arm in the two functions below.

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::keymap::HelpView;
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::widgets::Widget;

use crate::ui::keymap::{BINDINGS, MOUSE};
use crate::ui::theme::Theme;

/// What an overlay did with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Nothing was open; the key belongs to whatever is underneath.
    Ignored,
    /// The overlay had it, whether or not it did anything with it.
    Taken,
    /// Quitting works from here as it does from everywhere.
    Quit,
}

/// Everything modal, and whether any of it is up.
#[derive(Debug, Default)]
pub struct Overlays {
    pub help: bool,
    pub help_scroll: u16,
}

impl Overlays {
    /// Whether anything modal is on screen.
    ///
    /// Checked first in both `handle` and `handle_mouse`, so that a key or a
    /// click reaches the overlay rather than the panel under it.
    pub fn open(&self) -> bool {
        self.help
    }

    pub fn close(&mut self) {
        self.help = false;
        self.help_scroll = 0;
    }

    pub fn toggle_help(&mut self) {
        if self.help {
            self.close();
        } else {
            self.help = true;
            self.help_scroll = 0;
        }
    }

    /// Keys, while something is open.
    ///
    /// [`Key::Ignored`] only ever means "nothing is open". Once one is, every
    /// key is taken: a modal overlay that let a key through to the panel it is
    /// drawn over is a dialogue you can type through.
    pub fn handle(&mut self, key: KeyEvent) -> Key {
        if !self.help {
            return Key::Ignored;
        }
        // Raw keys rather than the table: `j` scrolls the list here and should
        // not also move a cursor in a panel nobody can see.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return Key::Quit,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) | KeyCode::Char('q') => self.close(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.help_scroll = self.help_scroll.saturating_add(1)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.help_scroll = self.help_scroll.saturating_sub(1)
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.help_scroll = self.help_scroll.saturating_add(10)
            }
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
            KeyCode::Home => self.help_scroll = 0,
            _ => {}
        }
        Key::Taken
    }

    /// The wheel, while something is open.
    pub fn scroll(&mut self, delta: i16) {
        if !self.help {
            return;
        }
        self.help_scroll = self.help_scroll.saturating_add_signed(delta);
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if !self.help {
            return;
        }
        HelpView {
            theme,
            bindings: BINDINGS,
            mouse: MOUSE,
            scroll: self.help_scroll,
            title: "keys",
        }
        .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn nothing_is_open_to_begin_with() {
        let mut o = Overlays::default();
        assert!(!o.open());
        assert_eq!(o.handle(key('j')), Key::Ignored);
    }

    #[test]
    fn the_help_opens_and_closes_on_the_same_key() {
        let mut o = Overlays::default();
        o.toggle_help();
        assert!(o.open());
        assert_eq!(o.handle(key('?')), Key::Taken);
        assert!(!o.open());
    }

    /// Escape closes; it does not quit. The rule the key table holds, held
    /// again here because an overlay is the place it is most tempting to
    /// break.
    #[test]
    fn escape_closes_the_overlay_rather_than_the_program() {
        let mut o = Overlays::default();
        o.toggle_help();
        assert_eq!(
            o.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Key::Taken
        );
        assert!(!o.open());
    }

    /// A modal overlay is modal: while it is up, no key reaches the panel
    /// underneath. This is the test that fails if somebody adds a
    /// fall-through.
    #[test]
    fn no_key_falls_through_an_open_overlay() {
        let mut o = Overlays::default();
        for c in ['t', 'r', 'd', 'x', 'i', 'g'] {
            o.help = true;
            o.help_scroll = 0;
            assert_eq!(
                o.handle(key(c)),
                Key::Taken,
                "{c:?} fell through to the panel below"
            );
            assert!(o.open(), "{c:?} closed it");
        }
    }

    /// Except quitting, which works from everywhere including here.
    #[test]
    fn ctrl_c_still_quits() {
        let mut o = Overlays::default();
        o.toggle_help();
        assert_eq!(
            o.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Key::Quit
        );
        assert!(o.open(), "and it did not close on the way out");
    }

    #[test]
    fn scrolling_never_goes_above_the_top() {
        let mut o = Overlays::default();
        o.toggle_help();
        o.handle(key('k'));
        assert_eq!(o.help_scroll, 0);
        o.handle(key('j'));
        assert_eq!(o.help_scroll, 1);
        o.scroll(-5);
        assert_eq!(o.help_scroll, 0);
    }

    #[test]
    fn a_closed_overlay_draws_nothing() {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let before = buf.clone();
        Overlays::default().render(area, &mut buf, &t);
        assert_eq!(buf, before);
    }

    fn drawn(o: &Overlays) -> String {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        o.render(area, &mut buf, &t);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_open_help_draws_the_key_table() {
        let mut o = Overlays::default();
        o.toggle_help();
        let text = drawn(&o);
        assert!(text.contains("navigation"), "{text}");
        assert!(text.contains("next panel"), "{text}");
        // The list is longer than any terminal is tall, which is what the
        // scroll is for. The end of it is reachable rather than silently cut.
        assert!(
            !text.contains("quit"),
            "the whole table fitted; nothing to scroll"
        );
    }

    #[test]
    fn scrolling_reaches_the_end_of_the_table() {
        let mut o = Overlays::default();
        o.toggle_help();
        o.scroll(60);
        let text = drawn(&o);
        assert!(text.contains("quit"), "{text}");
    }
}
