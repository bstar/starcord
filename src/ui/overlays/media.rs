//! Looking at one picture properly.
//!
//! `Enter` on a message with something viewable on it opens this over the
//! whole window. What it shows is one list — every picture in the channel, in
//! the order they were posted — rather than only the ones on the message that
//! opened it, so `h` and `l` walk a conversation's photographs the way anybody
//! would expect them to and do not stop at a message boundary.
//!
//! ## Two sizes and no more
//!
//! `Fit` scales the picture into the box; `Actual` draws it at one image pixel
//! per terminal pixel, which on a cell of sixteen by thirty-two is a picture
//! about a sixth the size a browser would show it at. There is no zoom
//! percentage and no panning: this is a terminal, the two useful answers are
//! "all of it" and "how big is it really", and everything between them is a
//! control nobody would find.
//!
//! ## Saving asks for the bytes again
//!
//! What the media store holds has been resized to fit a rectangle. Writing
//! that out would hand somebody a thumbnail of the file they asked to keep, so
//! `s` asks the core for [`Want::Bytes`](crate::discord::media::Want::Bytes)
//! and writes what comes back — which is the file, byte for byte, signature
//! and all.

use std::path::PathBuf;

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::text::{Line, Span};
use starkit::ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::discord::media::MediaKey;
use crate::discord::snowflake::MessageId;
use crate::ui::panels::chat::media::{MediaStore, Placement, Shape};
use crate::ui::panels::{fit, rgb};
use crate::ui::theme::Theme;

/// One thing the viewer can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub message: MessageId,
    pub key: MediaKey,
    /// Where it came from, for `o` and `y`.
    pub url: String,
    pub filename: String,
}

/// How large the picture is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Zoom {
    #[default]
    Fit,
    Actual,
}

impl Zoom {
    fn next(self) -> Self {
        match self {
            Zoom::Fit => Zoom::Actual,
            Zoom::Actual => Zoom::Fit,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Zoom::Fit => "fitted",
            Zoom::Actual => "actual size",
        }
    }
}

/// What a key asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Taken,
    Close,
    /// Hand this URL to whatever the system opens pictures with.
    Open(String),
    /// Put this on the clipboard.
    Copy(String),
    /// Write what is on screen to `[media] save_dir`.
    Save,
    Quit,
}

#[derive(Debug, Clone)]
pub struct Viewer {
    pub items: Vec<Item>,
    pub index: usize,
    pub zoom: Zoom,
    /// Cell aspect, for drawing a picture at its own shape.
    aspect: f32,
}

impl Viewer {
    /// A viewer over `items`, opened on the first one belonging to `message`.
    pub fn new(items: Vec<Item>, message: Option<MessageId>, aspect: f32) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        let index = message
            .and_then(|id| items.iter().position(|i| i.message == id))
            .unwrap_or(0);
        Some(Self {
            items,
            index,
            zoom: Zoom::Fit,
            aspect: if aspect > 0.0 { aspect } else { 2.0 },
        })
    }

    pub fn current(&self) -> &Item {
        &self.items[self.index.min(self.items.len() - 1)]
    }

    /// `h` and `l`, and the wheel. Wraps, because a viewer that stops at the
    /// end of a channel's pictures leaves somebody pressing a key that does
    /// nothing.
    pub fn step(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        if n == 0 {
            return;
        }
        self.index = ((self.index as isize + delta).rem_euclid(n)) as usize;
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                _ => Action::Taken,
            };
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => Action::Close,
            KeyCode::Char('h') | KeyCode::Left => {
                self.step(-1);
                Action::Taken
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.step(1);
                Action::Taken
            }
            KeyCode::Char('z') => {
                self.zoom = self.zoom.next();
                Action::Taken
            }
            KeyCode::Char('o') => Action::Open(self.current().url.clone()),
            KeyCode::Char('y') => Action::Copy(self.current().url.clone()),
            KeyCode::Char('s') => Action::Save,
            _ => Action::Taken,
        }
    }

    /// A click: the left half goes back, the right half goes on.
    pub fn click(&mut self, area: Rect, x: u16, _y: u16) -> Action {
        if x < area.x + area.width / 2 {
            self.step(-1);
        } else {
            self.step(1);
        }
        Action::Taken
    }

    /// Where the picture goes, given how big it turned out to be.
    fn picture_rect(&self, inner: Rect, size: Option<(u32, u32)>) -> Rect {
        let box_ = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: inner.height.saturating_sub(1),
        };
        let Some((w, h)) = size.filter(|(w, h)| *w > 0 && *h > 0) else {
            return box_;
        };
        let (cols, rows) = match self.zoom {
            // One image pixel per terminal pixel. The cell size is not known
            // here, so it is taken from the aspect and a sixteen-pixel column,
            // which is what the media store assumes everywhere else.
            Zoom::Actual => (
                (w / 16).max(1) as u16,
                ((h as f32 / (16.0 * self.aspect)).round() as u16).max(1),
            ),
            Zoom::Fit => {
                let by_width = (
                    box_.width,
                    ((f32::from(box_.width) * (h as f32 / w as f32) / self.aspect).round() as u16)
                        .max(1),
                );
                if by_width.1 <= box_.height {
                    by_width
                } else {
                    (
                        ((f32::from(box_.height) * self.aspect * (w as f32 / h as f32)).round()
                            as u16)
                            .max(1),
                        box_.height,
                    )
                }
            }
        };
        let cols = cols.min(box_.width).max(1);
        let rows = rows.min(box_.height).max(1);
        Rect {
            x: box_.x + (box_.width - cols) / 2,
            y: box_.y + (box_.height - rows) / 2,
            width: cols,
            height: rows,
        }
    }
}

