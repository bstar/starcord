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

use super::anim::Animations;
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
    /// The one picture this application has scaled up itself -- see
    /// [`Scaled`].
    scaled: Option<Scaled>,
    /// The largest decode ever asked for, per picture, and whether it was
    /// asked to fill its box.
    ///
    /// A picture is first fetched at the size of the slot that placed it, and
    /// the media viewer wants it very much larger. Without this the viewer
    /// would draw the thumbnail the message list fetched, blown up; with a
    /// plain "ask again" it would ask on every frame for ever, because a
    /// picture that is genuinely smaller than the box never grows. The flag
    /// is for a picture that was measured before it was placed: the
    /// measurement fetched it at its own size, and the slot wants it at the
    /// slot's.
    asked: HashMap<MediaKey, Asked>,
}

/// What has been asked of the core for one picture so far: the largest
/// box, and the box it was last asked to fill, if any.
#[derive(Debug, Clone, Copy, Default)]
struct Asked {
    w: u32,
    h: u32,
    filled: Option<(u32, u32)>,
}

/// The one picture this application has grown, kept between frames.
///
/// Scaling happens on the drawing thread, so it must not happen per frame:
/// the copy is rebuilt only when the picture, the frame of it or the size
/// asked for changes, and there is one of these because one picture at a
/// time is ever grown -- the media viewer's, which covers the window.
#[derive(Debug)]
struct Scaled {
    key: MediaKey,
    frame: usize,
    pixels: (u32, u32),
    /// [`ImageId::of_arc`] of `image`. The protocol is built from these
    /// pixels, so it has to be named after them rather than after the
    /// picture they were made from.
    id: ImageId,
    image: Arc<image::RgbaImage>,
}

/// What [`MediaStore::scale`] answered.
#[derive(Debug)]
struct Grown {
    id: ImageId,
    image: Arc<image::RgbaImage>,
    /// The identity the copy this one replaced went under, which now names
    /// nothing: `ImageId::of_arc` is an address, and the next picture along
    /// may be handed the one just freed. The caller makes the terminal
    /// forget it.
    stale: Option<ImageId>,
}

/// How much bigger a picture has to be made to fill a box of pixels, and the
/// size to make it. `None` when it already fills the box or overflows it.
///
/// [`starkit::ratatui_image::Resize::Fit`] never upsizes -- its own
/// documentation says so -- so a picture smaller than the rectangle it was
/// given sits in the middle of it at whatever size it happens to be, and
/// growing it is this application's own work: scale the pixels to exactly
/// the size of the rectangle they will be placed over, and hand *that*
/// picture to the terminal. This is the whole of the arithmetic, and it is
/// pure so that the label the viewer writes and the pixels the paint pass
/// makes cannot disagree about the factor.
pub fn fill_up(picture: (u32, u32), room: (u32, u32)) -> Option<Grow> {
    let ((w, h), (room_w, room_h)) = (picture, room);
    if w == 0 || h == 0 || room_w == 0 || room_h == 0 {
        return None;
    }
    // The rectangle already has the picture's own shape, so the two ratios
    // are within a rounding of each other; the smaller is taken, so the
    // result cannot spill past the rectangle and be shrunk straight back by
    // the encoder.
    let factor = (f64::from(room_w) / f64::from(w)).min(f64::from(room_h) / f64::from(h));
    // Under a twentieth there is nothing to see. A rectangle is a whole number
    // of cells and a picture is not, so one that already fills its box lands
    // just above one every time, and a resize pass for three per cent costs the
    // frame it happens on and shows nobody anything -- it would print as
    // `×1.0` on the meta line, which is a way of saying "not really".
    if !factor.is_finite() || (factor * 10.0).round() < 11.0 {
        return None;
    }
    Some(Grow {
        pixels: (
            ((f64::from(w) * factor).round() as u32).clamp(1, room_w),
            ((f64::from(h) * factor).round() as u32).clamp(1, room_h),
        ),
        factor: factor as f32,
    })
}

/// What [`fill_up`] decided: the pixels to make, and how much bigger than
/// itself the picture ends up. The factor is always at least a twentieth above
/// one, which is the least the meta line can say.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grow {
    pub pixels: (u32, u32),
    pub factor: f32,
}

