//! A Discord core that is not one.
//!
//! `starcord --replay testdata/gateway/session.json` runs the whole interface
//! against a recorded timeline: a READY, a connection that drops and comes
//! back, and whatever else the file says, played on a thread that answers
//! commands locally. It exists because the interface was written before the
//! gateway could be pointed at a real account, and it stays because it is the
//! only way to exercise a reconnect, an unread badge or an empty server
//! without arranging for one to happen.
//!
//! It is a **core**, not a UI helper, which is why it is the one file under
//! `src/ui/` that reaches into `discord::state`. Everything the real core does
//! to `State`, it does the same way — `payload::decode` then `state::apply` —
//! so a fixture that plays here is a fixture that would play there, and a
//! dispatch this cannot apply is one the real core could not either.
//!
//! ## The file
//!
//! ```json
//! {
//!   "steps": [
//!     { "at_ms": 0,     "dispatch": "READY", "data": { … } },
//!     { "at_ms": 20000, "connection": "reconnecting", "reason": "…" },
//!     { "at_ms": 25000, "connection": "ready" }
//!   ]
//! }
//! ```
//!
//! `at_ms` is measured from the moment the login is accepted, not from
//! startup, so a drop at twenty seconds is twenty seconds of somebody using
//! it.
//!
//! ## Messages
//!
//! A timeline is a poor way to describe a conversation that is already there
//! when a channel is opened, so the messages come from a second file named by
//! `"messages"` and resolved beside the session. Opening a channel fills its
//! store from that file's `messages`; `LoadOlder` prepends its `older` once;
//! sending appends an echo and answers with a `SendResult`. Everything goes
//! through `MessageStore`, the same type the real core's `ops` write to, so a
//! page that behaves here behaves there.

use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use serde::Deserialize;

use crate::discord::auth::TokenStoreKind;
use crate::discord::gateway::payload;
use crate::discord::handle::{AuthEvent, Connection, HandleParts, MessagesChange, Nonce, Note};
use crate::discord::model::Message;
use crate::discord::snowflake::{ChannelId, MessageId};
use crate::discord::state::{apply, State};
use crate::discord::{Command, Event, Handle};

/// How far ahead of the clock a step may be before the thread sleeps rather
/// than spinning. Small enough that a command is still answered promptly.
const TICK: Duration = Duration::from_millis(25);

/// The event side, with the real core's drop bookkeeping.
///
/// A copy of `handle::EventSink`, which has the same three fields and no
/// constructor. One `EventSink::new(tx, dropped)` upstream deletes this; until
/// there is one, a replay that quietly dropped events without ever sending an
/// `Event::Refresh` would be a replay that cannot reproduce the one case the
/// counter exists for.
struct Sink {
    tx: crossbeam_channel::Sender<Event>,
    dropped: Arc<AtomicU64>,
    needs_refresh: std::sync::atomic::AtomicBool,
}

impl Sink {
    fn send(&self, event: Event) {
        use std::sync::atomic::Ordering;
        if self.needs_refresh.swap(false, Ordering::Relaxed)
            && self.tx.try_send(Event::Refresh).is_err()
        {
            self.needs_refresh.store(true, Ordering::Relaxed);
        }
        if self.tx.try_send(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            self.needs_refresh.store(true, Ordering::Relaxed);
        }
    }
}

/// One thing that happens at a moment.
#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    /// Milliseconds after the login was accepted.
    #[serde(default)]
    pub at_ms: u64,
    /// A gateway dispatch name — `READY`, `MESSAGE_CREATE` — decoded and
    /// applied exactly as the real gateway would.
    #[serde(default)]
    pub dispatch: Option<String>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    /// A connection change, for the drops a recording cannot contain.
    #[serde(default)]
    pub connection: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// A line in the status bar.
    #[serde(default)]
    pub note: Option<String>,
}

/// One channel's messages, as the fixture file states them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Page {
    /// The window a channel opens on, oldest first.
    #[serde(default)]
    pub messages: Vec<Message>,
    /// What `LoadOlder` answers with, once.
    #[serde(default)]
    pub older: Vec<Message>,
}

