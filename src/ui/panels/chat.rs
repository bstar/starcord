//! The message list.
//!
//! **Not yet.** Reading a conversation is the second UI milestone: grouping,
//! wrapping, dividers, the wrap cache and virtual scrolling all land together,
//! because each of them is only testable against the others. What is here is
//! the frame and one line saying so, which is what makes the dock a real dock
//! at this milestone rather than a five-panel one with a hole in it.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;

use super::empty;
use crate::ui::theme::Theme;

pub struct View<'a> {
    pub theme: &'a Theme,
    /// What the header says: `#general · Some Guild`, or nothing when no
    /// channel is open.
    pub title: &'a str,
    pub focused: bool,
}

pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) {
    let text = if v.title.is_empty() {
        "choose a channel"
    } else {
        "messages arrive with the next milestone"
    };
    empty(body, buf, v.theme, text);
}
