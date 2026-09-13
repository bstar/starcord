//! Animated pictures, and the clock that moves them.
//!
//! A GIF arrives from the core already decoded into whole frames with a delay
//! each. What is left is the policy question — *which* of them may move, and
//! how often the loop has to wake up to move them — and that is the whole of
//! this module.
//!
//! ## Only what is on screen, and only four of it
//!
//! Every advanced frame is a picture re-encoded and handed to the terminal.
//! One is cheap; a scrollback of them is a core. So two bounds hold, and both
//! are arithmetic rather than judgement:
//!
//! - a picture that was not drawn last frame does not advance, because
//!   [`Animations::set_visible`] is fed from the drawing pass and nothing else;
//! - past [`CAP`] of them, the rest show frame zero. Which four is the order
//!   they were drawn in, which is top to bottom, so the one somebody is looking
//!   at is the one that moves.
//!
//! ## The floor, and the wake-up
//!
//! The core already refuses to believe a delay below twenty milliseconds. This
//! puts a second floor at [`MIN_DELAY`], because a terminal redrawing a picture
//! protocol cannot keep up with fifty frames a second across four pictures and
//! the honest thing is to play it slower rather than to fall behind.
//!
//! [`Animations::next_due`] is what the event loop's poll timeout is clamped
//! to. Without it the frame rate would be the ceiling on the animation and a
//! hundred-millisecond frame would arrive up to thirty-three milliseconds late,
//! every time, which is visible as a limp.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::config::Animate;
use crate::discord::media::{Decoded, MediaKey};

/// How many pictures may move at once.
pub const CAP: usize = 4;

/// The shortest a frame is shown, whatever the file asks for.
pub const MIN_DELAY: Duration = Duration::from_millis(50);

/// Where one picture is in its loop.
#[derive(Debug, Clone, Copy)]
struct Play {
    frame: usize,
    /// When the frame after this one is due.
    due: Instant,
}

/// Every animation on screen, and whether any of them moved.
#[derive(Debug, Default)]
pub struct Animations {
    playing: HashMap<MediaKey, Play>,
    /// What the drawing pass saw last frame, in the order it drew it.
    visible: Vec<MediaKey>,
    policy: Animate,
}

impl Animations {
    pub fn new(policy: Animate) -> Self {
        Self {
            playing: HashMap::new(),
            visible: Vec::new(),
            policy,
        }
    }

    pub fn set_policy(&mut self, policy: Animate) {
        if self.policy != policy {
            self.policy = policy;
            self.playing.clear();
        }
    }

    /// What the drawing pass put on the screen, in the order it drew it.
    pub fn set_visible(&mut self, keys: Vec<MediaKey>) {
        self.visible = keys;
    }

    /// What the last drawing pass saw, so a second pass can add to it rather
    /// than replace it.
    pub fn visible(&self) -> &[MediaKey] {
        &self.visible
    }

    /// Which frame to draw for a picture. Anything unknown shows its first.
    pub fn frame_of(&self, key: &MediaKey) -> usize {
        self.playing.get(key).map(|p| p.frame).unwrap_or(0)
    }

    /// Whether anything at all is moving, for the loop's poll timeout.
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        self.playing
            .values()
            .map(|p| p.due.saturating_duration_since(now))
            .min()
    }

    /// Advance whatever is due, and say whether the screen has to be redrawn.
    ///
    /// `delays` answers "how long does frame *n* of this picture stay up", and
    /// is the one thing the caller has that this does not: the pixels live in
    /// the media store and this module deliberately does not reach into it.
    pub fn tick(
        &mut self,
        now: Instant,
        focused: bool,
        delays: impl Fn(&MediaKey) -> Option<Vec<Duration>>,
    ) -> bool {
        if self.policy == Animate::Never || (self.policy == Animate::Focused && !focused) {
            let was = !self.playing.is_empty();
            self.playing.clear();
            return was;
        }

        let mut moved = false;
        let mut kept: HashMap<MediaKey, Play> = HashMap::new();
        for key in self.visible.iter().take(CAP) {
            let Some(delays) = delays(key) else { continue };
            if delays.len() < 2 {
                continue;
            }
            let play = match self.playing.get(key) {
                Some(play) => {
                    let mut play = *play;
                    // One frame per tick at most. Catching up on a loop that
                    // fell behind -- a suspended terminal, a slow frame --
                    // would be a burst of encodings nobody sees.
                    if play.due <= now {
                        play.frame = (play.frame + 1) % delays.len();
                        play.due = now + floor(delays[play.frame]);
                        moved = true;
                    }
                    play
                }
                None => {
                    moved = true;
                    Play {
                        frame: 0,
                        due: now + floor(delays[0]),
                    }
                }
            };
            kept.insert(key.clone(), play);
        }
        // Anything that scrolled away, or fell past the cap, is forgotten and
        // starts again at its first frame when it comes back. A GIF's phase is
        // not worth a map that grows for the length of a session.
        moved |= kept.len() != self.playing.len();
        self.playing = kept;
        moved
    }
}

fn floor(delay: Duration) -> Duration {
    delay.max(MIN_DELAY)
}

