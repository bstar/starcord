//! Signing in.
//!
//! The whole frame while there is no session. Two ways in: scan a code with
//! the phone app, which is what the desktop client does and means the password
//! is never typed into a terminal at all, or paste a token, which is what
//! somebody who already has one wants.
//!
//! Nothing in this file ever prints what was typed. The token field draws
//! `••••` and the length, and the only place the characters go is into
//! `Command::LoginWithToken`.
//!
//! ## The code is drawn, not described
//!
//! The core does the remote-auth handshake and hands over the finished
//! matrix -- one `bool` per module -- because nothing under `discord/` draws.
//! What a dark module looks like is this file's decision, and there are two
//! answers. Where the terminal has a graphics protocol the matrix is
//! rasterised and placed as a picture, which is what a phone camera reads
//! most reliably. Where it has not, each cell carries two module rows as a
//! half block, which is the only way twenty-nine modules fit on a screen at
//! all.
//!
//! Both are drawn in black on white rather than in the theme's colours. A
//! camera is looking for contrast between the modules and the quiet zone
//! around them, and a scheme that is beautiful behind prose is one a scanner
//! gives up on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::graphics::{Graphics, ImageId};
use starkit::input::{Edit, TextInput};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};
use starkit::ratatui_image::Image;
use starkit::theme::color::Rgb;

use super::panels::rgb;
use super::theme::Theme;

/// The name, as it is written everywhere a person reads it.
pub const TITLE: &str = "STAR/CORD";

/// How wide the panel is drawn, when there is room.
const PANEL_COLS: u16 = 56;
const PANEL_ROWS: u16 = 16;

/// Modules of blank around the code.
///
/// The specification asks for four. Two is what fits in a terminal, and it is
/// enough for every scanner tried against it because the panel behind it is
/// white as well -- the quiet zone is really the whole light band between the
/// code and the border.
const QUIET: usize = 2;

/// A dark module, and the blank around it. Not the theme's: see the note at
/// the top of the file.
const DARK: Rgb = Rgb {
    r: 0x14,
    g: 0x14,
    b: 0x14,
};
const LIGHT: Rgb = Rgb {
    r: 0xff,
    g: 0xff,
    b: 0xff,
};

/// Two module rows in one cell.
const BOTH: &str = "\u{2588}";
const UPPER: &str = "\u{2580}";
const LOWER: &str = "\u{2584}";

/// A code, once the core has one.
#[derive(Debug, Clone)]
pub struct Code {
    /// What the code encodes: `https://discord.com/ra/<fingerprint>`.
    pub url: String,
    pub expires: Instant,
    /// One `bool` per module, `true` for dark.
    pub matrix: Vec<Vec<bool>>,
    /// Who scanned it, once somebody has. The phone still has to confirm.
    pub scanned: Option<String>,
}

impl Code {
    /// Modules per side, quiet zone included.
    fn side(&self) -> usize {
        self.matrix.len() + QUIET * 2
    }

    /// Whether a module is dark, in quiet-zone coordinates.
    fn dark(&self, x: usize, y: usize) -> bool {
        let (Some(x), Some(y)) = (x.checked_sub(QUIET), y.checked_sub(QUIET)) else {
            return false;
        };
        self.matrix
            .get(y)
            .and_then(|row| row.get(x))
            .copied()
            .unwrap_or(false)
    }

    fn left(&self) -> Duration {
        self.expires.saturating_duration_since(Instant::now())
    }
}

#[derive(Debug)]
pub enum Stage {
    /// Neither has been chosen yet.
    Choosing,
    /// Waiting for a code, and then to be scanned. `None` is the gap between
    /// asking the core for one and the handshake coming back with it, which is
    /// a second or two of real network and is worth saying out loud.
    Qr(Option<Box<Code>>),
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
    /// Ask the core for a remote-auth code. Also what `r` means: a new code
    /// is the same request, and the old handshake is dropped by the core.
    StartQr,
    /// Stop waiting to be scanned, and close the socket doing the waiting.
    CancelQr,
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

    /// The login failed, with whatever the core could say about it.
    ///
    /// Its own stage rather than a line under the menu: a close code from the
    /// remote-auth socket is the whole of what happened, and `r` from here is
    /// the one key somebody will reach for.
    pub fn failed(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.message = Some(reason.clone());
        self.stage = Stage::Error(reason);
    }

