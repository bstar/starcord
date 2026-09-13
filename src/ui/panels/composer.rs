//! Writing a message.
//!
//! **Not yet.** Composing is the third UI milestone — drafts, replies, edits,
//! attachments and the `@#:` autocomplete — and the key scheme it needs is
//! already in `keymap.rs`, tested, waiting for it. This draws the frame and a
//! prompt, so the layout below the chat is the real height it will be.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::Style;

use super::rgb;
use crate::ui::theme::Theme;

/// Rows the composer asks the dock for.
///
/// Four is the floor, not three: two borders, the header row the action words
/// sit on, and one line of text. At three there is no line of text at all, and
/// the panel is a box with `attach emoji gif` in it and nowhere to type --
/// which is what it looked like the first time it was drawn. It grows with the
/// draft in the milestone that gives it one.
pub const ROWS: u16 = 4;

pub struct View<'a> {
    pub theme: &'a Theme,
    pub focused: bool,
    /// Where a message would go, for the prompt.
    pub channel: &'a str,
}

pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) {
    if body.height == 0 || body.width == 0 {
        return;
    }
    // While it has focus it takes raw keys, because that is what it will do
    // when it can send: a letter typed into a text field is a letter. Nothing
    // comes of them yet, so the way out is on the line -- a panel that
    // swallows the keyboard and says nothing about it looks like a freeze.
    let prompt = if v.focused {
        "sending arrives with the next milestone — tab or esc to leave".to_string()
    } else if v.channel.is_empty() {
        "open a channel to write in it".to_string()
    } else {
        format!("message {}", v.channel)
    };
    buf.set_string(
        body.x,
        body.y,
        super::fit(&prompt, body.width),
        Style::default().fg(rgb(v.theme.empty_fg)),
    );
}
