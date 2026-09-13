//! Pictures on the screen: what is known about each one, and where it goes.
//!
//! The renderer reserves cells for a picture before anything has been
//! downloaded — that is what stops the message list reflowing under the reader
//! when bytes arrive — and records a slot saying which picture belongs in
//! them. This is the other half: one pass after the text, which turns each
//! slot into an absolute rectangle, asks the core for whatever is missing, and
//! draws whatever is here.
//!
//! ## Why the drawing is a second pass
//!
//! A protocol image is not a cell. It is an escape sequence that the terminal
//! places over a region, and ratatui carries it in the first cell of that
//! region; anything written into those cells afterwards would either be
//! painted over by the terminal or would paint over the sequence. So the text
//! goes down first, every panel finishes, and the pictures land last — which
//! is also the only point at which the full set of what is on screen is known,
//! and that set is what [`Graphics::forget_unused`] needs.
//!
//! ## Naming, again
//!
//! [`MediaStore`] is keyed by [`MediaKey`], which names a picture by what it
//! is rather than by the signed URL it currently lives at, so a refreshed link
//! is the same entry. The graphics cache underneath is keyed by
//! [`ImageId::of`] over the same key plus the rectangle, so the same avatar in
//! the rail and in a message header is two encodings of one picture and a
//! resize throws away only what changed shape.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use starkit::graphics::{self, Graphics, ImageId, Mode};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::widgets::Widget;
use starkit::ratatui_image::Image;
use starkit::theme::color::Rgb;

use crate::discord::media::{Decoded, MediaError, MediaKey, MediaPriority, MediaRequest, Want};
use crate::ui::panels::{fit, rgb};
use crate::ui::theme::Theme;

/// What marks a moving picture this terminal will not play.
const PLAY: &str = " \u{25b6} ";

/// How many custom emoji get a protocol image in one frame.
///
/// A wall of reactions can name a hundred, and each one is an upload to the
/// terminal. Past the cap the rest of them draw as text, which is what they
/// would have been with no pictures at all.
pub const EMOJI_PER_FRAME: usize = 64;

/// Pixels per cell, for deciding how large a decode to ask the core for.
///
/// An estimate on purpose. The exact cell size is the terminal's and changes
/// with a font zoom; asking for a little more than is needed costs a slightly
/// larger resize once, and asking for less would show a soft picture until
/// something invalidated it. Both are generous for the fonts people use.
const PX_PER_COL: u32 = 16;
const PX_PER_ROW: u32 = 32;

/// Headroom over the number of pictures on screen, in the graphics cache.
///
/// The capacity has to be larger than one frame's worth or the last insert of
/// a frame evicts the first, and every frame rebuilds every picture. The
/// spare entries also carry a picture through the frame where a wheel scroll
/// has just taken it off the top and is about to bring it back.
const CACHE_HEADROOM: usize = 16;

/// How many decoded pictures to hold on to.
///
/// Pixels, not protocols: the terminal's copy is bounded by the graphics
/// cache, and this is the process's. Past the cap the ones nobody has looked
/// at for longest are dropped and will be fetched again from the on-disk
/// cache, which is a file read.
const DECODED_CAP: usize = 192;

/// What is known about one picture.
#[derive(Debug, Clone)]
pub enum MediaState {
    /// Nobody has asked for it.
    Missing,
    /// Asked for, and the answer has not come back.
    Loading,
    Ready(Arc<Decoded>),
    /// It will not arrive. Kept rather than forgotten, so a picture that
    /// cannot be fetched is not asked for again on every frame.
    Failed,
}

#[derive(Debug)]
struct Entry {
    state: MediaState,
    /// The frame this was last wanted on, for the sweep.
    seen: u64,
}

/// Every picture the message list knows about.
#[derive(Debug, Default)]
pub struct MediaStore {
    entries: HashMap<MediaKey, Entry>,
    /// Bumped when the viewport moves, so the core can drop queued work for a
    /// part of the conversation that has already scrolled past.
    generation: u64,
    frame: u64,
    requests: Vec<MediaRequest>,
    cancels: Vec<MediaKey>,
}

