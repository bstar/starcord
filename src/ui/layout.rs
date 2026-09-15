//! Where everything is, worked out once a frame.
//!
//! [`LayoutState::regions`] is the only place in the program that decides a
//! rectangle. It is called at the top of `draw`, its answer is kept in
//! `layout.last`, and `handle_mouse` reads that rather than recomputing
//! anything. Two pieces of code that have to agree about geometry, one of which
//! is only ever exercised by a pointer, is where a layout bug lives; there is
//! one here.
//!
//! ## One column
//!
//! ```text
//! SERVERS        a list, folds to one row      (or MESSAGES at home)
//! CHANNELS       a list, folds to one row
//! CONVERSATION   takes the slack
//! COMPOSE        as tall as what is written in it
//! MEMBERS        a list, folds to one row
//! status         one row, never anything else
//! ```
//!
//! Every module is always there, in the order you drill through them, and the
//! arithmetic below is the whole of it: a plain top-down stack, no tree and no
//! `Layout`. There is nothing to hide, nothing to drag and nothing to write
//! back to `config.toml`, which is three states the window used to have and
//! does not any more.
//!
//! ## The accordion
//!
//! At most one of the three lists is expanded. Focusing a list opens it and
//! folds whichever other one was open; a folded list is [`COLLAPSED_ROWS`]
//! tall and draws one line saying what is currently selected in it. That is
//! the whole of the invariant, and it is why the height arithmetic only ever
//! has to account for one open list.
//!
//! ## Short terminals
//!
//! There is no degradation ladder any more. Five modules at their folded
//! heights, plus a four-row conversation, a four-row composer and the status
//! line, is [`MIN_ROWS`]; below that, or below [`MIN_COLS`], the caller draws
//! one line saying so. Above the floor the slack goes to the open list first,
//! up to `[ui] list_rows`, then to the composer, and the conversation keeps
//! its minimum throughout.

use starkit::chrome::header;
use starkit::ratatui::layout::Rect;

use super::panels::{ModuleId, COLUMN};

/// Below this the layout is not drawn at all.
///
/// Sixty columns is the narrowest a message is worth wrapping to with a name
/// beside it. Twenty-one rows is arithmetic rather than judgement: it is the
/// sum of the constants below, which is what makes it the height at which
/// every module still has a row of content.
pub const MIN_COLS: u16 = 60;

/// A module with nothing open in it: two borders, the header row the action
/// words sit on, and one row of content. A module with no content row is a box
/// with a title.
pub const COLLAPSED_ROWS: u16 = 2 + header::ROWS + 1;

/// Rows the conversation and the composer need before either is worth drawing.
const CHAT_MIN_ROWS: u16 = 4;
const COMPOSER_MIN_ROWS: u16 = 4;

pub const MIN_ROWS: u16 = 3 * COLLAPSED_ROWS + CHAT_MIN_ROWS + COMPOSER_MIN_ROWS + 1;

/// One frame's geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct Regions {
    /// The whole of it, padding already taken off.
    pub area: Rect,
    /// One rect per module, indexed by [`ModuleId::index`], in [`COLUMN`]
    /// order. Every one of them is the full width of the area.
    modules: [Rect; 5],
    pub status: Rect,
}

impl Regions {
    pub fn rect_of(&self, m: ModuleId) -> Rect {
        self.modules[m.index()]
    }

    /// Which module a cell is in.
    pub fn hit(&self, x: u16, y: u16) -> Option<ModuleId> {
        COLUMN.into_iter().find(|m| {
            let r = self.rect_of(*m);
            x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
        })
    }
}

/// Which module has the keyboard, and which list is open.
pub struct LayoutState {
    focus: ModuleId,
    /// The one list that is expanded, if any. The accordion is this field
    /// being an `Option` rather than a set: there is no state in which two
    /// lists are open, so there is no code that has to close the other one.
    expanded: Option<ModuleId>,
    /// `[ui] list_rows`: the tallest an open list may grow to.
    pub list_rows: u16,
    pub last: Option<Regions>,
}

impl LayoutState {
    pub fn new(list_rows: u16) -> Self {
        Self {
            focus: ModuleId::Servers,
            expanded: Some(ModuleId::Servers),
            list_rows,
            last: None,
        }
    }