/// Where the box lands: most of the window, with a margin so the conversation
/// behind it is still visible at the edges.
pub fn rect(area: Rect) -> Rect {
    let w = area.width.saturating_sub(4).max(8);
    let h = area.height.saturating_sub(2).max(5);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    viewer: &Viewer,
    store: &MediaStore,
) -> Vec<Placement> {
    let r = rect(area);
    if r.width < 8 || r.height < 5 {
        return Vec::new();
    }
    Clear.render(r, buf);

    let t = theme;
    let item = viewer.current();
    let counter = format!("{}/{}", viewer.index + 1, viewer.items.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(rgb(t.border_focused)))
        .title(Span::styled(
            format!(
                "{}{} \u{b7} {counter} ",
                starkit::chrome::frame::TITLE_LEAD,
                item.filename
            ),
            Style::default()
                .fg(rgb(t.header_fg))
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                format!(
                    " h l move \u{b7} z {} \u{b7} s save \u{b7} o open \u{b7} y copy \u{b7} esc close ",
                    viewer.zoom.name()
                ),
                Style::default().fg(rgb(t.dim)),
            ))
            .right_aligned(),
        )
        .style(Style::default().bg(rgb(t.panel_bg)));
    let inner = block.inner(r);
    block.render(r, buf);
    starkit::chrome::frame::render_corners(r, buf, t, true);
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
    }

    let size = store.size(&item.key);
    let picture = viewer.picture_rect(inner, size);
    let caption = match size {
        Some((w, h)) => format!("{w}x{h}  {}", item.url),
        None => item.url.clone(),
    };
    buf.set_string(
        inner.x,
        inner.y + inner.height - 1,
        fit(&caption, inner.width),
        Style::default().fg(rgb(t.dim)),
    );

    vec![Placement {
        rect: picture,
        clip: inner,
        clipped: false,
        key: item.key.clone(),
        shape: Shape::Picture,
        alt: item.filename.clone(),
    }]
}