impl MediaStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self, key: &MediaKey) -> MediaState {
        self.entries
            .get(key)
            .map(|e| e.state.clone())
            .unwrap_or(MediaState::Missing)
    }

    /// The core has answered.
    pub fn arrived(&mut self, key: MediaKey, result: Result<Arc<Decoded>, MediaError>) {
        let state = match result {
            Ok(decoded) => MediaState::Ready(decoded),
            Err(MediaError::Cancelled) => MediaState::Missing,
            Err(_) => MediaState::Failed,
        };
        let frame = self.frame;
        self.entries
            .entry(key)
            .and_modify(|e| e.state = state.clone())
            .or_insert(Entry { state, seen: frame });
    }

    /// The reader scrolled, or changed channel.
    pub fn viewport_moved(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Start a frame. Everything asked for after this counts as on screen.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.requests.clear();
        self.cancels.clear();
    }

    /// This picture is on screen at this size.
    ///
    /// One request per key per frame, and only for a slot nothing has been
    /// asked for: the state goes to `Loading` the moment the request is
    /// recorded, which is what stops a picture being fetched thirty times a
    /// second while it downloads.
    fn want(&mut self, key: &MediaKey, cols: u16, rows: u16) -> MediaState {
        let frame = self.frame;
        let entry = self.entries.entry(key.clone()).or_insert(Entry {
            state: MediaState::Missing,
            seen: frame,
        });
        entry.seen = frame;
        if matches!(entry.state, MediaState::Missing) {
            entry.state = MediaState::Loading;
            self.requests.push(MediaRequest {
                key: key.clone(),
                want: Want::Decoded {
                    max_w: u32::from(cols).max(1) * PX_PER_COL,
                    max_h: u32::from(rows).max(1) * PX_PER_ROW,
                },
                priority: MediaPriority::Visible,
                generation: self.generation,
            });
            return MediaState::Loading;
        }
        entry.state.clone()
    }

    /// Finish a frame: cancel what left the viewport, and bound the pixels.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        for (key, entry) in self.entries.iter_mut() {
            if entry.seen != frame && matches!(entry.state, MediaState::Loading) {
                // Queued rather than in flight, as far as this side knows. The
                // core drops it if it has not started and lets it finish if it
                // has, and either way the next frame that wants it asks again.
                entry.state = MediaState::Missing;
                self.cancels.push(key.clone());
            }
        }
        if self.entries.len() > DECODED_CAP {
            let mut ages: Vec<(u64, MediaKey)> = self
                .entries
                .iter()
                .filter(|(_, e)| e.seen != frame)
                .map(|(k, e)| (e.seen, k.clone()))
                .collect();
            ages.sort_by_key(|(seen, _)| *seen);
            let excess = self.entries.len() - DECODED_CAP;
            for (_, key) in ages.into_iter().take(excess) {
                self.entries.remove(&key);
            }
        }
    }

    /// What to ask the core for, once per frame.
    pub fn take_requests(&mut self) -> Vec<MediaRequest> {
        std::mem::take(&mut self.requests)
    }

    /// What to tell the core it no longer needs.
    pub fn take_cancels(&mut self) -> Vec<MediaKey> {
        std::mem::take(&mut self.cancels)
    }

    /// Forget everything. For a channel change, where none of it is on screen
    /// any more and the next frame will say what is.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.viewport_moved();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// What a placement is, which decides what is drawn when there are no pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A picture with rows of its own.
    Picture,
    /// A square in a gutter or a rail: an avatar, a server icon. Falls back to
    /// the initials, which is what the rail drew before there were pictures.
    Icon { initials: String, colour: Rgb },
    /// Two cells on a line of text. Falls back to the name, cut to fit.
    Emoji { name: String },
    /// A still standing in for something this terminal will not play, with a
    /// marker over it saying so.
    Play,
}

