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
//! ## Fitting includes growing
//!
//! `Resize::Fit` never upsizes, so a picture smaller than the window used to
//! sit in the middle of it at whatever size it happened to be -- which is most
//! of what a conversation carries, and is the one thing somebody who opened the
//! viewer was asking for. So the rectangle is measured in *pixels*, through
//! [`Graphics::cell_size`](starkit::graphics::Graphics::cell_size): the core is
//! asked for the picture at that many rather than at a cell-count estimate, and
//! the paint pass grows whatever is smaller than the rectangle to fill it --
//! see [`fill_up`]. The meta line says `×2.4` when it did. A terminal that
//! never measured a cell has no honest pixel count to work from and gets what
//! it always got.
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

use crate::discord::media::{Decoded, MediaKey};
use crate::discord::snowflake::MessageId;
use crate::ui::panels::chat::media::{fill_up, MediaStore, Placement, Shape};
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

/// Which of the channel's pictures the viewer opens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Open {
    /// The oldest one there is.
    First,
    /// The first one on this message, which is what `enter` on a message
    /// means.
    Message(MessageId),
    /// This exact picture. A click landed on it, and a message with three
    /// photographs on it would otherwise open on the wrong one.
    Attachment { message: MessageId, url: String },
}

#[derive(Debug, Clone)]
pub struct Viewer {
    pub items: Vec<Item>,
    pub index: usize,
    pub zoom: Zoom,
    /// How many pixels a cell is, where the terminal said. The aspect is
    /// taken from it, and so is the pixel size of the rectangle a picture is
    /// grown to fill.
    cell: Option<(u16, u16)>,
    /// Cell aspect, for drawing a picture at its own shape.
    aspect: f32,
}