    /// The whole geometry of one frame, or `None` when the terminal is too
    /// small to draw anything honest in.
    ///
    /// `wanted` is how many body rows the open list would like. The app
    /// measures it, because what is in a list is the app's business and how
    /// tall it is allowed to be is this module's.
    pub fn regions(
        &mut self,
        full: Rect,
        composer_rows: u16,
        pad: (u16, u16),
        wanted: u16,
    ) -> Option<&Regions> {
        let area = inset(full, pad);
        if area.width < MIN_COLS || area.height < MIN_ROWS {
            self.last = None;
            return None;
        }

        // The status line first, off the bottom, because it is never hidden,
        // never focused and never resized.
        let status = Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        };
        let body = Rect {
            height: area.height - 1,
            ..area
        };

        // Three folded lists is the floor everything else is measured from.
        // `room` is what is left once the conversation has its minimum, and
        // the floor above is what guarantees it is at least four.
        let folded = 3 * COLLAPSED_ROWS;
        let room = body.height - folded - CHAT_MIN_ROWS;
        let compose = composer_rows.clamp(COMPOSER_MIN_ROWS, room.max(COMPOSER_MIN_ROWS));
        // The open list takes what it asked for, up to `[ui] list_rows` and up
        // to what the composer left. One row of it is already in the folded
        // height, so this is only the extra.
        let extra = match self.expanded {
            Some(_) => (wanted.clamp(1, self.list_rows.max(1)) - 1).min(room - compose),
            None => 0,
        };
        let conversation = body.height - folded - compose - extra;

        let height = |m: ModuleId| match m {
            ModuleId::Conversation => conversation,
            ModuleId::Compose => compose,
            _ if self.expanded == Some(m) => COLLAPSED_ROWS + extra,
            _ => COLLAPSED_ROWS,
        };

        let mut modules = [Rect::new(0, 0, 0, 0); 5];
        let mut y = body.y;
        for m in COLUMN {
            let h = height(m);
            modules[m.index()] = Rect {
                y,
                height: h,
                ..body
            };
            y += h;
        }

        self.last = Some(Regions {
            area,
            modules,
            status,
        });
        self.last.as_ref()
    }

    pub fn focus(&self) -> ModuleId {
        self.focus
    }

    pub fn expanded(&self) -> Option<ModuleId> {
        self.expanded
    }

    pub fn is_expanded(&self, m: ModuleId) -> bool {
        self.expanded == Some(m)
    }

    /// Focus a module, opening it if it is a list.
    ///
    /// A list you are choosing in is a list you can see, so focusing one opens
    /// it and the accordion folds whichever other one was open. The
    /// conversation and the composer leave the lists exactly where they were:
    /// `alt+4` means "write a message", not "put the window away".
    pub fn focus_set(&mut self, m: ModuleId) {
        if m.is_list() {
            self.expanded = Some(m);
        }
        self.focus = m;
    }

    /// Fold the open list, landing focus on `land` if it was in it.
    ///
    /// `land` is the conversation or the composer. Landing on another list
    /// would be a fold that opens something, which is not a fold.
    pub fn collapse(&mut self, land: ModuleId) {
        if let Some(open) = self.expanded.take() {
            if self.focus == open {
                self.focus = land;
            }
        }
    }

    /// Open a list, or fold it again if it is already open.
    pub fn toggle(&mut self, m: ModuleId, land: ModuleId) {
        if self.is_expanded(m) {
            self.collapse(land);
        } else {
            self.focus_set(m);
        }
    }
}

/// Columns a module has, before anything has been laid out.
///
/// The composer has to say how tall it wants to be before `regions` can decide
/// anything, and how tall it is depends on how wide it is. Every module is the
/// full width of the area, so that answer needs no layout: it is the terminal
/// with the padding taken off.
pub fn content_width(full: Rect, pad: (u16, u16)) -> u16 {
    inset(full, pad).width
}

