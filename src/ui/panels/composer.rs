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
/// One line of text plus its border, which is the floor. It grows with the
/// draft in the milestone that gives it one.
pub const ROWS: u16 = 3;

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
    let prompt = if v.channel.is_empty() {
        "open a channel to write in it".to_string()
    } else {
        format!("message {} — sending arrives with the next milestone", v.channel)
    };
    let text: String = prompt.chars().take(usize::from(body.width)).collect();
    buf.set_string(
        body.x,
        body.y,
        text,
        Style::default().fg(rgb(v.theme.empty_fg)),
    );
}
