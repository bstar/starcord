//! The message list.
//!
//! Everything a conversation needs to be read: grouping, wrapping, day and
//! unread dividers, reactions, embed cards, the typing line, virtual scrolling
//! from the bottom, and the cache that makes all of it cost nothing on the
//! frames where nothing changed.
//!
//! ## Anchored at the end, not at the top
//!
//! A chat list grows at the end, so a scroll offset counted from the top moves
//! under the reader every time somebody says something. The position is an
//! anchor instead — stuck to the end, or "row `n` of item `i` at the top" —
//! which is [`starkit::vlist::VirtualList`], and it is why scrolling back
//! through history does not jump when a page of older messages is prepended
//! above.
//!
//! ## Heights come out of the cache, and only out of the cache
//!
//! The one thing that has to be true for the scrolling to work is that the
//! height used to place a message is the height it is drawn at. So there is
//! one function that produces both — [`render`](render::render) — one cache in
//! front of it keyed by everything that would change its answer, and one pass
//! per frame that fills [`ChatState::heights`] before the list is asked
//! anything.
//!
//! ## Pictures are placed here and drawn later
//!
//! The rows a picture takes are reserved from its declared size, before any
//! bytes have arrived, so nothing reflows when they do. What this pass
//! produces for each one is a [`media::Placement`]: the absolute rectangle and
//! the panel to cut it to. The drawing itself is a separate pass over every
//! panel's placements at the end of the frame — see [`media`] for why.

pub mod anim;
pub mod layout;
pub mod media;
pub mod render;

use std::collections::HashMap;
use std::sync::Arc;

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::text::Span;
use starkit::vlist::VirtualList;
use starkit::wrap::width_of;

use self::anim::Animations;
use self::layout::{Row, Shape};
use self::media::{MediaStore, Placement};
use self::render::{Cache, Key, Names, RenderCtx, Rendered, Revealed, SlotKind};
use super::{empty, fit, rgb};
use crate::config::Config;
use crate::discord::media::MediaKey;
use crate::discord::model::{Message, PartialEmoji};
use crate::discord::snowflake::{ChannelId, MessageId};
use crate::discord::state::messages::PendingState;
use crate::discord::state::State;
use crate::ui::theme::Theme;

/// How many messages are copied out of the store for one channel.
///
/// The store holds five hundred; this is what the panel measures. A window
/// larger than anything a reader will scroll through in one sitting, and small
/// enough that filling the heights each frame is a few hundred hash lookups.
const WINDOW: usize = 500;

/// The scrollbar, and the marks the dividers are drawn with.
const SCROLLBAR: &str = "\u{2590}";
const DIVIDER: &str = "\u{2500}";

/// Where the cursor and the view were when a channel was last left.
#[derive(Debug, Clone, Copy, Default)]
struct Memory {
    anchor: Option<MessageId>,
    cursor: Option<MessageId>,
}

/// Something on screen a click can land on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Message(MessageId),
    Link(String),
    Reaction(MessageId, PartialEmoji),
    /// The `↩` line: jump to what was quoted.
    Reply(MessageId),
    Attachment(MessageId, String),
    LoadOlder,
    /// The bar down the right border: a click or a drag scrolls to that
    /// fraction of the conversation.
    Scrollbar,
}

/// The whole of the message panel's state.
pub struct ChatState {
    pub cache: Cache,
    pub revealed: Revealed,
    pub names: Names,
    /// Which pictures belong to which messages, so that one arriving
    /// invalidates the measurements of the messages that draw it and nothing
    /// else. A channel of photographs used to re-measure every message in the
    /// window for each one that landed.
    media_owners: HashMap<MediaKey, Vec<MessageId>>,
    /// Per message, bumped when a picture that message owns arrives. Part of
    /// the cache key, which is the whole point of it.
    gens: HashMap<MessageId, u64>,
    /// Every animation on screen, and the clock that moves them.
    pub anim: Animations,
    /// What is known about every picture on screen.
    pub media: MediaStore,
    /// Where this frame's pictures go. Filled while drawing, drained by the
    /// caller afterwards: the panel knows the rectangles and the application
    /// owns the terminal's graphics.
    slots: Vec<Placement>,

    channel: Option<ChannelId>,
    memory: HashMap<ChannelId, Memory>,

    messages: Vec<Arc<Message>>,
    pending: Vec<PendingRow>,
    /// How far each in-flight upload has got, from `Event::UploadProgress`.
    /// The store knows a message is uploading and not how much of it has gone,
    /// because the counter moves far too often to be state.
    uploads: HashMap<crate::discord::handle::Nonce, (u64, u64)>,
    rows: Vec<Row>,
    heights: Vec<u16>,
    rendered: Vec<Option<Arc<Rendered>>>,
    /// Row index the cursor is on. Always a selectable row when there is one.
    cursor: usize,
    /// Whether the cursor follows the newest message. True while the view is
    /// at the end and nobody has moved it, which is what puts `r` and `e` on
    /// the thing that just arrived rather than on the top of the scrollback.
    follow_end: bool,
    list: VirtualList,

    typing: Vec<String>,
    has_older: bool,
    loading_older: bool,
    /// Whether a fetch has already been asked for at this top.
    asked_older: bool,
    first_unread: Option<MessageId>,
    newer_hidden: usize,
    /// Messages below the viewport as of the last frame.
    below: usize,