/// One picture, and the cells it was given.
#[derive(Debug, Clone)]
pub struct Placement {
    /// Where it goes, in screen coordinates, before clipping.
    pub rect: Rect,
    /// The panel it belongs to. A picture half off the bottom of the message
    /// list is drawn for the rows that are inside this.
    pub clip: Rect,
    /// Whether rows were already taken off the top of it, by a scroll that has
    /// carried the picture part-way off the viewport. The bottom is cut here;
    /// the top has to be cut before the rectangle exists, because a rectangle
    /// cannot start above the screen.
    pub clipped: bool,
    pub key: MediaKey,
    pub shape: Shape,
    /// What to say in the cells when the picture will never arrive: the chip
    /// the renderer would have drawn instead of it.
    pub alt: String,
}

/// What one frame's painting produced, for the caller to act on.
#[derive(Debug, Default)]
pub struct Painted {
    /// Every picture actually put on the screen.
    pub drawn: HashSet<ImageId>,
    /// How many emoji were drawn as pictures, against the cap.
    pub emoji: usize,
}

/// Draw every placement, and say what was drawn.
///
/// The caller owns the order: placements are painted in the order they were
/// recorded, so a picture recorded later sits on top of one recorded earlier,
/// exactly as the buffer behaves for text.
pub fn paint(
    places: &[Placement],
    graphics: &mut Graphics,
    store: &mut MediaStore,
    theme: &Theme,
    buf: &mut Buffer,
) -> Painted {
    let mut out = Painted::default();
    // Big enough that nothing drawn this frame is evicted by something else
    // drawn this frame, which is the failure that turns a cache into a cost.
    graphics.set_capacity(places.len() + CACHE_HEADROOM);

    for place in places {
        let Some(rect) = intersect(place.rect, place.clip) else {
            continue;
        };
        let clipped = place.clipped || rect != place.rect;
        if matches!(place.shape, Shape::Emoji { .. }) && out.emoji >= EMOJI_PER_FRAME {
            fallback(place, rect, buf, theme, &MediaState::Loading);
            continue;
        }

        let state = store.want(&place.key, place.rect.width, place.rect.height);
        let MediaState::Ready(decoded) = &state else {
            fallback(place, rect, buf, theme, &state);
            continue;
        };
        let Some(img) = first_frame(decoded) else {
            fallback(place, rect, buf, theme, &MediaState::Failed);
            continue;
        };

        let id = ImageId::of(&place.key);
        if draw_one(graphics, id, img, rect, clipped, buf) {
            out.drawn.insert(id);
        }
        if matches!(place.shape, Shape::Emoji { .. }) {
            out.emoji += 1;
        }
        if matches!(place.shape, Shape::Play) {
            overlay_play(rect, buf, theme);
        }
    }
    out
}

/// Draw one picture into one rectangle. True when a protocol was used, which
/// is what the caller collects for [`Graphics::forget_unused`].
fn draw_one(
    graphics: &mut Graphics,
    id: ImageId,
    img: &Arc<image::RgbaImage>,
    rect: Rect,
    clipped: bool,
    buf: &mut Buffer,
) -> bool {
    match graphics.mode() {
        // `[ui] graphics = off` means no picture at all, not a coarse one.
        Mode::Off => return false,
        Mode::Blocks => {
            graphics::halfblocks(img, rect, buf);
            return false;
        }
        _ => {}
    }
    // A clipped rectangle is half blocks unless the protocol tolerates one.
    // kitty does -- it takes a placement and crops it -- and sixel and iTerm2
    // do not: they would draw the whole picture over whatever is below the
    // panel, and leave it there.
    if clipped && graphics.name() != "kitty" {
        graphics::halfblocks(img, rect, buf);
        return false;
    }
    match graphics.protocol(id, img, rect) {
        Some(protocol) => {
            Image::new(protocol).render(rect, buf);
            // A one-cell image leaves the cursor a row low; see the note on
            // `mend_unit_placeholder`. Called for every size because it only
            // rewrites the exact tail that is wrong.
            graphics::mend_unit_placeholder(buf, rect.x, rect.y);
            true
        }
        None => {
            graphics::halfblocks(img, rect, buf);
            false
        }
    }
}

