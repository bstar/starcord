//! Where everything is, worked out once a frame.
//!
//! [`LayoutState::regions`] is the only place in the program that decides a
//! rectangle. It is called at the top of `draw`, its answer is kept in
//! `layout.last`, and `handle_mouse` reads that rather than recomputing
//! anything. Two pieces of code that have to agree about geometry, one of which
//! is only ever exercised by a pointer, is where a layout bug lives; there is
//! one here.
//!
//! ## The tree
//!
//! ```text
//! Body (Horizontal)
//! ├─ Guilds   Fixed(8) rail
//! ├─ Left (Vertical) Fixed(left_cols)
//! │  ├─ Channels Flex(100 - dms_share)
//! │  └─ Dms      Flex(dms_share)
//! ├─ Centre (Vertical) Flex(1)
//! │  ├─ Chat     Flex(1)
//! │  └─ Composer Fixed(rows)
//! └─ Members  Fixed(members_cols)
//! ```
//!
//! The status line is not in the tree. It is one row off the bottom before the
//! dock is asked anything, because it is never hidden, never focused and never
//! resized, and putting it in would mean a second panel id that is not a panel.
//!
//! ## Sizes are the configuration's, not the dock's
//!
//! The tree is rebuilt whenever the numbers it was built from change. That
//! sounds wasteful and is eight allocations; what it buys is that `[layout]`
//! in `config.toml` is the single truth for how wide the columns are, so a
//! seam drag writes a number there and the next solve picks it up, rather than
//! the dock and the file holding two versions of the same fact.
//!
//! ## Narrow terminals
//!
//! The dock cannot express "chat needs forty columns": a leaf's `min` is
//! measured along the axis its parent splits on, and chat's parent splits
//! vertically. So the ladder below does it, by taking panels away before the
//! dock is asked, and the arithmetic is the whole of it — every threshold is
//! the width at which the panels to its left stop fitting at their minimums.
//!
//! `auto_hidden` is kept apart from what the user closed, so widening the
//! terminal brings back the layout they had rather than one the ladder
//! invented.

use std::collections::{BTreeSet, HashMap};

use starkit::dock::{Dock, Node, Size};
use starkit::ratatui::layout::Rect;

use super::panels::{ModuleId, FOCUS_ORDER};
use crate::config;

/// Below this the layout is not drawn at all.
///
/// Sixty by twelve is the smallest size at which a channel list, a message and
/// a composer are all legible at once. Under it the honest answer is to say so
/// rather than to draw six panels two cells wide.
pub const MIN_COLS: u16 = 60;
pub const MIN_ROWS: u16 = 12;

/// The guild rail, which is a strip of two-cell icons and does not resize.
pub const RAIL_COLS: u16 = 8;
/// The narrowest a message is worth wrapping to.
pub const CHAT_MIN_COLS: u16 = 40;
/// The channel and DM column, and how far a drag may take it.
pub const LEFT_MIN_COLS: u16 = 20;
pub const LEFT_MAX_COLS: u16 = 50;
/// The member list, likewise.
pub const MEMBERS_MIN_COLS: u16 = 18;
pub const MEMBERS_MAX_COLS: u16 = 40;
/// Rows the chat and composer need before either is worth drawing.
///
/// Four each, and the same four: two borders, the header row the action words
/// sit on, and one row of content. A panel with no content row is a box with a
/// title.
const CHAT_MIN_ROWS: u16 = 4;
const COMPOSER_MIN_ROWS: u16 = 4;
/// Rows a list in the left column needs: a border, a header and one entry.
const LIST_MIN_ROWS: u16 = 4;

/// What the pointer is in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    /// Index into [`Regions::seams`], and where the pointer was last seen, so
    /// a move can be turned into a delta.
    Seam { seam: usize, x: u16, y: u16 },
    /// The message list's scrollbar. Carries nothing: where the pointer is on
    /// the track is the whole of the answer, so a drag that has wandered off
    /// the column sideways still scrolls.
    Scrollbar,
}

