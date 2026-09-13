//! The contract between the Discord core and everything else.
//!
//! The core runs on its own OS thread with its own tokio runtime. The UI loop
//! is synchronous and never sees a future: it sends [`Command`]s down a bounded
//! channel, drains [`Event`]s once a frame, and reads the truth out of
//! [`State`] behind an `RwLock`.
//!
//! The important property is that **events carry no state**. `Event::Guilds`
//! means "the guild list changed", not what it changed to. So an event may be
//! coalesced, delayed or dropped outright and the next frame still draws the
//! truth. That is what makes a bounded channel safe: when it fills, the counter
//! goes up and the next send is preceded by [`Event::Refresh`], which tells the
//! UI to re-read everything rather than trust its incremental picture.
//!
//! The variants below are the whole surface, including the ones no milestone
//! has reached yet. They are here rather than added one at a time because the
//! UI is written against this file, and a `match` that has to grow a new arm
//! every fortnight is a merge conflict rather than a design.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::discord::auth::{StorePreference, Token, TokenStoreKind};
use crate::discord::model::{PresenceStatus, User};
use crate::discord::snowflake::{ChannelId, EmojiId, GuildId, MessageId, UserId};
use crate::discord::state::State;
use crate::paths::Paths;

/// How many commands may be in flight before the UI is told to slow down.
const COMMAND_CAPACITY: usize = 256;
/// How many events may be queued before they start being dropped.
const EVENT_CAPACITY: usize = 4096;
/// How long a `Drop` waits for the core thread to finish.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// A message this client sent, before Discord has given it an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Nonce(pub u64);

impl std::fmt::Display for Nonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Ties an asynchronous answer to the request that asked for it, so a stale
/// search result cannot overwrite a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// Something to attach to a message.
#[derive(Debug, Clone)]
pub enum Upload {
    Path(PathBuf),
    /// A pasted image, which has no path.
    Bytes {
        filename: String,
        data: Arc<Vec<u8>>,
        content_type: String,
    },
}

/// A reaction, which is either a unicode character or a custom emoji.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EmojiRef {
    Unicode(String),
    Custom {
        name: String,
        id: EmojiId,
        animated: bool,
    },
}

// Pictures live in `media`, spelled here because this file is the UI's whole
// vocabulary and it should not have to know which module a type came from.
#[allow(unused_imports)]
pub use crate::discord::media::{Decoded, MediaError, MediaKey, MediaPriority, MediaRequest, Want};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Channel(ChannelId),
    Guild(GuildId),
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub content: String,
    pub author: Option<UserId>,
    pub offset: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SearchPage {
    pub total: u32,
    pub message_ids: Vec<(ChannelId, MessageId)>,
}

/// What kind of thing an external program is being asked to open, so the right
/// one is chosen: a player for video, a browser for a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalKind {
    Link,
    Image,
    Video,
}

/// What happened to a channel's messages. The id says which message; the UI
/// reads the message itself out of `State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagesChange {
    Appended(MessageId),
    /// History arrived at the top; this many rows were inserted above.
    Prepended(usize),
    /// The whole window changed — a jump, or a reload.
    Replaced,
    Updated(MessageId),
    Removed(MessageId),
    Reactions(MessageId),
    Pending(Nonce),
    /// A fetch started or finished, for the spinner row.
    Loading(bool),
}

/// Everything the UI can ask the core to do.
///
/// What is *not* here is the point. There is no friend request, no relationship
/// change, no guild join or leave, no invite, no DM to somebody who is not
/// already a friend, and nothing bulk. Those are absent by design rather than
/// unimplemented: a terminal client that can do them is a terminal client that
/// will be used to do them automatically.
#[derive(Debug)]
pub enum Command {
    LoginWithToken(Token),
    StartRemoteAuth,
    CancelRemoteAuth,
    Logout,