/// What the cells hold while there are no pixels for them.
///
/// A picture that is on its way is a quiet block of `░`; one that will never
/// arrive says what it was, in the same rows, because the alternative is a
/// placeholder that waits forever.
fn fallback(place: &Placement, rect: Rect, buf: &mut Buffer, theme: &Theme, state: &MediaState) {
    let failed = matches!(state, MediaState::Failed);
    match &place.shape {
        Shape::Picture | Shape::Play if failed => {
            for y in 0..rect.height {
                buf.set_string(
                    rect.x,
                    rect.y + y,
                    " ".repeat(usize::from(rect.width)),
                    Style::default(),
                );
            }
            let label = if place.alt.is_empty() {
                "[the picture could not be fetched]"
            } else {
                place.alt.as_str()
            };
            buf.set_string(
                rect.x,
                rect.y,
                fit(label, rect.width),
                Style::default().fg(rgb(theme.chat.system_fg)),
            );
        }
        Shape::Picture | Shape::Play => {
            graphics::placeholder(rect, buf, Style::default().fg(rgb(theme.chat.spoiler_bg)));
        }
        Shape::Icon { initials, colour } => {
            buf.set_string(
                rect.x,
                rect.y + rect.height / 2,
                fit(initials, rect.width),
                Style::default()
                    .fg(rgb(*colour))
                    .add_modifier(Modifier::BOLD),
            );
        }
        Shape::Emoji { name } => {
            // Two cells is not a name, so it is the first two characters of
            // one: enough to tell two reactions apart while the pictures are
            // on their way, and it is what the row measured.
            let short: String = name.chars().take(usize::from(rect.width)).collect();
            buf.set_string(
                rect.x,
                rect.y,
                fit(&short, rect.width),
                Style::default().fg(rgb(theme.dim)),
            );
        }
    }
}

/// ` ▶ ` across the middle of a still that stands for a video.
fn overlay_play(rect: Rect, buf: &mut Buffer, theme: &Theme) {
    if rect.width < 3 || rect.height == 0 {
        return;
    }
    let x = rect.x + (rect.width - 3) / 2;
    let y = rect.y + rect.height / 2;
    buf.set_string(
        x,
        y,
        PLAY,
        Style::default()
            .fg(rgb(theme.fg))
            .bg(rgb(theme.panel_bg))
            .add_modifier(Modifier::BOLD),
    );
}

/// The picture to draw for a decoding. An animation shows its first frame
/// until there is something to advance it.
fn first_frame(decoded: &Decoded) -> Option<&Arc<image::RgbaImage>> {
    match decoded {
        Decoded::Still(img) => Some(img),
        Decoded::Animated { frames, .. } => frames.first(),
        Decoded::Bytes(_) => None,
    }
}