    /// Start waiting for a code. The request itself is the caller's.
    pub fn start_qr(&mut self) {
        self.message = None;
        self.stage = Stage::Qr(None);
    }

    /// The core has one.
    pub fn qr_ready(&mut self, url: String, expires_in: Duration, matrix: Vec<Vec<bool>>) {
        self.message = None;
        self.stage = Stage::Qr(Some(Box::new(Code {
            url,
            expires: Instant::now() + expires_in,
            matrix,
            scanned: None,
        })));
    }

    /// Somebody's phone read it. The confirmation is still on the phone.
    pub fn scanned(&mut self, username: String) {
        if let Stage::Qr(Some(code)) = &mut self.stage {
            code.scanned = Some(username);
        }
    }

    /// Whether the code on screen has run out, so a new one is worth asking
    /// for. Answered once and then acted on: the countdown is redrawn thirty
    /// times a second and the request is not.
    pub fn expired(&self) -> bool {
        match &self.stage {
            Stage::Qr(Some(code)) => code.scanned.is_none() && code.left().is_zero(),
            _ => false,
        }
    }

    pub fn is_qr(&self) -> bool {
        matches!(self.stage, Stage::Qr(_))
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
            Stage::Qr(_) => match key.code {
                KeyCode::Esc => {
                    self.stage = Stage::Choosing;
                    Outcome::CancelQr
                }
                // A code nobody scanned in time, or one that will not scan.
                KeyCode::Char('r') => {
                    self.stage = Stage::Qr(None);
                    Outcome::StartQr
                }
                KeyCode::Char('2') => {
                    self.stage = Stage::PasteToken(Box::new(TextInput::single()));
                    Outcome::CancelQr
                }
                _ => Outcome::Nothing,
            },
            Stage::Connecting => match key.code {
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
                    KeyCode::Char('1') | KeyCode::Char('r') => {
                        self.start_qr();
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
    ///
    /// The code stage asks for what the code needs; everything else is the
    /// same box it has always been. A panel that changed size between stages
    /// would be one that jumped under the reader, which is why the extra rows
    /// are only ever taken when there is a code to put in them.
    pub fn rect(area: Rect, stage: &Stage) -> Rect {
        let (want_w, want_h) = match stage {
            Stage::Qr(Some(code)) => {
                let side = code.side() as u16;
                // One module per column and two per row, plus the title, the
                // instructions, the countdown and the keys.
                (
                    PANEL_COLS.max(side + 8),
                    PANEL_ROWS.max(side.div_ceil(2) + 12),
                )
            }
            _ => (PANEL_COLS, PANEL_ROWS),
        };
        let w = want_w.min(area.width);
        let h = want_h.min(area.height);
        Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, graphics: &mut Graphics) {
        let t = theme;
        buf.set_style(area, Style::default().bg(rgb(t.bg)));
        let panel = Self::rect(area, &self.stage);
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
        let put = |buf: &mut Buffer, y: &mut u16, text: &str, style: Style| {
            if *y >= inner.y + inner.height {
                return;
            }
            let text: String = text.chars().take(usize::from(inner.width)).collect();
            buf.set_string(inner.x, *y, text, style);
            *y += 1;
        };

        put(
            buf,
            &mut y,
            TITLE,
            Style::default()
                .fg(rgb(t.accent))
                .add_modifier(Modifier::BOLD),
        );
        put(buf, &mut y, "", Style::default());

        let dim = Style::default().fg(rgb(t.dim));
        let body = Style::default().fg(rgb(t.fg));
        let key = Style::default()
            .fg(rgb(t.hint_key_fg))
            .add_modifier(Modifier::BOLD);

        match &self.stage {
            Stage::Choosing => {
                put(buf, &mut y, "sign in", body);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "1   scan a code with the phone app", key);
                put(buf, &mut y, "2   paste a token", key);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "q   quit", dim);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "scanning is the safer of the two: no", dim);
                put(buf, &mut y, "password is typed into this terminal.", dim);
            }
            Stage::Error(reason) => {
                put(buf, &mut y, "that did not work", body);
                put(buf, &mut y, "", dim);
                // Whatever the core could say, whole. A close code from the
                // remote-auth socket is the only evidence there is, and
                // shortening it to "login failed" throws it away.
                for line in starkit::wrap::wrap(reason, inner.width) {
                    put(buf, &mut y, line.drawn(reason), dim);
                }
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "r   try again", key);
                put(buf, &mut y, "2   paste a token", key);
                put(buf, &mut y, "q   quit", dim);
            }
            Stage::Qr(None) => {
                put(buf, &mut y, "scan this with the Discord app", body);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "asking Discord for a code\u{2026}", dim);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "esc  back", dim);
            }
            Stage::Qr(Some(code)) => {
                put(buf, &mut y, "scan this with the Discord app", body);
                put(
                    buf,
                    &mut y,
                    "open Discord on your phone \u{2192} Settings \u{2192} Scan QR Code",
                    dim,
                );
                put(buf, &mut y, "", dim);

                let side = code.side() as u16;
                let cells = Rect {
                    x: inner.x + (inner.width.saturating_sub(side)) / 2,
                    y,
                    width: side.min(inner.width),
                    height: side.div_ceil(2),
                };
                if cells.height + 4 <= inner.height.saturating_sub(y - inner.y) {
                    draw_code(code, cells, buf, graphics);
                    y += cells.height + 1;
                } else {
                    // Not enough rows for the code. The URL is what it encodes
                    // and can be typed or copied, which beats a picture with
                    // half of it missing.
                    put(buf, &mut y, "the terminal is too short for the code:", dim);
                    put(buf, &mut y, &code.url, body);
                    put(buf, &mut y, "", dim);
                }

                match &code.scanned {
                    Some(who) => {
                        put(buf, &mut y, &format!("scanned by {who}"), body);
                        put(buf, &mut y, "confirm it on the phone", dim);
                    }
                    None => put(
                        buf,
                        &mut y,
                        &format!("waiting for scan\u{2026} {}", clock(code.left())),
                        dim,
                    ),
                }
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "esc  back        r  a new code", dim);
            }
            Stage::PasteToken(input) => {
                put(buf, &mut y, "paste your token and press enter", body);
                put(buf, &mut y, "", dim);
                // Never the characters. A terminal's scrollback, a screen
                // share and a photograph of a screen are all places a token
                // must not be.
                let masked = "\u{2022}".repeat(input.text().chars().count().min(40));
                let shown = if masked.is_empty() {
                    "—".to_string()
                } else {
                    format!("{masked}  ({} characters)", input.text().chars().count())
                };
                put(buf, &mut y, &shown, body);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "esc  back", dim);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "it is stored in the system keyring,", dim);
                put(buf, &mut y, "or in a private file if there is", dim);
                put(buf, &mut y, "no keyring. Never in the config.", dim);
            }
            Stage::Connecting => {
                put(buf, &mut y, "connecting\u{2026}", body);
                put(buf, &mut y, "", dim);
                put(buf, &mut y, "esc  cancel", dim);
            }
        }

        // The error stage has already said it, in full and where it belongs.
        if let (Some(message), false) = (&self.message, matches!(self.stage, Stage::Error(_))) {
            let y = panel.y + panel.height.saturating_sub(2);
            let text: String = message.chars().take(usize::from(inner.width)).collect();
            buf.set_string(inner.x, y, text, Style::default().fg(rgb(t.error)));
        }
    }
}