    Connect,
    Disconnect,
    SetStatus(PresenceStatus),
    /// Which channel the user is looking at, and whether the terminal has
    /// focus. Acks and notifications both depend on it.
    SetFocus {
        channel: Option<ChannelId>,
        terminal_focused: bool,
    },

    OpenChannel(ChannelId),
    CloseChannel(ChannelId),
    LoadOlder(ChannelId),
    LoadNewer(ChannelId),
    JumpTo {
        channel: ChannelId,
        message: MessageId,
    },
    RequestMembers {
        guild: GuildId,
        channel: ChannelId,
        ranges: Vec<(u32, u32)>,
    },

    SendMessage {
        channel: ChannelId,
        content: String,
        reply_to: Option<MessageId>,
        mention_author: bool,
        attachments: Vec<Upload>,
    },
    RetrySend(Nonce),
    CancelSend(Nonce),
    EditMessage {
        channel: ChannelId,
        message: MessageId,
        content: String,
    },
    DeleteMessage {
        channel: ChannelId,
        message: MessageId,
    },

    Typing(ChannelId),
    MarkRead {
        channel: ChannelId,
        up_to: MessageId,
    },

    /// Remember the half-typed text in a channel. Held in memory and written
    /// to `session.toml`, which is mode 0600 for exactly this reason.
    SetDraft {
        channel: ChannelId,
        text: String,
    },
    /// Remember where the view is anchored in a channel; `None` is the bottom.
    SetAnchor {
        channel: ChannelId,
        message: Option<MessageId>,
    },
    /// Write the session now rather than at the next autosave. Sent on the way
    /// out, where "in thirty seconds" is too late.
    SaveSession,
    AddReaction {
        channel: ChannelId,
        message: MessageId,
        emoji: EmojiRef,
    },
    RemoveReaction {
        channel: ChannelId,
        message: MessageId,
        emoji: EmojiRef,
    },

    /// Refused unless the other account is already a friend.
    OpenDm(UserId),
    Search {
        id: RequestId,
        scope: SearchScope,
        query: SearchQuery,
    },

    FetchMedia(MediaRequest),
    CancelMedia(MediaKey),
    OpenExternal {
        url: String,
        kind: ExternalKind,
    },

    GifTrending {
        id: RequestId,
    },
    GifSearch {
        id: RequestId,
        query: String,
    },
    GifSuggest {
        id: RequestId,
        prefix: String,
    },

    Shutdown,
}

impl Command {
    /// A short name for the log, since most of these carry content that must
    /// not be written to one.
    pub fn name(&self) -> &'static str {
        match self {
            Command::LoginWithToken(_) => "LoginWithToken",
            Command::StartRemoteAuth => "StartRemoteAuth",
            Command::CancelRemoteAuth => "CancelRemoteAuth",
            Command::Logout => "Logout",
            Command::Connect => "Connect",
            Command::Disconnect => "Disconnect",
            Command::SetStatus(_) => "SetStatus",
            Command::SetFocus { .. } => "SetFocus",
            Command::OpenChannel(_) => "OpenChannel",
            Command::CloseChannel(_) => "CloseChannel",
            Command::LoadOlder(_) => "LoadOlder",
            Command::LoadNewer(_) => "LoadNewer",
            Command::JumpTo { .. } => "JumpTo",
            Command::RequestMembers { .. } => "RequestMembers",
            Command::SendMessage { .. } => "SendMessage",
            Command::RetrySend(_) => "RetrySend",
            Command::CancelSend(_) => "CancelSend",
            Command::EditMessage { .. } => "EditMessage",
            Command::DeleteMessage { .. } => "DeleteMessage",
            Command::Typing(_) => "Typing",
            Command::MarkRead { .. } => "MarkRead",
            Command::SetDraft { .. } => "SetDraft",
            Command::SetAnchor { .. } => "SetAnchor",
            Command::SaveSession => "SaveSession",
            Command::AddReaction { .. } => "AddReaction",
            Command::RemoveReaction { .. } => "RemoveReaction",
            Command::OpenDm(_) => "OpenDm",
            Command::Search { .. } => "Search",
            Command::FetchMedia(_) => "FetchMedia",
            Command::CancelMedia(_) => "CancelMedia",
            Command::OpenExternal { .. } => "OpenExternal",
            Command::GifTrending { .. } => "GifTrending",
            Command::GifSearch { .. } => "GifSearch",
            Command::GifSuggest { .. } => "GifSuggest",
            Command::Shutdown => "Shutdown",
        }
    }
}