/// The delays of a decoded picture, when it is one that moves.
pub fn delays_of(decoded: &Decoded) -> Option<Vec<Duration>> {
    match decoded {
        Decoded::Animated { frames, delays, .. } if frames.len() > 1 => Some(delays.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u64) -> MediaKey {
        MediaKey::Gif {
            url: format!("https://example.invalid/{n}.gif"),
        }
    }

    /// Every picture in the test has the same three frames.
    fn three(_key: &MediaKey) -> Option<Vec<Duration>> {
        Some(vec![Duration::from_millis(100); 3])
    }

    #[test]
    fn a_picture_that_is_not_on_screen_never_advances() {
        let mut a = Animations::new(Animate::Always);
        let now = Instant::now();
        a.set_visible(vec![key(1)]);
        a.tick(now, true, three);
        assert_eq!(a.frame_of(&key(1)), 0);
        assert_eq!(a.frame_of(&key(2)), 0);

        let later = now + Duration::from_millis(150);
        a.tick(later, true, three);
        assert_eq!(a.frame_of(&key(1)), 1, "the visible one moved");
        assert_eq!(a.frame_of(&key(2)), 0, "and the other one did not exist");
    }

    /// Past the cap the rest show their first frame, which is what the drawing
    /// pass asks for when it is told nothing.
    #[test]
    fn only_four_pictures_move_at_once() {
        let mut a = Animations::new(Animate::Always);
        let now = Instant::now();
        let keys: Vec<MediaKey> = (0..8).map(key).collect();
        a.set_visible(keys.clone());
        a.tick(now, true, three);
        let later = now + Duration::from_millis(150);
        a.tick(later, true, three);

        for k in keys.iter().take(CAP) {
            assert_eq!(a.frame_of(k), 1, "{k:?} should be playing");
        }
        for k in keys.iter().skip(CAP) {
            assert_eq!(a.frame_of(k), 0, "{k:?} is past the cap");
        }
    }

    /// A file asking for five milliseconds gets fifty.
    #[test]
    fn a_delay_is_floored() {
        let mut a = Animations::new(Animate::Always);
        let now = Instant::now();
        a.set_visible(vec![key(1)]);
        a.tick(now, true, |_| Some(vec![Duration::from_millis(5); 4]));
        assert_eq!(a.next_due(now), Some(MIN_DELAY));

        // And forty milliseconds later nothing is due yet.
        let soon = now + Duration::from_millis(40);
        assert!(!a.tick(soon, true, |_| Some(vec![Duration::from_millis(5); 4])));
        assert_eq!(a.frame_of(&key(1)), 0);
    }

    /// The loop's timeout: the earliest thing that needs redrawing.
    #[test]
    fn the_next_frame_due_is_the_soonest_of_them() {
        let mut a = Animations::new(Animate::Always);
        let now = Instant::now();
        a.set_visible(vec![key(1), key(2)]);
        a.tick(now, true, |k| {
            if *k == key(1) {
                Some(vec![Duration::from_millis(500); 3])
            } else {
                Some(vec![Duration::from_millis(80); 3])
            }
        });
        assert_eq!(a.next_due(now), Some(Duration::from_millis(80)));
        assert_eq!(Animations::default().next_due(now), None);
    }

    /// `never` stops everything, and `focused` stops it while the window is
    /// behind another one.
    #[test]
    fn the_policy_decides_whether_anything_moves_at_all() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);

        let mut never = Animations::new(Animate::Never);
        never.set_visible(vec![key(1)]);
        never.tick(now, true, three);
        never.tick(later, true, three);
        assert_eq!(never.frame_of(&key(1)), 0);
        assert_eq!(never.next_due(now), None);

        let mut focused = Animations::new(Animate::Focused);
        focused.set_visible(vec![key(1)]);
        focused.tick(now, false, three);
        focused.tick(later, false, three);
        assert_eq!(focused.frame_of(&key(1)), 0, "nobody is looking");
        focused.tick(now, true, three);
        focused.tick(later, true, three);
        assert_eq!(focused.frame_of(&key(1)), 1);

        let mut always = Animations::new(Animate::Always);
        always.set_visible(vec![key(1)]);
        always.tick(now, false, three);
        always.tick(later, false, three);
        assert_eq!(always.frame_of(&key(1)), 1, "always means always");
    }

    /// A still picture is never given a slot, so it costs nothing to have one
    /// on screen.
    #[test]
    fn a_still_picture_is_not_an_animation() {
        let mut a = Animations::new(Animate::Always);
        a.set_visible(vec![key(1)]);
        assert!(!a.tick(Instant::now(), true, |_| None));
        assert_eq!(a.next_due(Instant::now()), None);

        // Nor is a one-frame GIF, which is what a truncated decode leaves.
        let mut one = Animations::new(Animate::Always);
        one.set_visible(vec![key(1)]);
        assert!(!one.tick(Instant::now(), true, |_| Some(vec![MIN_DELAY])));
        assert_eq!(one.next_due(Instant::now()), None);
    }

    #[test]
    fn the_helper_only_answers_for_something_that_moves() {
        let still = Decoded::Still(std::sync::Arc::new(image::RgbaImage::new(2, 2)));
        assert!(delays_of(&still).is_none());
        let moving = Decoded::Animated {
            frames: vec![
                std::sync::Arc::new(image::RgbaImage::new(2, 2)),
                std::sync::Arc::new(image::RgbaImage::new(2, 2)),
            ],
            delays: vec![Duration::from_millis(60); 2],
            looped: true,
        };
        assert_eq!(delays_of(&moving).map(|d| d.len()), Some(2));
    }
}