    /// The width the heights were measured at, so a resize clears the cache.
    width: u16,
    /// Where each row landed last frame, for the pointer.
    hits: Vec<(Rect, Hit)>,
    body: Option<Rect>,
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatState {
    pub fn new() -> Self {
        Self {
            cache: Cache::default(),
            revealed: Revealed::default(),
            names: Names::default(),
            media_owners: HashMap::new(),
            gens: HashMap::new(),
            anim: Animations::default(),
            media: MediaStore::new(),
            slots: Vec::new(),
            channel: None,
            memory: HashMap::new(),
            messages: Vec::new(),
            pending: Vec::new(),
            uploads: HashMap::new(),
            rows: Vec::new(),
            heights: Vec::new(),
            rendered: Vec::new(),
            cursor: 0,
            follow_end: true,
            list: VirtualList::new(),
            typing: Vec::new(),
            has_older: false,
            loading_older: false,
            asked_older: false,
            first_unread: None,
            newer_hidden: 0,
            below: 0,
            width: 0,
            hits: Vec::new(),
            body: None,
        }
    }

    pub fn channel(&self) -> Option<ChannelId> {
        self.channel
    }

    /// Messages the reader has scrolled past, so the status line can say
    /// `↓ 3 new` and mean something they can act on.
    ///
    /// Two sources, and the larger wins. What is below the viewport is what
    /// they would see by pressing `G`; `newer_hidden` is what the store
    /// refused to append because the view had already left the end, and is
    /// not on screen at all.
    pub fn new_below(&self) -> usize {
        if self.list.is_at_end() {
            0
        } else {
            self.below.max(self.newer_hidden)
        }
    }

    pub fn at_bottom(&self) -> bool {
        self.list.is_at_end()
    }

    /// The newest message, which is what is acknowledged when marking read.
    pub fn newest(&self) -> Option<MessageId> {
        self.messages.last().map(|m| m.id)
    }

    pub fn selected(&self) -> Option<Arc<Message>> {
        self.rows.get(self.cursor)?.message(&self.messages)
    }

    /// How far one message's files have got.
    pub fn upload_progress(
        &mut self,
        nonce: crate::discord::handle::Nonce,
        sent: u64,
        total: u64,
    ) {
        self.uploads.insert(nonce, (sent, total));
    }

    /// Whether a reaction chip on a message is already this account's.
    ///
    /// The chip carries the answer and the click has to know it: adding a
    /// reaction that is already there is a request Discord answers with
    /// nothing, so the chip would have looked stuck.
    pub fn selected_reaction_is_mine(
        &self,
        message: MessageId,
        emoji: &crate::discord::handle::EmojiRef,
    ) -> Option<bool> {
        let msg = self.messages.iter().find(|m| m.id == message)?;
        let key = emoji.key();
        Some(
            msg.reactions
                .iter()
                .any(|r| r.emoji.reaction_key() == key && r.me),
        )
    }

    /// The thread started from the message under the cursor, if there is one.
    pub fn selected_thread(&self) -> Option<ChannelId> {
        self.rendered.get(self.cursor)?.as_ref()?.thread
    }

    /// Every picture on screen that has a thread of its own hanging off it is
    /// not a thing; this is the other half of the arrival path.
    ///
    /// A picture arrived. Only the messages that draw it are measured again:
    /// the generation is per message and is part of the cache key, so a
    /// channel full of photographs no longer re-measures all five hundred of
    /// them for every one that lands.
    pub fn media_arrived(&mut self, key: &MediaKey) {
        let Some(owners) = self.media_owners.get(key) else {
            return;
        };
        for owner in owners.clone() {
            let gen = self.gens.entry(owner).or_insert(0);
            *gen = gen.wrapping_add(1);
        }
    }

    /// How far down the conversation the view is, 0.0 at the top.
    pub fn scrolled(&self) -> f32 {
        if self.heights.len() != self.rows.len() || self.rows.is_empty() {
            return 0.0;
        }
        let Some(body) = self.body else { return 0.0 };
        let total: u32 = self.heights.iter().map(|h| u32::from(*h)).sum();
        let room = total.saturating_sub(u32::from(body.height));
        if room == 0 {
            return 0.0;
        }
        let get = |i: usize| self.heights.get(i).copied().unwrap_or(0);
        let visible = self.list.visible(body, get, self.rows.len());
        let first = visible.first().map(|v| v.index).unwrap_or(0);
        let skip = visible.first().map(|v| v.skip).unwrap_or(0);
        let above: u32 = self.heights.iter().take(first).map(|h| u32::from(*h)).sum::<u32>()
            + u32::from(skip);
        (above as f32 / room as f32).clamp(0.0, 1.0)
    }

    /// Put the view that far down the conversation. For a scrollbar drag.
    pub fn scroll_to_fraction(&mut self, fraction: f32) {
        if self.heights.len() != self.rows.len() || self.rows.is_empty() {
            return;
        }
        let Some(body) = self.body else { return };
        let total: u32 = self.heights.iter().map(|h| u32::from(*h)).sum();
        let room = total.saturating_sub(u32::from(body.height));
        if room == 0 {
            return;
        }
        let want = (fraction.clamp(0.0, 1.0) * room as f32) as u32;
        if want >= room {
            self.to_bottom();
            return;
        }
        let mut acc = 0u32;
        let mut index = 0usize;
        for (i, h) in self.heights.iter().enumerate() {
            if acc + u32::from(*h) > want {
                index = i;
                break;
            }
            acc += u32::from(*h);
            index = i;
        }
        self.list.scroll_to(index);
        self.follow_end = false;
        self.media.viewport_moved();
    }

    /// What the reader can see, for the session file: the message at the top
    /// of the viewport, or `None` when the view is following the end.
    pub fn anchor(&self) -> Option<MessageId> {
        if self.list.is_at_end() {
            return None;
        }
        self.rows
            .get(self.list.anchor())
            .and_then(|r| r.message(&self.messages))
            .map(|m| m.id)
    }

    /// Leave one channel and arrive in another, keeping where each was left.
    pub fn open(&mut self, channel: ChannelId) {
        self.remember();
        if self.channel == Some(channel) {
            return;
        }
        self.channel = Some(channel);
        // Another conversation's pictures: whatever is queued for it is work
        // nobody is waiting for any more, and the generation is what says so.
        self.media.viewport_moved();
        self.messages.clear();
        self.rows.clear();
        self.heights.clear();
        self.rendered.clear();
        self.cursor = 0;
        self.follow_end = true;
        self.asked_older = false;
        let memory = self.memory.get(&channel).copied().unwrap_or_default();
        self.list = VirtualList::new();
        if memory.anchor.is_some() {
            // Resolved once the messages are in; until then the end is the
            // honest place to be.
            self.list.to_end();
        }
    }

    fn remember(&mut self) {
        let Some(channel) = self.channel else { return };
        let memory = Memory {
            anchor: self.anchor(),
            cursor: self.selected().map(|m| m.id),
        };
        self.memory.insert(channel, memory);
    }

    /// The anchor to hand the core, so `session.toml` can restore it.
    pub fn saved_anchor(&self, channel: ChannelId) -> Option<MessageId> {
        if self.channel == Some(channel) {
            return self.anchor();
        }
        self.memory.get(&channel).and_then(|m| m.anchor)
    }

    /// Take the window, the pending sends and the typing list out of `State`.
    ///
    /// Copied rather than borrowed, and once per change rather than once per
    /// frame: a read guard held across a draw stalls the gateway, which is
    /// applying dispatches under the write lock.
    pub fn refresh(&mut self, state: &State, cfg: &Config, tz: &jiff::tz::TimeZone) {
        let Some(channel) = self.channel else {
            self.messages.clear();
            self.rows.clear();
            return;
        };

        let store = state.messages(channel);
        self.messages = store.map(|s| s.latest(WINDOW)).unwrap_or_default();
        self.has_older = store.is_some_and(|s| s.has_older());
        self.loading_older = store.is_some_and(|s| s.loading());
        self.newer_hidden = store.map(|s| s.newer_hidden()).unwrap_or(0);
        self.pending = store
            .map(|s| {
                s.pending()
                    .iter()
                    .map(|p| PendingRow {
                        nonce: p.nonce,
                        content: p.content.clone(),
                        failed: matches!(p.state, PendingState::Failed(_)),
                        uploading: matches!(p.state, PendingState::Uploading { .. }),
                        files: p.attachments.len(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Progress for sends that have finished is progress nobody will draw.
        let live: std::collections::HashSet<crate::discord::handle::Nonce> =
            self.pending.iter().map(|p| p.nonce).collect();
        self.uploads.retain(|nonce, _| live.contains(nonce));

        let guild = state.channel(channel).and_then(|c| c.guild_id);
        self.typing = state
            .typing(channel)
            .into_iter()
            .filter(|id| Some(*id) != state.me().map(|u| u.id))
            .map(|id| state.display_name(guild, id))
            .collect();

        self.first_unread = first_unread(state, channel, &self.messages);
        self.names = names_for(state, channel, &self.messages);

        self.rows = layout::rows(&Shape {
            messages: &self.messages,
            pending: self.pending.len(),
            group_window_secs: cfg.chat.group_window_secs,
            has_older: self.has_older || self.loading_older,
            first_unread: self.first_unread,
            typing: !self.typing.is_empty(),
            tz: tz.clone(),
        });
        if self.follow_end {
            self.cursor = self.rows.len().saturating_sub(1);
        }
        self.clamp_cursor();
        self.restore_anchor();
    }

    /// Put the view back where it was when this channel was last open, once
    /// there are messages for the anchor to name.
    fn restore_anchor(&mut self) {
        let Some(channel) = self.channel else { return };
        let Some(memory) = self.memory.get(&channel).copied() else {
            return;
        };
        let Some(want) = memory.anchor else { return };
        if !self.list.is_at_end() || self.messages.is_empty() {
            return;
        }
        if let Some(row) = self.row_of(want) {
            self.list.scroll_to(row);
            self.follow_end = false;
            // The cursor goes back where it was as well, if that message is
            // still held; the anchor alone would put the view in the right
            // place with `r` and `e` pointing at the top of it.
            self.cursor = memory.cursor.and_then(|id| self.row_of(id)).unwrap_or(row);
            self.clamp_cursor();
            let entry = self.memory.entry(channel).or_default();
            entry.anchor = None;
            entry.cursor = None;
        }
    }

    fn row_of(&self, message: MessageId) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.message(&self.messages).is_some_and(|m| m.id == message))
    }

    fn clamp_cursor(&mut self) {
        if self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        if self.rows.get(self.cursor).is_some_and(Row::selectable) {
            return;
        }
        // Land on the nearest message rather than on a divider, searching
        // backwards first because the cursor usually arrived from below.
        let from = self.cursor.min(self.rows.len() - 1);
        for delta in 0..self.rows.len() {
            if let Some(i) = from.checked_sub(delta) {
                if self.rows[i].selectable() {
                    self.cursor = i;
                    return;
                }
            }
            if from + delta < self.rows.len() && self.rows[from + delta].selectable() {
                self.cursor = from + delta;
                return;
            }
        }
        self.cursor = 0;
    }

    // -- moving ------------------------------------------------------------

    /// `j` and `k`: message-wise, with the view following.
    pub fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let mut at = self.cursor as isize;
        let step = if delta >= 0 { 1 } else { -1 };
        let mut left = delta.abs();
        while left > 0 {
            let next = at + step;
            if next < 0 || next as usize >= self.rows.len() {
                break;
            }
            at = next;
            if self.rows[at as usize].selectable() {
                left -= 1;
            }
        }
        self.cursor = at.clamp(0, self.rows.len() as isize - 1) as usize;
        self.follow_end = self.cursor + 1 >= self.rows.len();
        self.clamp_cursor();
        self.follow_cursor();
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn to_top(&mut self) {
        self.cursor = 0;
        self.follow_end = false;
        self.clamp_cursor();
        self.list.scroll_to(0);
    }

    /// `G` and `End`: follow the conversation again.
    ///
    /// `to_*` on a `&mut self` is what clippy calls an ambiguous name and it
    /// is the name every list in the program uses for this; renaming it to
    /// `go_to_bottom` would make one list spell it differently from the rest.
    #[allow(clippy::wrong_self_convention)]
    pub fn to_bottom(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
        self.follow_end = true;
        self.clamp_cursor();
        self.list.to_end();
    }

    /// Keep the cursor's row inside the viewport.
    fn follow_cursor(&mut self) {
        let Some(body) = self.body else { return };
        if self.heights.len() != self.rows.len() {
            return;
        }
        let heights = |i: usize| self.heights.get(i).copied().unwrap_or(0);
        let visible = self.list.visible(body, heights, self.rows.len());
        let first = visible.first().map(|v| v.index).unwrap_or(0);
        let last = visible.last().map(|v| v.index).unwrap_or(0);
        if self.cursor < first {
            self.list.scroll_to(self.cursor);
        } else if self.cursor > last || self.cursor == self.rows.len() - 1 {
            if self.cursor + 1 >= self.rows.len() {
                self.list.to_end();
            } else {
                // Enough rows above it that the cursor lands at the bottom.
                let mut acc = 0u32;
                let mut top = self.cursor;
                while top > 0 {
                    let h = u32::from(heights(top));
                    if acc + h >= u32::from(body.height) {
                        break;
                    }
                    acc += h;
                    top -= 1;
                }
                self.list.scroll_to(top);
            }
        }
    }

    /// The wheel, and page keys.
    pub fn scroll(&mut self, rows: i32) {
        let Some(body) = self.body else { return };
        if self.heights.len() != self.rows.len() {
            return;
        }
        let heights = |i: usize| self.heights.get(i).copied().unwrap_or(0);
        self.list.scroll_rows(rows, body, heights, self.rows.len());
        self.media.viewport_moved();
        self.follow_end = self.list.is_at_end();
        if rows < 0 {
            self.asked_older = false;
        }
    }

    /// Whether the top of the list is on screen and there is more above it.
    ///
    /// Answered once per arrival at the top: a fetch asked for on every frame
    /// while sitting at the top is a request every thirty-three milliseconds.
    pub fn wants_older(&mut self) -> bool {
        if !self.has_older || self.loading_older || self.asked_older {
            return false;
        }
        let Some(body) = self.body else { return false };
        if self.heights.len() != self.rows.len() {
            return false;
        }
        let heights = |i: usize| self.heights.get(i).copied().unwrap_or(0);
        let visible = self.list.visible(body, heights, self.rows.len());
        let at_top = visible.first().is_some_and(|v| v.index == 0);
        if at_top {
            self.asked_older = true;
        }
        at_top
    }

    /// Older messages arrived above the view; keep the reader where they were.
    ///
    /// The anchor is a row index and `count` rows were inserted in front of
    /// it, so the same message is at the top afterwards and the screen does
    /// not move.
    pub fn prepended(&mut self, count: usize) {
        self.asked_older = false;
        if self.list.is_at_end() || count == 0 {
            return;
        }
        self.list.prepended(count);
        self.follow_end = false;
        self.cursor += count;
    }

    /// `space`: uncover the spoiler the cursor is on, or cover it again.
    ///
    /// One message's worth. The revealed set is keyed by message and ordinal,
    /// so uncovering one spoiler cannot change what another message looks
    /// like — which is the property the cache key exists to preserve.
    pub fn reveal(&mut self) -> bool {
        let Some(msg) = self.selected() else {
            return false;
        };
        let Some(rendered) = self.rendered.get(self.cursor).and_then(|r| r.clone()) else {
            return false;
        };
        let mut changed = false;
        let indices: Vec<u16> = {
            let mut v: Vec<u16> = rendered.spoilers.iter().map(|s| s.index).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        for index in indices {
            if !self.revealed.insert((msg.id, index)) {
                self.revealed.remove(&(msg.id, index));
            }
            changed = true;
        }
        if changed {
            self.cache.forget(msg.id);
        }
        changed
    }

    /// What a click landed on.
    ///
    /// The small targets first. A message's own box covers every row it took,
    /// so testing in the order things were recorded would mean a link inside a
    /// message could never be clicked: the message is always underneath it and
    /// always matches.
    pub fn hit(&self, x: u16, y: u16) -> Option<Hit> {
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        self.hits
            .iter()
            .find(|(r, hit)| !matches!(hit, Hit::Message(_)) && inside(r))
            .or_else(|| self.hits.iter().find(|(r, _)| inside(r)))
            .map(|(_, hit)| hit.clone())
    }

    /// Put the cursor on a message, by id.
    pub fn select(&mut self, message: MessageId) {
        if let Some(row) = self.row_of(message) {
            self.cursor = row;
            self.follow_end = row + 1 >= self.rows.len();
            self.follow_cursor();
        }
    }

    /// Jump to the message the one under the cursor was replying to.
    pub fn jump_to_reply(&mut self) -> Option<MessageId> {
        let msg = self.selected()?;
        let target = msg.reply_target()?;
        if self.row_of(target).is_some() {
            self.select(target);
        }
        Some(target)
    }

    // -- drawing -----------------------------------------------------------

    /// Measure everything the frame might draw, before the list is asked
    /// where anything goes.
    fn prepare(&mut self, body: Rect, v: &Params<'_>) {
        if self.width != body.width {
            self.cache.clear();
            self.width = body.width;
        }
        self.body = Some(body);

        let theme_hash = hash_of(&v.theme.id);
        let threads_hash = hash_of(&{
            let mut ids: Vec<u64> = self.names.threads.keys().map(|m| m.0).collect();
            ids.sort_unstable();
            ids
        });
        let revealed_hash = hash_of(&{
            let mut ids: Vec<(u64, u16)> = self.revealed.iter().map(|(m, i)| (m.0, *i)).collect();
            ids.sort_unstable();
            ids
        });

        let width = body.width.saturating_sub(1).max(1);
        self.heights = Vec::with_capacity(self.rows.len());
        self.rendered = Vec::with_capacity(self.rows.len());
        // Rebuilt rather than accumulated: a message that left the window
        // takes its claim on a picture with it, and the whole window is
        // measured here, so this is the complete answer every frame.
        let mut owners: HashMap<MediaKey, Vec<MessageId>> = HashMap::new();
        let mut wanted: Vec<MediaKey> = Vec::new();

        for row in 0..self.rows.len() {
            match &self.rows[row] {
                Row::Message { index, head } => {
                    let (index, head) = (*index, *head);
                    let Some(msg) = self.messages.get(index).cloned() else {
                        self.heights.push(0);
                        self.rendered.push(None);
                        continue;
                    };
                    let key = Key {
                        message: msg.id,
                        edited: msg.edited_timestamp.map(|t| t.as_second()).unwrap_or(0),
                        width,
                        theme: theme_hash,
                        reactions: hash_of(
                            &msg.reactions
                                .iter()
                                .map(|r| (r.emoji.reaction_key(), r.count, r.me))
                                .collect::<Vec<_>>(),
                        ),
                        first_in_group: head,
                        revealed: revealed_hash,
                        media_gen: self
                            .gens
                            .get(&msg.id)
                            .copied()
                            .unwrap_or(0)
                            .wrapping_add(threads_hash),
                        avatars: v.cfg.chat.show_avatars,
                        timestamps: v.cfg.chat.timestamps,
                    };
                    let ctx = RenderCtx {
                        theme: v.theme,
                        width,
                        avatars: v.cfg.chat.show_avatars,
                        timestamps: v.cfg.chat.timestamps,
                        aspect: v.aspect,
                        max_image_rows: v.cfg.chat.max_image_rows,
                        pictures: v.pictures,
                        emoji_images: v.cfg.chat.emoji_images,
                        show_embeds: v.cfg.chat.show_embeds,
                        spoilers: v.cfg.chat.spoilers,
                        me: v.me,
                        revealed: &self.revealed,
                        names: &self.names,
                        media: Some(&self.media),
                        tz: v.tz.clone(),
                    };
                    let built = self
                        .cache
                        .get_or_insert(key, || render::render(&msg, head, &ctx));
                    for slot in &built.images {
                        owners.entry(slot.key.clone()).or_default().push(msg.id);
                    }
                    for slot in &built.emoji {
                        owners.entry(slot.key.clone()).or_default().push(msg.id);
                    }
                    for key in &built.measure {
                        owners.entry(key.clone()).or_default().push(msg.id);
                        wanted.push(key.clone());
                    }
                    self.heights.push(built.height);
                    self.rendered.push(Some(built));
                }
                _ => {
                    self.heights.push(1);
                    self.rendered.push(None);
                }
            }
        }
        self.media_owners = owners;
        let held: std::collections::HashSet<MessageId> =
            self.messages.iter().map(|m| m.id).collect();
        self.gens.retain(|id, _| held.contains(id));
        // The pictures nothing said the size of: asked for here rather than in
        // the drawing pass, because a chip has no rectangle to be asked from.
        for key in wanted {
            self.media.want_measure(&key);
        }
        self.clamp_cursor();
    }

    /// Draw the panel. `outer` is the framed rect, for the scrollbar on its
    /// right border; `body` is what the frame left for contents.
    pub fn render(&mut self, outer: Rect, body: Rect, buf: &mut Buffer, v: &Params<'_>) {
        self.hits.clear();
        self.slots.clear();
        if body.width == 0 || body.height == 0 {
            return;
        }
        if self.channel.is_none() {
            empty(body, buf, v.theme, "choose a channel");
            return;
        }
        self.prepare(body, v);
        if self.rows.is_empty() {
            empty(body, buf, v.theme, "no messages here yet");
            return;
        }

        let t = v.theme;
        let heights: Vec<u16> = self.heights.clone();
        let get = |i: usize| heights.get(i).copied().unwrap_or(0);
        let visible = self.list.visible(body, get, self.rows.len());
        let text_width = body.width.saturating_sub(1).max(1);
        self.below = match visible.last() {
            Some(last) => self.rows[last.index + 1..]
                .iter()
                .filter(|r| r.selectable())
                .count(),
            None => 0,
        };

        for item in &visible {
            let row = &self.rows[item.index];
            let selected = item.index == self.cursor && v.focused;
            match row {
                Row::LoadOlder => {
                    let text = if self.loading_older {
                        "\u{2026} loading older messages"
                    } else {
                        "\u{2191} older messages"
                    };
                    buf.set_string(
                        item.area.x,
                        item.area.y,
                        fit(text, text_width),
                        Style::default().fg(rgb(t.dim)),
                    );
                    self.hits.push((item.area, Hit::LoadOlder));
                }
                Row::Day(label) => divider(item.area, buf, t, label, t.chat.divider_fg),
                Row::NewMessages => divider(item.area, buf, t, "new messages", t.chat.unread_fg),
                Row::Typing => {
                    let text = typing_line(&self.typing);
                    buf.set_string(
                        item.area.x,
                        item.area.y,
                        fit(&text, text_width),
                        Style::default()
                            .fg(rgb(t.dim))
                            .add_modifier(Modifier::ITALIC),
                    );
                }
                Row::Pending { index } => {
                    let row = self.pending.get(*index).cloned().unwrap_or_default();
                    let mark = if row.failed {
                        "\u{2715} "
                    } else {
                        "\u{00b7} "
                    };
                    let style = if row.failed {
                        Style::default().fg(rgb(t.error))
                    } else {
                        Style::default().fg(rgb(t.chat.disconnected_dim))
                    };
                    let trailer = upload_note(&row, self.uploads.get(&row.nonce).copied());
                    buf.set_string(
                        item.area.x,
                        item.area.y,
                        fit(&format!("  {mark}{}{trailer}", row.content), text_width),
                        style,
                    );
                }
                Row::Message { index, .. } => {
                    let Some(rendered) = self.rendered[item.index].clone() else {
                        continue;
                    };
                    let id = self.messages.get(*index).map(|m| m.id);
                    draw_message(&rendered, item.area, item.skip, buf, t, selected);
                    if v.pictures {
                        collect_slots(&mut self.slots, &rendered, item.area, item.skip, body, t);
                    }
                    if let Some(id) = id {
                        self.hits.push((item.area, Hit::Message(id)));
                        collect_hits(&mut self.hits, &rendered, item.area, item.skip, id);
                    }
                }
            }
        }

        if scrollbar(outer, buf, t, &self.list, &heights, self.rows.len(), body) {
            // The whole track, not the thumb: a click anywhere on it goes
            // there, which is what every scrollbar has always done and what
            // makes a drag work from wherever the pointer happens to be.
            self.hits.push((
                Rect {
                    x: outer.x + outer.width - 1,
                    y: body.y,
                    width: 1,
                    height: body.height,
                },
                Hit::Scrollbar,
            ));
        }
    }

    /// Where the scrollbar's track is, for a drag that has left it sideways.
    pub fn scrollbar_track(&self) -> Option<Rect> {
        self.hits
            .iter()
            .find(|(_, hit)| *hit == Hit::Scrollbar)
            .map(|(rect, _)| *rect)
    }

    /// This frame's pictures, for the pass that draws them.
    ///
    /// Taken rather than borrowed: the drawing needs the terminal's graphics,
    /// which the application owns, and handing the rectangles over is what
    /// keeps this panel from owning a second copy of it.
    pub fn take_slots(&mut self) -> Vec<Placement> {
        std::mem::take(&mut self.slots)
    }
}

/// One optimistic row, and what it is waiting on.
#[derive(Debug, Clone)]
struct PendingRow {
    nonce: crate::discord::handle::Nonce,
    content: String,
    failed: bool,
    uploading: bool,
    files: usize,
}

impl Default for PendingRow {
    fn default() -> Self {
        Self {
            nonce: crate::discord::handle::Nonce(0),
            content: String::new(),
            failed: false,
            uploading: false,
            files: 0,
        }
    }
}

/// What to put after a message that has not gone yet.
///
/// A percentage rather than a spinner: a twenty-megabyte picture on a slow
/// connection is thirty seconds of something, and "sending" for thirty seconds
/// looks exactly like a client that has stopped.
fn upload_note(row: &PendingRow, progress: Option<(u64, u64)>) -> String {
    if row.failed {
        return "  (not sent)".into();
    }
    if !row.uploading {
        return String::new();
    }
    let files = if row.files == 1 {
        "1 file".to_string()
    } else {
        format!("{} files", row.files)
    };
    match progress {
        Some((sent, total)) if total > 0 => {
            let percent = (sent.saturating_mul(100) / total).min(100);
            format!("  ({files}, {percent}%)")
        }
        _ => format!("  ({files}\u{2026})"),
    }
}

/// Everything the panel needs each frame that is not its own state.
pub struct Params<'a> {
    pub theme: &'a Theme,
    pub cfg: &'a Config,
    pub focused: bool,
    pub pictures: bool,
    pub aspect: f32,
    pub me: Option<crate::discord::snowflake::UserId>,
    pub tz: jiff::tz::TimeZone,
}

fn draw_message(
    rendered: &Rendered,
    area: Rect,
    skip: u16,
    buf: &mut Buffer,
    theme: &Theme,
    selected: bool,
) {
    for (n, line) in rendered
        .lines
        .iter()
        .skip(usize::from(skip))
        .take(usize::from(area.height))
        .enumerate()
    {
        let y = area.y + n as u16;
        let mut x = area.x;
        for span in &line.spans {
            if x >= area.x + area.width {
                break;
            }
            let room = area.x + area.width - x;
            let text = fit_span(&span.content, room);
            if text.is_empty() {
                continue;
            }
            let w = width_of(&text);
            buf.set_string(x, y, text, span.style);
            x += w;
        }
        if selected {
            // A bar in the first column rather than a highlight across the
            // row: a selected message keeps its own colours, and a message
            // that changes colour when it is selected is a message whose
            // mentions and code you cannot read while acting on it.
            buf.set_string(
                area.x,
                y,
                "\u{2595}",
                Style::default().fg(rgb(theme.accent)),
            );
        }
    }
}

fn fit_span(text: &str, room: u16) -> String {
    if width_of(text) <= room {
        return text.to_string();
    }
    render::cut(text, room)
}

/// Turn one message's picture slots into absolute placements.
///
/// `area` is where the message was drawn and `skip` is how many of its own
/// rows are above the top of the viewport, which is the one case where a
/// picture starts above the screen: a rectangle cannot, so the rows are taken
/// off the top here and the placement is marked as already cut.
fn collect_slots(
    out: &mut Vec<Placement>,
    rendered: &Rendered,
    area: Rect,
    skip: u16,
    clip: Rect,
    theme: &Theme,
) {
    let horizontal = |col: u16, cols: u16| -> Option<(u16, u16)> {
        let width = cols.min(area.width.saturating_sub(col));
        (width > 0).then_some((area.x + col, width))
    };
    for slot in &rendered.images {
        let Some((x, width)) = horizontal(slot.col, slot.cols) else {
            continue;
        };
        let top = i32::from(area.y) + i32::from(slot.row) - i32::from(skip);
        let floor = i32::from(clip.y);
        let (y, height) = if top < floor {
            (floor, i32::from(slot.rows) - (floor - top))
        } else {
            (top, i32::from(slot.rows))
        };
        if height <= 0 {
            continue;
        }
        let shape = match slot.kind {
            SlotKind::Avatar => media::Shape::Icon {
                initials: slot.alt.clone(),
                colour: theme.chat.author_fg,
            },
            SlotKind::Gifv => media::Shape::Play,
            SlotKind::Still | SlotKind::Gif => media::Shape::Picture,
        };
        out.push(Placement {
            rect: Rect {
                x,
                y: y as u16,
                width,
                height: height as u16,
            },
            clip,
            clipped: top < floor,
            key: slot.key.clone(),
            shape,
            alt: slot.alt.clone(),
        });
    }
    for slot in &rendered.emoji {
        let Some((x, width)) = horizontal(slot.col, 2) else {
            continue;
        };
        let Some(row) = slot.row.checked_sub(skip) else {
            continue;
        };
        out.push(Placement {
            rect: Rect {
                x,
                y: area.y + row,
                width,
                height: 1,
            },
            clip,
            clipped: false,
            key: slot.key.clone(),
            shape: media::Shape::Emoji {
                name: slot.name.clone(),
            },
            alt: String::new(),
        });
    }
}

fn collect_hits(
    out: &mut Vec<(Rect, Hit)>,
    rendered: &Rendered,
    area: Rect,
    skip: u16,
    id: MessageId,
) {
    let place = |row: u16, col: u16, width: u16| -> Option<Rect> {
        let row = row.checked_sub(skip)?;
        if row >= area.height {
            return None;
        }
        Some(Rect {
            x: area.x + col.min(area.width),
            y: area.y + row,
            width: width.min(area.width.saturating_sub(col)),
            height: 1,
        })
    };
    for link in &rendered.links {
        if let Some(rect) = place(link.row, link.col, link.width) {
            out.push((rect, Hit::Link(link.url.clone())));
        }
    }
    for chip in &rendered.reactions {
        if let Some(rect) = place(chip.row, chip.col, chip.width) {
            out.push((rect, Hit::Reaction(id, chip.emoji.clone())));
        }
    }
    for attachment in &rendered.attachments {
        if let Some(rect) = place(attachment.row, 0, area.width) {
            out.push((rect, Hit::Attachment(id, attachment.url.clone())));
        }
    }
    if let Some(row) = rendered.reply_row {
        if let Some(rect) = place(row, 0, area.width) {
            out.push((rect, Hit::Reply(id)));
        }
    }
}

fn divider(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    label: &str,
    colour: starkit::theme::color::Rgb,
) {
    let _ = theme;
    if area.width == 0 {
        return;
    }
    let label = format!(" {label} ");
    let label_w = width_of(&label).min(area.width);
    let lead = (area.width.saturating_sub(label_w)) / 2;
    let trail = area.width.saturating_sub(lead + label_w);
    let style = Style::default().fg(rgb(colour));
    let mut x = area.x;
    buf.set_string(x, area.y, DIVIDER.repeat(usize::from(lead)), style);
    x += lead;
    buf.set_string(x, area.y, render::cut(&label, label_w), style);
    x += label_w;
    buf.set_string(x, area.y, DIVIDER.repeat(usize::from(trail)), style);
}

fn typing_line(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("  {one} is typing\u{2026}"),
        [a, b] => format!("  {a} and {b} are typing\u{2026}"),
        [a, b, ..] => format!(
            "  {a}, {b} and {} others are typing\u{2026}",
            names.len() - 2
        ),
    }
}

/// One column of `▐` down the panel's right border.
fn scrollbar(
    outer: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    list: &VirtualList,
    heights: &[u16],
    len: usize,
    body: Rect,
) -> bool {
    if outer.width < 2 || body.height < 3 || len == 0 {
        return false;
    }
    let total: u32 = heights.iter().map(|h| u32::from(*h)).sum();
    if total <= u32::from(body.height) {
        return false;
    }
    let get = |i: usize| heights.get(i).copied().unwrap_or(0);
    let visible = list.visible(body, get, len);
    let first = visible.first().map(|v| v.index).unwrap_or(0);
    let skip = visible.first().map(|v| v.skip).unwrap_or(0);
    let above: u32 = heights
        .iter()
        .take(first)
        .map(|h| u32::from(*h))
        .sum::<u32>()
        + u32::from(skip);

    let track = body.height;
    let thumb = ((u32::from(track) * u32::from(track)) / total).max(1) as u16;
    let room = track.saturating_sub(thumb);
    let scrolled = total.saturating_sub(u32::from(body.height)).max(1);
    let at = ((above * u32::from(room)) / scrolled).min(u32::from(room)) as u16;

    let x = outer.x + outer.width - 1;
    let style = Style::default().fg(rgb(theme.accent));
    for i in 0..thumb {
        let y = body.y + at + i;
        if y < body.y + body.height {
            buf.set_string(x, y, SCROLLBAR, style);
        }
    }
    true
}

/// The oldest message the read state says has not been seen.
fn first_unread(state: &State, channel: ChannelId, messages: &[Arc<Message>]) -> Option<MessageId> {
    let read = state.read_state(channel)?;
    let last = read.last_message_id?;
    messages.iter().find(|m| m.id > last).map(|m| m.id)
}

/// The names the ids in this window refer to.
fn names_for(state: &State, channel: ChannelId, messages: &[Arc<Message>]) -> Names {
    let mut names = Names::default();
    let guild = state.channel(channel).and_then(|c| c.guild_id);
    if let Some(guild) = guild.and_then(|g| state.guild(g)) {
        for (id, role) in &guild.roles {
            names.roles.insert(*id, role.name.clone());
        }
    }
    if let Some(me) = state.me() {
        names.users.insert(me.id, me.display_name().to_string());
    }
    for msg in messages {
        names
            .users
            .insert(msg.author.id, msg.author_name().to_string());
        // `mentions` carries the whole user object, which is the only reason
        // a mention can be drawn as a name without a member fetch.
        for user in &msg.mentions {
            names.users.insert(user.id, user.display_name().to_string());
        }
        for inline in mentioned_channels(msg) {
            if let Some(channel) = state.channel(inline) {
                let name = channel
                    .name()
                    .map(str::to_string)
                    .unwrap_or_else(|| state.dm_title(inline));
                names.channels.insert(inline, name);
            }
        }
    }
    names
}

/// Channel ids a message writes as `<#id>`.
///
/// Scanned out of the raw content rather than parsed: it is one pass over a
/// string that is at most four kilobytes, it happens once per change, and the
/// alternative is walking the whole AST for the one inline that carries an id
/// the message object does not.
fn mentioned_channels(msg: &Message) -> Vec<ChannelId> {
    let mut out = Vec::new();
    let bytes = msg.content.as_bytes();
    let mut i = 0usize;
    while let Some(at) = msg.content[i..].find("<#") {
        let start = i + at + 2;
        let end = match bytes[start..].iter().position(|b| *b == b'>') {
            Some(n) => start + n,
            None => break,
        };
        if let Ok(id) = msg.content[start..end].parse::<u64>() {
            out.push(ChannelId(id));
        }
        i = end + 1;
        if i >= msg.content.len() {
            break;
        }
    }
    out
}

fn hash_of<T: std::hash::Hash>(value: &T) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// What the panel's border says, so the header is not a second opinion.
pub fn title(location: &str) -> String {
    if location.is_empty() {
        "chat".into()
    } else {
        location.to_string()
    }
}

/// A row of a message, as plain text. For the tests next door and for `y`.
pub fn text_of(rendered: &Rendered) -> Vec<String> {
    rendered
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// A span, for tests that want to know what colour something came out.
pub fn spans_of(rendered: &Rendered, row: usize) -> Vec<Span<'static>> {
    rendered
        .lines
        .get(row)
        .map(|l| l.spans.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::model::User;
    use crate::ui::theme::tests_support::theme;

    const BODY: Rect = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };

    fn message(id: u64, at: i64) -> Message {
        Message {
            id: MessageId(id),
            channel_id: ChannelId(1),
            author: User {
                id: crate::discord::snowflake::UserId(10),
                username: "alex".into(),
                ..User::default()
            },
            content: format!("message {id}"),
            timestamp: jiff::Timestamp::from_second(at).ok(),
            ..Message::default()
        }
    }

    /// A panel with `n` messages in it, already laid out once.
    fn panel(n: u64) -> (ChatState, crate::ui::theme::Theme, Config) {
        let mut chat = ChatState::new();
        chat.open(ChannelId(1));
        chat.messages = (0..n)
            .map(|i| Arc::new(message(100 + i, 1_000_000 + (i as i64) * 1_000)))
            .collect();
        chat.rows = layout::rows(&Shape {
            messages: &chat.messages,
            pending: 0,
            group_window_secs: 420,
            has_older: false,
            first_unread: None,
            typing: false,
            tz: jiff::tz::TimeZone::UTC,
        });
        chat.cursor = chat.rows.len().saturating_sub(1);
        (chat, theme("terminal"), Config::default())
    }

    fn draw(chat: &mut ChatState, t: &crate::ui::theme::Theme, cfg: &Config) -> Vec<String> {
        let mut buf = Buffer::empty(BODY);
        chat.render(
            BODY,
            BODY,
            &mut buf,
            &Params {
                theme: t,
                cfg,
                focused: true,
                pictures: false,
                aspect: 2.0,
                me: None,
                tz: jiff::tz::TimeZone::UTC,
            },
        );
        (0..BODY.height)
            .map(|y| {
                (0..BODY.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// A conversation opens on the newest message, not on the oldest.
    #[test]
    fn it_opens_at_the_end() {
        let (mut chat, t, cfg) = panel(40);
        let rows = draw(&mut chat, &t, &cfg);
        assert!(chat.at_bottom());
        assert!(
            rows.iter().any(|r| r.contains("message 139")),
            "the newest is not on screen: {rows:?}"
        );
        assert_eq!(chat.selected().map(|m| m.id), Some(MessageId(139)));
    }

    /// Scrolling up leaves the end, and `G` comes back to it.
    #[test]
    fn scrolling_up_leaves_the_end_and_g_returns() {
        let (mut chat, t, cfg) = panel(40);
        draw(&mut chat, &t, &cfg);
        chat.scroll(-20);
        assert!(!chat.at_bottom());
        let rows = draw(&mut chat, &t, &cfg);
        assert!(!rows.iter().any(|r| r.contains("message 139")), "{rows:?}");
        chat.to_bottom();
        let rows = draw(&mut chat, &t, &cfg);
        assert!(rows.iter().any(|r| r.contains("message 139")), "{rows:?}");
    }

    /// The one property history paging has to have: a page arriving above the
    /// viewport must not move what is in it.
    #[test]
    fn older_messages_arriving_above_do_not_move_the_view() {
        let (mut chat, t, cfg) = panel(40);
        draw(&mut chat, &t, &cfg);
        chat.scroll(-15);
        let before = draw(&mut chat, &t, &cfg);
        let top = before[0].clone();

        // Ten older messages, prepended the way the core reports them.
        let older: Vec<Arc<Message>> = (0..10)
            .map(|i| Arc::new(message(90 + i, 990_000 + (i as i64) * 1_000)))
            .collect();
        let mut all = older;
        all.extend(chat.messages.iter().cloned());
        chat.messages = all;
        chat.rows = layout::rows(&Shape {
            messages: &chat.messages,
            pending: 0,
            group_window_secs: 420,
            has_older: false,
            first_unread: None,
            typing: false,
            tz: jiff::tz::TimeZone::UTC,
        });
        chat.prepended(10);

        let after = draw(&mut chat, &t, &cfg);
        assert_eq!(after[0], top, "the view jumped");
    }

    /// The top of the list asks for more history once, not once a frame.
    #[test]
    fn the_top_asks_for_older_messages_once() {
        let (mut chat, t, cfg) = panel(40);
        chat.has_older = true;
        chat.rows.insert(0, Row::LoadOlder);
        draw(&mut chat, &t, &cfg);
        chat.to_top();
        draw(&mut chat, &t, &cfg);
        assert!(chat.wants_older(), "it did not ask at the top");
        assert!(!chat.wants_older(), "it asked twice");
        // Scrolling away and coming back asks again.
        chat.scroll(-1);
        assert!(chat.wants_older() || chat.at_bottom());
    }

    /// `j` and `k` move message-wise and never stop on a divider.
    #[test]
    fn the_cursor_moves_message_by_message() {
        let (mut chat, t, cfg) = panel(40);
        draw(&mut chat, &t, &cfg);
        let last = chat.selected().map(|m| m.id);
        chat.move_cursor(-1);
        assert_ne!(chat.selected().map(|m| m.id), last);
        chat.move_cursor(-100);
        assert_eq!(
            chat.selected().map(|m| m.id),
            Some(MessageId(100)),
            "it did not stop at the first message"
        );
        for row in &chat.rows[chat.cursor..=chat.cursor] {
            assert!(row.selectable());
        }
    }

    /// Each channel remembers where it was left.
    #[test]
    fn every_channel_keeps_its_own_place() {
        let (mut chat, t, cfg) = panel(40);
        draw(&mut chat, &t, &cfg);
        chat.scroll(-12);
        let anchor = chat.anchor();
        assert!(anchor.is_some(), "scrolled up but reported no anchor");

        chat.open(ChannelId(2));
        assert!(chat.at_bottom(), "the new channel starts at the end");
        assert_eq!(chat.saved_anchor(ChannelId(1)), anchor);
    }

    /// A channel with nothing in it says so rather than drawing nothing.
    #[test]
    fn an_empty_channel_says_so() {
        let mut chat = ChatState::new();
        let t = theme("terminal");
        let cfg = Config::default();
        let rows = draw(&mut chat, &t, &cfg);
        assert!(
            rows.iter().any(|r| r.contains("choose a channel")),
            "{rows:?}"
        );

        chat.open(ChannelId(1));
        let rows = draw(&mut chat, &t, &cfg);
        assert!(rows.iter().any(|r| r.contains("no messages")), "{rows:?}");
    }

    /// A resize throws the measurements away, because they were measured for
    /// another width.
    #[test]
    fn a_new_width_clears_the_cache() {
        let (mut chat, t, cfg) = panel(5);
        draw(&mut chat, &t, &cfg);
        assert!(!chat.cache.is_empty());
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
        chat.render(
            Rect::new(0, 0, 60, 10),
            Rect::new(0, 0, 60, 10),
            &mut buf,
            &Params {
                theme: &t,
                cfg: &cfg,
                focused: true,
                pictures: false,
                aspect: 2.0,
                me: None,
                tz: jiff::tz::TimeZone::UTC,
            },
        );
        assert_eq!(chat.width, 60);
    }

    /// A click finds what is under it: a link, a chip, a message.
    #[test]
    fn a_click_lands_on_what_was_drawn() {
        let mut chat = ChatState::new();
        chat.open(ChannelId(1));
        let mut msg = message(100, 1_000_000);
        msg.content = "see https://example.invalid/x for it".into();
        chat.messages = vec![Arc::new(msg)];
        chat.rows = layout::rows(&Shape {
            messages: &chat.messages,
            pending: 0,
            group_window_secs: 420,
            has_older: false,
            first_unread: None,
            typing: false,
            tz: jiff::tz::TimeZone::UTC,
        });
        let t = theme("terminal");
        let cfg = Config::default();
        draw(&mut chat, &t, &cfg);

        let link = chat
            .hits
            .iter()
            .find(|(_, h)| matches!(h, Hit::Link(_)))
            .map(|(r, _)| *r)
            .expect("the link was not recorded");
        assert_eq!(
            chat.hit(link.x, link.y),
            Some(Hit::Link("https://example.invalid/x".into()))
        );
        // A click elsewhere on the same message selects it instead.
        assert_eq!(
            chat.hit(link.x, link.y.saturating_sub(1)),
            Some(Hit::Message(MessageId(100)))
        );
    }

    /// Every picture a frame places is inside the panel it belongs to, and a
    /// picture the scroll has carried part-way off the top is cut rather than
    /// dropped.
    #[test]
    fn a_picture_is_placed_inside_the_panel_and_cut_at_its_edge() {
        use crate::discord::model::Attachment;

        let mut chat = ChatState::new();
        chat.open(ChannelId(1));
        let mut messages: Vec<Arc<Message>> = Vec::new();
        for i in 0..6u64 {
            let mut m = message(200 + i, 1_000_000 + (i as i64) * 100_000);
            m.author.id = crate::discord::snowflake::UserId(10 + i);
            m.attachments.push(Attachment {
                id: crate::discord::snowflake::AttachmentId(600 + i),
                filename: format!("p{i}.png"),
                content_type: Some("image/png".into()),
                url: format!("https://cdn.invalid/p{i}.png"),
                width: Some(400),
                height: Some(400),
                ..Attachment::default()
            });
            messages.push(Arc::new(m));
        }
        chat.messages = messages;
        chat.rows = layout::rows(&Shape {
            messages: &chat.messages,
            pending: 0,
            group_window_secs: 420,
            has_older: false,
            first_unread: None,
            typing: false,
            tz: jiff::tz::TimeZone::UTC,
        });
        chat.cursor = chat.rows.len().saturating_sub(1);

        let t = theme("terminal");
        let cfg = Config::default();
        let body = Rect::new(3, 2, 40, 10);
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 14));
        let params = Params {
            theme: &t,
            cfg: &cfg,
            focused: true,
            pictures: true,
            aspect: 2.0,
            me: None,
            tz: jiff::tz::TimeZone::UTC,
        };
        chat.render(body, body, &mut buf, &params);
        let slots = chat.take_slots();

        assert!(!slots.is_empty(), "nothing was placed");
        for slot in &slots {
            assert_eq!(slot.clip, body);
            assert!(slot.rect.x >= body.x, "{:?}", slot.rect);
            assert!(
                slot.rect.x + slot.rect.width <= body.x + body.width,
                "a picture ran off the right of the panel: {:?}",
                slot.rect
            );
            assert!(slot.rect.y >= body.y, "{:?}", slot.rect);
            assert!(
                media::intersect(slot.rect, body).is_some(),
                "a placement nothing can draw: {:?}",
                slot.rect
            );
        }
        // The list is anchored at the end, so the topmost picture is the one
        // the viewport has cut, and it says so.
        assert!(
            slots.iter().any(|s| s.clipped),
            "nothing was cut at the top of a full viewport"
        );
    }

    /// The typing line names who, and says how many when there are more than
    /// two."""
    #[test]
    fn the_typing_line_names_people() {
        assert_eq!(typing_line(&[]), "");
        assert!(typing_line(&["alex".into()]).contains("alex is typing"));
        assert!(typing_line(&["a".into(), "b".into()]).contains("a and b are typing"));
        let many: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert!(typing_line(&many).contains("2 others"));
    }

    /// Scrolling up puts a count in the status bar; `G` takes it away.
    #[test]
    fn the_status_bar_counts_what_is_below_the_viewport() {
        let (mut chat, t, cfg) = panel(40);
        draw(&mut chat, &t, &cfg);
        assert_eq!(chat.new_below(), 0, "at the end there is nothing below");

        chat.scroll(-20);
        draw(&mut chat, &t, &cfg);
        assert!(chat.new_below() > 0, "scrolled up and counted nothing");

        chat.to_bottom();
        draw(&mut chat, &t, &cfg);
        assert_eq!(chat.new_below(), 0);
    }
}