/// The message fixture: channels by id.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Conversations {
    #[serde(default)]
    pub channels: std::collections::HashMap<String, Page>,
}

impl Conversations {
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    fn page(&self, channel: ChannelId) -> Option<&Page> {
        self.channels.get(&channel.to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub steps: Vec<Step>,
    /// A file of messages, beside this one. Relative to the session file, so a
    /// fixture can be copied as a pair.
    #[serde(default)]
    pub messages: Option<String>,
    /// Filled in by [`Session::read`], because the path is only known there.
    #[serde(skip)]
    pub conversations: Conversations,
}

impl Session {
    pub fn parse(text: &str) -> Result<Self> {
        let session: Session = serde_json::from_str(text).context("parsing the replay session")?;
        anyhow::ensure!(!session.steps.is_empty(), "the session has no steps in it");
        Ok(session)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut session = Self::parse(&text)?;
        if let Some(name) = session.messages.clone() {
            let beside = path.parent().unwrap_or(Path::new(".")).join(&name);
            session.conversations = Conversations::read(&beside)?;
        }
        Ok(session)
    }

    /// The first READY in the file, for the user the login reports.
    fn first_ready(&self) -> Option<&serde_json::Value> {
        self.steps
            .iter()
            .find(|s| s.dispatch.as_deref() == Some("READY"))
            .and_then(|s| s.data.as_ref())
    }
}

/// A handle whose core does nothing at all.
///
/// For tests that want an `App` and not a conversation. The second value has
/// to be held: dropping it closes the event channel, and a closed channel is a
/// core that has gone away.
pub fn silent() -> (Handle, Idle) {
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(16);
    let (event_tx, event_rx) = crossbeam_channel::bounded(16);
    let handle = Handle::from_parts(HandleParts {
        commands: command_tx,
        events: event_rx,
        state: Arc::new(RwLock::new(State::new())),
        status: Arc::new(ArcSwap::from_pointee(Connection::LoggedOut)),
        thread: None,
        dropped: Arc::new(AtomicU64::new(0)),
    });
    (
        handle,
        Idle {
            _commands: command_rx,
            _events: event_tx,
        },
    )
}

/// The ends of a silent core's channels, kept alive.
pub struct Idle {
    _commands: tokio::sync::mpsc::Receiver<Command>,
    _events: crossbeam_channel::Sender<Event>,
}

/// A core that is already at a moment in the timeline, with no thread at all.
///
/// Everything with `at_ms <= upto` is applied before the handle is handed
/// back, and the status is whatever the last of them left. For snapshots and
/// for tests that want a populated `State` rather than a conversation: a
/// replay runs on a clock, and a test that waited on one would be a test that
/// renders a different frame on a loaded machine.
pub fn loaded(session: &Session, upto_ms: u64) -> (Handle, Idle) {
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(16);
    let (event_tx, event_rx) = crossbeam_channel::bounded(4096);
    let state = Arc::new(RwLock::new(State::new()));
    let status = Arc::new(ArcSwap::from_pointee(Connection::LoggedOut));
    let dropped = Arc::new(AtomicU64::new(0));

    let sink = Sink {
        tx: event_tx.clone(),
        dropped: Arc::clone(&dropped),
        needs_refresh: std::sync::atomic::AtomicBool::new(false),
    };
    for step in session.steps.iter().filter(|s| s.at_ms <= upto_ms) {
        play(step, &sink, &state, &status);
    }
    // The conversations are put in as well, because a channel's history is
    // not something a timeline can describe: it is already there when the
    // channel is opened. This is what the command loop would have done on the
    // first `OpenChannel`, done up front so a snapshot does not depend on a
    // round trip.
    {
        let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
        for (id, page) in &session.conversations.channels {
            let Ok(id) = id.parse::<u64>() else { continue };
            let store = guard.messages_mut(ChannelId(id));
            store.replace(page.messages.clone(), true);
            store.set_has_older(!page.older.is_empty());
            store.set_open(true);
        }
        guard.touch();
    }

    // The events are thrown away: the caller is about to read `State`, which
    // is the truth, and a queue of notifications about how it got there would
    // only make the first frame depend on how many of them fitted.
    while event_rx.try_recv().is_ok() {}

    (
        Handle::from_parts(HandleParts {
            commands: command_tx,
            events: event_rx,
            state,
            status,
            thread: None,
            dropped,
        }),
        Idle {
            _commands: command_rx,
            _events: event_tx,
        },
    )
}

/// A handle backed by a recorded session.
pub fn replay(path: &Path) -> Result<Handle> {
    let session = Session::read(path)?;
    Ok(spawn(session))
}

/// Start the replay thread and hand back the handle to it.
pub fn spawn(session: Session) -> Handle {
    let (command_tx, mut command_rx) = tokio::sync::mpsc::channel(256);
    let (event_tx, event_rx) = crossbeam_channel::bounded(4096);
    let state = Arc::new(RwLock::new(State::new()));
    let status = Arc::new(ArcSwap::from_pointee(Connection::LoggedOut));
    let dropped = Arc::new(AtomicU64::new(0));

    let sink = Sink {
        tx: event_tx,
        dropped: Arc::clone(&dropped),
        needs_refresh: std::sync::atomic::AtomicBool::new(false),
    };

    let thread = {
        let state = Arc::clone(&state);
        let status = Arc::clone(&status);
        std::thread::Builder::new()
            .name("starcord-replay".into())
            .spawn(move || run(session, &mut command_rx, sink, state, status))
            .expect("spawning the replay thread")
    };

    Handle::from_parts(HandleParts {
        commands: command_tx,
        events: event_rx,
        state,
        status,
        thread: Some(thread),
        dropped,
    })
}

fn run(
    session: Session,
    commands: &mut tokio::sync::mpsc::Receiver<Command>,
    sink: Sink,
    state: Arc<RwLock<State>>,
    status: Arc<ArcSwap<Connection>>,
) {
    let user = session
        .first_ready()
        .and_then(|d| d.get("user"))
        .and_then(|u| serde_json::from_value(u.clone()).ok())
        .map(Arc::new);

    // Nothing happens until somebody signs in, because the login screen is
    // what a person sees first and a replay that skipped it would not exercise
    // the thing it is there to exercise. Any token is accepted: the point is
    // the interface, not the credential.
    let conversations = session.conversations.clone();
    let mut served = Served::default();
    let mut signed_in = false;
    while !signed_in {
        match commands.blocking_recv() {
            Some(Command::LoginWithToken(_)) | Some(Command::Connect) => signed_in = true,
            Some(Command::Shutdown) | None => return,
            Some(_) => {}
        }
    }

    set(&status, &sink, Connection::Connecting);
    if let Some(user) = &user {
        sink.send(Event::Auth(AuthEvent::LoggedIn {
            user: Arc::clone(user),
            stored_in: TokenStoreKind::Memory,
        }));
    }
    set(&status, &sink, Connection::Identifying);

    let started = Instant::now();
    let mut next = 0usize;

    loop {
        // Commands first: a person pressing a key should not wait on the
        // timeline.
        loop {
            match commands.try_recv() {
                Ok(Command::Shutdown) => return,
                Ok(command) => answer(
                    command,
                    &sink,
                    &state,
                    &conversations,
                    &mut served,
                    user.as_deref(),
                ),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                // The handle was dropped.
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
            }
        }

        let elapsed = started.elapsed();
        while next < session.steps.len() {
            let step = &session.steps[next];
            if Duration::from_millis(step.at_ms) > elapsed {
                break;
            }
            play(step, &sink, &state, &status);
            next += 1;
        }

        // Every step played and nothing left to do but answer commands.
        std::thread::sleep(TICK);
    }
}

fn play(step: &Step, sink: &Sink, state: &Arc<RwLock<State>>, status: &Arc<ArcSwap<Connection>>) {
    if let (Some(name), Some(data)) = (step.dispatch.as_deref(), step.data.as_ref()) {
        let raw = serde_json::value::to_raw_value(data).ok();
        let dispatch = payload::decode(name, raw.as_deref());
        let events = {
            let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
            apply::apply(&mut guard, dispatch)
        };
        let ready = name == "READY";
        for event in events {
            sink.send(event);
        }
        if ready {
            set(
                status,
                sink,
                Connection::Ready {
                    since: Instant::now(),
                    resumed: false,
                },
            );
            // The status says the socket is up; this says `State` is worth
            // drawing. The UI clears the login screen on it, which is why it
            // is sent once, after READY, and not on every reconnection.
            sink.send(Event::Ready);
        }
    }

    if let Some(word) = step.connection.as_deref() {
        let reason = step.reason.clone().unwrap_or_else(|| "the replay".into());
        let connection = match word {
            "connecting" => Connection::Connecting,
            "identifying" => Connection::Identifying,
            "resuming" => Connection::Resuming,
            "reconnecting" => Connection::Reconnecting {
                attempt: 1,
                next_in: Duration::from_secs(5),
                reason,
            },
            "offline" => Connection::Offline,
            "logged-out" => Connection::LoggedOut,
            "rejected" => Connection::AuthFailed(reason),
            _ => Connection::Ready {
                since: Instant::now(),
                resumed: true,
            },
        };
        set(status, sink, connection);
    }

    if let Some(text) = &step.note {
        sink.send(Event::Note(Note::info(text.clone())));
    }
}

/// What has already been handed out, so a page arrives once.
#[derive(Debug, Default)]
struct Served {
    opened: std::collections::HashSet<ChannelId>,
    older: std::collections::HashSet<ChannelId>,
    /// The id the next echoed send gets. Above every fixture id, and climbing,
    /// so two sends are in order.
    next_id: u64,
}

impl Served {
    fn mint(&mut self) -> MessageId {
        if self.next_id == 0 {
            self.next_id = 590_000_000_000_000_000;
        }
        self.next_id += 1;
        MessageId(self.next_id)
    }
}

/// Answer a command the way the real core would, as far as a file can.
fn answer(
    command: Command,
    sink: &Sink,
    state: &Arc<RwLock<State>>,
    conversations: &Conversations,
    served: &mut Served,
    me: Option<&crate::discord::model::User>,
) {
    match command {
        Command::OpenChannel(channel) => {
            // The events are sent whether or not there is anything to send:
            // the UI's spinner is driven by them, and a spinner that never
            // stops is exactly the bug this should be able to reproduce.
            sink.send(Event::Messages(channel, MessagesChange::Loading(true)));
            if served.opened.insert(channel) {
                if let Some(page) = conversations.page(channel) {
                    let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
                    let store = guard.messages_mut(channel);
                    store.replace(page.messages.clone(), true);
                    store.set_has_older(!page.older.is_empty());
                    store.set_open(true);
                    guard.touch();
                }
            }
            sink.send(Event::Messages(channel, MessagesChange::Loading(false)));
            sink.send(Event::Messages(channel, MessagesChange::Replaced));
        }
        Command::LoadOlder(channel) => {
            sink.send(Event::Messages(channel, MessagesChange::Loading(true)));
            let mut added = 0usize;
            if served.older.insert(channel) {
                if let Some(page) = conversations.page(channel) {
                    if !page.older.is_empty() {
                        let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
                        let store = guard.messages_mut(channel);
                        added = store.prepend(page.older.clone());
                        store.set_has_older(false);
                        guard.touch();
                    }
                }
            }
            sink.send(Event::Messages(channel, MessagesChange::Loading(false)));
            if added > 0 {
                sink.send(Event::Messages(channel, MessagesChange::Prepended(added)));
            }
        }
        Command::LoadNewer(channel) => {
            sink.send(Event::Messages(channel, MessagesChange::Loading(false)));
        }
        Command::SendMessage {
            channel,
            content,
            reply_to,
            ..
        } => {
            // The echo the gateway would send, built here so the optimistic
            // row is replaced by a real message rather than left pending.
            let id = served.mint();
            let mut message = Message {
                id,
                channel_id: channel,
                content,
                timestamp: Some(jiff::Timestamp::now()),
                ..Message::default()
            };
            if let Some(me) = me {
                message.author = me.clone();
            }
            if let Some(to) = reply_to {
                message.kind = crate::discord::model::MessageKind::Reply;
                message.message_reference = Some(crate::discord::model::MessageReference {
                    message_id: Some(to),
                    channel_id: Some(channel),
                    ..Default::default()
                });
            }
            {
                let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
                guard.messages_mut(channel).receive(message);
                guard.touch();
            }
            sink.send(Event::Messages(channel, MessagesChange::Appended(id)));
            sink.send(Event::SendResult {
                nonce: Nonce(0),
                result: Ok(id),
            });
        }
        Command::EditMessage {
            channel,
            message,
            content,
        } => {
            let changed = {
                let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
                let payload = serde_json::json!({
                    "content": content,
                    "edited_timestamp": jiff::Timestamp::now().to_string(),
                });
                let changed = guard.messages_mut(channel).update(message, &payload);
                if changed {
                    guard.touch();
                }
                changed
            };
            if changed {
                sink.send(Event::Messages(channel, MessagesChange::Updated(message)));
            }
        }
        Command::DeleteMessage { channel, message } => {
            let removed = {
                let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
                let removed = guard.messages_mut(channel).remove(message);
                if removed {
                    guard.touch();
                }
                removed
            };
            if removed {
                sink.send(Event::Messages(channel, MessagesChange::Removed(message)));
            }
        }
        Command::MarkRead { channel, .. } => sink.send(Event::ReadState(channel)),
        Command::Logout => {
            sink.send(Event::Auth(AuthEvent::LoggedOut));
        }
        // Everything else is accepted and does nothing, which is what a
        // replay can honestly do about it.
        _ => {}
    }
}

fn set(status: &Arc<ArcSwap<Connection>>, sink: &Sink, connection: Connection) {
    let shared = Arc::new(connection);
    status.store(Arc::clone(&shared));
    sink.send(Event::Status(shared));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::auth::Token;

    fn fixture() -> Session {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/gateway/session.json");
        Session::read(&path).expect("the replay fixture has to parse")
    }

    /// Wait for a predicate, or give up. A replay runs on a clock, and a test
    /// that slept a fixed time would be a test that fails on a loaded machine.
    fn wait_for(handle: &Handle, mut f: impl FnMut(&Event) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            for event in handle.drain() {
                if f(&event) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn the_committed_session_parses_and_has_a_ready() {
        let session = fixture();
        assert!(session.first_ready().is_some(), "no READY in the timeline");
        assert!(
            session.steps.windows(2).all(|w| w[0].at_ms <= w[1].at_ms),
            "the steps are not in time order, so they would play out of order"
        );
    }

    /// Every dispatch in the file decodes into something the state machine
    /// knows. A fixture with a typo in an event name is a fixture that plays
    /// silently and teaches nothing.
    #[test]
    fn every_dispatch_in_the_file_is_one_the_core_understands() {
        for step in fixture().steps {
            let Some(name) = step.dispatch else { continue };
            let data = step.data.expect("a dispatch with no payload");
            let raw = serde_json::value::to_raw_value(&data).unwrap();
            let decoded = payload::decode(&name, Some(&raw));
            assert!(
                !matches!(
                    decoded,
                    payload::Dispatch::Unknown { .. } | payload::Dispatch::Malformed { .. }
                ),
                "{name} did not decode into anything this client applies"
            );
        }
    }

    /// The fixture has to carry what the first milestone draws: servers,
    /// categories, channels, DMs and friends.
    #[test]
    fn the_fixture_fills_the_first_screen() {
        let handle = spawn(fixture());
        handle.send(Command::LoginWithToken(
            Token::new("replay.token.long.enough").unwrap(),
        ));
        assert!(
            wait_for(&handle, |e| matches!(e, Event::Ready)),
            "the replay never reached READY"
        );

        let state = handle.state();
        assert!(state.guild_count() >= 3, "{} servers", state.guild_count());
        assert!(state.dm_count() >= 2, "{} DMs", state.dm_count());
        assert!(state.me().is_some());

        let first = state.guilds_ordered()[0].id;
        let channels = state.channels_ordered(first);
        assert!(channels.len() >= 4);
        assert!(
            channels
                .iter()
                .any(|c| c.kind == crate::discord::model::ChannelKind::GuildCategory),
            "no category, so the fold cannot be exercised"
        );
        assert!(
            state
                .guilds_ordered()
                .iter()
                .flat_map(|g| state.channels_ordered(g.id))
                .any(|c| state.unread(c.id).notable()),
            "nothing is unread, so the unread marks are never drawn"
        );
    }

    /// The drop and the recovery, which is the reason the timeline exists.
    #[test]
    fn the_connection_drops_and_comes_back() {
        let session = fixture();
        let drop_at = session
            .steps
            .iter()
            .find(|s| s.connection.as_deref() == Some("reconnecting"))
            .map(|s| s.at_ms)
            .expect("the timeline has to contain a drop");
        let back_at = session
            .steps
            .iter()
            .find(|s| s.at_ms > drop_at && s.connection.as_deref() == Some("ready"))
            .map(|s| s.at_ms)
            .expect("and a recovery after it");
        assert!(back_at > drop_at);
        assert_eq!(drop_at, 20_000);
        assert_eq!(back_at, 25_000);
    }

    /// Nothing happens until somebody signs in, because the login screen is
    /// the first thing this is for.
    #[test]
    fn nothing_plays_before_the_login() {
        let handle = spawn(fixture());
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(handle.drain().count(), 0);
        assert!(!handle.status().is_ready());
    }

    /// Opening a channel answers, so the spinner stops.
    #[test]
    fn opening_a_channel_is_answered() {
        let handle = spawn(fixture());
        handle.send(Command::LoginWithToken(
            Token::new("replay.token.long.enough").unwrap(),
        ));
        assert!(wait_for(&handle, |e| matches!(e, Event::Ready)));

        let channel = handle
            .state()
            .channels_ordered(handle.state().guilds_ordered()[0].id)
            .into_iter()
            .find(|c| c.kind.is_text())
            .expect("a text channel")
            .id;
        handle.send(Command::OpenChannel(channel));
        assert!(wait_for(&handle, |e| matches!(
            e,
            Event::Messages(_, MessagesChange::Loading(false))
        )));
    }

    /// The same fixture, with no clock in it.
    #[test]
    fn a_loaded_core_is_ready_before_anybody_asks() {
        let (handle, _idle) = loaded(&fixture(), 1_000);
        assert!(handle.status().is_ready());
        assert!(handle.state().me().is_some());
        assert!(handle.state().guild_count() >= 3);
        assert_eq!(handle.drain().count(), 0, "it queues nothing to replay");

        // And the presences from the first second are in, which is what makes
        // a drawn frame the same one every time.
        let alex = handle
            .state()
            .dms_ordered()
            .iter()
            .flat_map(|c| c.recipient_ids())
            .find(|id| {
                handle.state().presence(*id) == crate::discord::model::PresenceStatus::Online
            })
            .is_some();
        assert!(alex, "no presence was applied");
    }

    /// Stopping before a step means the step did not happen.
    #[test]
    fn a_loaded_core_stops_where_it_was_told_to() {
        let (early, _idle) = loaded(&fixture(), 0);
        assert!(early.status().is_ready(), "READY is at zero");
        let any_online = early
            .state()
            .dms_ordered()
            .iter()
            .flat_map(|c| c.recipient_ids())
            .any(|id| early.state().presence(id) == crate::discord::model::PresenceStatus::Online);
        assert!(!any_online, "the presences are all after zero");
    }

    #[test]
    fn a_session_with_no_steps_is_refused() {
        assert!(Session::parse("{\"steps\":[]}").is_err());
        assert!(Session::parse("not json").is_err());
    }

    #[test]
    fn the_silent_core_says_nothing() {
        let (handle, _idle) = silent();
        assert_eq!(handle.drain().count(), 0);
        assert!(handle.state().me().is_none());
        handle.send(Command::Connect);
    }
}
