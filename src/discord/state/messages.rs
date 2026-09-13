//! One channel's messages.
//!
//! A `VecDeque` in ascending id order, which is ascending time order, because
//! a snowflake encodes the millisecond it was minted. That single fact removes
//! most of the bookkeeping a chat client would otherwise need: there is no
//! separate sort key, no tie-break, and history that arrives out of order
//! merges by binary search rather than by a re-sort.
//!
//! Four flags carry the parts that are not in the deque:
//!
//! - **`has_older`** — there is more above, so scrolling to the top asks for
//!   it. Set when a fetch came back full, and set again when the cap evicts
//!   from the front, because the messages that were dropped are older messages
//!   that can be fetched again.
//! - **`at_latest`** — the bottom of the deque is the bottom of the channel.
//!   While it is false the view is somewhere in history, and a MESSAGE_CREATE
//!   must *not* be appended: doing so would put a message the reader cannot
//!   see below a gap they do not know about. It is counted in `newer_hidden`
//!   instead, which is the "↓ 3 new" in the status line.
//! - **`loading`** — a fetch is in flight, which is both the spinner row and
//!   the interlock that stops a scroll from asking twice.
//!
//! The cap is 500 messages for a channel the reader has open and 50 for one
//! they do not, and `set_open` trims on the way down. Fifty is enough that
//! reopening a channel draws instantly from memory while the fetch runs.
//!
//! **Pending sends live here too.** A message this client sent is on screen
//! before Discord has heard of it, keyed by a nonce; the echo comes back
//! through the gateway carrying that nonce, and matching it is what turns the
//! optimistic row into a real message. They are kept beside the deque rather
//! than in it because they have no id, and an id is what the deque is ordered
//! by.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::discord::handle::Nonce;
use crate::discord::model::{Message, PartialEmoji, Reaction};
use crate::discord::snowflake::MessageId;

/// How many messages are kept for a channel the reader is looking at.
pub const OPEN_CAP: usize = 500;
/// And for one they are not. Enough to draw a first frame from memory.
pub const CLOSED_CAP: usize = 50;
/// What one history request asks for, which is also Discord's maximum.
pub const PAGE: usize = 50;

/// Where a message this client sent has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingState {
    /// Attachments are going up.
    Uploading { sent: u64, total: u64 },
    /// The POST is in flight.
    Sending,
    /// It did not go. The row stays, with a way to retry, because a message
    /// that vanishes silently is a message the sender believes they sent.
    Failed(String),
}

/// A message on screen that Discord has not acknowledged.
#[derive(Debug, Clone)]
pub struct PendingSend {
    pub nonce: Nonce,
    pub content: String,
    pub reply_to: Option<MessageId>,
    pub mention_author: bool,
    pub state: PendingState,
    /// When the optimistic row appeared, for the fallback that inserts the
    /// HTTP response when no echo arrives.
    pub created: jiff::Timestamp,
}

impl PendingSend {
    pub fn new(nonce: Nonce, content: String, reply_to: Option<MessageId>, mention: bool) -> Self {
        Self {
            nonce,
            content,
            reply_to,
            mention_author: mention,
            state: PendingState::Sending,
            created: jiff::Timestamp::now(),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self.state, PendingState::Failed(_))
    }
}

/// What arriving message turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    /// Appended to the bottom, where the reader is.
    Appended,
    /// Merged into history that is already held, which happens when a resumed
    /// session replays something already fetched.
    Merged,
    /// The reader is scrolled up; it is counted rather than shown.
    Hidden,
    /// Already held. A resume replays, and a replay must not duplicate.
    Duplicate,
}

#[derive(Debug, Default)]
pub struct MessageStore {
    messages: VecDeque<Arc<Message>>,
    pending: Vec<PendingSend>,
    has_older: bool,
    at_latest: bool,
    newer_hidden: usize,
    loading: bool,
    open: bool,
}