/// The filter a picture is grown with.
///
/// Catmull-Rom rather than Lanczos3, which is the choice STAR/FOLD's preview
/// made for the same reason: both are sharp enough at these sizes, and
/// Lanczos3's negative lobes ring on a hard edge. A conversation's pictures
/// are as often a screenshot of text or a diagram as a photograph, and a halo
/// around every letter is the worse of the two failures.
const SMOOTH: image::imageops::FilterType = image::imageops::FilterType::CatmullRom;

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

    /// The pixels, when there are any.
    pub fn decoded(&self, key: &MediaKey) -> Option<Arc<Decoded>> {
        match self.entries.get(key).map(|e| &e.state) {
            Some(MediaState::Ready(decoded)) => Some(Arc::clone(decoded)),
            _ => None,
        }
    }

    /// How big the picture turned out to be, in pixels.
    ///
    /// The renderer reserves rows from the size an attachment *declares*, and
    /// an embed often declares nothing at all. Those get one row until the
    /// bytes arrive; this is what the second measurement reads, and it is the
    /// reason a `MediaStore` is threaded into the render context rather than
    /// only into the drawing pass.
    ///
    /// The size on the wire, not the size of the pixels held here: those are
    /// scaled to the box that asked for them, and a box measured from its
    /// own answer would grow on every measurement.
    pub fn size(&self, key: &MediaKey) -> Option<(u32, u32)> {
        self.decoded(key)?.natural()
    }

    /// Ask for this picture for a box measured in pixels, not in cells.
    ///
    /// For the media viewer, which covers the window and knows how many
    /// pixels a cell is: what the message list asked for was sized to a slot
    /// a few rows tall, through the estimate above. Nothing is asked to
    /// *fill* the box -- a placement that names its own pixels grows the
    /// picture itself, with the size the terminal reported and a filter
    /// chosen for it, and a fill as well would be a second decode of one
    /// picture whose arrival would decide how sharp it looked.
    ///
    /// Only ever asks for more than it asked for last time, since a picture
    /// genuinely smaller than the box never grows and a plain "ask again"
    /// would ask on every frame for ever.
    pub fn want_pixels(&mut self, key: &MediaKey, want: (u32, u32)) {
        let want = (want.0.max(1), want.1.max(1));
        let frame = self.frame;
        let entry = self.entries.entry(key.clone()).or_insert(Entry {
            state: MediaState::Missing,
            seen: frame,
        });
        entry.seen = frame;
        let loading = matches!(entry.state, MediaState::Loading);
        let asked = self.asked.get(key).copied().unwrap_or_default();
        if want.0 <= asked.w && want.1 <= asked.h {
            return;
        }
        self.asked.insert(
            key.clone(),
            Asked {
                w: want.0.max(asked.w),
                h: want.1.max(asked.h),
                ..asked
            },
        );
        if loading {
            return;
        }
        if let Some(entry) = self.entries.get_mut(key) {
            if matches!(entry.state, MediaState::Missing) {
                entry.state = MediaState::Loading;
            }
        }
        self.requests.push(MediaRequest {
            key: key.clone(),
            want: Want::Decoded {
                max_w: want.0,
                max_h: want.1,
                fill: false,
            },
            priority: MediaPriority::Visible,
            generation: self.generation,
        });
    }

    /// The pixels to draw for a placement that asked to grow, scaled to
    /// `pixels` if the copy already held is not that one.
    ///
    /// The old copy is dropped only once the new one has been allocated, so
    /// the two cannot share an address, and the identity the old one went
    /// under comes back so the caller can make the terminal let go of the
    /// protocol built from it before anything else inherits that address.
    fn scale(
        &mut self,
        key: &MediaKey,
        frame: usize,
        source: &Arc<image::RgbaImage>,
        pixels: (u32, u32),
    ) -> Grown {
        if let Some(held) = self
            .scaled
            .as_ref()
            .filter(|s| s.key == *key && s.frame == frame && s.pixels == pixels)
        {
            return Grown {
                id: held.id,
                image: Arc::clone(&held.image),
                stale: None,
            };
        }
        let started = std::time::Instant::now();
        let image = Arc::new(image::imageops::resize(
            &**source, pixels.0, pixels.1, SMOOTH,
        ));
        tracing::debug!(
            "grew a picture to {}x{} in {:?}",
            pixels.0,
            pixels.1,
            started.elapsed()
        );
        let id = ImageId::of_arc(&image);
        let stale = self
            .scaled
            .replace(Scaled {
                key: key.clone(),
                frame,
                pixels,
                id,
                image: Arc::clone(&image),
            })
            .map(|old| old.id)
            .filter(|old| *old != id);
        Grown { id, image, stale }
    }

    /// Ask for a picture nothing said the size of.
    ///
    /// The renderer draws it as a chip and cannot give it a rectangle, so the
    /// size asked for is a guess at the largest it could usefully be drawn;
    /// what comes back is what settles how many rows it gets on the next
    /// measurement. Marked as wanted this frame, or the sweep at the end of
    /// the frame would cancel it the moment it was asked for.
    pub fn want_measure(&mut self, key: &MediaKey) {
        let frame = self.frame;
        let entry = self.entries.entry(key.clone()).or_insert(Entry {
            state: MediaState::Missing,
            seen: frame,
        });
        entry.seen = frame;
        if matches!(entry.state, MediaState::Missing) {
            entry.state = MediaState::Loading;
            let (max_w, max_h) = (64 * PX_PER_COL, 32 * PX_PER_ROW);
            self.requests.push(MediaRequest {
                key: key.clone(),
                want: Want::Decoded {
                    max_w,
                    max_h,
                    fill: false,
                },
                priority: MediaPriority::Prefetch,
                generation: self.generation,
            });
            // Recorded as not filled, so the slot that places it once it is
            // measured asks for it again at the slot's own size.
            self.asked.entry(key.clone()).or_insert(Asked {
                w: max_w,
                h: max_h,
                filled: None,
            });
        }
    }

    /// Ask for the undecoded bytes of something already on screen.
    ///
    /// For saving a picture to disk: the pixels held here have been resized to
    /// fit a rectangle, and writing those out would hand somebody a thumbnail
    /// of the file they asked for.
    pub fn want_bytes(&mut self, key: &MediaKey) {
        self.requests.push(MediaRequest {
            key: key.clone(),
            want: Want::Bytes,
            priority: MediaPriority::Visible,
            generation: self.generation,
        });
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
    ///
    /// The queues are not cleared here. They are emptied by `take_requests`
    /// and `take_cancels` at the end of every frame, and clearing them again
    /// at the top of the next one throws away anything asked for *between*
    /// frames -- which is when a key is handled, and is how the media viewer's
    /// `s` used to ask for the bytes and never get them.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// This picture is on screen at this size.
    ///
    /// One request per key per frame, and only for a slot nothing has been
    /// asked for: the state goes to `Loading` the moment the request is
    /// recorded, which is what stops a picture being fetched thirty times a
    /// second while it downloads.
    /// The pixels for a slot of this many cells, asking for them if need be.
    ///
    /// Asked to fill the slot, so the picture is the size of the box the
    /// renderer reserved for it. A picture already here that was only ever
    /// measured, or that was fetched for a smaller box than this one -- the
    /// panel widened, the setting went up -- is asked for again at the new
    /// size, and what is here is drawn until the new pixels arrive.
    fn want(&mut self, key: &MediaKey, cols: u16, rows: u16) -> MediaState {
        let frame = self.frame;
        let (max_w, max_h) = (
            u32::from(cols).max(1) * PX_PER_COL,
            u32::from(rows).max(1) * PX_PER_ROW,
        );
        let entry = self.entries.entry(key.clone()).or_insert(Entry {
            state: MediaState::Missing,
            seen: frame,
        });
        entry.seen = frame;
        let asked = self.asked.get(key).copied();
        let ask = match (&entry.state, asked) {
            (MediaState::Missing, _) => {
                entry.state = MediaState::Loading;
                true
            }
            // Here, and the record of what was asked says it was less than
            // this slot: measured rather than placed, or placed smaller.
            (MediaState::Ready(_), Some(asked)) => match asked.filled {
                None => true,
                Some((w, h)) => max_w > w || max_h > h,
            },
            _ => false,
        };
        let asked = asked.unwrap_or_default();
        if ask {
            self.requests.push(MediaRequest {
                key: key.clone(),
                want: Want::Decoded {
                    max_w,
                    max_h,
                    fill: true,
                },
                priority: MediaPriority::Visible,
                generation: self.generation,
            });
            self.asked.insert(
                key.clone(),
                Asked {
                    w: max_w.max(asked.w),
                    h: max_h.max(asked.h),
                    filled: Some((max_w, max_h)),
                },
            );
        }
        entry.state.clone()
    }

    /// Finish a frame: cancel what left the viewport, and bound the pixels.
    ///
    /// The scaled copy goes with them when the picture it was made from is no
    /// longer on screen -- the viewer closed, or moved on to the next
    /// picture. Safe here and nowhere else: the caller has already told the
    /// terminal to forget every protocol this frame did not draw, so nothing
    /// holds an identity that is about to become a freed address.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        if let Some(scaled) = &self.scaled {
            let on_screen = self
                .entries
                .get(&scaled.key)
                .is_some_and(|entry| entry.seen == frame);
            if !on_screen {
                self.scaled = None;
            }
        }
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
        self.asked.clear();
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
    /// How many pixels the rectangle holds, where the terminal has measured a
    /// cell and this placement wants the picture grown to fill it. The media
    /// viewer is the one that does, and it is also what the core is asked for
    /// -- pixels rather than the cell-count estimate, and as they are rather
    /// than filled, since the growing happens here. `None` is every other
    /// placement: the estimate, and whatever the core's own fill made of it.
    pub room: Option<(u32, u32)>,
}