/// Draw the code into `cells`, one way or the other.
///
/// A protocol image where the terminal has one, because a camera reads a
/// rasterised code far more reliably than one made of block characters, and
/// half blocks everywhere else. `cells` is one column per module and one row
/// per two, which at the usual cell shape is already square in pixels -- so
/// the same rectangle serves both and the panel does not change size with the
/// terminal.
fn draw_code(code: &Code, cells: Rect, buf: &mut Buffer, graphics: &mut Graphics) {
    let side = code.side();
    if side == 0 || cells.width == 0 || cells.height == 0 {
        return;
    }
    // The identity is the code itself. Nothing else about the picture can
    // change: it is black and white by construction and the modules are the
    // whole of it.
    let id = ImageId::of(&code.url);
    let modules: Vec<Vec<bool>> = (0..side)
        .map(|y| (0..side).map(|x| code.dark(x, y)).collect())
        .collect();
    if let Some(protocol) = graphics.raster(id, cells, |w, h| raster_code(&modules, w, h)) {
        Image::new(protocol).render(cells, buf);
        return;
    }
    halfblock_code(code, cells, buf);
}

/// The code as pixels: square modules, centred, on white.
fn raster_code(modules: &[Vec<bool>], w: u32, h: u32) -> image::RgbaImage {
    let side = modules.len().max(1) as u32;
    let mut img = image::RgbaImage::from_pixel(w.max(1), h.max(1), light_px());
    // Whole pixels per module, so no module is a row wider than its
    // neighbour: a scanner reads the grid, and an uneven one is a grid with
    // the wrong spacing.
    let scale = (w / side).min(h / side).max(1);
    let used = scale * side;
    let (ox, oy) = ((w.saturating_sub(used)) / 2, (h.saturating_sub(used)) / 2);
    for (my, row) in modules.iter().enumerate() {
        for (mx, dark) in row.iter().enumerate() {
            if !dark {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let (x, y) = (ox + mx as u32 * scale + dx, oy + my as u32 * scale + dy);
                    if x < w && y < h {
                        img.put_pixel(x, y, dark_px());
                    }
                }
            }
        }
    }
    img
}