/// Shrink a rect by the configured padding, never past nothing.
fn inset(area: Rect, pad: (u16, u16)) -> Rect {
    let (x, y) = pad;
    let width = area.width.saturating_sub(x.saturating_mul(2));
    let height = area.height.saturating_sub(y.saturating_mul(2));
    if width == 0 || height == 0 {
        return Rect {
            width: 0,
            height: 0,
            ..area
        };
    }
    Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn state() -> LayoutState {
        LayoutState::new(8)
    }

    fn at(width: u16, height: u16) -> (LayoutState, Option<Regions>) {
        let mut s = state();
        let r = s
            .regions(Rect::new(0, 0, width, height), 3, (0, 0), 3)
            .cloned();
        (s, r)
    }

    /// The property the whole module rests on: the modules cover the body
    /// exactly, top to bottom, each of them the full width. A gap is a cell
    /// nothing redraws, which keeps whatever was there last; an overlap is two
    /// modules writing the same cell in an order nobody chose.
    #[test]
    fn the_modules_tile_the_body_from_top_to_bottom() {
        for width in [59u16, 60, 61, 100, 200] {
            for height in [20u16, 21, 22, 30, 60] {
                for expanded in [
                    None,
                    Some(ModuleId::Servers),
                    Some(ModuleId::Channels),
                    Some(ModuleId::Members),
                ] {
                    for composer_rows in [3u16, 12] {
                        for wanted in [0u16, 3, 50] {
                            let mut s = state();
                            s.expanded = expanded;
                            let full = Rect::new(0, 0, width, height);
                            let Some(r) = s.regions(full, composer_rows, (0, 0), wanted).cloned()
                            else {
                                assert!(
                                    width < MIN_COLS || height < MIN_ROWS,
                                    "{width}x{height} refused to lay out"
                                );
                                continue;
                            };

                            let mut y = r.area.y;
                            for m in COLUMN {
                                let rect = r.rect_of(m);
                                assert_eq!(rect.x, r.area.x, "{m:?} at {width}x{height}");
                                assert_eq!(rect.width, r.area.width, "{m:?} at {width}x{height}");
                                assert_eq!(rect.y, y, "{m:?} at {width}x{height} is not stacked");
                                assert!(rect.height >= COLLAPSED_ROWS, "{m:?} is {rect:?}");
                                y += rect.height;
                            }
                            assert_eq!(y, r.status.y, "the stack does not reach the status line");
                            assert_eq!(r.status.height, 1);
                            assert_eq!(r.status.y, r.area.y + r.area.height - 1);
                        }
                    }
                }
            }
        }
    }

    /// Below the floor there is no layout at all, and the caller draws one
    /// line saying so.
    #[test]
    fn a_terminal_below_the_floor_gets_nothing() {
        for (w, h) in [(59u16, 30u16), (60, 20), (40, 8), (0, 0)] {
            let (mut s, r) = at(w, h);
            assert!(r.is_none(), "{w}x{h} laid out");
            assert!(s.last.is_none());
            // And the next frame at a workable size recovers.
            assert!(s.regions(Rect::new(0, 0, 100, 30), 3, (0, 0), 3).is_some());
        }
    }

    /// Padding comes off the outside and the floor is measured after it, so a
    /// padded sixty-four-column terminal is a sixty-column layout.
    #[test]
    fn padding_is_taken_before_the_floor_is_measured() {
        let mut s = state();
        let r = s
            .regions(Rect::new(0, 0, 64, 23), 3, (2, 1), 3)
            .cloned()
            .expect("60x21 left");
        assert_eq!(r.area, Rect::new(2, 1, 60, 21));
        assert_eq!(content_width(Rect::new(0, 0, 64, 23), (2, 1)), 60);

        let mut s = state();
        assert!(
            s.regions(Rect::new(0, 0, 63, 23), 3, (2, 1), 3).is_none(),
            "59 columns after padding is below the floor"
        );
    }

    /// What the slack is spent on, in order: the open list first, up to
    /// `list_rows`, then the composer, and the conversation keeps its minimum
    /// throughout.
    #[test]
    fn the_column_shrinks_the_open_list_first_then_the_composer() {
        // width, height, rows the composer asked for, extra rows the list got
        let cases = [
            (100u16, 30u16, 3u16, 7u16),
            (100, 24, 3, 3),
            (100, 24, 10, 0),
            (100, 21, 3, 0),
        ];
        for (w, h, rows, extra) in cases {
            let mut s = state();
            s.expanded = Some(ModuleId::Channels);
            let r = s
                .regions(Rect::new(0, 0, w, h), rows, (0, 0), 50)
                .cloned()
                .expect("above the floor");
            assert_eq!(
                r.rect_of(ModuleId::Channels).height,
                COLLAPSED_ROWS + extra,
                "{w}x{h} with a {rows}-row composer"
            );
        }

        // A composer taller than the room left is held to the room rather than
        // refused, and takes it before the conversation gives anything up.
        let mut s = state();
        s.expanded = Some(ModuleId::Channels);
        let r = s
            .regions(Rect::new(0, 0, 100, 24), 10, (0, 0), 50)
            .cloned()
            .unwrap();
        assert_eq!(r.rect_of(ModuleId::Compose).height, 7);
        assert_eq!(r.rect_of(ModuleId::Conversation).height, CHAT_MIN_ROWS);
    }

    /// Whatever anybody asks for, the conversation keeps four rows.
    #[test]
    fn the_conversation_never_drops_below_its_minimum() {
        for height in MIN_ROWS..=60 {
            for composer_rows in [3u16, 10, 40] {
                for wanted in [0u16, 5, 200] {
                    let mut s = state();
                    s.expanded = Some(ModuleId::Members);
                    let r = s
                        .regions(Rect::new(0, 0, 100, height), composer_rows, (0, 0), wanted)
                        .cloned()
                        .unwrap();
                    assert!(
                        r.rect_of(ModuleId::Conversation).height >= CHAT_MIN_ROWS,
                        "{height} rows, composer {composer_rows}, wanted {wanted}"
                    );
                }
            }
        }
    }

    /// The composer never grows into the conversation, and an open member list
    /// cannot take back what the composer already has.
    #[test]
    fn the_composer_cannot_eat_the_chat() {
        let mut s = state();
        s.collapse(ModuleId::Conversation);
        let r = s
            .regions(Rect::new(0, 0, 120, 24), 40, (0, 0), 0)
            .cloned()
            .unwrap();
        assert!(r.rect_of(ModuleId::Conversation).height >= CHAT_MIN_ROWS);

        s.focus_set(ModuleId::Members);
        let r = s
            .regions(Rect::new(0, 0, 120, 24), 40, (0, 0), 50)
            .cloned()
            .unwrap();
        assert!(r.rect_of(ModuleId::Conversation).height >= CHAT_MIN_ROWS);
        assert_eq!(
            r.rect_of(ModuleId::Members).height,
            COLLAPSED_ROWS,
            "the member list took room the composer already had"
        );
    }

    /// At the floor every module is exactly its folded height, open or not.
    #[test]
    fn at_the_floor_every_module_is_its_folded_height() {
        let mut s = state();
        let r = s
            .regions(Rect::new(0, 0, MIN_COLS, MIN_ROWS), 3, (0, 0), 50)
            .cloned()
            .unwrap();
        for m in COLUMN {
            assert_eq!(r.rect_of(m).height, COLLAPSED_ROWS, "{m:?}");
        }
        assert_eq!(COLLAPSED_ROWS, 4);
    }

    /// An open list is as tall as it has rows, and no taller than `list_rows`.
    #[test]
    fn an_open_list_is_capped_by_list_rows_and_by_its_length() {
        for (wanted, want_extra) in [(0u16, 0u16), (1, 0), (3, 2), (8, 7), (50, 7)] {
            let mut s = state();
            let r = s
                .regions(Rect::new(0, 0, 100, 40), 3, (0, 0), wanted)
                .cloned()
                .unwrap();
            assert_eq!(
                r.rect_of(ModuleId::Servers).height,
                COLLAPSED_ROWS + want_extra,
                "a list of {wanted} rows"
            );
        }

        // And a smaller ceiling is a smaller list.
        let mut s = LayoutState::new(3);
        let r = s
            .regions(Rect::new(0, 0, 100, 40), 3, (0, 0), 50)
            .cloned()
            .unwrap();
        assert_eq!(r.rect_of(ModuleId::Servers).height, COLLAPSED_ROWS + 2);
    }

    /// Focusing a list opens it, and the one that was open folds.
    #[test]
    fn focusing_a_list_opens_it_and_folds_the_other() {
        let mut s = state();
        assert!(s.is_expanded(ModuleId::Servers));
        s.focus_set(ModuleId::Channels);
        assert!(s.is_expanded(ModuleId::Channels));
        assert!(!s.is_expanded(ModuleId::Servers));
        assert_eq!(s.focus(), ModuleId::Channels);

        // The conversation and the composer leave the lists alone.
        s.focus_set(ModuleId::Compose);
        assert!(s.is_expanded(ModuleId::Channels));
        assert_eq!(s.focus(), ModuleId::Compose);
    }

    /// Folding the list that has focus lands the focus where the caller says.
    #[test]
    fn folding_the_focused_list_lands_where_it_is_told() {
        let mut s = state();
        s.focus_set(ModuleId::Members);
        s.collapse(ModuleId::Compose);
        assert_eq!(s.expanded(), None);
        assert_eq!(s.focus(), ModuleId::Compose);

        // Folding a list that does not have focus leaves focus where it is.
        s.focus_set(ModuleId::Channels);
        s.focus_set(ModuleId::Conversation);
        s.collapse(ModuleId::Compose);
        assert_eq!(s.expanded(), None);
        assert_eq!(s.focus(), ModuleId::Conversation);
    }

    /// `alt+m`, both ways.
    #[test]
    fn toggling_a_list_opens_it_and_folds_it_again() {
        let mut s = state();
        s.toggle(ModuleId::Members, ModuleId::Compose);
        assert!(s.is_expanded(ModuleId::Members));
        s.toggle(ModuleId::Members, ModuleId::Compose);
        assert_eq!(s.expanded(), None);
        assert_eq!(s.focus(), ModuleId::Compose);
    }

    #[test]
    fn hit_testing_answers_the_module_that_was_drawn() {
        let (_, r) = at(100, 30);
        let r = r.unwrap();
        for m in COLUMN {
            let rect = r.rect_of(m);
            assert_eq!(r.hit(rect.x, rect.y), Some(m));
            assert_eq!(
                r.hit(rect.x + rect.width - 1, rect.y + rect.height - 1),
                Some(m)
            );
        }
        assert_eq!(
            r.hit(r.status.x, r.status.y),
            None,
            "the status is not a module"
        );
    }

    /// What the accordion is, stated once: whatever order anything is asked
    /// for in, at most one list is open, only a list is ever open, and a
    /// focused list is the open one.
    ///
    /// The landing module is one of the two that are not lists, which is what
    /// `collapse` is for: folding a list to land on another list would be a
    /// fold that opens something.
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Focus(usize),
        Collapse(bool),
        Toggle(usize, bool),
    }

    fn land(compose: bool) -> ModuleId {
        if compose {
            ModuleId::Compose
        } else {
            ModuleId::Conversation
        }
    }

    proptest! {
        #[test]
        fn at_most_one_list_is_open(ops in proptest::collection::vec(
            prop_oneof![
                (0usize..5).prop_map(Op::Focus),
                proptest::bool::ANY.prop_map(Op::Collapse),
                (0usize..5, proptest::bool::ANY).prop_map(|(i, c)| Op::Toggle(i, c)),
            ],
            0..40,
        )) {
            let mut s = state();
            for op in ops {
                match op {
                    Op::Focus(i) => s.focus_set(COLUMN[i]),
                    Op::Collapse(c) => s.collapse(land(c)),
                    Op::Toggle(i, c) => s.toggle(COLUMN[i], land(c)),
                }
                let open = COLUMN.into_iter().filter(|m| s.is_expanded(*m)).count();
                prop_assert!(open <= 1, "{open} lists are open");
                if let Some(m) = s.expanded() {
                    prop_assert!(m.is_list(), "{m:?} is not a list and is open");
                }
                if s.focus().is_list() {
                    prop_assert_eq!(
                        s.expanded(),
                        Some(s.focus()),
                        "the focused list is folded"
                    );
                }
            }
        }
    }
}