/// What one frame's painting produced, for the caller to act on.
#[derive(Debug, Default)]
pub struct Painted {
    /// Every picture actually put on the screen.
    pub drawn: HashSet<ImageId>,
    /// How many emoji were drawn as pictures, against the cap.
    pub emoji: usize,
    /// Every animation that was on screen, in the order it was drawn, for the
    /// clock that decides which of them may move.
    pub animated: Vec<MediaKey>,
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
    anim: &Animations,
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

        let state = match place.room {
            Some(room) => {
                store.want_pixels(&place.key, room);
                store.state(&place.key)
            }
            None => store.want(&place.key, place.rect.width, place.rect.height),
        };
        let MediaState::Ready(decoded) = &state else {
            fallback(place, rect, buf, theme, &state);
            continue;
        };
        // A picture with frames moves, whatever it was placed as. The poster
        // of a video is one frame and gets the play marker; a gifv's "still"
        // that turns out to be an animated WebP -- which is what Klipy hands
        // Discord -- is the GIF itself, and a marker over it would say the
        // terminal cannot play what it is playing.
        let moves = crate::ui::panels::chat::anim::delays_of(decoded).is_some();
        let frame = if moves { anim.frame_of(&place.key) } else { 0 };
        let Some(img) = frame_at(decoded, frame) else {
            fallback(place, rect, buf, theme, &MediaState::Failed);
            continue;
        };
        if moves {
            out.animated.push(place.key.clone());
        }