fn dark_px() -> image::Rgba<u8> {
    image::Rgba([DARK.r, DARK.g, DARK.b, 0xff])
}

fn light_px() -> image::Rgba<u8> {
    image::Rgba([LIGHT.r, LIGHT.g, LIGHT.b, 0xff])
}

/// The code as characters: two module rows per cell.
fn halfblock_code(code: &Code, cells: Rect, buf: &mut Buffer) {
    let style = Style::default().fg(rgb(DARK)).bg(rgb(LIGHT));
    for cy in 0..cells.height {
        for cx in 0..cells.width {
            let upper = code.dark(usize::from(cx), usize::from(cy) * 2);
            let lower = code.dark(usize::from(cx), usize::from(cy) * 2 + 1);
            let glyph = match (upper, lower) {
                (true, true) => BOTH,
                (true, false) => UPPER,
                (false, true) => LOWER,
                (false, false) => " ",
            };
            buf[(cells.x + cx, cells.y + cy)]
                .set_symbol(glyph)
                .set_style(style);
        }
    }
}

/// `1:42`, for a countdown.
///
/// Rounded up rather than down. A code with a hundred and two seconds left on
/// it has had a few microseconds taken off by the time the frame is drawn, and
/// a countdown that opens on `1:41` reads as a second already lost; rounding
/// up also means the last second of the code is `0:01` rather than `0:00`
/// held for a whole second.
fn clock(left: Duration) -> String {
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Whether a stage is waiting on the core rather than on the keyboard.
pub fn waiting(stage: &Stage) -> bool {
    matches!(stage, Stage::Connecting | Stage::Qr(_))
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

    /// A terminal with no graphics protocol, which is what the half-block
    /// path is for and what a snapshot can see.
    fn blocks() -> Graphics {
        let mut g = Graphics::disabled();
        g.set_mode(starkit::graphics::Mode::Blocks);
        g
    }

    /// A fixed code, so a snapshot is a snapshot of the drawing and not of
    /// whatever a generator produced today. Three finder squares and a body
    /// that alternates, which is the shape a reader recognises.
    fn matrix() -> Vec<Vec<bool>> {
        const N: usize = 21;
        let mut m = vec![vec![false; N]; N];
        let mut finder = |ox: usize, oy: usize| {
            for y in 0..7 {
                for x in 0..7 {
                    let edge = x == 0 || y == 0 || x == 6 || y == 6;
                    let core = (2..=4).contains(&x) && (2..=4).contains(&y);
                    m[oy + y][ox + x] = edge || core;
                }
            }
        };
        finder(0, 0);
        finder(N - 7, 0);
        finder(0, N - 7);
        for (y, row) in m.iter_mut().enumerate().take(N - 8).skip(8) {
            for (x, cell) in row.iter_mut().enumerate() {
                *cell = (x * 3 + y * 5) % 4 < 2;
            }
        }
        m
    }

    fn code_screen() -> LoginScreen {
        let mut s = LoginScreen::new();
        s.qr_ready(
            "https://discord.com/ra/0123456789abcdef".into(),
            Duration::from_secs(102),
            matrix(),
        );
        s
    }

    fn screen_text(s: &LoginScreen, w: u16, h: u16) -> String {
        let t = theme("terminal");
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        s.render(area, &mut buf, &t, &mut blocks());
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
    fn a_failure_is_its_own_stage_and_r_tries_again() {
        let mut s = LoginScreen::new();
        s.handle(key('2'));
        s.connecting();
        s.failed("the handshake failed (close 4002)");
        assert!(matches!(s.stage, Stage::Error(_)));
        let drawn = screen_text(&s, 80, 24);
        assert!(drawn.contains("close 4002"), "{drawn}");
        assert!(drawn.contains("try again"), "{drawn}");

        assert_eq!(s.handle(key('r')), Outcome::StartQr);
        assert!(matches!(s.stage, Stage::Qr(None)));
    }

    /// The code stage, drawn with half blocks. Two module rows to a cell, a
    /// quiet zone around it, and black on white whatever the theme is.
    #[test]
    fn the_code_is_drawn_as_half_blocks() {
        let s = code_screen();
        insta::assert_snapshot!("qr-halfblocks-100x30", screen_text(&s, 100, 30));

        let drawn = screen_text(&s, 100, 30);
        assert!(drawn.contains("STAR/CORD"));
        assert!(drawn.contains("Scan QR Code"), "{drawn}");
        assert!(drawn.contains("waiting for scan\u{2026} 1:42"), "{drawn}");
        assert!(
            drawn.contains(BOTH) || drawn.contains(UPPER) || drawn.contains(LOWER),
            "no modules were drawn"
        );
    }

    /// A terminal too short for the code says so and gives the URL, which can
    /// be typed into a phone. Half a code would scan as nothing.
    #[test]
    fn a_short_terminal_gets_the_url_instead() {
        let s = code_screen();
        insta::assert_snapshot!("qr-halfblocks-60x12", screen_text(&s, 60, 12));
        let drawn = screen_text(&s, 60, 12);
        assert!(drawn.contains("too short"), "{drawn}");
        assert!(drawn.contains("discord.com/ra/"), "{drawn}");
    }

    /// Once somebody's phone has read it, the screen stops counting down and
    /// says whose phone and what to do on it.
    #[test]
    fn a_scanned_code_names_who_scanned_it() {
        let mut s = code_screen();
        s.scanned("sam".into());
        insta::assert_snapshot!("qr-scanned-100x30", screen_text(&s, 100, 30));
        let drawn = screen_text(&s, 100, 30);
        assert!(drawn.contains("scanned by sam"), "{drawn}");
        assert!(drawn.contains("confirm it on the phone"), "{drawn}");
        assert!(!drawn.contains("waiting for scan"), "{drawn}");
        assert!(!s.expired(), "a scanned code is not replaced under them");
    }

    #[test]
    fn the_error_stage_says_what_the_core_said() {
        let mut s = LoginScreen::new();
        s.failed("the code was not confirmed in time (close 4003)");
        insta::assert_snapshot!("qr-error-100x30", screen_text(&s, 100, 30));
    }

    /// A code nobody scanned runs out, and the screen says so by asking for
    /// another one. Only once: `start_qr` is what clears it.
    #[test]
    fn a_code_that_ran_out_is_replaced() {
        let mut s = LoginScreen::new();
        s.qr_ready("https://discord.com/ra/x".into(), Duration::ZERO, matrix());
        assert!(s.expired());
        s.start_qr();
        assert!(!s.expired());
        assert!(matches!(s.stage, Stage::Qr(None)));
    }

    /// `esc` out of the code stage closes the socket that was waiting on it.
    /// Leaving one open is a handshake Discord is holding for nobody.
    #[test]
    fn leaving_the_code_stage_cancels_the_handshake() {
        let mut s = code_screen();
        assert_eq!(
            s.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Outcome::CancelQr
        );
        assert!(matches!(s.stage, Stage::Choosing));

        // And so does going to the token field from it.
        let mut s = code_screen();
        assert_eq!(s.handle(key('2')), Outcome::CancelQr);
        assert!(matches!(s.stage, Stage::PasteToken(_)));
    }

    /// The quiet zone is real: the modules the core sent are inside it, and
    /// the two rings around them are blank whatever the code says.
    #[test]
    fn the_code_carries_a_quiet_zone() {
        let code = Code {
            url: "x".into(),
            expires: Instant::now(),
            matrix: vec![vec![true; 3]; 3],
            scanned: None,
        };
        assert_eq!(code.side(), 3 + QUIET * 2);
        for i in 0..code.side() {
            assert!(!code.dark(i, 0), "the outer ring is not blank");
            assert!(!code.dark(0, i), "the outer ring is not blank");
            assert!(!code.dark(i, code.side() - 1));
        }
        assert!(code.dark(QUIET, QUIET), "the code itself is missing");
        assert!(code.dark(QUIET + 2, QUIET + 2));
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
