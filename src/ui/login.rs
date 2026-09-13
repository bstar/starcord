//! Signing in.
//!
//! The whole frame while there is no session. Two ways in: scan a code with
//! the phone app, which is what the desktop client does and means the password
//! is never typed into a terminal at all, or paste a token, which is what
//! somebody who already has one wants.
//!
//! The code stage is the fifth milestone and draws a line saying so. The paste
//! stage is here because it is the one that can be tested without an account.
//!
//! Nothing in this file ever prints what was typed. The token field draws
//! `••••` and the length, and the only place the characters go is into
//! `Command::LoginWithToken`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::input::{Edit, TextInput};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use super::panels::rgb;
use super::theme::Theme;

/// The name, as it is written everywhere a person reads it.
pub const TITLE: &str = "STAR/CORD";

/// How wide the panel is drawn, when there is room.
const PANEL_COLS: u16 = 56;
const PANEL_ROWS: u16 = 16;

#[derive(Debug)]
pub enum Stage {
    /// Neither has been chosen yet.
    Choosing,
    /// Waiting to be scanned. The fifth milestone fills this in; the matrix is
    /// carried here rather than in the app because it is only ever drawn.
    Qr {
        url: String,
        expires: Instant,
        matrix: Vec<Vec<bool>>,
    },
    PasteToken(Box<TextInput>),
    Connecting,
    Error(String),
}

/// What the login screen decided a key meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing; the screen has not finished with it.
    Nothing,
    /// It was typing, or a movement inside the field.
    Consumed,
    /// Ask the core for a remote-auth code.
    StartQr,
    /// Log in with what was typed.
    Submit(String),
    /// Quit the program.
    Quit,
}

pub struct LoginScreen {
    pub stage: Stage,
    /// What the core last said, kept under the panel so that a failure is
    /// visible while the next attempt is typed.
    pub message: Option<String>,
}

impl Default for LoginScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginScreen {
    pub fn new() -> Self {
        Self {
            stage: Stage::Choosing,
            message: None,
        }
    }

    pub fn failed(&mut self, reason: impl Into<String>) {
        self.message = Some(reason.into());
        self.stage = Stage::Choosing;
    }

    pub fn connecting(&mut self) {
        self.stage = Stage::Connecting;
    }