impl Viewer {
    /// A viewer over `items`, opened on whichever of them `on` names.
    pub fn new(items: Vec<Item>, on: Open, cell: Option<(u16, u16)>) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        // A picture named by its URL, then by its message, then the first
        // there is: the same click that reaches an attachment carries both,
        // and a link that has been re-signed since the list was built still
        // lands on the right message.
        let index = match &on {
            Open::First => None,
            Open::Message(id) => items.iter().position(|i| i.message == *id),
            Open::Attachment { message, url } => items
                .iter()
                .position(|i| i.url == *url)
                .or_else(|| items.iter().position(|i| i.message == *message)),
        }
        .unwrap_or(0);
        Some(Self {
            items,
            index,
            zoom: Zoom::Fit,
            cell: cell.filter(|(w, h)| *w > 0 && *h > 0),
            aspect: match cell {
                Some((w, h)) if w > 0 && h > 0 => f32::from(h) / f32::from(w),
                _ => 2.0,
            },
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
            // One image pixel per terminal pixel, at the cell the terminal
            // reported. Where it reported none, the aspect and a sixteen-pixel
            // column, which is what the media store assumes everywhere else.
            Zoom::Actual => {
                let (cell_w, cell_h) = self
                    .cell
                    .map(|(cw, ch)| (f32::from(cw), f32::from(ch)))
                    .unwrap_or((16.0, 16.0 * self.aspect));
                (
                    ((w as f32 / cell_w).round() as u16).max(1),
                    ((h as f32 / cell_h).round() as u16).max(1),
                )
            }
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
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
    }

    let size = store.size(&item.key);
    let picture = viewer.picture_rect(inner, size);

    // What the rectangle holds in pixels, which is what the picture is asked
    // for and grown to. Only for a still: an animation is scaled by the core,
    // every frame of it at once and inside a pixel budget, because growing one
    // frame per redraw here would be thirty resizes a second on the thread
    // that draws.
    let pixels = match store.decoded(&item.key).as_deref() {
        Some(Decoded::Animated { .. }) | Some(Decoded::Bytes(_)) => None,
        Some(Decoded::Still { image, .. }) => Some((image.width(), image.height())),
        // Nothing has arrived yet. The room is still what to ask for.
        None => Some((0, 0)),
    };
    let room = pixels.and_then(|_| {
        viewer.cell.map(|(cw, ch)| {
            (
                u32::from(picture.width) * u32::from(cw),
                u32::from(picture.height) * u32::from(ch),
            )
        })
    });
    let grown = pixels
        .zip(room)
        .and_then(|(pixels, room)| fill_up(pixels, room));

    let caption = match (size, grown) {
        (Some((w, h)), Some(grown)) => {
            format!("{w}x{h}  \u{d7}{:.1}  {}", grown.factor, item.url)
        }
        (Some((w, h)), None) => format!("{w}x{h}  {}", item.url),
        (None, _) => item.url.clone(),
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
        room,
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

    /// A measured cell, sixteen by thirty-two: the aspect of two these tests
    /// were written against, now said in pixels.
    const CELL: Option<(u16, u16)> = Some((16, 32));

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
        let mut v = Viewer::new(items(), Open::Message(MessageId(2)), CELL).expect("four pictures");
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
        assert!(Viewer::new(Vec::new(), Open::First, CELL).is_none());
    }

    #[test]
    fn the_keys_ask_for_what_they_say() {
        let mut v = Viewer::new(items(), Open::First, CELL).unwrap();
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
        let mut v = Viewer::new(items(), Open::First, CELL).unwrap();
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
        let mut v = Viewer::new(items(), Open::First, CELL).unwrap();
        let area = Rect::new(0, 0, 100, 30);
        v.click(area, 90, 10);
        assert_eq!(v.index, 1);
        v.click(area, 5, 10);
        assert_eq!(v.index, 0);
    }

    #[test]
    fn it_draws_a_frame_and_places_one_picture() {
        let v = Viewer::new(items(), Open::Message(MessageId(2)), CELL).unwrap();
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

    /// A click lands on one attachment of a message that carries three, and
    /// that is the one the viewer opens on -- not the message's first.
    #[test]
    fn it_opens_on_the_attachment_that_was_clicked() {
        let v = Viewer::new(
            items(),
            Open::Attachment {
                message: MessageId(2),
                url: "https://cdn.invalid/3.png".into(),
            },
            CELL,
        )
        .expect("four pictures");
        assert_eq!(v.current().filename, "3.png");

        // A link that has been re-signed since the list was built is still
        // that message's picture.
        let v = Viewer::new(
            items(),
            Open::Attachment {
                message: MessageId(2),
                url: "https://cdn.invalid/3.png?ex=deadbeef".into(),
            },
            CELL,
        )
        .expect("four pictures");
        assert_eq!(v.current().filename, "2.png", "it fell back to the message");
    }

    /// A picture smaller than the window is asked for at the window's pixels
    /// and grown to them, and the meta line says by how much. With no measured
    /// cell there are no pixels to talk about, and nothing is asked or grown.
    #[test]
    fn a_small_picture_is_grown_to_the_window_and_the_line_says_so() {
        let v = Viewer::new(items(), Open::First, CELL).unwrap();
        let mut store = MediaStore::new();
        store.arrived(
            v.current().key.clone(),
            Ok(std::sync::Arc::new(Decoded::Still {
                image: std::sync::Arc::new(image::RgbaImage::new(64, 64)),
                natural: (64, 64),
            })),
        );
        let t = theme("terminal");
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);

        let places = render(area, &mut buf, &t, &v, &store);
        let room = places[0].room.expect("the window, in pixels");
        assert_eq!(
            room,
            (
                u32::from(places[0].rect.width) * 16,
                u32::from(places[0].rect.height) * 32
            )
        );
        let line: String = (0..area.width)
            .map(|x| buf[(x, area.height - 3)].symbol().to_string())
            .collect();
        assert!(line.contains("64x64"), "{line:?}");
        assert!(
            line.contains('\u{d7}'),
            "the factor is not on the line: {line:?}"
        );

        // No cell, no pixels: what it always did.
        let v = Viewer::new(items(), Open::First, None).unwrap();
        let mut buf = Buffer::empty(area);
        let places = render(area, &mut buf, &t, &v, &store);
        assert_eq!(places[0].room, None);
        let line: String = (0..area.width)
            .map(|x| buf[(x, area.height - 3)].symbol().to_string())
            .collect();
        assert!(!line.contains('\u{d7}'), "{line:?}");
    }
}
