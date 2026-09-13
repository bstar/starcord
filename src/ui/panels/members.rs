//! Who is in this server.
//!
//! **Not yet.** A member list is a lazy subscription over a range the server
//! keeps in sync, which is core work rather than drawing work, and it lands
//! with the milestone that adds the subscription. The frame is here so the
//! layout is the width it will be.

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;

use super::empty;
use crate::ui::theme::Theme;

pub struct View<'a> {
    pub theme: &'a Theme,
    pub focused: bool,
}

pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) {
    empty(body, buf, v.theme, "members soon");
}