/// One frame's geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct Regions {
    /// The whole of it, padding already taken off.
    pub area: Rect,
    pub panels: HashMap<ModuleId, Rect>,
    pub status: Rect,
    /// The draggable borders, in the order [`Dock::seam_at`] indexes them.
    pub seams: Vec<(usize, Rect)>,
    /// Panels the dock could not give their minimum to. Drawn as an empty
    /// frame rather than as a squeezed one.
    pub too_small: Vec<ModuleId>,
}

impl Regions {
    pub fn rect_of(&self, id: ModuleId) -> Option<Rect> {
        self.panels.get(&id).copied()
    }

    /// Which panel a cell is in.
    pub fn hit(&self, x: u16, y: u16) -> Option<ModuleId> {
        self.panels
            .iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
            .map(|(id, _)| *id)
    }

    /// The panels that were drawn, in tab order.
    pub fn visible(&self) -> Vec<ModuleId> {
        FOCUS_ORDER
            .into_iter()
            .filter(|p| self.panels.contains_key(p))
            .collect()
    }
}

/// What the tree was last built from. Cheap to compare, and comparing it is
/// what keeps the rebuild off the hot path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shape {
    left: u16,
    members: u16,
    dms_share: u16,
    composer_rows: u16,
    hidden: BTreeSet<ModuleId>,
}

pub struct LayoutState {
    dock: Dock<ModuleId>,
    shape: Option<Shape>,
    focus: ModuleId,
    /// Chat and the composer only.
    pub zen: bool,
    /// What the user closed, which survives a resize.
    user_hidden: BTreeSet<ModuleId>,
    /// What the width closed, which does not.
    auto_hidden: BTreeSet<ModuleId>,
    /// The persisted sizes. A seam drag writes here.
    pub cfg: config::Layout,
    pub drag: Option<Drag>,
    pub last: Option<Regions>,
}

impl LayoutState {
    pub fn new(cfg: &config::Layout) -> Self {
        let mut user_hidden = BTreeSet::new();
        if !cfg.show_channels {
            user_hidden.insert(ModuleId::Channels);
        }
        if !cfg.show_dms {
            user_hidden.insert(ModuleId::Dms);
        }
        if !cfg.show_members {
            user_hidden.insert(ModuleId::Members);
        }
        if cfg.guilds == config::GuildsStyle::Hidden {
            user_hidden.insert(ModuleId::Servers);
        }
        Self {
            dock: Dock::new(build(&Shape {
                left: cfg.left_cols,
                members: cfg.members_cols,
                dms_share: cfg.dms_share,
                composer_rows: COMPOSER_MIN_ROWS,
                hidden: BTreeSet::new(),
            })),
            shape: None,
            focus: ModuleId::Conversation,
            zen: cfg.zen,
            user_hidden,
            auto_hidden: BTreeSet::new(),
            cfg: cfg.clone(),
            drag: None,
            last: None,
        }
    }

    /// The whole geometry of one frame, or `None` when the terminal is too
    /// small to draw anything honest in.
    pub fn regions(&mut self, full: Rect, composer_rows: u16, pad: (u16, u16)) -> Option<&Regions> {
        let area = inset(full, pad);
        if area.width < MIN_COLS || area.height < MIN_ROWS {
            self.last = None;
            return None;
        }

        // The status line first, off the bottom, before the dock sees anything.
        let status = Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        };
        let body = Rect {
            height: area.height - 1,
            ..area
        };

        let left = self.ladder(body);
        // The composer takes what it asked for, up to whatever leaves the
        // message list its own minimum. The caller has already held it to
        // `[compose] max_rows`; this is the floor under the panel above it.
        let ceiling = body
            .height
            .saturating_sub(CHAT_MIN_ROWS)
            .max(COMPOSER_MIN_ROWS);
        let composer_rows = composer_rows.clamp(COMPOSER_MIN_ROWS, ceiling);