    /// The login screen eats raw keys, ahead of every table.
    ///
    /// It has to: the token field is a text field, and `q` in a token is a
    /// letter. The three ways out — `esc`, `enter`, and a finished login — are
    /// all keys nothing could mean to type.
    pub fn handle(&mut self, key: KeyEvent) -> Outcome {
        match &mut self.stage {
            Stage::PasteToken(input) => match input.handle(key) {
                Edit::Submit => {
                    let text = input.take();
                    if text.trim().is_empty() {
                        self.message = Some("that is empty".into());
                        return Outcome::Consumed;
                    }
                    self.stage = Stage::Connecting;
                    Outcome::Submit(text)
                }
                Edit::Cancel => {
                    self.stage = Stage::Choosing;
                    Outcome::Consumed
                }
                Edit::Consumed => Outcome::Consumed,
                // An unhandled key while a field has focus is still the
                // field's: falling through to the global table here is how a
                // token containing `q` would quit the program.
                Edit::Ignored => Outcome::Consumed,
            },
            Stage::Qr { .. } | Stage::Connecting => match key.code {
                KeyCode::Esc => {
                    self.stage = Stage::Choosing;
                    Outcome::Consumed
                }
                _ => Outcome::Nothing,
            },
            Stage::Choosing | Stage::Error(_) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    return Outcome::Quit;
                }
                match key.code {
                    KeyCode::Char('1') => {
                        self.message = None;
                        Outcome::StartQr
                    }
                    KeyCode::Char('2') | KeyCode::Enter => {
                        self.message = None;
                        self.stage = Stage::PasteToken(Box::new(TextInput::single()));
                        Outcome::Consumed
                    }
                    KeyCode::Char('q') | KeyCode::Esc => Outcome::Quit,
                    _ => Outcome::Nothing,
                }
            }
        }
    }

    /// A bracketed paste. The one place a token is expected to arrive from.
    pub fn paste(&mut self, text: &str) -> bool {
        match &mut self.stage {
            Stage::PasteToken(input) => {
                // One line: a token pasted out of a browser's developer tools
                // often comes with quotes and a newline around it, and a
                // multi-line field would keep them.
                input.paste(text.trim().trim_matches('"'));
                true
            }
            _ => false,
        }
    }

    /// Where the panel sits, so the renderer and a click agree.
    pub fn rect(area: Rect) -> Rect {
        let w = PANEL_COLS.min(area.width);
        let h = PANEL_ROWS.min(area.height);
        Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let t = theme;
        buf.set_style(area, Style::default().bg(rgb(t.bg)));
        let panel = Self::rect(area);
        if panel.width < 24 || panel.height < 6 {
            // Nothing useful fits; say the one thing that is true.
            buf.set_string(
                area.x,
                area.y,
                "terminal too small",
                Style::default().fg(rgb(t.error)),
            );
            return;
        }

        Clear.render(panel, buf);
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(rgb(t.border_focused)))
            .style(Style::default().bg(rgb(t.panel_bg)))
            .render(panel, buf);
        starkit::chrome::frame::render_corners(panel, buf, t, true);

        let inner = Rect {
            x: panel.x + 2,
            y: panel.y + 1,
            width: panel.width.saturating_sub(4),
            height: panel.height.saturating_sub(2),
        };
        let mut y = inner.y;
        let mut put = |text: &str, style: Style, y: &mut u16| {
            if *y >= inner.y + inner.height {
                return;
            }
            let text: String = text.chars().take(usize::from(inner.width)).collect();
            buf.set_string(inner.x, *y, text, style);
            *y += 1;
        };

        put(
            TITLE,
            Style::default()
                .fg(rgb(t.accent))
                .add_modifier(Modifier::BOLD),
            &mut y,
        );
        put("", Style::default(), &mut y);

        let dim = Style::default().fg(rgb(t.dim));
        let body = Style::default().fg(rgb(t.fg));
        let key = Style::default()
            .fg(rgb(t.hint_key_fg))
            .add_modifier(Modifier::BOLD);

        match &self.stage {
            Stage::Choosing | Stage::Error(_) => {
                put("sign in", body, &mut y);
                put("", dim, &mut y);
                put("1   scan a code with the phone app", key, &mut y);
                put("2   paste a token", key, &mut y);
                put("", dim, &mut y);
                put("q   quit", dim, &mut y);
                put("", dim, &mut y);
                put("scanning is the safer of the two: no", dim, &mut y);
                put("password is typed into this terminal.", dim, &mut y);
            }
            Stage::Qr { url, expires, .. } => {
                put("scan this with the Discord app", body, &mut y);
                put("", dim, &mut y);
                // The code itself arrives with the milestone that asks for it;
                // the URL is what it encodes and is worth showing anyway.
                put("[ the code arrives with a later", dim, &mut y);
                put("  milestone ]", dim, &mut y);
                put("", dim, &mut y);
                put(url, body, &mut y);
                let left = expires.saturating_duration_since(Instant::now());
                put(&format!("waiting for scan… {}", clock(left)), dim, &mut y);
                put("", dim, &mut y);
                put("esc  back", dim, &mut y);
            }
            Stage::PasteToken(input) => {
                put("paste your token and press enter", body, &mut y);
                put("", dim, &mut y);
                // Never the characters. A terminal's scrollback, a screen
                // share and a photograph of a screen are all places a token
                // must not be.
                let masked = "\u{2022}".repeat(input.text().chars().count().min(40));
                let shown = if masked.is_empty() {
                    "—".to_string()
                } else {
                    format!("{masked}  ({} characters)", input.text().chars().count())
                };
                put(&shown, body, &mut y);
                put("", dim, &mut y);
                put("esc  back", dim, &mut y);
                put("", dim, &mut y);
                put("it is stored in the system keyring,", dim, &mut y);
                put("or in a private file if there is", dim, &mut y);
                put("no keyring. Never in the config.", dim, &mut y);
            }
            Stage::Connecting => {
                put("connecting…", body, &mut y);
                put("", dim, &mut y);
                put("esc  cancel", dim, &mut y);
            }
        }

        if let Some(message) = &self.message {
            let y = panel.y + panel.height.saturating_sub(2);
            let text: String = message.chars().take(usize::from(inner.width)).collect();
            buf.set_string(inner.x, y, text, Style::default().fg(rgb(t.error)));
        }
    }
}