        // A picture smaller than the rectangle it was given is grown to fill
        // it, which `Resize::Fit` will not do. The pixels handed over are then
        // this application's own, so they carry their own identity: the frame
        // is part of what a protocol is built from either way, so two frames
        // of one picture are two entries rather than one that flickers.
        let grown = place
            .room
            .and_then(|room| fill_up(img.dimensions(), room))
            .map(|grow| store.scale(&place.key, frame, img, grow.pixels));
        if let Some(stale) = grown.as_ref().and_then(|g| g.stale) {
            graphics.forget(stale);
        }
        let (id, pixels) = match &grown {
            Some(grown) => (grown.id, &grown.image),
            None => (ImageId::of(&(&place.key, frame)), img),
        };
        if draw_one(graphics, id, pixels, rect, clipped, buf) {
            out.drawn.insert(id);
        }
        if matches!(place.shape, Shape::Emoji { .. }) {
            out.emoji += 1;
        }
        if matches!(place.shape, Shape::Play) && !moves {
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

/// The picture to draw for a decoding, at a frame.
///
/// A still ignores the frame; an animation that has been asked for one past
/// its end -- a decode replaced by a shorter one while it was playing -- wraps
/// rather than disappearing.
fn frame_at(decoded: &Decoded, frame: usize) -> Option<&Arc<image::RgbaImage>> {
    match decoded {
        Decoded::Still { image, .. } => Some(image),
        Decoded::Animated { frames, .. } => {
            if frames.is_empty() {
                None
            } else {
                frames.get(frame % frames.len())
            }
        }
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
        Arc::new(Decoded::Still {
            image: Arc::new(image::RgbaImage::new(8, 8)),
            natural: (8, 8),
        })
    }

    /// What a request asked for: the box, and whether it was to be filled.
    fn fill_of(r: &MediaRequest) -> (u32, u32, bool) {
        match r.want {
            Want::Decoded { max_w, max_h, fill } => (max_w, max_h, fill),
            Want::Bytes => panic!("bytes"),
        }
    }

    fn place(key: MediaKey, rect: Rect, clip: Rect) -> Placement {
        Placement {
            rect,
            clip,
            clipped: false,
            key,
            shape: Shape::Picture,
            alt: String::new(),
            room: None,
        }
    }

    /// A request made between frames -- which is when a key is handled -- is
    /// still asked for. The media viewer's `s` is the one that noticed.
    #[test]
    fn something_asked_for_between_frames_survives_the_next_one() {
        let mut store = MediaStore::new();
        store.begin_frame();
        store.end_frame();
        assert!(store.take_requests().is_empty());

        // Between frames: no `begin_frame` has happened since.
        store.want_bytes(&key(3));
        store.begin_frame();
        assert_eq!(
            store.take_requests().len(),
            1,
            "the request was thrown away by the frame after it"
        );
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
        // Drained as the end of a frame drains it, so what is counted below is
        // the second ask and not both of them.
        store.take_requests();
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

    /// A slot asks for pixels that fill it. A picture that was measured
    /// first arrived at its own size, and the slot that then places it asks
    /// again, at the slot's; one that was placed is asked again only when
    /// its box grows.
    #[test]
    fn a_slot_asks_for_its_box_and_a_measured_picture_is_asked_again() {
        let mut store = MediaStore::new();
        store.begin_frame();
        store.want_measure(&key(5));
        let asked = store.take_requests();
        assert_eq!(asked.len(), 1);
        assert!(
            !fill_of(&asked[0]).2,
            "a measurement wants the picture as it is"
        );
        store.arrived(key(5), Ok(still()));

        store.begin_frame();
        assert!(matches!(store.want(&key(5), 20, 6), MediaState::Ready(_)));
        let asked = store.take_requests();
        assert_eq!(asked.len(), 1, "placed after a measurement: asked again");
        assert_eq!(fill_of(&asked[0]), (20 * PX_PER_COL, 6 * PX_PER_ROW, true));
        store.arrived(key(5), Ok(still()));

        store.begin_frame();
        store.want(&key(5), 20, 6);
        assert!(
            store.take_requests().is_empty(),
            "the same box asks nothing"
        );
        store.want(&key(5), 30, 6);
        let asked = store.take_requests();
        assert_eq!(asked.len(), 1, "a wider box asks again");
        assert_eq!(fill_of(&asked[0]), (30 * PX_PER_COL, 6 * PX_PER_ROW, true));
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
        let painted = paint(
            &places,
            &mut graphics,
            &mut store,
            &Animations::default(),
            &t,
            &mut buf,
        );
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
                room: None,
            });
        }
        store.begin_frame();
        let painted = paint(
            &places,
            &mut graphics,
            &mut store,
            &Animations::default(),
            &t,
            &mut buf,
        );
        store.end_frame();
        assert_eq!(
            painted.emoji, EMOJI_PER_FRAME,
            "the cap was not what stopped it"
        );
    }