/// Where the login stands.
#[derive(Debug, Clone)]
pub enum AuthEvent {
    /// There is no stored token, or the stored one was rejected.
    NeedsLogin,
    QrReady {
        url: String,
        fingerprint: String,
        expires_in: Duration,
        /// The code itself, one `bool` per module, `true` for dark. The core
        /// does not draw, so it hands over the squares and the UI decides what
        /// a dark module looks like in a terminal.
        matrix: Vec<Vec<bool>>,
    },
    /// The phone scanned it and Discord said whose phone it was.
    QrScanned {
        username: String,
        avatar: Option<MediaKey>,
    },
    LoggedIn {
        user: Arc<User>,
        stored_in: TokenStoreKind,
    },
    Failed(String),
    LoggedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    Info,
    Warning,
    Error,
}

/// Something to tell the user in the status line.
#[derive(Debug, Clone)]
pub struct Note {
    pub level: NoteLevel,
    /// Repeats of the same key replace rather than stack, so a flapping
    /// connection does not become a wall of identical lines.
    pub key: Option<&'static str>,
    pub text: String,
}

impl Note {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            level: NoteLevel::Info,
            key: None,
            text: text.into(),
        }
    }

    pub fn warning(key: &'static str, text: impl Into<String>) -> Self {
        Self {
            level: NoteLevel::Warning,
            key: Some(key),
            text: text.into(),
        }
    }

    pub fn error(key: &'static str, text: impl Into<String>) -> Self {
        Self {
            level: NoteLevel::Error,
            key: Some(key),
            text: text.into(),
        }
    }
}

/// The connection, as the status line shows it.
#[derive(Debug, Clone)]
pub enum Connection {
    LoggedOut,
    Connecting,
    /// The socket is open and IDENTIFY has gone out; READY has not come back.
    Identifying,
    Ready {
        since: Instant,
        /// Whether this was a RESUME rather than a fresh IDENTIFY, which is
        /// worth showing: a resume means nothing was missed.
        resumed: bool,
    },
    Resuming,
    Reconnecting {
        attempt: u32,
        next_in: Duration,
        reason: String,
    },
    /// Terminal. The token was rejected and retrying would be worse than
    /// stopping.
    AuthFailed(String),
    Offline,
}

impl Connection {
    /// A word for the status line.
    pub fn word(&self) -> &'static str {
        match self {
            Connection::LoggedOut => "logged out",
            Connection::Connecting => "connecting",
            Connection::Identifying => "identifying",
            Connection::Ready { .. } => "online",
            Connection::Resuming => "resuming",
            Connection::Reconnecting { .. } => "reconnecting",
            Connection::AuthFailed(_) => "rejected",
            Connection::Offline => "offline",
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Connection::Ready { .. })
    }

    /// Whether the core has stopped trying on its own.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Connection::AuthFailed(_) | Connection::LoggedOut | Connection::Offline
        )
    }
}

