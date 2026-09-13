//! The settings a panel owns, listed and changed in place.
//!
//! Five rows, every one of them something the key table can also do. That is
//! deliberate: the overlay is for finding a setting, the key is for using it
//! once you know it exists, and a setting that only one of the two can reach
//! is a setting that is either undiscoverable or tedious.
//!
//! Changing a row does two things — it changes the running program, and it
//! writes the key through [`starkit::config::edit`], which rewrites one line
//! of `config.toml` and leaves every comment in the file alone. A settings
//! overlay that serialised the whole struct back would silently delete the
//! commentary the template was written to carry.

use starkit::chrome::settings::{self, SettingsView};
use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::widgets::Widget;

use crate::config::Config;
use crate::ui::panels::PanelId;
use crate::ui::theme::Theme;

/// One thing the overlay can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Theme,
    Timestamps,
    Avatars,
    Animate,
    SendKey,
}

impl Setting {
    /// The table. Order is the order the rows are drawn in, and the labels are
    /// what the file calls them so that somebody who reads one can find the
    /// other.
    pub const ALL: &'static [Setting] = &[
        Setting::Theme,
        Setting::Timestamps,
        Setting::Avatars,
        Setting::Animate,
        Setting::SendKey,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Setting::Theme => "theme",
            Setting::Timestamps => "timestamps",
            Setting::Avatars => "avatars",
            Setting::Animate => "animate pictures",
            Setting::SendKey => "send with",
        }
    }

    /// Where it lives in `config.toml`.
    pub fn where_written(self) -> (&'static str, &'static str) {
        match self {
            Setting::Theme => ("ui", "theme"),
            Setting::Timestamps => ("chat", "timestamps"),
            Setting::Avatars => ("chat", "show_avatars"),
            Setting::Animate => ("media", "animate"),
            Setting::SendKey => ("compose", "send_key"),
        }
    }

    /// What the row shows right now.
    pub fn value(self, cfg: &Config) -> String {
        match self {
            Setting::Theme => cfg.ui.theme.clone(),
            Setting::Timestamps => cfg.chat.timestamps.name().into(),
            Setting::Avatars => on_off(cfg.chat.show_avatars).into(),
            Setting::Animate => cfg.media.animate.name().into(),
            Setting::SendKey => match cfg.compose.send_key {
                crate::config::SendKey::Enter => "enter".into(),
                crate::config::SendKey::CtrlEnter => "ctrl+enter".into(),
            },
        }
    }
}

fn on_off(yes: bool) -> &'static str {
    if yes {
        "on"
    } else {
        "off"
    }
}

/// The open overlay.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Whose settings these are, for the heading.
    pub panel: PanelId,
    pub cursor: usize,
    pub scroll: usize,
}

impl Settings {
    pub fn new(panel: PanelId) -> Self {
        Self {
            panel,
            cursor: 0,
            scroll: 0,
        }
    }

    pub fn selected(&self) -> Setting {
        Setting::ALL[self.cursor.min(Setting::ALL.len() - 1)]
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
            // Right and left both change it; there is no row with more than a
            // handful of values and cycling one way is enough to reach them
            // all, so left is the same key backwards rather than a no-op.
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                Action::Change(self.selected(), true)
            }
            KeyCode::Left | KeyCode::Char('h') => Action::Change(self.selected(), false),
            _ => Action::Taken,
        }
    }

    fn step(&mut self, delta: isize) {
        let n = Setting::ALL.len() as isize;
        self.cursor = ((self.cursor as isize + delta).rem_euclid(n)) as usize;
        self.scroll = settings::clamp_scroll(self.cursor, self.scroll, Setting::ALL.len());
    }

    pub fn scroll_by(&mut self, delta: i16) {
        self.step(delta.signum() as isize);
    }

    /// What a click landed on.
    ///
    /// Through STAR/KIT's own hit test, which measures the list the same way
    /// the widget draws it: a row that scrolled out of sight is not a row
    /// anything can click, and the two answers cannot drift because there is
    /// only one of them. A click off the list closes the overlay, which is
    /// what clicking outside a dialogue has always meant here.
    pub fn click(&mut self, area: Rect, x: u16, y: u16) -> Action {
        match settings::hit(area, Setting::ALL.len(), self.scroll, x, y) {
            Some(index) => {
                self.cursor = index;
                Action::Change(self.selected(), true)
            }
            None => Action::Close,
        }
    }

    pub fn rows(&self, cfg: &Config) -> Vec<settings::Row> {
        Setting::ALL
            .iter()
            .map(|s| settings::Row::setting(s.label(), s.value(cfg)))
            .collect()
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, cfg: &Config) {
        let rows = self.rows(cfg);
        SettingsView {
            theme,
            heading: "settings",
            title: self.panel.title(),
            rows: &rows,
            cursor: self.cursor,
            scroll: self.scroll,
        }
        .render(area, buf);
    }
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Taken,
    Close,
    /// Change this setting; `true` steps forward.
    Change(Setting, bool),
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Every setting names a real place in the file. A row that wrote to a key
    /// nothing reads would change the program until it was restarted and then
    /// quietly stop.
    #[test]
    fn every_row_writes_somewhere_the_config_reads() {
        let cfg = Config::default();
        let toml = toml::to_string(&cfg).expect("the config serialises");
        for s in Setting::ALL {
            let (section, key) = s.where_written();
            assert!(
                toml.contains(&format!("[{section}]")),
                "{section} is not a table in the config"
            );
            assert!(
                toml.contains(&format!("{key} =")),
                "{key} is not a key in the config"
            );
            assert!(!s.value(&cfg).is_empty(), "{s:?} has no value to show");
        }
    }

    #[test]
    fn the_cursor_wraps_and_enter_changes_the_row_it_is_on() {
        let mut s = Settings::new(PanelId::Chat);
        assert_eq!(s.selected(), Setting::Theme);
        s.handle(key(KeyCode::Up));
        assert_eq!(s.selected(), Setting::SendKey, "up from the top wraps");
        s.handle(key(KeyCode::Down));
        assert_eq!(
            s.handle(key(KeyCode::Enter)),
            Action::Change(Setting::Theme, true)
        );
        assert_eq!(
            s.handle(key(KeyCode::Left)),
            Action::Change(Setting::Theme, false)
        );
    }

    #[test]
    fn escape_closes_it() {
        let mut s = Settings::new(PanelId::Chat);
        assert_eq!(s.handle(key(KeyCode::Esc)), Action::Close);
    }

    #[test]
    fn it_draws_the_rows_and_their_values() {
        let t = theme("terminal");
        let cfg = Config::default();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        Settings::new(PanelId::Chat).render(area, &mut buf, &t, &cfg);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("timestamps"), "{text}");
        assert!(text.contains("short"), "{text}");
    }
}