impl MessageStore {
    /// An empty store.
    ///
    /// `at_latest` starts false, which is the same as `Default` and is the
    /// point: nothing has been fetched, so there is no claim to make about
    /// whether the bottom of the deque is the bottom of the channel.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn has_older(&self) -> bool {
        self.has_older
    }

    pub fn at_latest(&self) -> bool {
        self.at_latest
    }

    pub fn newer_hidden(&self) -> usize {
        self.newer_hidden
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn pending(&self) -> &[PendingSend] {
        &self.pending
    }

    pub fn oldest(&self) -> Option<&Arc<Message>> {
        self.messages.front()
    }

    pub fn newest(&self) -> Option<&Arc<Message>> {
        self.messages.back()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<Message>> {
        self.messages.iter()
    }

    fn cap(&self) -> usize {
        if self.open {
            OPEN_CAP
        } else {
            CLOSED_CAP
        }
    }

    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    pub fn set_has_older(&mut self, has_older: bool) {
        self.has_older = has_older;
    }

    pub fn set_at_latest(&mut self, at_latest: bool) {
        self.at_latest = at_latest;
        if at_latest {
            self.newer_hidden = 0;
        }
    }

    /// Mark the channel open or closed, trimming to the smaller cap on the way
    /// down.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
        self.trim();
    }

    /// Where a message with this id belongs, and whether it is already there.
    fn locate(&self, id: MessageId) -> Result<usize, usize> {
        self.messages.binary_search_by(|held| held.id.cmp(&id))
    }

    pub fn get(&self, id: MessageId) -> Option<Arc<Message>> {
        self.locate(id)
            .ok()
            .map(|at| Arc::clone(&self.messages[at]))
    }

    pub fn contains(&self, id: MessageId) -> bool {
        self.locate(id).is_ok()
    }

    /// A slice of the deque, oldest first.
    ///
    /// The UI's one read path. It copies `Arc`s rather than borrowing, because
    /// the lock has to be dropped before a frame is drawn.
    pub fn window(&self, start: usize, count: usize) -> Vec<Arc<Message>> {
        self.messages
            .iter()
            .skip(start)
            .take(count)
            .cloned()
            .collect()
    }

    /// The newest `count`, which is what a channel opens on.
    pub fn latest(&self, count: usize) -> Vec<Arc<Message>> {
        let start = self.messages.len().saturating_sub(count);
        self.window(start, count)
    }

    /// Insert or replace one message, keeping the order.
    ///
    /// Returns whether it was new.
    fn insert(&mut self, message: Message) -> bool {
        match self.locate(message.id) {
            Ok(at) => {
                self.messages[at] = Arc::new(message);
                false
            }
            Err(at) => {
                self.messages.insert(at, Arc::new(message));
                true
            }
        }
    }

    /// Drop from the front until the cap is met.
    ///
    /// Evicting from the front is evicting history, which can be fetched again
    /// — so `has_older` becomes true, and the scrollback still works.
    fn trim(&mut self) {
        let cap = self.cap();
        while self.messages.len() > cap {
            self.messages.pop_front();
            self.has_older = true;
        }
    }

    /// The result of opening a channel or jumping: the window is whatever came
    /// back and nothing that was held before.
    pub fn replace(&mut self, messages: Vec<Message>, at_latest: bool) {
        let full = messages.len() >= PAGE;
        self.messages.clear();
        for message in messages {
            self.insert(message);
        }
        self.has_older = full;
        self.at_latest = at_latest;
        if at_latest {
            self.newer_hidden = 0;
        }
        self.trim();
    }

    /// Older history, fetched because the reader reached the top.
    ///
    /// Returns how many rows were actually added; a resumed fetch that overlaps
    /// what is held adds fewer than it fetched, and the scroll anchor has to
    /// move by the real number.
    pub fn prepend(&mut self, messages: Vec<Message>) -> usize {
        let fetched = messages.len();
        let mut added = 0;
        for message in messages {
            if self.insert(message) {
                added += 1;
            }
        }
        // A full page back means there is very likely another one behind it.
        self.has_older = fetched >= PAGE;
        // Trimming here would undo the fetch that was just made, so the cap is
        // enforced from the *other* end: the newest go, not the oldest.
        let cap = self.cap();
        while self.messages.len() > cap {
            self.messages.pop_back();
            self.at_latest = false;
        }
        added
    }

    /// Newer history, fetched because the reader scrolled down into a gap.
    pub fn append(&mut self, messages: Vec<Message>, at_latest: bool) -> usize {
        let mut added = 0;
        for message in messages {
            if self.insert(message) {
                added += 1;
            }
        }
        if at_latest {
            self.at_latest = true;
            self.newer_hidden = 0;
        }
        self.trim();
        added
    }

    /// A MESSAGE_CREATE.
    pub fn receive(&mut self, message: Message) -> Received {
        // The echo of something this client sent. Matching by nonce is what
        // makes the optimistic row become the real one rather than the real one
        // appearing below it.
        if let Some(nonce) = message.nonce.as_deref() {
            if let Some(at) = self
                .pending
                .iter()
                .position(|p| p.nonce.to_string() == nonce)
            {
                self.pending.remove(at);
                // The reader sent it, so the reader is at the bottom.
                self.at_latest = true;
                self.newer_hidden = 0;
                let fresh = self.insert(message);
                self.trim();
                return if fresh {
                    Received::Appended
                } else {
                    Received::Merged
                };
            }
        }

        if self.contains(message.id) {
            self.insert(message);
            return Received::Duplicate;
        }

        if !self.at_latest {
            // The view is up in history. Appending would hide the message
            // below a gap nobody can see.
            self.newer_hidden += 1;
            return Received::Hidden;
        }

        let newest = self.messages.back().map(|m| m.id);
        let appended = newest.is_none_or(|newest| message.id > newest);
        self.insert(message);
        self.trim();
        if appended {
            Received::Appended
        } else {
            Received::Merged
        }
    }

    /// A MESSAGE_UPDATE, which is a partial message.
    ///
    /// Returns false when the message is not held, which is ordinary: editing
    /// something that scrolled out of the window changes nothing here.
    pub fn update(&mut self, id: MessageId, payload: &serde_json::Value) -> bool {
        let Ok(at) = self.locate(id) else {
            return false;
        };
        let merged = self.messages[at].merge_update(payload);
        self.messages[at] = Arc::new(merged);
        true
    }

    /// Replace a whole message, for the send fallback that inserts the HTTP
    /// response when no gateway echo arrived.
    pub fn upsert(&mut self, message: Message) -> bool {
        let fresh = self.insert(message);
        self.trim();
        fresh
    }

    pub fn remove(&mut self, id: MessageId) -> bool {
        match self.locate(id) {
            Ok(at) => {
                self.messages.remove(at);
                true
            }
            Err(_) => false,
        }
    }

    pub fn remove_many(&mut self, ids: &[MessageId]) -> usize {
        ids.iter().filter(|id| self.remove(**id)).count()
    }

    /// MESSAGE_REACTION_ADD.
    pub fn add_reaction(&mut self, id: MessageId, emoji: &PartialEmoji, me: bool) -> bool {
        self.edit_reactions(id, |reactions| {
            match reactions.iter_mut().find(|r| r.emoji == *emoji) {
                Some(existing) => {
                    existing.count = existing.count.saturating_add(1);
                    existing.me |= me;
                }
                None => reactions.push(Reaction {
                    count: 1,
                    me,
                    emoji: emoji.clone(),
                    ..Default::default()
                }),
            }
        })
    }

    /// MESSAGE_REACTION_REMOVE.
    pub fn remove_reaction(&mut self, id: MessageId, emoji: &PartialEmoji, me: bool) -> bool {
        self.edit_reactions(id, |reactions| {
            if let Some(at) = reactions.iter().position(|r| r.emoji == *emoji) {
                let reaction = &mut reactions[at];
                reaction.count = reaction.count.saturating_sub(1);
                if me {
                    reaction.me = false;
                }
                if reaction.count == 0 {
                    reactions.remove(at);
                }
            }
        })
    }

    /// MESSAGE_REACTION_REMOVE_ALL.
    pub fn clear_reactions(&mut self, id: MessageId) -> bool {
        self.edit_reactions(id, |reactions| reactions.clear())
    }

    /// MESSAGE_REACTION_REMOVE_EMOJI: every reaction of one kind goes,
    /// whoever added it.
    pub fn clear_emoji(&mut self, id: MessageId, emoji: &PartialEmoji) -> bool {
        self.edit_reactions(id, |reactions| reactions.retain(|r| r.emoji != *emoji))
    }

    fn edit_reactions(&mut self, id: MessageId, f: impl FnOnce(&mut Vec<Reaction>)) -> bool {
        let Ok(at) = self.locate(id) else {
            return false;
        };
        let mut message = (*self.messages[at]).clone();
        f(&mut message.reactions);
        self.messages[at] = Arc::new(message);
        true
    }

    // -- pending sends -----------------------------------------------------

    pub fn add_pending(&mut self, send: PendingSend) {
        if self.pending.iter().any(|p| p.nonce == send.nonce) {
            return;
        }
        self.pending.push(send);
    }

    pub fn pending_mut(&mut self, nonce: Nonce) -> Option<&mut PendingSend> {
        self.pending.iter_mut().find(|p| p.nonce == nonce)
    }

    pub fn take_pending(&mut self, nonce: Nonce) -> Option<PendingSend> {
        let at = self.pending.iter().position(|p| p.nonce == nonce)?;
        Some(self.pending.remove(at))
    }

    pub fn fail_pending(&mut self, nonce: Nonce, reason: impl Into<String>) -> bool {
        match self.pending_mut(nonce) {
            Some(pending) => {
                pending.state = PendingState::Failed(reason.into());
                true
            }
            None => false,
        }
    }

    /// Whether this store holds an outstanding send with this nonce.
    pub fn has_pending(&self, nonce: Nonce) -> bool {
        self.pending.iter().any(|p| p.nonce == nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: u64) -> Message {
        Message {
            id: MessageId(id),
            content: format!("message {id}"),
            ..Default::default()
        }
    }

    fn with_nonce(id: u64, nonce: &str) -> Message {
        Message {
            nonce: Some(nonce.to_string()),
            ..message(id)
        }
    }

    fn store_at_latest() -> MessageStore {
        let mut store = MessageStore::new();
        store.set_open(true);
        store.replace(vec![message(1), message(2), message(3)], true);
        store
    }

    fn ids(store: &MessageStore) -> Vec<u64> {
        store.iter().map(|m| m.id.get()).collect()
    }

    #[test]
    fn history_is_held_oldest_first_whatever_order_it_arrives_in() {
        let mut store = MessageStore::new();
        store.replace(vec![message(3), message(1), message(2)], true);
        assert_eq!(ids(&store), vec![1, 2, 3]);
        assert_eq!(store.oldest().unwrap().id, MessageId(1));
        assert_eq!(store.newest().unwrap().id, MessageId(3));
    }

    #[test]
    fn a_full_page_back_means_there_is_more_behind_it() {
        let mut store = MessageStore::new();
        store.replace((1..=PAGE as u64).map(message).collect(), true);
        assert!(store.has_older(), "a full page is not the whole channel");

        let mut short = MessageStore::new();
        short.replace(vec![message(1)], true);
        assert!(!short.has_older(), "one message back is the whole channel");
    }

    #[test]
    fn a_live_message_is_appended_when_the_reader_is_at_the_bottom() {
        let mut store = store_at_latest();
        assert_eq!(store.receive(message(4)), Received::Appended);
        assert_eq!(ids(&store), vec![1, 2, 3, 4]);
        assert_eq!(store.newer_hidden(), 0);
    }

    #[test]
    fn a_live_message_is_counted_rather_than_hidden_below_a_gap() {
        let mut store = store_at_latest();
        store.set_at_latest(false);

        assert_eq!(store.receive(message(4)), Received::Hidden);
        assert_eq!(store.receive(message(5)), Received::Hidden);
        assert_eq!(
            ids(&store),
            vec![1, 2, 3],
            "a message was put below a gap the reader cannot see"
        );
        assert_eq!(store.newer_hidden(), 2);

        // Scrolling back to the bottom clears the count.
        store.set_at_latest(true);
        assert_eq!(store.newer_hidden(), 0);
    }

    #[test]
    fn a_replayed_message_does_not_appear_twice() {
        let mut store = store_at_latest();
        assert_eq!(store.receive(message(2)), Received::Duplicate);
        assert_eq!(ids(&store), vec![1, 2, 3]);
    }

    #[test]
    fn the_echo_of_a_pending_send_replaces_it() {
        let mut store = store_at_latest();
        let nonce = Nonce(81237712343);
        store.add_pending(PendingSend::new(nonce, "hello".into(), None, false));
        assert_eq!(store.pending().len(), 1);

        assert_eq!(
            store.receive(with_nonce(4, "81237712343")),
            Received::Appended
        );
        assert!(
            store.pending().is_empty(),
            "the optimistic row is still there beside the real one"
        );
        assert_eq!(ids(&store), vec![1, 2, 3, 4]);
    }

    #[test]
    fn an_echo_arriving_while_scrolled_up_brings_the_view_back_down() {
        let mut store = store_at_latest();
        store.set_at_latest(false);
        let nonce = Nonce(7);
        store.add_pending(PendingSend::new(nonce, "hello".into(), None, false));

        assert_eq!(store.receive(with_nonce(4, "7")), Received::Appended);
        assert!(
            store.at_latest(),
            "the reader sent it, so the reader is at the bottom"
        );
    }

    #[test]
    fn a_nonce_from_somebody_elses_client_is_an_ordinary_message() {
        let mut store = store_at_latest();
        store.add_pending(PendingSend::new(Nonce(1), "mine".into(), None, false));
        assert_eq!(store.receive(with_nonce(4, "999")), Received::Appended);
        assert_eq!(store.pending().len(), 1, "somebody else cleared my pending");
    }

    #[test]
    fn eviction_from_the_front_leaves_history_fetchable() {
        let mut store = MessageStore::new();
        store.set_open(true);
        store.set_at_latest(true);
        for id in 1..=(OPEN_CAP as u64 + 10) {
            store.receive(message(id));
        }
        assert_eq!(store.len(), OPEN_CAP);
        assert!(
            store.has_older(),
            "messages were dropped without recording that there are older ones"
        );
        assert_eq!(store.oldest().unwrap().id, MessageId(11));
    }

    #[test]
    fn closing_a_channel_trims_it_to_the_smaller_cap() {
        let mut store = MessageStore::new();
        store.set_open(true);
        store.replace((1..=200u64).map(message).collect(), true);
        assert_eq!(store.len(), 200);

        store.set_open(false);
        assert_eq!(store.len(), CLOSED_CAP);
        assert!(store.has_older());
        assert_eq!(
            store.newest().unwrap().id,
            MessageId(200),
            "the newest are what reopening should draw"
        );
    }

    #[test]
    fn prepending_older_history_counts_only_what_was_new() {
        let mut store = MessageStore::new();
        store.set_open(true);
        store.replace(vec![message(10), message(11)], true);

        let added = store.prepend(vec![message(8), message(9), message(10)]);
        assert_eq!(added, 2, "an overlap was counted as new");
        assert_eq!(ids(&store), vec![8, 9, 10, 11]);
        assert!(!store.has_older(), "three back is not a full page");
    }

    #[test]
    fn an_update_changes_a_held_message_and_ignores_one_that_is_not() {
        let mut store = store_at_latest();
        let payload = serde_json::json!({"content": "edited"});
        assert!(store.update(MessageId(2), &payload));
        assert_eq!(store.get(MessageId(2)).unwrap().content, "edited");
        assert!(!store.update(MessageId(99), &payload));
    }

    #[test]
    fn a_deletion_removes_one_row_and_a_bulk_one_removes_what_it_holds() {
        let mut store = store_at_latest();
        assert!(store.remove(MessageId(2)));
        assert!(!store.remove(MessageId(2)));
        assert_eq!(ids(&store), vec![1, 3]);

        assert_eq!(
            store.remove_many(&[MessageId(1), MessageId(3), MessageId(99)]),
            2
        );
        assert!(store.is_empty());
    }

    #[test]
    fn reactions_are_added_removed_and_cleared() {
        let mut store = store_at_latest();
        let thumb = PartialEmoji {
            id: None,
            name: Some("👍".into()),
            animated: false,
        };
        let tada = PartialEmoji {
            id: None,
            name: Some("🎉".into()),
            animated: false,
        };

        assert!(store.add_reaction(MessageId(1), &thumb, true));
        store.add_reaction(MessageId(1), &thumb, false);
        store.add_reaction(MessageId(1), &tada, false);

        let message = store.get(MessageId(1)).unwrap();
        assert_eq!(message.reactions.len(), 2);
        assert_eq!(message.reactions[0].count, 2);
        assert!(message.reactions[0].me, "my own reaction was forgotten");

        store.remove_reaction(MessageId(1), &thumb, true);
        let message = store.get(MessageId(1)).unwrap();
        assert_eq!(message.reactions[0].count, 1);
        assert!(!message.reactions[0].me);

        // The last one takes the chip with it.
        store.remove_reaction(MessageId(1), &thumb, false);
        assert_eq!(store.get(MessageId(1)).unwrap().reactions.len(), 1);

        store.clear_reactions(MessageId(1));
        assert!(store.get(MessageId(1)).unwrap().reactions.is_empty());
        assert!(!store.clear_reactions(MessageId(99)));
    }

    #[test]
    fn removing_one_emoji_leaves_the_others() {
        let mut store = store_at_latest();
        let thumb = PartialEmoji {
            id: None,
            name: Some("👍".into()),
            animated: false,
        };
        let tada = PartialEmoji {
            id: None,
            name: Some("🎉".into()),
            animated: false,
        };
        store.add_reaction(MessageId(1), &thumb, true);
        store.add_reaction(MessageId(1), &tada, false);

        store.clear_emoji(MessageId(1), &thumb);
        let message = store.get(MessageId(1)).unwrap();
        assert_eq!(message.reactions.len(), 1);
        assert_eq!(message.reactions[0].emoji, tada);
    }

    #[test]
    fn a_failed_send_keeps_its_row() {
        let mut store = store_at_latest();
        store.add_pending(PendingSend::new(Nonce(5), "hi".into(), None, false));
        assert!(store.fail_pending(Nonce(5), "you are not in that channel"));
        assert!(store.pending()[0].failed());
        assert!(!store.fail_pending(Nonce(6), "no such send"));

        let taken = store.take_pending(Nonce(5)).expect("the row is there");
        assert_eq!(taken.content, "hi");
        assert!(store.pending().is_empty());
    }

    #[test]
    fn a_window_is_a_copy_that_outlives_the_lock() {
        let mut store = MessageStore::new();
        store.set_open(true);
        store.replace((1..=10u64).map(message).collect(), true);
        assert_eq!(
            store
                .window(2, 3)
                .iter()
                .map(|m| m.id.get())
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert_eq!(
            store
                .latest(3)
                .iter()
                .map(|m| m.id.get())
                .collect::<Vec<_>>(),
            vec![8, 9, 10]
        );
        assert_eq!(store.window(100, 5).len(), 0, "past the end is empty");
        assert_eq!(store.latest(100).len(), 10);
    }

    /// Every operation the store has, applied in whatever order proptest picks,
    /// with the invariants checked after each one. The invariants are the whole
    /// contract: sorted, unique, capped, and never evicting without saying so.
    #[derive(Debug, Clone)]
    enum Step {
        Receive(u64),
        Prepend(u64, u64),
        Update(u64),
        Remove(u64),
        Pending(u64),
        Echo(u64, u64),
        Open(bool),
        AtLatest(bool),
        React(u64),
    }

    fn check(store: &MessageStore) {
        let ids: Vec<u64> = store.iter().map(|m| m.id.get()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted, "the deque is not sorted and unique");
        assert!(
            store.len() <= store.cap(),
            "over the cap at {}",
            store.len()
        );

        let mut nonces: Vec<u64> = store.pending().iter().map(|p| p.nonce.0).collect();
        let before = nonces.len();
        nonces.sort_unstable();
        nonces.dedup();
        assert_eq!(before, nonces.len(), "two pending sends share a nonce");

        if store.at_latest() {
            assert_eq!(
                store.newer_hidden(),
                0,
                "counting hidden messages while showing the bottom"
            );
        }
    }

    use proptest::prelude::{any, Strategy as _};

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(96))]

        #[test]
        fn the_invariants_hold_under_any_sequence(
            steps in proptest::collection::vec(
                proptest::prop_oneof![
                    (1u64..80).prop_map(Step::Receive),
                    (1u64..80, 1u64..8).prop_map(|(a, b)| Step::Prepend(a, b)),
                    (1u64..80).prop_map(Step::Update),
                    (1u64..80).prop_map(Step::Remove),
                    (1u64..8).prop_map(Step::Pending),
                    (1u64..80, 1u64..8).prop_map(|(a, b)| Step::Echo(a, b)),
                    any::<bool>().prop_map(Step::Open),
                    any::<bool>().prop_map(Step::AtLatest),
                    (1u64..80).prop_map(Step::React),
                ],
                0..120,
            )
        ) {
            let mut store = MessageStore::new();
            store.set_open(true);
            store.set_at_latest(true);
            let thumb = PartialEmoji {
                id: None,
                name: Some("x".into()),
                animated: false,
            };

            for step in steps {
                match step {
                    Step::Receive(id) => {
                        store.receive(message(id));
                    }
                    Step::Prepend(id, count) => {
                        let page: Vec<Message> =
                            (0..count).map(|n| message(id.saturating_add(n))).collect();
                        store.prepend(page);
                    }
                    Step::Update(id) => {
                        store.update(MessageId(id), &serde_json::json!({"content": "e"}));
                    }
                    Step::Remove(id) => {
                        store.remove(MessageId(id));
                    }
                    Step::Pending(nonce) => {
                        store.add_pending(PendingSend::new(
                            Nonce(nonce),
                            "x".into(),
                            None,
                            false,
                        ));
                    }
                    Step::Echo(id, nonce) => {
                        store.receive(with_nonce(id, &nonce.to_string()));
                    }
                    Step::Open(open) => store.set_open(open),
                    Step::AtLatest(at) => store.set_at_latest(at),
                    Step::React(id) => {
                        store.add_reaction(MessageId(id), &thumb, true);
                    }
                }
                check(&store);
            }
        }
    }
}