/// A notification that something changed. Never the change itself.
#[derive(Debug)]
pub enum Event {
    Status(Arc<Connection>),
    Auth(AuthEvent),
    /// READY has been applied and `State` is worth drawing.
    Ready,
    /// The session file was read. Carries the whole of it, because it is small
    /// and because the UI needs the drafts before it draws anything.
    SessionLoaded(Arc<crate::session::Session>),
    Note(Note),
    Guilds,
    /// One guild's channel list, or the DM list when `None`.
    Channels(Option<GuildId>),
    Messages(ChannelId, MessagesChange),
    SendResult {
        nonce: Nonce,
        result: Result<MessageId, String>,
    },
    UploadProgress {
        nonce: Nonce,
        sent: u64,
        total: u64,
    },
    Typing(ChannelId),
    ReadState(ChannelId),
    Presence(UserId),
    Members(GuildId),
    Relationships,
    /// Something addressed to this account arrived, which is what a desktop
    /// notification and a beep hang off.
    Mention {
        channel: ChannelId,
        message: MessageId,
    },
    Media {
        key: MediaKey,
        result: Result<Arc<Decoded>, MediaError>,
    },
    Gifs {
        id: RequestId,
        result: Result<Vec<String>, String>,
    },
    Search {
        id: RequestId,
        result: Result<SearchPage, String>,
    },
    /// Events were dropped. Whatever the UI believes about its incremental
    /// state is now suspect; re-read everything.
    Refresh,
}

/// How the core is configured.
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    pub locale: String,
    pub store: StorePreference,
    /// Connect as soon as a stored token is found.
    pub auto_connect: bool,
    /// Send op 14 instead of op 37 for member lists, for a session where the
    /// newer opcode turns out not to be accepted.
    pub legacy_lazy_request: bool,
    /// Look up the current web-client build number at startup. Off in tests and
    /// under `--offline`, where the pinned constant is used instead: it is a
    /// request to Discord's CDN, and a test suite has no business making one.
    pub discover_build: bool,
    /// Write every raw dispatch here, as `<seq>_<event>.json`. Set by
    /// `STARCORD_RECORD_GATEWAY`; recordings carry real names and message text
    /// until they are scrubbed.
    pub record_gateway: Option<PathBuf>,
    /// `[media]`: cache size, attachment cap, and the player argv.
    pub media: crate::discord::media::MediaConfig,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            locale: "en-US".into(),
            store: StorePreference::default(),
            auto_connect: true,
            legacy_lazy_request: false,
            discover_build: true,
            record_gateway: None,
            media: crate::discord::media::MediaConfig::default(),
        }
    }
}

/// The sending half of the event channel, with the drop bookkeeping.
#[derive(Clone)]
pub struct EventSink {
    tx: crossbeam_channel::Sender<Event>,
    dropped: Arc<AtomicU64>,
    needs_refresh: Arc<std::sync::atomic::AtomicBool>,
}