/// The part of `rect` inside `clip`, or nothing.
pub fn intersect(rect: Rect, clip: Rect) -> Option<Rect> {
    let x0 = rect.x.max(clip.x);
    let y0 = rect.y.max(clip.y);
    let x1 = (rect.x + rect.width).min(clip.x + clip.width);
    let y1 = (rect.y + rect.height).min(clip.y + clip.height);
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::{EmojiId, MessageId};
    use crate::ui::theme::tests_support::theme;

    fn key(n: u64) -> MediaKey {
        MediaKey::Attachment {
            message: MessageId(n),
            id: n,
            url: format!("https://cdn.invalid/{n}.png"),
        }
    }

    /// A terminal with no protocol, drawing half blocks. Everything the paint
    /// pass does short of handing bytes to a terminal happens here.
    fn blocks() -> Graphics {
        let mut g = Graphics::disabled();
        g.set_mode(Mode::Blocks);
        g
    }

    fn still() -> Arc<Decoded> {
        Arc::new(Decoded::Still(Arc::new(image::RgbaImage::new(8, 8))))
    }

    fn place(key: MediaKey, rect: Rect, clip: Rect) -> Placement {
        Placement {
            rect,
            clip,
            clipped: false,
            key,
            shape: Shape::Picture,
            alt: String::new(),
        }
    }

    /// The whole state machine: nothing is asked for twice, an answer sticks,
    /// and a failure is not retried on every frame.
    #[test]
    fn a_picture_is_asked_for_once_and_the_answer_sticks() {
        let mut store = MediaStore::new();
        store.begin_frame();
        assert!(matches!(store.want(&key(1), 20, 6), MediaState::Loading));
        assert_eq!(store.take_requests().len(), 1, "one request for one slot");

        // A second slot on the same picture in the same frame asks for nothing.
        assert!(matches!(store.want(&key(1), 20, 6), MediaState::Loading));
        assert!(store.take_requests().is_empty());

        // And so does the next frame, while it is still in flight.
        store.begin_frame();
        store.want(&key(1), 20, 6);
        assert!(
            store.take_requests().is_empty(),
            "it asked again mid-flight"
        );

        store.arrived(key(1), Ok(still()));
        store.begin_frame();
        assert!(matches!(store.want(&key(1), 20, 6), MediaState::Ready(_)));
        assert!(store.take_requests().is_empty());

        store.arrived(key(1), Err(MediaError::Expired));
        store.begin_frame();
        assert!(matches!(store.want(&key(1), 20, 6), MediaState::Failed));
        assert!(
            store.take_requests().is_empty(),
            "a failure was fetched again"
        );
    }

    /// A cancelled request is not a failure: it is one nobody waited for, and
    /// the next frame that wants the picture asks again.
    #[test]
    fn a_cancelled_request_can_be_asked_for_again() {
        let mut store = MediaStore::new();
        store.begin_frame();
        store.want(&key(2), 10, 4);
        store.arrived(key(2), Err(MediaError::Cancelled));
        store.begin_frame();
        assert!(matches!(store.want(&key(2), 10, 4), MediaState::Loading));
        assert_eq!(store.take_requests().len(), 1);
    }

    /// Scrolling away from something that has not arrived tells the core to
    /// drop it.
    #[test]
    fn what_leaves_the_viewport_is_cancelled() {
        let mut store = MediaStore::new();
        store.begin_frame();
        store.want(&key(3), 10, 4);
        store.end_frame();
        assert!(store.take_cancels().is_empty(), "it is still on screen");

        store.begin_frame();
        store.end_frame();
        assert_eq!(store.take_cancels(), vec![key(3)]);
        // And it is askable again, because nothing arrived.
        store.begin_frame();
        assert!(matches!(store.want(&key(3), 10, 4), MediaState::Loading));
    }

    /// A picture that did arrive is kept when it scrolls off: the pixels are
    /// the expensive half, and a wheel back up should not refetch them.
    #[test]
    fn a_decoded_picture_survives_leaving_the_viewport() {
        let mut store = MediaStore::new();
        store.begin_frame();
        store.want(&key(4), 10, 4);
        store.arrived(key(4), Ok(still()));
        store.begin_frame();
        store.end_frame();
        assert!(store.take_cancels().is_empty());
        assert!(matches!(store.state(&key(4)), MediaState::Ready(_)));
    }

    /// The pixels are bounded. Everything on screen survives a sweep; the
    /// oldest of what is not goes.
    #[test]
    fn the_decoded_pictures_are_bounded() {
        let mut store = MediaStore::new();
        for i in 0..(DECODED_CAP as u64 + 50) {
            store.begin_frame();
            store.want(&key(i), 4, 2);
            store.arrived(key(i), Ok(still()));
            store.end_frame();
        }
        assert!(
            store.len() <= DECODED_CAP,
            "{} entries kept, cap is {DECODED_CAP}",
            store.len()
        );
        assert!(
            matches!(
                store.state(&key(DECODED_CAP as u64 + 49)),
                MediaState::Ready(_)
            ),
            "the newest was swept"
        );
    }

    /// The rule every placement goes through: a rectangle is cut to the panel
    /// it belongs to, and one entirely outside it is not drawn at all.
    #[test]
    fn a_placement_is_cut_to_its_panel() {
        let clip = Rect::new(10, 5, 20, 10);
        assert_eq!(
            intersect(Rect::new(12, 6, 4, 2), clip),
            Some(Rect::new(12, 6, 4, 2))
        );
        // Half off the bottom: the visible rows survive.
        assert_eq!(
            intersect(Rect::new(12, 13, 4, 6), clip),
            Some(Rect::new(12, 13, 4, 2))
        );
        // Half off the left.
        assert_eq!(
            intersect(Rect::new(8, 6, 4, 2), clip),
            Some(Rect::new(10, 6, 2, 2))
        );
        assert_eq!(intersect(Rect::new(0, 0, 4, 2), clip), None);
        assert_eq!(intersect(Rect::new(12, 20, 4, 2), clip), None);
    }

    /// What was drawn is what is kept, and it is named by the key rather than
    /// by the bytes: the same picture in two panels is one entry.
    #[test]
    fn the_drawn_set_names_the_pictures_that_landed() {
        let t = theme("terminal");
        let mut graphics = blocks();
        let mut store = MediaStore::new();
        let clip = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(clip);

        store.arrived(key(1), Ok(still()));
        store.begin_frame();
        let places = vec![
            place(key(1), Rect::new(0, 0, 10, 4), clip),
            place(key(1), Rect::new(12, 0, 6, 2), clip),
            // Off the panel entirely: never asked for, never drawn.
            place(key(9), Rect::new(0, 40, 10, 4), clip),
        ];
        let painted = paint(&places, &mut graphics, &mut store, &t, &mut buf);
        store.end_frame();

        // Half blocks are drawn by this side rather than by the terminal, so
        // there is no protocol to keep -- and the picture did land.
        assert!(painted.drawn.is_empty());
        assert_eq!(buf[(0, 0)].symbol(), graphics::HALF.to_string());
        assert_eq!(buf[(13, 1)].symbol(), graphics::HALF.to_string());
        // The one thing this is really here to hold: a slot outside the panel
        // is not fetched, so scrolling does not queue the whole channel.
        assert!(
            store.take_requests().is_empty(),
            "a slot outside the panel was fetched"
        );
        assert!(matches!(store.state(&key(9)), MediaState::Missing));
    }

    /// Past the cap an emoji is text. Sixty-five on one screen is a reaction
    /// wall, and the sixty-fifth upload is not worth the frame.
    #[test]
    fn the_emoji_protocols_are_capped_per_frame() {
        let t = theme("terminal");
        let mut graphics = blocks();
        let mut store = MediaStore::new();
        let clip = Rect::new(0, 0, 200, 2);
        let mut buf = Buffer::empty(clip);

        let mut places = Vec::new();
        for i in 0..(EMOJI_PER_FRAME as u64 + 4) {
            let key = MediaKey::Emoji {
                id: EmojiId(i + 1),
                animated: false,
                size: 32,
            };
            store.arrived(key.clone(), Ok(still()));
            places.push(Placement {
                rect: Rect::new((i as u16 * 2) % 200, 0, 2, 1),
                clip,
                clipped: false,
                key,
                shape: Shape::Emoji {
                    name: format!("e{i}"),
                },
                alt: String::new(),
            });
        }
        store.begin_frame();
        let painted = paint(&places, &mut graphics, &mut store, &t, &mut buf);
        store.end_frame();
        assert_eq!(
            painted.emoji, EMOJI_PER_FRAME,
            "the cap was not what stopped it"
        );
    }
}
