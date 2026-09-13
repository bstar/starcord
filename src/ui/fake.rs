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
//! ## Pictures
//!
//! `"media"` maps a name to a file beside the session: an avatar or a server
//! icon by its hash, a custom emoji by its id, and anything addressed by a URL
//! by the last segment of it. `FetchMedia` is answered by decoding that file
//! through the same [`crate::discord::media::decode`] the core uses, so a
//! picture that draws here is one that would draw there.
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
use crate::discord::handle::{
    AuthEvent, Connection, HandleParts, MessagesChange, Nonce, Note, SearchPage,
};
use crate::discord::media::{decode, Decoded, MediaError, MediaKey, Want};
use crate::discord::model::{GifPage, GifResult, Message};
use crate::discord::snowflake::{ChannelId, MessageId};
use crate::discord::state::{apply, State};
use crate::discord::{Command, Event, Handle};

/// How far ahead of the clock a step may be before the thread sleeps rather
/// than spinning. Small enough that a command is still answered promptly.
const TICK: Duration = Duration::from_millis(25);

/// The scripted scan: a phone reads the code, and then confirms on it.
const QR_SCAN_AFTER: Duration = Duration::from_secs(3);
const QR_CONFIRM_AFTER: Duration = Duration::from_secs(6);

/// What the code encodes. `.invalid` rather than `discord.com`, so a code
/// photographed off a test run leads nowhere: this one is a picture of a
/// pattern and not a handshake anybody can join.
const QR_URL_BASE: &str = "https://discord.invalid/ra/";
const QR_FINGERPRINT: &str = "0123456789abcdef0123456789abcdef";

/// A code-shaped matrix for the replay.
///
/// Not a QR code: encoding one properly is the core's job and needs the
/// `qrcode` crate, which the fake has no business reaching for. What this has
/// is the three finder squares and a body derived from the fingerprint, which
/// is everything the drawing has to get right and nothing a camera will
/// mistake for a real one.
fn qr_matrix(seed: &str) -> Vec<Vec<bool>> {
    const N: usize = 25;
    let bytes = seed.as_bytes();
    let mut m = vec![vec![false; N]; N];
    for (y, row) in m.iter_mut().enumerate() {
        for (x, cell) in row.iter_mut().enumerate() {
            let b = bytes[(x * 7 + y * 13) % bytes.len()] as usize;
            *cell = (b + x * 3 + y * 5) % 5 < 2;
        }
    }
    for (ox, oy) in [(0, 0), (N - 7, 0), (0, N - 7)] {
        for y in 0..8 {
            for x in 0..8 {
                let (px, py) = (ox + x, oy + y);
                if px >= N || py >= N {
                    continue;
                }
                let inside = x < 7 && y < 7;
                let edge = x == 0 || y == 0 || x == 6 || y == 6;
                let core = (2..=4).contains(&x) && (2..=4).contains(&y);
                m[py][px] = inside && (edge || core);
            }
        }
    }
    m
}

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

/// The fixture's pictures, by the name a [`MediaKey`] boils down to.
#[derive(Debug, Clone, Default)]
pub struct Media {
    files: std::collections::HashMap<String, Arc<Vec<u8>>>,
}

impl Media {
    /// The name a key is looked up by.
    ///
    /// A hash for the things Discord addresses by content, an id for an emoji,
    /// and the last segment of the path for everything carrying a URL --
    /// signature parameters and all, because the fixture's URLs are made up
    /// and the file name is the only part of one worth matching on.
    pub fn name(key: &MediaKey) -> String {
        match key {
            MediaKey::Avatar { hash, .. } | MediaKey::GuildIcon { hash, .. } => hash.clone(),
            MediaKey::Emoji { id, .. } => format!("emoji/{id}"),
            MediaKey::Sticker { id } => format!("sticker/{id}"),
            MediaKey::Attachment { url, .. }
            | MediaKey::EmbedImage { url }
            | MediaKey::Gif { url } => url
                .split('?')
                .next()
                .unwrap_or(url)
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string(),
        }
    }

    pub fn bytes(&self, key: &MediaKey) -> Option<Arc<Vec<u8>>> {
        self.files.get(&Self::name(key)).cloned()
    }