        let shape = Shape {
            left,
            members: self
                .cfg
                .members_cols
                .clamp(MEMBERS_MIN_COLS, MEMBERS_MAX_COLS),
            dms_share: self.cfg.dms_share.clamp(10, 90),
            composer_rows,
            hidden: self.hidden(),
        };
        if self.shape.as_ref() != Some(&shape) {
            // Focus is this struct's, not the dock's. `Dock::focus_set` opens a
            // panel it is given, which is right for `alt+3` and wrong for a
            // rebuild: handing the dock the focus here would un-hide whatever
            // the ladder had just taken away.
            self.dock = Dock::new(build(&shape));
            for id in &shape.hidden {
                self.dock.hide(*id);
            }
            self.shape = Some(shape);
        }

        let solved = self.dock.layout(body);
        let panels = solved
            .placed
            .iter()
            .map(|p| (p.id, p.area))
            .collect::<HashMap<_, _>>();
        let seams = self
            .dock
            .seams()
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.area))
            .collect();

        self.last = Some(Regions {
            area,
            panels,
            status,
            seams,
            too_small: solved.too_small.clone(),
        });
        self.last.as_ref()
    }

    /// Take panels away until what is left fits, and say how wide the left
    /// column may be.
    ///
    /// Every threshold here is arithmetic rather than a constant: the member
    /// list goes when the rail, the left column, a forty-column message and the
    /// member list stop fitting side by side; the rail goes when the first
    /// three stop fitting; and the left column falls to its minimum when even
    /// it and the message do not. With the defaults those are 98, 74 and 66
    /// columns.
    fn ladder(&mut self, body: Rect) -> u16 {
        self.auto_hidden.clear();
        let mut left = self.cfg.left_cols.clamp(LEFT_MIN_COLS, LEFT_MAX_COLS);
        let members = self
            .cfg
            .members_cols
            .clamp(MEMBERS_MIN_COLS, MEMBERS_MAX_COLS);
        let width = body.width;

        if width < RAIL_COLS + left + CHAT_MIN_COLS + members {
            self.auto_hidden.insert(ModuleId::Members);
        }
        if width < RAIL_COLS + left + CHAT_MIN_COLS {
            self.auto_hidden.insert(ModuleId::Servers);
            // The DM list folds into the channel panel as a tab rather than
            // vanishing: at this width the left column is the only list there
            // is, and losing half of what it can reach would be worse than
            // reaching it with a keystroke.
            self.auto_hidden.insert(ModuleId::Dms);
        }
        if width < left + CHAT_MIN_COLS {
            left = LEFT_MIN_COLS;
        }

        // There is deliberately no height rule. At the twelve-row floor the
        // body is eleven rows, which is two four-row lists with three to
        // spare, so a fold by height could never fire above the floor and a
        // rule that cannot fire is a rule nobody can check.
        let _ = body.height;
        left
    }

    /// Everything that is closed this frame, for whatever reason.
    fn hidden(&self) -> BTreeSet<ModuleId> {
        let mut out = self.user_hidden.clone();
        out.extend(self.auto_hidden.iter().copied());
        if self.zen {
            for p in FOCUS_ORDER {
                if p.closable() {
                    out.insert(p);
                }
            }
        }
        // Chat and the composer are the application; nothing closes them.
        out.retain(|p| p.closable());
        out
    }

    /// Whether the DM list is folded into the channel panel as a tab, which is
    /// what the narrow and short layouts do with it.
    pub fn dms_folded(&self) -> bool {
        self.auto_hidden.contains(&ModuleId::Dms) && !self.user_hidden.contains(&ModuleId::Dms)
    }

    /// Whether the width, rather than the user, closed this panel.
    pub fn auto_hidden(&self, id: ModuleId) -> bool {
        self.auto_hidden.contains(&id)
    }

    pub fn is_open(&self, id: ModuleId) -> bool {
        !self.user_hidden.contains(&id)
    }

    /// Close or open a panel, as a person asking for it rather than as the
    /// ladder.
    pub fn toggle(&mut self, id: ModuleId) {
        if !id.closable() {
            return;
        }
        if self.user_hidden.contains(&id) {
            self.user_hidden.remove(&id);
        } else {
            self.user_hidden.insert(id);
            if self.focus == id {
                self.focus = ModuleId::Conversation;
            }
        }
        self.write_back();
    }

    pub fn show(&mut self, id: ModuleId) {
        if self.user_hidden.remove(&id) {
            self.write_back();
        }
    }

    pub fn set_zen(&mut self, zen: bool) {
        self.zen = zen;
        self.cfg.zen = zen;
        if zen && self.focus.closable() {
            self.focus = ModuleId::Conversation;
        }
    }

    /// Keep `[layout]` in step with what the panels are actually doing, so the
    /// value written to `config.toml` is never a guess.
    fn write_back(&mut self) {
        self.cfg.show_channels = !self.user_hidden.contains(&ModuleId::Channels);
        self.cfg.show_dms = !self.user_hidden.contains(&ModuleId::Dms);
        self.cfg.show_members = !self.user_hidden.contains(&ModuleId::Members);
        if self.user_hidden.contains(&ModuleId::Servers) {
            self.cfg.guilds = config::GuildsStyle::Hidden;
        } else if self.cfg.guilds == config::GuildsStyle::Hidden {
            self.cfg.guilds = config::GuildsStyle::Rail;
        }
    }

    pub fn focus(&self) -> ModuleId {
        self.focus
    }

    /// Focus a panel, opening it if it was closed: `alt+3` should reach the
    /// third panel whether or not it is on screen.
    pub fn focus_set(&mut self, id: ModuleId) {
        if !id.closable() {
            self.focus = id;
            return;
        }
        if self.zen {
            self.set_zen(false);
        }
        self.show(id);
        self.focus = id;
    }

    /// Tab and shift-tab, over what is actually drawn.
    pub fn focus_step(&mut self, forward: bool) {
        let visible = match &self.last {
            Some(r) => r.visible(),
            None => return,
        };
        if visible.is_empty() {
            return;
        }
        let at = visible.iter().position(|&p| p == self.focus);
        let next = match at {
            Some(i) if forward => (i + 1) % visible.len(),
            Some(i) => (i + visible.len() - 1) % visible.len(),
            // Focus is on a panel the ladder took away; land on the first one
            // that is there rather than on nothing.
            None => 0,
        };
        self.focus = visible[next];
    }

    /// Which seam a cell is on, so a press on a border starts a drag rather
    /// than clicking whatever the panel drew there.
    pub fn seam_at(&self, x: u16, y: u16) -> Option<usize> {
        self.dock.seam_at(x, y)
    }

    /// Which way a seam moves, so the pointer's delta is measured along it.
    pub fn seam_axis(&self, seam: usize) -> Option<starkit::dock::Axis> {
        self.dock.seams().get(seam).map(|s| s.axis)
    }

    /// Move a seam, and record where it ended up.
    ///
    /// The recording is the whole of it. This struct rebuilds the dock from
    /// `[layout]` whenever those numbers change, so a drag that only moved the
    /// tree would be undone by the next solve; reading the new extents back
    /// out of the dock and into the config is what makes the drag the thing
    /// that persists. It also means the number written to `config.toml` is
    /// what the panels actually are rather than what the drag intended.
    ///
    /// Returns whether anything the file records changed. The seam between the
    /// message list and the composer is not one of them: the composer sizes
    /// itself from what is written in it, so that seam springs back, and the
    /// alternative is a number that fights the text.
    pub fn drag_seam(&mut self, seam: usize, delta: i16) -> bool {
        if delta == 0 {
            return false;
        }
        self.dock.drag_seam(seam, delta);
        let before = (
            self.cfg.left_cols,
            self.cfg.members_cols,
            self.cfg.dms_share,
        );

        if let Some(r) = self.dock.rect_of(ModuleId::Channels) {
            self.cfg.left_cols = r.width.clamp(LEFT_MIN_COLS, LEFT_MAX_COLS);
        }
        if let Some(r) = self.dock.rect_of(ModuleId::Members) {
            self.cfg.members_cols = r.width.clamp(MEMBERS_MIN_COLS, MEMBERS_MAX_COLS);
        }
        if let (Some(channels), Some(dms)) = (
            self.dock.rect_of(ModuleId::Channels),
            self.dock.rect_of(ModuleId::Dms),
        ) {
            let total = u32::from(channels.height) + u32::from(dms.height);
            if let Some(share) = (u32::from(dms.height) * 100).checked_div(total) {
                self.cfg.dms_share = share.clamp(10, 90) as u16;
            }
        }

        // Keep the remembered shape in step, or the next frame rebuilds the
        // tree from the old numbers and the drag is undone between frames.
        if let Some(shape) = &mut self.shape {
            shape.left = self.cfg.left_cols;
            shape.members = self.cfg.members_cols;
            shape.dms_share = self.cfg.dms_share;
        }

        before
            != (
                self.cfg.left_cols,
                self.cfg.members_cols,
                self.cfg.dms_share,
            )
    }
}