/// A name in `dir` that is not taken: `cat.png`, then `cat-1.png`.
///
/// A save that silently replaced a file would be the one destructive thing in
/// the program, and the two pictures a conversation calls `image.png` are
/// nearly always two different pictures.
pub fn free_name(dir: &std::path::Path, filename: &str) -> PathBuf {
    let filename = filename.trim();
    let filename = if filename.is_empty() {
        "attachment"
    } else {
        filename
    };
    // Only the last component, whatever the far end called it: a filename with
    // a slash in it is somebody else's path traversal.
    let filename = filename.rsplit(['/', '\\']).next().unwrap_or("attachment");
    let stem = std::path::Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".into());
    let extension = std::path::Path::new(filename)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let first = dir.join(format!("{stem}{extension}"));
    if !first.exists() {
        return first;
    }
    for n in 1..1000 {
        let next = dir.join(format!("{stem}-{n}{extension}"));
        if !next.exists() {
            return next;
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    fn item(message: u64, n: u64) -> Item {
        Item {
            message: MessageId(message),
            key: MediaKey::Attachment {
                message: MessageId(message),
                id: n,
                url: format!("https://cdn.invalid/{n}.png"),
            },
            url: format!("https://cdn.invalid/{n}.png"),
            filename: format!("{n}.png"),
        }
    }

    fn items() -> Vec<Item> {
        vec![item(1, 1), item(2, 2), item(2, 3), item(3, 4)]
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// It opens on the message it was asked for, and walks the whole channel
    /// from there rather than stopping at that message's last picture.
    #[test]
    fn it_opens_on_the_message_and_walks_the_channel() {
        let mut v = Viewer::new(items(), Some(MessageId(2)), 2.0).expect("four pictures");
        assert_eq!(v.current().filename, "2.png");
        v.handle(key('l'));
        assert_eq!(v.current().filename, "3.png", "the same message's second");
        v.handle(key('l'));
        assert_eq!(v.current().filename, "4.png", "and on to the next message");
        v.handle(key('l'));
        assert_eq!(v.current().filename, "1.png", "and round");
        v.handle(key('h'));
        assert_eq!(v.current().filename, "4.png", "and back the other way");
    }

    #[test]
    fn nothing_to_show_is_no_viewer_at_all() {
        assert!(Viewer::new(Vec::new(), None, 2.0).is_none());
    }

    #[test]
    fn the_keys_ask_for_what_they_say() {
        let mut v = Viewer::new(items(), None, 2.0).unwrap();
        assert_eq!(v.handle(key('o')), Action::Open(v.current().url.clone()));
        assert_eq!(v.handle(key('y')), Action::Copy(v.current().url.clone()));
        assert_eq!(v.handle(key('s')), Action::Save);
        assert_eq!(v.handle(key('q')), Action::Close);
        assert_eq!(
            v.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Close
        );
        assert_eq!(
            v.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
    }

    /// `z` swaps the two sizes, and each one keeps the picture's shape.
    #[test]
    fn zoom_fits_then_shows_it_at_its_own_size() {
        let mut v = Viewer::new(items(), None, 2.0).unwrap();
        let inner = Rect::new(0, 0, 80, 30);

        let fitted = v.picture_rect(inner, Some((400, 200)));
        assert!(fitted.width <= inner.width && fitted.height < inner.height);
        // Twice as wide as tall, on cells twice as tall as wide, is four cells
        // across for every one down.
        assert!(
            fitted.width >= fitted.height * 3,
            "the shape was not kept: {fitted:?}"
        );

        v.handle(key('z'));
        assert_eq!(v.zoom, Zoom::Actual);
        let actual = v.picture_rect(inner, Some((32, 64)));
        assert_eq!((actual.width, actual.height), (2, 2));

        // A picture nothing is known about takes the whole box.
        let unknown = v.picture_rect(inner, None);
        assert_eq!(unknown.width, inner.width);
    }

    /// A click on the left half goes back and on the right half goes on.
    #[test]
    fn the_halves_of_the_window_are_the_two_directions() {
        let mut v = Viewer::new(items(), None, 2.0).unwrap();
        let area = Rect::new(0, 0, 100, 30);
        v.click(area, 90, 10);
        assert_eq!(v.index, 1);
        v.click(area, 5, 10);
        assert_eq!(v.index, 0);
    }

    #[test]
    fn it_draws_a_frame_and_places_one_picture() {
        let v = Viewer::new(items(), Some(MessageId(2)), 2.0).unwrap();
        let t = theme("terminal");
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let places = render(area, &mut buf, &t, &v, &MediaStore::new());
        assert_eq!(places.len(), 1);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("2.png"), "{text}");
        assert!(text.contains("2/4"), "{text}");
    }
}