/// `1:42`, for a countdown.
fn clock(left: Duration) -> String {
    let secs = left.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Whether a stage is waiting on the core rather than on the keyboard.
pub fn waiting(stage: &Stage) -> bool {
    matches!(stage, Stage::Connecting | Stage::Qr { .. })
}

/// The token a screen is holding, for the moment it is handed to the core.
/// Takes it, so this function is the only place it is copied out.
pub fn take_token(screen: &mut LoginScreen) -> Option<Arc<str>> {
    match &mut screen.stage {
        Stage::PasteToken(input) => {
            let text = input.take();
            (!text.trim().is_empty()).then(|| Arc::from(text.trim()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn screen_text(s: &LoginScreen, w: u16, h: u16) -> String {
        let t = theme("terminal");
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        s.render(area, &mut buf, &t);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_title_is_the_name_as_it_is_written() {
        assert_eq!(TITLE, "STAR/CORD");
        let s = LoginScreen::new();
        assert!(screen_text(&s, 80, 24).contains("STAR/CORD"));
    }

    #[test]
    fn two_opens_the_token_field_and_esc_comes_back() {
        let mut s = LoginScreen::new();
        assert_eq!(s.handle(key('2')), Outcome::Consumed);
        assert!(matches!(s.stage, Stage::PasteToken(_)));
        assert_eq!(
            s.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Outcome::Consumed
        );
        assert!(matches!(s.stage, Stage::Choosing));
    }

    /// The one rule this file exists to keep: nothing typed into the token
    /// field is ever drawn.
    #[test]
    fn the_token_is_never_on_the_screen() {
        let mut s = LoginScreen::new();
        s.handle(key('2'));
        assert!(s.paste("mfa.aVeryRealLookingSecretValue"));
        let drawn = screen_text(&s, 80, 24);
        assert!(
            !drawn.contains("aVeryRealLooking"),
            "the token was drawn:\n{drawn}"
        );
        assert!(drawn.contains('\u{2022}'), "and nothing stood in for it");
        assert!(drawn.contains("31 characters"));
    }

    /// A token pasted from a browser's developer tools arrives wrapped in
    /// quotes and a newline; neither is part of it.
    #[test]
    fn a_pasted_token_is_trimmed_of_what_a_browser_puts_round_it() {
        let mut s = LoginScreen::new();
        s.handle(key('2'));
        s.paste("  \"a.token.here\"\n");
        match s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Outcome::Submit(text) => assert_eq!(text, "a.token.here"),
            other => panic!("{other:?}"),
        }
    }

    /// While the field has focus every letter is a letter. `q` in a token must
    /// not quit, and this is the screen that would have let it.
    #[test]
    fn a_letter_in_the_token_field_is_never_a_command() {
        let mut s = LoginScreen::new();
        s.handle(key('2'));
        for c in ['q', '1', '2', 'j', 't'] {
            assert_eq!(s.handle(key(c)), Outcome::Consumed, "{c:?} escaped");
        }
        match s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Outcome::Submit(text) => assert_eq!(text, "q12jt"),
            other => panic!("{other:?}"),
        }
    }

    /// And on the choosing screen, where there is no field, `q` does quit.
    #[test]
    fn q_quits_from_the_choice() {
        let mut s = LoginScreen::new();
        assert_eq!(s.handle(key('q')), Outcome::Quit);
        assert_eq!(
            LoginScreen::new().handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Outcome::Quit
        );
    }

    #[test]
    fn an_empty_field_is_not_submitted() {
        let mut s = LoginScreen::new();
        s.handle(key('2'));
        assert_eq!(
            s.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Consumed
        );
        assert!(matches!(s.stage, Stage::PasteToken(_)));
        assert_eq!(s.message.as_deref(), Some("that is empty"));
    }

    #[test]
    fn a_failure_is_shown_and_the_choice_comes_back() {
        let mut s = LoginScreen::new();
        s.handle(key('2'));
        s.connecting();
        s.failed("the token was rejected");
        assert!(matches!(s.stage, Stage::Choosing));
        assert!(screen_text(&s, 80, 24).contains("rejected"));
    }

    /// A terminal too small for the panel says so rather than drawing a box
    /// with nothing in it.
    #[test]
    fn a_tiny_terminal_says_so() {
        let s = LoginScreen::new();
        assert!(screen_text(&s, 20, 4).contains("too small"));
    }

    #[test]
    fn the_countdown_reads_as_a_clock() {
        assert_eq!(clock(Duration::from_secs(102)), "1:42");
        assert_eq!(clock(Duration::from_secs(0)), "0:00");
        assert_eq!(clock(Duration::from_secs(9)), "0:09");
    }
}