    /// Growing is one way only: to the box when the picture is smaller than
    /// it, and nothing at all when it already fills it or overflows it --
    /// scaling *down* is what the encoder's own `Fit` is for, and doing it
    /// here would be the same work twice.
    #[test]
    fn a_picture_grows_to_its_box_and_never_shrinks_to_it() {
        // Exactly the box: there is nothing to make.
        assert_eq!(fill_up((800, 600), (800, 600)), None);
        // And near enough is the same answer: a rectangle is a whole number of
        // cells and a picture is not, so this is what filling one looks like.
        assert_eq!(fill_up((1000, 1000), (1020, 1020)), None);
        assert!(
            fill_up((1000, 1000), (1060, 1060)).is_some(),
            "a twentieth is worth the pass"
        );
        // Larger in one dimension is still larger.
        assert_eq!(fill_up((1600, 600), (800, 600)), None);
        assert_eq!(fill_up((1600, 1200), (800, 600)), None);

        // Half the size of the box, and the box has the same shape: twice.
        let grown = fill_up((400, 300), (800, 600)).expect("it is smaller than the box");
        assert_eq!(grown.pixels, (800, 600));
        assert!((grown.factor - 2.0).abs() < 1e-6, "{grown:?}");

        // A box that is not quite the picture's shape grows by the smaller of
        // the two ratios, so the result cannot spill past the rectangle and be
        // shrunk straight back by the encoder.
        let grown = fill_up((100, 100), (500, 240)).expect("it is smaller than the box");
        assert_eq!(grown.pixels, (240, 240));
        assert!((grown.factor - 2.4).abs() < 1e-6, "{grown:?}");

        // Nothing to divide by.
        assert_eq!(fill_up((0, 100), (500, 500)), None);
        assert_eq!(fill_up((100, 100), (0, 500)), None);
    }