impl EventSink {
    /// A sink with its own counters, for a caller that holds both ends: the
    /// tests in `ops` and `media`, and `probe --media`, which drives one fetch
    /// with no core behind it.
    pub fn detached(tx: crossbeam_channel::Sender<Event>) -> Self {
        Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            needs_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn send(&self, event: Event) {
        // A dropped event means the UI's incremental picture may be wrong, so
        // the next one that fits is preceded by a Refresh. Sending the Refresh
        // eagerly would just be another event to drop.
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

/// The UI's view of the core.
pub struct Handle {
    commands: tokio::sync::mpsc::Sender<Command>,
    events: crossbeam_channel::Receiver<Event>,
    state: Arc<RwLock<State>>,
    status: Arc<ArcSwap<Connection>>,
    thread: Option<std::thread::JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
    /// Commands that did not fit. A number that climbs means the core is behind
    /// or wedged, which is worth showing rather than hiding.
    refused: Arc<AtomicU64>,
}

/// Everything a `Handle` needs, so a fake core can build one.
pub struct HandleParts {
    pub commands: tokio::sync::mpsc::Sender<Command>,
    pub events: crossbeam_channel::Receiver<Event>,
    pub state: Arc<RwLock<State>>,
    pub status: Arc<ArcSwap<Connection>>,
    pub thread: Option<std::thread::JoinHandle<()>>,
    pub dropped: Arc<AtomicU64>,
}

impl Handle {
    /// Start the core on its own thread.
    pub fn spawn(config: DiscordConfig, paths: Paths) -> anyhow::Result<Handle> {
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = crossbeam_channel::bounded(EVENT_CAPACITY);
        let state = Arc::new(RwLock::new(State::new()));
        let status = Arc::new(ArcSwap::from_pointee(Connection::LoggedOut));
        let dropped = Arc::new(AtomicU64::new(0));

        let sink = EventSink {
            tx: event_tx,
            dropped: Arc::clone(&dropped),
            needs_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        let thread = {
            let state = Arc::clone(&state);
            let status = Arc::clone(&status);
            std::thread::Builder::new()
                .name("starcord-discord".into())
                .spawn(move || {
                    if let Err(e) =
                        super::core::run(config, paths, command_rx, sink.clone(), state, status)
                    {
                        tracing::error!("the discord core stopped: {e}");
                        sink.send(Event::Note(Note::error(
                            "core-stopped",
                            format!("the Discord core stopped: {e}"),
                        )));
                    }
                })?
        };

        Ok(Handle {
            commands: command_tx,
            events: event_rx,
            state,
            status,
            thread: Some(thread),
            dropped,
            refused: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Build a handle around something that is not the real core, for the UI's
    /// tests and for replaying a recorded session.
    pub fn from_parts(parts: HandleParts) -> Handle {
        Handle {
            commands: parts.commands,
            events: parts.events,
            state: parts.state,
            status: parts.status,
            thread: parts.thread,
            dropped: parts.dropped,
            refused: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Ask the core to do something.
    ///
    /// Never blocks: the UI thread calling this is the thread that has to draw
    /// the next frame. A full channel means the core is not keeping up, which
    /// is a thing to report rather than a thing to wait for.
    pub fn send(&self, command: Command) {
        let name = command.name();
        if self.commands.try_send(command).is_err() {
            let total = self.refused.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!("dropped a {name} command; {total} refused so far");
        }
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Everything queued, for the once-a-frame drain.
    pub fn drain(&self) -> crossbeam_channel::TryIter<'_, Event> {
        self.events.try_iter()
    }

    /// The truth. Copy what the frame needs and drop the guard before drawing:
    /// the core takes the write lock to apply a dispatch, and a guard held
    /// across a render stalls the gateway.
    pub fn state(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> Arc<Connection> {
        self.status.load_full()
    }

    pub fn events_dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn commands_refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // `try_send` rather than `blocking_send`: if the channel is full the
        // core is busy, and closing the channel below stops it anyway.
        let _ = self.commands.try_send(Command::Shutdown);

        let Some(thread) = self.thread.take() else {
            return;
        };

        // Joining without a bound would hang a quit on a socket that is not
        // answering. The thread is detached after the grace period; the process
        // is on its way out, and a detached thread with a closed command
        // channel finishes on its own.
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!("the discord thread did not stop within {SHUTDOWN_GRACE:?}");
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Err(e) = thread.join() {
            tracing::error!("the discord thread panicked: {e:?}");
        }
    }
}

/// Coalescing bookkeeping shared by the ack and typing paths.
///
/// Not used in M1 and defined here because both of its consumers are `Command`
/// handlers: a typing indicator may be sent no more than once every nine
/// seconds per channel, and an ack is coalesced to the highest id per channel
/// per second.
#[derive(Debug, Default)]
pub struct Coalescer {
    last: HashMap<ChannelId, Instant>,
}

impl Coalescer {
    /// Whether enough time has passed to act on this channel again.
    pub fn ready(&mut self, channel: ChannelId, every: Duration, now: Instant) -> bool {
        match self.last.get(&channel) {
            Some(last) if now.duration_since(*last) < every => false,
            _ => {
                self.last.insert(channel, now);
                true
            }
        }
    }

    pub fn forget(&mut self, channel: ChannelId) {
        self.last.remove(&channel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts() -> (
        Handle,
        crossbeam_channel::Sender<Event>,
        tokio::sync::mpsc::Receiver<Command>,
    ) {
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(4);
        let (event_tx, event_rx) = crossbeam_channel::bounded(8);
        let handle = Handle::from_parts(HandleParts {
            commands: command_tx,
            events: event_rx,
            state: Arc::new(RwLock::new(State::new())),
            status: Arc::new(ArcSwap::from_pointee(Connection::LoggedOut)),
            thread: None,
            dropped: Arc::new(AtomicU64::new(0)),
        });
        (handle, event_tx, command_rx)
    }

    #[test]
    fn a_full_command_channel_is_counted_rather_than_waited_on() {
        let (handle, _events, _commands) = parts();
        for _ in 0..4 {
            handle.send(Command::Connect);
        }
        assert_eq!(handle.commands_refused(), 0);
        handle.send(Command::Connect);
        assert_eq!(
            handle.commands_refused(),
            1,
            "the fifth should not have fitted"
        );
    }

    #[test]
    fn a_dropped_event_becomes_a_refresh_on_the_next_one_that_fits() {
        let (event_tx, event_rx) = crossbeam_channel::bounded(2);
        let sink = EventSink {
            tx: event_tx,
            dropped: Arc::new(AtomicU64::new(0)),
            needs_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        sink.send(Event::Guilds);
        sink.send(Event::Guilds);
        // Full now.
        sink.send(Event::Guilds);
        assert_eq!(sink.dropped.load(Ordering::Relaxed), 1);

        // Drain, then send again: the first thing through must be a Refresh.
        let _ = event_rx.try_recv();
        let _ = event_rx.try_recv();
        sink.send(Event::Presence(UserId(1)));

        assert!(
            matches!(event_rx.try_recv(), Ok(Event::Refresh)),
            "a UI that missed an event was not told to re-read"
        );
        assert!(matches!(event_rx.try_recv(), Ok(Event::Presence(_))));
    }

    #[test]
    fn the_drain_yields_everything_queued_and_then_stops() {
        let (handle, events, _commands) = parts();
        events.send(Event::Guilds).unwrap();
        events.send(Event::Relationships).unwrap();
        assert_eq!(handle.drain().count(), 2);
        assert_eq!(handle.drain().count(), 0);
    }

    #[test]
    fn a_connection_knows_whether_it_has_given_up() {
        assert!(Connection::AuthFailed("no".into()).is_terminal());
        assert!(Connection::LoggedOut.is_terminal());
        assert!(!Connection::Reconnecting {
            attempt: 3,
            next_in: Duration::from_secs(4),
            reason: "closed".into()
        }
        .is_terminal());
        assert!(Connection::Ready {
            since: Instant::now(),
            resumed: false
        }
        .is_ready());
    }

    #[test]
    fn a_command_never_names_its_contents() {
        // The names go into the log; the payloads must not.
        let token = Token::new("mfa.aVeryRealLookingSecret").unwrap();
        let command = Command::LoginWithToken(token);
        assert_eq!(command.name(), "LoginWithToken");
        assert!(!format!("{command:?}").contains("aVeryRealLookingSecret"));
    }

    #[test]
    fn the_coalescer_holds_a_channel_for_its_interval() {
        let mut coalescer = Coalescer::default();
        let start = Instant::now();
        let every = Duration::from_secs(9);

        assert!(coalescer.ready(ChannelId(1), every, start));
        assert!(!coalescer.ready(ChannelId(1), every, start + Duration::from_secs(8)));
        assert!(
            coalescer.ready(ChannelId(2), every, start + Duration::from_secs(1)),
            "channels are held independently"
        );
        assert!(coalescer.ready(ChannelId(1), every, start + Duration::from_secs(10)));

        coalescer.forget(ChannelId(1));
        assert!(coalescer.ready(ChannelId(1), every, start + Duration::from_secs(10)));
    }
}