    /// Answer one request the way the media task would: decode to the size
    /// that was asked for, or say why not.
    pub fn answer(&self, key: &MediaKey, want: Want) -> Result<Arc<Decoded>, MediaError> {
        let bytes = self
            .bytes(key)
            .ok_or_else(|| MediaError::Unsupported("the fixture has no such picture".into()))?;
        match want {
            Want::Bytes => Ok(Arc::new(Decoded::Bytes(bytes))),
            Want::Decoded { max_w, max_h } => {
                Ok(Arc::new(decode::decode(&bytes, max_w, max_h)?.decoded))
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Files beside this one, by the name a key boils down to.
    #[serde(default)]
    pub media: std::collections::HashMap<String, String>,
    /// Filled in by [`Session::read`], for the same reason as the messages.
    #[serde(skip)]
    pub pictures: Media,
    /// A file of messages, beside this one. Relative to the session file, so a
    /// fixture can be copied as a pair.
    #[serde(default)]
    pub messages: Option<String>,
    /// What the GIF picker is answered with. Everything in it is a URL the
    /// `media` table above also names, so the tiles are real pictures.
    #[serde(default)]
    pub gifs: Vec<GifResult>,
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
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        if let Some(name) = session.messages.clone() {
            session.conversations = Conversations::read(&dir.join(&name))?;
        }
        for (name, file) in &session.media {
            let at = dir.join(file);
            let bytes = std::fs::read(&at).with_context(|| format!("reading {}", at.display()))?;
            session.pictures.files.insert(name.clone(), Arc::new(bytes));
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
    let state = Arc::new(RwLock::new(State::new()));
    let handle = Handle::from_parts(HandleParts {
        commands: command_tx,
        events: event_rx,
        state: Arc::clone(&state),
        status: Arc::new(ArcSwap::from_pointee(Connection::LoggedOut)),
        thread: None,
        dropped: Arc::new(AtomicU64::new(0)),
    });
    (
        handle,
        Idle {
            commands: command_rx,
            events: event_tx,
            pictures: Media::default(),
            gifs: Vec::new(),
            conversations: Conversations::default(),
            state,
            served: Served::default(),
        },
    )
}

/// The ends of a core's channels, kept alive.
///
/// Holding it is what keeps the handle's channels open; a dropped one is a
/// core that has gone away. [`Idle::pump`] is also the way a test with no
/// thread answers what the interface asked for, which is how a snapshot gets
/// real pictures in it without waiting on a clock.
pub struct Idle {
    commands: tokio::sync::mpsc::Receiver<Command>,
    events: crossbeam_channel::Sender<Event>,
    pictures: Media,
    gifs: Vec<GifResult>,
    conversations: Conversations,
    /// The same `State` the handle reads, so a test can put something in it
    /// that no command produces -- an upload halfway up the wire, say.
    state: Arc<RwLock<State>>,
    served: Served,
}

impl Idle {
    /// The `State` behind the handle, for a test that has to put something
    /// into it that no command produces.
    pub fn state(&self) -> &Arc<RwLock<State>> {
        &self.state
    }

    /// Answer every command waiting, as far as a file can. Returns how many.
    ///
    /// The same [`answer`] the replay thread uses, so a test and a run see the
    /// same core. Nothing here is on a clock: a test that waited on one would
    /// be a test that renders a different frame on a loaded machine.
    pub fn pump(&mut self) -> usize {
        let sink = Sink {
            tx: self.events.clone(),
            dropped: Arc::new(AtomicU64::new(0)),
            needs_refresh: std::sync::atomic::AtomicBool::new(false),
        };
        let mut answered = 0;
        while let Ok(command) = self.commands.try_recv() {
            let before = self.events.len();
            answer(
                command,
                &sink,
                &self.state,
                &self.conversations,
                &self.pictures,
                &self.gifs,
                &mut self.served,
                None,
            );
            if self.events.len() > before {
                answered += 1;
            }
        }
        answered
    }
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

    let held = Arc::clone(&state);
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
            commands: command_rx,
            events: event_tx,
            pictures: session.pictures.clone(),
            gifs: session.gifs.clone(),
            conversations: session.conversations.clone(),
            state: held,
            served: Served::default(),
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
        .and_then(|u| serde_json::from_value::<crate::discord::model::User>(u.clone()).ok())
        .map(Arc::new);

    // Nothing happens until somebody signs in, because the login screen is
    // what a person sees first and a replay that skipped it would not exercise
    // the thing it is there to exercise. Any token is accepted: the point is
    // the interface, not the credential.
    let conversations = session.conversations.clone();
    let pictures = session.pictures.clone();
    let gifs = session.gifs.clone();
    let mut served = Served::default();
    let mut signed_in = false;
    // The scan, on a clock, because the whole of what the code screen does is
    // wait: a code arrives, somebody's phone reads it three seconds later, and
    // the phone confirms three seconds after that.
    let mut qr: Option<Instant> = None;
    let mut scanned = false;
    // There is no stored token in a replay, and saying so is what puts the
    // interface on the code screen without anybody pressing anything.
    sink.send(Event::Auth(AuthEvent::NeedsLogin));
    while !signed_in {
        match commands.try_recv() {
            Ok(Command::LoginWithToken(_)) | Ok(Command::Connect) => signed_in = true,
            Ok(Command::StartRemoteAuth) => {
                qr = Some(Instant::now());
                scanned = false;
                sink.send(Event::Auth(AuthEvent::QrReady {
                    url: format!("{QR_URL_BASE}{QR_FINGERPRINT}"),
                    fingerprint: QR_FINGERPRINT.into(),
                    expires_in: Duration::from_secs(120),
                    matrix: qr_matrix(QR_FINGERPRINT),
                }));
            }
            Ok(Command::CancelRemoteAuth) => qr = None,
            Ok(Command::Shutdown) => return,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
            Ok(_) | Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        }
        if let Some(at) = qr {
            let waited = at.elapsed();
            if !scanned && waited >= QR_SCAN_AFTER {
                scanned = true;
                let who = user
                    .as_ref()
                    .map(|u| u.display_name().to_string())
                    .unwrap_or_else(|| "somebody".into());
                sink.send(Event::Auth(AuthEvent::QrScanned {
                    username: who,
                    avatar: None,
                }));
            }
            if waited >= QR_CONFIRM_AFTER {
                signed_in = true;
            }
        }
        if !signed_in {
            std::thread::sleep(TICK);
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
                    &pictures,
                    &gifs,
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
#[allow(clippy::too_many_arguments)]
fn answer(
    command: Command,
    sink: &Sink,
    state: &Arc<RwLock<State>>,
    conversations: &Conversations,
    pictures: &Media,
    gifs: &[GifResult],
    served: &mut Served,
    me: Option<&crate::discord::model::User>,
) {
    match command {
        Command::FetchMedia(request) => {
            // Decoded on this thread rather than on a pool: the fixture's
            // pictures are a kilobyte each, and a replay that spawned threads
            // to resize them would be modelling the wrong thing.
            let result = pictures.answer(&request.key, request.want);
            sink.send(Event::Media {
                key: request.key,
                result,
            });
        }
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
        // The picker and the search box ask, and a replay that answered
        // neither would be a replay that cannot show either of them working.
        Command::GifTrending { id } | Command::GifSuggest { id, .. } => {
            sink.send(Event::Gifs {
                id,
                result: Ok(GifPage {
                    results: gifs.to_vec(),
                    categories: Vec::new(),
                    suggestions: Vec::new(),
                }),
            });
        }
        Command::GifSearch { id, query } => {
            let needle = query.trim().to_lowercase();
            let results: Vec<GifResult> = gifs
                .iter()
                .filter(|g| needle.is_empty() || g.title.to_lowercase().contains(&needle))
                .cloned()
                .collect();
            sink.send(Event::Gifs {
                id,
                result: Ok(GifPage {
                    results,
                    categories: Vec::new(),
                    suggestions: Vec::new(),
                }),
            });
        }
        Command::Search { id, scope, query } => {
            let needle = query.content.trim().to_lowercase();
            let only = match scope {
                crate::discord::handle::SearchScope::Channel(channel) => Some(channel),
                crate::discord::handle::SearchScope::Guild(_) => None,
            };
            let mut found: Vec<Arc<Message>> = Vec::new();
            for (raw, page) in &conversations.channels {
                let Ok(id) = raw.parse::<u64>() else { continue };
                if only.is_some_and(|c| c.0 != id) {
                    continue;
                }
                for msg in page.older.iter().chain(page.messages.iter()) {
                    if msg.content.to_lowercase().contains(&needle) {
                        found.push(Arc::new(msg.clone()));
                    }
                }
            }
            // Newest first, which is what Discord's own search returns.
            found.sort_by_key(|m| std::cmp::Reverse(m.id));
            let total = found.len() as u32;
            let offset = query.offset.min(total);
            let page: Vec<Arc<Message>> =
                found.into_iter().skip(offset as usize).take(25).collect();
            sink.send(Event::Search {
                id,
                result: Ok(SearchPage {
                    total,
                    messages: page,
                    offset,
                }),
            });
        }
        Command::JumpTo { channel, message } => {
            // The store is left knowing it is not at the bottom, which is what
            // makes `G` a way back to the present rather than a no-op.
            if let Some(page) = conversations.page(channel) {
                let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
                let store = guard.messages_mut(channel);
                let mut all = page.older.clone();
                all.extend(page.messages.clone());
                store.replace(all, false);
                store.set_open(true);
                guard.touch();
            }
            let _ = message;
            sink.send(Event::Messages(channel, MessagesChange::Replaced));
        }
        Command::AddReaction {
            channel,
            message,
            emoji,
        }
        | Command::RemoveReaction {
            channel,
            message,
            emoji,
        } => {
            // The chip toggles, whichever of the two commands arrived: the
            // gateway would echo exactly one of them and the replay is the
            // gateway here.
            let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
            let partial = emoji.as_partial();
            let mine = guard
                .messages(channel)
                .and_then(|s| s.get(message))
                .map(|m| m.reactions.iter().any(|r| r.emoji == partial && r.me))
                .unwrap_or(false);
            let store = guard.messages_mut(channel);
            if mine {
                store.remove_reaction(message, &partial, true);
            } else {
                store.add_reaction(message, &partial, true);
            }
            guard.touch();
            drop(guard);
            sink.send(Event::Messages(channel, MessagesChange::Reactions(message)));
        }
        Command::OpenExternal { url, .. } => {
            sink.send(Event::Note(Note::info(format!("would open {url}"))));
        }
        Command::SendMessage {
            channel,
            content,
            reply_to,
            attachments,
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
            // Whatever was attached comes back on the echo, as Discord's own
            // would: a chip that vanished on send would look like a file that
            // never went.
            let nonce = Nonce(served.next_id);
            for (n, upload) in attachments.iter().enumerate() {
                let (filename, size) = match upload {
                    crate::discord::handle::Upload::Path(path) => (
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "file".into()),
                        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                    ),
                    crate::discord::handle::Upload::Bytes { filename, data, .. } => {
                        (filename.clone(), data.len() as u64)
                    }
                };
                sink.send(Event::UploadProgress {
                    nonce,
                    sent: size,
                    total: size.max(1),
                });
                message.attachments.push(crate::discord::model::Attachment {
                    id: crate::discord::snowflake::AttachmentId(n as u64 + 1),
                    filename: filename.clone(),
                    size,
                    url: format!("https://cdn.invalid/attachments/{filename}"),
                    proxy_url: String::new(),
                    content_type: None,
                    ..Default::default()
                });
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

    /// The scripted scan, end to end: a code, a phone reading it, and a
    /// session. It is the one flow that cannot be tested against Discord
    /// without a phone in somebody's hand, which is the reason it is scripted
    /// here at all.
    #[test]
    fn the_replay_plays_a_whole_qr_login() {
        let handle = spawn(fixture());
        assert!(
            wait_for(&handle, |e| matches!(e, Event::Auth(AuthEvent::NeedsLogin))),
            "it did not say there was no token"
        );
        handle.send(Command::StartRemoteAuth);

        let mut url = None;
        let mut side = 0usize;
        assert!(
            wait_for(&handle, |e| {
                if let Event::Auth(AuthEvent::QrReady { url: u, matrix, .. }) = e {
                    url = Some(u.clone());
                    side = matrix.len();
                    return true;
                }
                false
            }),
            "no code arrived"
        );
        assert!(side >= 21, "a code of {side} modules is not one");
        let url = url.unwrap();
        assert!(
            !url.contains("discord.com"),
            "the fixture's code points at the real thing: {url}"
        );

        assert!(
            wait_for(&handle, |e| matches!(
                e,
                Event::Auth(AuthEvent::QrScanned { .. })
            )),
            "nobody scanned it"
        );
        assert!(wait_for(&handle, |e| matches!(e, Event::Ready)), "no READY");
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

    /// The timeline does not play until somebody signs in, because the login
    /// screen is the first thing this is for. The one thing sent before that
    /// is the statement that there is no token, which is what puts the
    /// interface on the code screen.
    #[test]
    fn nothing_plays_before_the_login() {
        let handle = spawn(fixture());
        std::thread::sleep(Duration::from_millis(120));
        let events: Vec<Event> = handle.drain().collect();
        assert!(
            events
                .iter()
                .all(|e| matches!(e, Event::Auth(AuthEvent::NeedsLogin))),
            "the timeline started without a login: {events:?}"
        );
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