    /// The scaled copy is made once and handed out again, and the identity the
    /// last one went under comes back so the terminal can be told to let go of
    /// the protocol built from it.
    #[test]
    fn one_scaled_copy_is_kept_and_the_old_identity_comes_back() {
        let mut store = MediaStore::new();
        let source = Arc::new(image::RgbaImage::new(10, 10));

        let first = store.scale(&key(1), 0, &source, (40, 40));
        assert_eq!(first.image.dimensions(), (40, 40));
        assert!(first.stale.is_none(), "nothing preceded it");

        let again = store.scale(&key(1), 0, &source, (40, 40));
        assert_eq!(again.id, first.id, "it was built a second time");
        assert!(again.stale.is_none());

        // A different box is different pixels, under a different identity, and
        // the one before it is now nothing.
        let bigger = store.scale(&key(1), 0, &source, (80, 80));
        assert_ne!(bigger.id, first.id);
        assert_eq!(bigger.stale, Some(first.id));
        assert_eq!(bigger.image.dimensions(), (80, 80));

        // So is a different picture, and a different frame of one.
        let other = store.scale(&key(2), 0, &source, (80, 80));
        assert_eq!(other.stale, Some(bigger.id));
        let frame = store.scale(&key(2), 1, &source, (80, 80));
        assert_eq!(frame.stale, Some(other.id));
    }

    /// A placement that names its pixels is asked for at those pixels, as they
    /// are rather than filled, and only ever again for more.
    #[test]
    fn a_placement_that_names_pixels_asks_for_them_once() {
        let mut store = MediaStore::new();
        store.begin_frame();
        store.want_pixels(&key(7), (1200, 800));
        let asked = store.take_requests();
        assert_eq!(asked.len(), 1);
        assert_eq!(fill_of(&asked[0]), (1200, 800, false));

        store.arrived(key(7), Ok(still()));
        store.begin_frame();
        store.want_pixels(&key(7), (1200, 800));
        assert!(store.take_requests().is_empty(), "the same box asked again");
        store.want_pixels(&key(7), (1600, 800));
        assert_eq!(store.take_requests().len(), 1, "a wider box asks again");
    }

    /// The grown copy is let go of once the picture it was made from is off
    /// the screen, which is what closing the viewer is.
    #[test]
    fn the_grown_copy_goes_when_the_picture_does() {
        let mut store = MediaStore::new();
        let source = Arc::new(image::RgbaImage::new(10, 10));
        store.begin_frame();
        store.arrived(key(1), Ok(still()));
        store.want_pixels(&key(1), (40, 40));
        store.scale(&key(1), 0, &source, (40, 40));
        store.end_frame();
        assert!(store.scaled.is_some(), "it was on screen this frame");

        store.begin_frame();
        store.end_frame();
        assert!(store.scaled.is_none(), "nothing on screen is it any more");
    }
}