/// The tree, from the numbers it is built out of.
fn build(s: &Shape) -> Node<ModuleId> {
    // Weights rather than percentages: `Flex` is a share of what the fixed
    // children left, and two weights summing to a hundred is the same
    // arithmetic written in a way a reader can check against `dms_share`.
    let dms = s.dms_share.max(1);
    let channels = 100u16.saturating_sub(dms).max(1);

    Node::row(
        Size::Fill,
        vec![
            Node::leaf(ModuleId::Servers, Size::Fixed(RAIL_COLS), RAIL_COLS),
            Node::column(
                Size::Fixed(s.left),
                vec![
                    Node::leaf(ModuleId::Channels, Size::Flex(channels), LIST_MIN_ROWS),
                    Node::leaf(ModuleId::Dms, Size::Flex(dms), LIST_MIN_ROWS),
                ],
            ),
            Node::column(
                Size::Flex(1),
                vec![
                    Node::leaf(ModuleId::Conversation, Size::Flex(1), CHAT_MIN_ROWS),
                    Node::leaf(
                        ModuleId::Compose,
                        Size::Fixed(s.composer_rows),
                        COMPOSER_MIN_ROWS,
                    ),
                ],
            ),
            Node::leaf(ModuleId::Members, Size::Fixed(s.members), MEMBERS_MIN_COLS),
        ],
    )
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

    fn state() -> LayoutState {
        LayoutState::new(&config::Layout::default())
    }

    fn at(width: u16, height: u16) -> (LayoutState, Option<Regions>) {
        let mut s = state();
        let r = s
            .regions(Rect::new(0, 0, width, height), 3, (0, 0))
            .cloned();
        (s, r)
    }

    fn visible(width: u16) -> Vec<ModuleId> {
        let (_, r) = at(width, 30);
        r.map(|r| r.visible()).unwrap_or_default()
    }

    /// The property the whole module rests on: the panels cover the body
    /// exactly. A gap is a cell nothing redraws, which keeps whatever was
    /// there last; an overlap is two panels writing the same cell in an order
    /// that depends on the tree.
    #[test]
    fn the_panels_tile_the_body_with_no_gaps_and_no_overlaps() {
        for width in [59u16, 60, 61, 65, 66, 73, 74, 97, 98, 140, 200] {
            for height in [11u16, 12, 13, 24, 30, 60] {
                let (_, regions) = at(width, height);
                let Some(r) = regions else {
                    assert!(
                        width < MIN_COLS || height < MIN_ROWS,
                        "{width}x{height} refused to lay out"
                    );
                    continue;
                };

                let body = Rect {
                    height: r.area.height - 1,
                    ..r.area
                };
                let mut seen = vec![0u8; usize::from(body.width) * usize::from(body.height)];
                for (id, rect) in &r.panels {
                    for y in rect.y..rect.y + rect.height {
                        for x in rect.x..rect.x + rect.width {
                            let i = usize::from(y - body.y) * usize::from(body.width)
                                + usize::from(x - body.x);
                            assert_eq!(seen[i], 0, "{width}x{height}: {id:?} overlaps at {x},{y}");
                            seen[i] = 1;
                        }
                    }
                }
                assert!(
                    seen.iter().all(|&c| c == 1),
                    "{width}x{height}: {} cells nothing covered",
                    seen.iter().filter(|&&c| c == 0).count()
                );
                assert_eq!(r.status.height, 1);
                assert_eq!(r.status.y, r.area.y + r.area.height - 1);
            }
        }
    }

    /// The ladder, at every width it turns on.
    ///
    /// 98 is a rail, a twenty-six-column list, a forty-column message and a
    /// twenty-four-column member list. 74 is the same without the members. 66
    /// is the same without the rail. 60 is the same with the list at its
    /// minimum. 59 is nothing at all.
    #[test]
    fn the_degradation_ladder_is_its_arithmetic() {
        use ModuleId::*;

        assert_eq!(visible(59), Vec::<ModuleId>::new(), "too small to draw");
        assert_eq!(visible(60), vec![Channels, Conversation, Compose]);
        assert_eq!(visible(65), vec![Channels, Conversation, Compose]);
        assert_eq!(visible(66), vec![Channels, Conversation, Compose]);
        assert_eq!(visible(73), vec![Channels, Conversation, Compose]);
        assert_eq!(
            visible(74),
            vec![Servers, Channels, Dms, Conversation, Compose]
        );
        assert_eq!(
            visible(97),
            vec![Servers, Channels, Dms, Conversation, Compose]
        );
        assert_eq!(
            visible(98),
            vec![Servers, Channels, Dms, Conversation, Compose, Members]
        );
        assert_eq!(
            visible(140),
            vec![Servers, Channels, Dms, Conversation, Compose, Members]
        );
    }

    /// The left column narrows only when it has to, and the message never goes
    /// below the width it is wrapped to.
    #[test]
    fn the_message_panel_keeps_its_minimum_width() {
        for width in MIN_COLS..=160 {
            let (_, r) = at(width, 30);
            let r = r.expect("wide enough");
            let chat = r
                .rect_of(ModuleId::Conversation)
                .expect("chat is never closed");
            assert!(
                chat.width >= CHAT_MIN_COLS,
                "at {width} columns the message panel is {} wide",
                chat.width
            );
            assert!(
                r.too_small.is_empty(),
                "at {width} columns the dock could not place {:?}",
                r.too_small
            );
        }
    }

    /// Below the floor there is no layout at all, and the caller draws one
    /// line saying so.
    #[test]
    fn a_terminal_below_the_floor_gets_nothing() {
        for (w, h) in [(59u16, 30u16), (60, 11), (40, 8), (0, 0)] {
            let (mut s, r) = at(w, h);
            assert!(r.is_none(), "{w}x{h} laid out");
            assert!(s.last.is_none());
            // And the next frame at a workable size recovers.
            assert!(s.regions(Rect::new(0, 0, 100, 30), 3, (0, 0)).is_some());
        }
    }

    /// Padding comes off the outside and the floor is measured after it, so a
    /// padded eighty-column terminal is a seventy-six-column layout.
    #[test]
    fn padding_is_taken_before_the_floor_is_measured() {
        let mut s = state();
        let r = s
            .regions(Rect::new(0, 0, 64, 20), 3, (2, 1))
            .cloned()
            .expect("60 columns left");
        assert_eq!(r.area, Rect::new(2, 1, 60, 18));

        let mut s = state();
        assert!(
            s.regions(Rect::new(0, 0, 63, 20), 3, (2, 1)).is_none(),
            "59 columns after padding is below the floor"
        );
    }

    /// What the user closed survives a resize; what the width closed does not.
    /// Otherwise a moment at eighty columns would permanently lose the member
    /// list.
    #[test]
    fn widening_restores_what_the_width_took_and_not_what_the_user_did() {
        let mut s = state();
        s.toggle(ModuleId::Channels);
        s.regions(Rect::new(0, 0, 80, 30), 3, (0, 0));
        assert!(s.auto_hidden(ModuleId::Members));
        assert!(!s.is_open(ModuleId::Channels));

        let r = s
            .regions(Rect::new(0, 0, 140, 30), 3, (0, 0))
            .cloned()
            .unwrap();
        assert!(
            r.panels.contains_key(&ModuleId::Members),
            "the member list did not come back"
        );
        assert!(
            !r.panels.contains_key(&ModuleId::Channels),
            "the channel list came back and nobody asked it to"
        );
    }

    /// Zen is chat, composer and the status line.
    #[test]
    fn zen_leaves_the_conversation_and_nothing_else() {
        let mut s = state();
        s.set_zen(true);
        let r = s
            .regions(Rect::new(0, 0, 140, 30), 3, (0, 0))
            .cloned()
            .unwrap();
        assert_eq!(r.visible(), vec![ModuleId::Conversation, ModuleId::Compose]);
        s.set_zen(false);
        let r = s
            .regions(Rect::new(0, 0, 140, 30), 3, (0, 0))
            .cloned()
            .unwrap();
        assert_eq!(r.visible().len(), 6);
    }

    /// `[layout]` round-trips: what the panels are doing is what the file says.
    #[test]
    fn the_layout_table_round_trips() {
        let mut s = state();
        s.toggle(ModuleId::Members);
        s.toggle(ModuleId::Dms);
        s.set_zen(true);
        s.cfg.left_cols = 34;

        let text = toml::to_string(&s.cfg).unwrap();
        let back: config::Layout = toml::from_str(&text).unwrap();
        assert_eq!(back, s.cfg);

        let restored = LayoutState::new(&back);
        assert!(!restored.is_open(ModuleId::Members));
        assert!(!restored.is_open(ModuleId::Dms));
        assert!(restored.zen);
        assert_eq!(restored.cfg.left_cols, 34);

        // And closing the rail is spelled as a style rather than as a flag,
        // because that is the key the file has.
        let mut s = state();
        s.toggle(ModuleId::Servers);
        assert_eq!(s.cfg.guilds, config::GuildsStyle::Hidden);
        s.toggle(ModuleId::Servers);
        assert_eq!(s.cfg.guilds, config::GuildsStyle::Rail);
    }

    /// Focus walks what is drawn, and wraps. A panel the ladder took away is
    /// not a place tab can land.
    #[test]
    fn focus_walks_only_the_visible_panels() {
        let mut s = state();
        s.regions(Rect::new(0, 0, 80, 30), 3, (0, 0));
        assert!(s.auto_hidden(ModuleId::Members));

        s.focus_set(ModuleId::Conversation);
        let mut seen = vec![s.focus()];
        for _ in 0..5 {
            s.focus_step(true);
            seen.push(s.focus());
        }
        assert!(
            !seen.contains(&ModuleId::Members),
            "tab reached a panel that is not on screen: {seen:?}"
        );
        assert_eq!(seen[0], seen[5], "five steps over five panels should wrap");
    }

    /// Focusing a closed panel opens it, because `alt+6` means "the member
    /// list" and not "the member list if you left it open".
    #[test]
    fn focusing_a_closed_panel_opens_it() {
        let mut s = state();
        s.toggle(ModuleId::Members);
        assert!(!s.is_open(ModuleId::Members));
        s.focus_set(ModuleId::Members);
        assert!(s.is_open(ModuleId::Members));
        assert_eq!(s.focus(), ModuleId::Members);

        // And it leaves zen, which would otherwise hide it again on the next
        // frame and leave focus pointing at nothing.
        s.set_zen(true);
        s.focus_set(ModuleId::Channels);
        assert!(!s.zen);
    }

    /// Closing the focused panel moves focus somewhere that exists.
    #[test]
    fn closing_the_focused_panel_lands_on_the_chat() {
        let mut s = state();
        s.focus_set(ModuleId::Dms);
        s.toggle(ModuleId::Dms);
        assert_eq!(s.focus(), ModuleId::Conversation);
    }

    /// Seams are reported even before anything can drag them, because the
    /// pointer has to be able to tell a border from a row underneath it.
    #[test]
    fn the_seams_are_reported() {
        let (s, r) = at(140, 30);
        let r = r.unwrap();
        assert!(
            !r.seams.is_empty(),
            "a four-column body has borders between its columns"
        );
        for (i, rect) in &r.seams {
            // The middle of the seam rather than its corner: two seams meet at
            // a corner, and which one owns that cell is not the question being
            // asked here.
            let x = rect.x + rect.width / 2;
            let y = rect.y + rect.height / 2;
            assert_eq!(
                s.seam_at(x, y),
                Some(*i),
                "seam {i} is not where it says it is"
            );
        }
    }

    /// The composer never grows into the message list.
    #[test]
    fn the_composer_cannot_eat_the_chat() {
        let mut s = state();
        let r = s
            .regions(Rect::new(0, 0, 120, 20), 40, (0, 0))
            .cloned()
            .unwrap();
        let chat = r.rect_of(ModuleId::Conversation).unwrap();
        assert!(chat.height >= CHAT_MIN_ROWS, "chat is {} rows", chat.height);
    }

    /// The DM list folds into the channel panel rather than being closed, so
    /// the tab is still there and the user's own choice is untouched.
    #[test]
    fn a_narrow_terminal_folds_the_message_list_rather_than_closing_it() {
        let (s, r) = at(70, 30);
        let r = r.unwrap();
        assert!(!r.panels.contains_key(&ModuleId::Dms));
        assert!(s.dms_folded(), "it folded rather than closed");
        assert!(
            s.is_open(ModuleId::Dms),
            "and the user's choice is untouched"
        );

        // Closed by hand at the same width is closed, not folded: the channel
        // panel should not grow a tab for a list somebody put away.
        let mut s = state();
        s.toggle(ModuleId::Dms);
        s.regions(Rect::new(0, 0, 140, 30), 3, (0, 0));
        assert!(!s.dms_folded());
    }

    /// At the shortest terminal the layout draws in, both lists still fit.
    /// Written down because it is the reason there is no fold-by-height rule.
    #[test]
    fn both_lists_fit_at_the_row_floor() {
        let (_, r) = at(140, MIN_ROWS);
        let r = r.unwrap();
        assert!(r.panels.contains_key(&ModuleId::Dms));
        assert!(r.panels.contains_key(&ModuleId::Channels));
    }

    #[test]
    fn hit_testing_answers_the_panel_that_was_drawn() {
        let (_, r) = at(140, 30);
        let r = r.unwrap();
        for (id, rect) in &r.panels {
            assert_eq!(r.hit(rect.x, rect.y), Some(*id));
            assert_eq!(
                r.hit(rect.x + rect.width - 1, rect.y + rect.height - 1),
                Some(*id)
            );
        }
        assert_eq!(
            r.hit(r.status.x, r.status.y),
            None,
            "the status is not a panel"
        );
    }
}
