//! The thread everything else hangs off.
//!
//! One OS thread, one tokio runtime, one command loop. The UI never touches any
//! of it: it sends `Command`s and reads `State`.
//!
//! Two workers rather than the default of one per core. The work here is
//! waiting — on a socket, on Discord, on a disk — and the only genuinely
//! CPU-bound thing in the design is parsing a READY, which goes to a blocking
//! thread. A runtime sized to the machine would spend most of a laptop's cores
//! parked in `epoll`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::discord::auth::remote::{self, Authenticated};
use crate::discord::auth::{Token, TokenStore};
use crate::discord::gateway::{self, Bridge, Control, GatewayConfig};
use crate::discord::gifs::{Ask, Gifs};
use crate::discord::handle::{
    AuthEvent, Command, Connection, DiscordConfig, Event, EventSink, Note,
};
use crate::discord::http::{Http, HttpError};
use crate::discord::media::{self, cache::Cache};
use crate::discord::model::{PresenceStatus, User};
use crate::discord::ops::{self, Ops};
use crate::discord::props::{self, ClientProps};
use crate::discord::state::State;
use crate::paths::Paths;
use crate::session::{SessionStore, AUTOSAVE};

/// How much REST work may be in flight at once.
///
/// Not a rate limit — that is `http/limits.rs` — but a cap on concurrency, so a
/// scroll that asks for forty avatars does not open forty connections and then
/// queue every one of them behind the same bucket anyway.
const REST_CONCURRENCY: usize = 8;

/// How often the housekeeping task wakes.
///
/// Two seconds is short enough that a typing indicator lapses within a
/// noticeable fraction of its ten-second lease, and long enough that an idle
/// client is genuinely idle. Everything on this timer is something nothing
/// arrives to announce: a lease expiring, a guild subscription's grace running
/// out, a session that has been dirty for half a minute.
const HOUSEKEEPING: Duration = Duration::from_secs(2);

/// Entry point for the thread `Handle::spawn` starts.
pub fn run(
    config: DiscordConfig,
    paths: Paths,
    commands: mpsc::Receiver<Command>,
    events: EventSink,
    state: Arc<RwLock<State>>,
    status: Arc<ArcSwap<Connection>>,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(3)
        .enable_all()
        .thread_name("starcord-net")
        .build()?;

    runtime.block_on(async move {
        let bridge = Bridge {
            state,
            events,
            status,
        };
        let mut core = Core::new(config, paths, bridge)?;
        core.run(commands).await;
        Ok(())
    })
}

/// An answer a spawned task hands back to the command loop, because acting on
/// it needs `&mut Core` and a task does not have one.
#[derive(Debug)]
enum Internal {
    RemoteAuth(Result<Box<Authenticated>, String>),
}

/// A running gateway connection, and the means to stop it.
struct GatewayTask {
    control: mpsc::Sender<Control>,
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

struct Core {
    config: DiscordConfig,
    paths: Paths,
    http: Arc<Http>,
    store: TokenStore,
    bridge: Bridge,
    gateway: Option<GatewayTask>,
    /// The live socket's control channel, published for the tasks in `ops`
    /// that outlive any one connection.
    control: Arc<ArcSwapOption<mpsc::Sender<Control>>>,
    ops: Ops,
    /// The media task's end of its own queue. Built lazily, because it spawns a
    /// task and `Core::new` is not inside the runtime.
    media: Option<media::fetch::Media>,
    /// Live only while a QR code is on screen. Cancelling it closes the socket
    /// and drops the key.
    remote: Option<CancellationToken>,
    /// How a spawned task hands an answer back to the command loop.
    ///
    /// The QR login runs for minutes and must not hold up the keystroke behind
    /// it, so it is a task; but what it produces -- a token to store, a
    /// connection to open -- is `&mut self` work that only this loop may do.
    internal: Option<mpsc::Sender<Internal>>,
    /// The GIF picker, which has its own spacing rule and no other state.
    gifs: Gifs,
    session: Arc<std::sync::Mutex<SessionStore>>,
    token: Option<Token>,
    presence: PresenceStatus,
    rest: Arc<Semaphore>,
    /// Commands that arrived before the milestone that handles them. Counted
    /// rather than silently dropped, so "nothing happens when I press that" has
    /// a number behind it.
    unhandled: AtomicU64,
}

impl Core {
    fn new(config: DiscordConfig, paths: Paths, bridge: Bridge) -> anyhow::Result<Self> {
        // The build number from last time, if it is still fresh. Discovery
        // replaces it a moment later; starting from the cache means the first
        // request does not wait on Discord's CDN.
        let props = Arc::new(ClientProps::new(
            config.locale.clone(),
            props::PINNED_BUILD_NUMBER,
        ));
        let cached = props::load_cache(&paths, props.user_agent());
        let props = match cached {
            Some(cache) => Arc::new(props.with_build_number(cache.build_number)),
            None => props,
        };

        let http = Arc::new(Http::new(Arc::clone(&props))?);
        let store = TokenStore::new(paths, config.store);
        let rest = Arc::new(Semaphore::new(REST_CONCURRENCY));
        let control = Arc::new(ArcSwapOption::empty());
        let ops = Ops::new(
            Arc::clone(&http),
            bridge.clone(),
            Arc::clone(&rest),
            Arc::clone(&control),
            config.legacy_lazy_request,
        );

        let gifs = Gifs::new(
            Arc::clone(&http),
            config.gifs.clone(),
            bridge.events.clone(),
        );

        Ok(Self {
            config,
            paths,
            http,
            store,
            bridge,
            control,
            ops,
            media: None,
            remote: None,
            internal: None,
            gifs,
            session: Arc::new(std::sync::Mutex::new(SessionStore::load(paths))),
            gateway: None,
            token: None,
            presence: PresenceStatus::Online,
            rest,
            unhandled: AtomicU64::new(0),
        })
    }

    async fn run(&mut self, mut commands: mpsc::Receiver<Command>) {
        let (internal_tx, mut internal_rx) = mpsc::channel(8);
        self.internal = Some(internal_tx);

        if self.config.discover_build {
            self.start_build_discovery();
        }
        self.media = Some(self.start_media());

        // The session is on disk before anything connects, so a UI can restore
        // its drafts and its last channel while the gateway is still dialling.
        {
            let session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            self.bridge
                .events
                .send(Event::SessionLoaded(Arc::new(session.get().clone())));
        }
        let housekeeping = self.start_housekeeping();

        match self.store.load() {
            Some((token, kind)) => {
                tracing::info!("found a token in {}", kind.describe());
                self.token = Some(token);
                if self.config.auto_connect {
                    self.connect().await;
                }
            }
            None => {
                self.bridge.set_status(Connection::LoggedOut);
                self.bridge.events.send(Event::Auth(AuthEvent::NeedsLogin));
            }
        }

        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Shutdown) | None => break,
                    Some(command) => self.handle(command).await,
                },
                answer = internal_rx.recv() => match answer {
                    Some(answer) => self.finish(answer).await,
                    // Only possible once every sender is gone, and this struct
                    // holds one.
                    None => break,
                },
            }
        }

        if let Some(cancel) = self.remote.take() {
            cancel.cancel();
        }
        housekeeping.abort();
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .save_if_dirty();
        self.disconnect().await;
    }

    /// Start the task that fetches and decodes pictures.
    ///
    /// It has its own queue and its own concurrency cap rather than sharing the
    /// REST semaphore: a scroll asks for forty pictures at once, and a hundred
    /// megabytes of CDN transfer must not be able to hold up the request that
    /// sends a message.
    fn start_media(&self) -> media::fetch::Media {
        let dir = self
            .paths
            .media_cache_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        media::fetch::spawn(media::fetch::Context {
            http: Arc::clone(&self.http),
            cache: Arc::new(Cache::new(dir)),
            config: Arc::new(self.config.media.clone()),
            events: self.bridge.events.clone(),
        })
    }

    /// The timer that carries everything nothing announces.
    fn start_housekeeping(&self) -> tokio::task::JoinHandle<()> {
        let ops = self.ops.clone();
        let bridge = self.bridge.clone();
        let session = Arc::clone(&self.session);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(HOUSEKEEPING);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut was_ready = false;
            let mut since_save = Duration::ZERO;

            loop {
                ticker.tick().await;

                // A reconnect resets what the gateway knows, so everything that
                // was subscribed has to be said again. Losing the socket clears
                // the record of what it was told.
                let ready = bridge.status.load().is_ready();
                if ready && !was_ready {
                    ops::open::resubscribe(&ops);
                } else if !ready && was_ready {
                    ops.shared().subscriptions.forget_connection();
                }
                was_ready = ready;

                // Nobody sends a "stopped typing"; the lease simply lapses.
                let lapsed = {
                    let mut state = bridge.state.write().unwrap_or_else(|e| e.into_inner());
                    let lapsed = state.typing_mut().sweep(std::time::Instant::now());
                    if !lapsed.is_empty() {
                        state.touch();
                    }
                    lapsed
                };
                for channel in lapsed {
                    bridge.events.send(Event::Typing(channel));
                }

                ops::open::sweep_subscriptions(&ops);

                since_save += HOUSEKEEPING;
                if since_save >= AUTOSAVE {
                    since_save = Duration::ZERO;
                    session
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .save_if_dirty();
                }
            }
        })
    }

    fn with_session(&self, f: impl FnOnce(&mut SessionStore)) {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut session);
    }

    /// Refresh the build number in the background.
    ///
    /// Never on the login path. The number only matters for looking like a
    /// current browser, and a login that waits on a CDN to achieve that has the
    /// priorities backwards.
    fn start_build_discovery(&self) {
        let http = Arc::clone(&self.http);
        let paths = self.paths;
        let props = self.http.props();
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .user_agent(props.user_agent())
                .https_only(true)
                .build();
            let Ok(client) = client else { return };

            let found =
                props::discover_build_number(&client, props.user_agent(), props::DISCOVERY_BUDGET)
                    .await;

            match found {
                Some(build) => {
                    if build == props.build_number() {
                        tracing::debug!("the build number in use is current");
                    } else {
                        tracing::info!("client build number is {build}");
                        http.set_props(Arc::new(props.with_build_number(build)));
                    }
                    // Cached whether or not it changed. A number that matched
                    // is still a number that was confirmed today, and not
                    // writing it means rediscovering it on every start.
                    props::store_cache(
                        &paths,
                        &props::BuildCache {
                            build_number: build,
                            fetched_at: jiff::Timestamp::now(),
                            user_agent: props.user_agent().to_string(),
                        },
                    );
                }
                None => tracing::debug!(
                    "could not discover a build number; using {}",
                    props.build_number()
                ),
            }
        });
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::LoginWithToken(token) => self.login(token).await,
            Command::StartRemoteAuth => self.start_remote_auth(),
            Command::CancelRemoteAuth => {
                if let Some(cancel) = self.remote.take() {
                    tracing::info!("the scanned login was cancelled");
                    cancel.cancel();
                }
            }
            Command::Logout => self.logout().await,
            Command::Connect => self.connect().await,
            Command::Disconnect => {
                self.disconnect().await;
                self.bridge.set_status(Connection::Offline);
            }
            Command::SetStatus(status) => {
                self.presence = status;
                // Applied at the next IDENTIFY. Changing it on a live
                // connection is op 3, which arrives with the UI that can ask
                // for it.
                tracing::debug!("presence will be {} on the next connect", status.as_str());
            }
            Command::Shutdown => {}

            Command::SetFocus {
                channel,
                terminal_focused,
            } => {
                self.ops.set_focus(channel, terminal_focused);
                if let Some(channel) = channel {
                    let guild = self.ops.state().channel(channel).and_then(|c| c.guild_id);
                    self.with_session(|session| {
                        session.update(|s| {
                            let changed = s.last_channel != Some(channel) || s.last_guild != guild;
                            s.last_channel = Some(channel);
                            s.last_guild = guild;
                            changed
                        })
                    });
                }
            }

            // Everything below runs on its own task. The command loop is the
            // only consumer of the channel the UI writes to, and a fetch that
            // takes a second must not hold up the keystroke behind it.
            Command::OpenChannel(channel) => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::open::open_channel(&ops, channel).await });
            }
            Command::CloseChannel(channel) => ops::open::close_channel(&self.ops, channel),
            Command::LoadOlder(channel) => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::open::load_older(&ops, channel).await });
            }
            Command::LoadNewer(channel) => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::open::load_newer(&ops, channel).await });
            }
            Command::JumpTo { channel, message } => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::open::jump_to(&ops, channel, message).await });
            }

            Command::SendMessage {
                channel,
                content,
                reply_to,
                mention_author,
                attachments,
            } => {
                if !attachments.is_empty() {
                    // Uploads arrive with the media milestone. Refusing loudly
                    // beats sending the text and quietly dropping the file.
                    self.bridge.note(Note::warning(
                        "no-uploads",
                        "attachments are not supported yet; the text was not sent",
                    ));
                    return;
                }
                let ops = self.ops.clone();
                tokio::spawn(async move {
                    ops::send::send_message(&ops, channel, content, reply_to, mention_author).await
                });
            }
            Command::RetrySend(nonce) => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::send::retry(&ops, nonce).await });
            }
            Command::CancelSend(nonce) => ops::send::cancel(&self.ops, nonce),
            Command::EditMessage {
                channel,
                message,
                content,
            } => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::send::edit(&ops, channel, message, content).await });
            }
            Command::DeleteMessage { channel, message } => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::send::delete(&ops, channel, message).await });
            }

            Command::Typing(channel) => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::typing::typing(&ops, channel).await });
            }
            Command::MarkRead { channel, up_to } => {
                let ops = self.ops.clone();
                tokio::spawn(async move { ops::ack::mark_read(&ops, channel, up_to).await });
            }

            Command::SetDraft { channel, text } => {
                self.with_session(|session| session.update(|s| s.set_draft(channel, &text)));
            }
            Command::SetAnchor { channel, message } => {
                self.with_session(|session| session.update(|s| s.set_anchor(channel, message)));
            }
            Command::SaveSession => self.with_session(SessionStore::save_if_dirty),

            Command::FetchMedia(request) => {
                if let Some(media) = &self.media {
                    media.fetch(request);
                }
            }
            Command::CancelMedia(key) => {
                if let Some(media) = &self.media {
                    media.cancel(key);
                }
            }
            Command::OpenExternal { url, kind } => {
                if let Some(media) = &self.media {
                    media.open_external(url, kind);
                }
            }

            // The picker's own task, because `run` sleeps out the gap between
            // two searches and the command loop must not sleep with it.
            Command::GifTrending { id } => self.ask_gifs(id, Ask::Trending),
            Command::GifSearch { id, query } => self.ask_gifs(id, Ask::Search(query)),
            Command::GifSuggest { id, prefix } => self.ask_gifs(id, Ask::Suggest(prefix)),

            other => {
                let total = self.unhandled.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::debug!(
                    "{} is not implemented yet ({total} unhandled commands so far)",
                    other.name()
                );
            }
        }
    }

    fn ask_gifs(&self, id: crate::discord::handle::RequestId, ask: Ask) {
        let gifs = self.gifs.clone();
        tokio::spawn(async move { gifs.run(id, ask).await });
    }

    async fn login(&mut self, token: Token) {
        self.http.set_token(Some(token.clone()));

        let user = {
            let _permit = self.rest.acquire().await;
            crate::discord::auth::validate(&self.http).await
        };

        match user {
            Ok(user) => self.finish_login(token, user).await,
            Err(e) => {
                self.http.set_token(None);
                let message = match &e {
                    HttpError::Unauthorized => "the token was rejected".to_string(),
                    other => format!("could not check the token: {other}"),
                };
                tracing::warn!("login failed: {message}");
                self.bridge
                    .events
                    .send(Event::Auth(AuthEvent::Failed(message.clone())));
                self.bridge.set_status(Connection::AuthFailed(message));
            }
        }
    }

    /// Store a token that has already been checked, and connect with it.
    ///
    /// Shared by both ways in. A pasted token reaches it after `GET /users/@me`
    /// here; a scanned one after the same request inside the remote-auth
    /// exchange, which needs the account anyway to say who just signed in.
    async fn finish_login(&mut self, token: Token, user: User) {
        self.http.set_token(Some(token.clone()));

        let stored_in = match self.store.save(&token) {
            Ok(kind) => kind,
            Err(e) => {
                // Failing to store is not failing to log in. The session works;
                // it just will not survive a restart.
                tracing::warn!("could not store the token: {e}");
                self.bridge.note(Note::warning(
                    "token-store",
                    format!("signed in, but the token could not be stored: {e}"),
                ));
                crate::discord::auth::TokenStoreKind::Memory
            }
        };
        self.token = Some(token);
        let user = Arc::new(user);
        self.bridge
            .events
            .send(Event::Auth(AuthEvent::LoggedIn { user, stored_in }));
        self.connect().await;
    }

    /// Open the remote-auth socket and put a QR code on screen.
    ///
    /// The whole flow runs on its own task: it lasts for as long as somebody
    /// takes to find their phone, and the command loop has to stay answerable
    /// the entire time -- not least because the thing it most likely has to
    /// answer is `CancelRemoteAuth`.
    fn start_remote_auth(&mut self) {
        if self.remote.is_some() {
            tracing::debug!("a scanned login is already in progress");
            return;
        }

        // Signing in means not being signed in. A stale token left on the
        // client would be sent with the ticket exchange, which is a request
        // from one account to start a session for another.
        self.http.set_token(None);

        let cancel = CancellationToken::new();
        self.remote = Some(cancel.clone());

        let http = Arc::clone(&self.http);
        let props = self.http.props();
        let events = self.bridge.events.clone();
        let back = self.internal.clone();

        tokio::spawn(async move {
            let result = remote::run(http, props, cancel, &mut |event| {
                events.send(Event::Auth(event))
            })
            .await;

            // The error is a sentence, not a type: nothing downstream branches
            // on which part of the handshake failed, and the string is what the
            // status line shows.
            let result = result.map(Box::new).map_err(|e| e.to_string());
            if let Some(back) = back {
                let _ = back.send(Internal::RemoteAuth(result)).await;
            }
        });
    }

    /// Act on something a task finished.
    async fn finish(&mut self, answer: Internal) {
        match answer {
            Internal::RemoteAuth(Ok(authenticated)) => {
                self.remote = None;
                let Authenticated { token, user } = *authenticated;
                tracing::info!("signed in by a scanned code");
                self.finish_login(token, user).await;
            }
            Internal::RemoteAuth(Err(reason)) => {
                self.remote = None;
                self.http.set_token(None);
                tracing::info!("the scanned login ended: {reason}");
                self.bridge
                    .events
                    .send(Event::Auth(AuthEvent::Failed(reason)));
            }
        }
    }

    async fn logout(&mut self) {
        self.disconnect().await;
        self.store.clear();
        self.token = None;
        self.http.set_token(None);
        {
            let mut state = self.bridge.state.write().unwrap_or_else(|e| e.into_inner());
            *state = State::new();
        }
        self.bridge.events.send(Event::Auth(AuthEvent::LoggedOut));
        self.bridge.events.send(Event::Refresh);
        self.bridge.set_status(Connection::LoggedOut);
    }

    async fn connect(&mut self) {
        let Some(token) = self.token.clone() else {
            self.bridge.events.send(Event::Auth(AuthEvent::NeedsLogin));
            return;
        };
        if self.gateway.is_some() {
            tracing::debug!("already connected");
            return;
        }
        self.http.set_token(Some(token.clone()));

        let (control_tx, control_rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        let config = GatewayConfig {
            token,
            props: self.http.props(),
            presence: self.presence,
            record_dir: self.config.record_gateway.clone(),
        };
        let bridge = self.bridge.clone();
        let child = cancel.clone();
        let join = tokio::spawn(async move {
            gateway::run(config, bridge, control_rx, child).await;
        });

        self.control.store(Some(Arc::new(control_tx.clone())));
        self.gateway = Some(GatewayTask {
            control: control_tx,
            cancel,
            join,
        });
    }

    async fn disconnect(&mut self) {
        self.control.store(None);
        self.ops.shared().subscriptions.forget_connection();
        let Some(task) = self.gateway.take() else {
            return;
        };
        task.cancel.cancel();
        // Dropping the control sender is the second signal: the gateway's
        // `select!` sees the channel close even if it is mid-handshake.
        drop(task.control);
        match tokio::time::timeout(Duration::from_secs(3), task.join).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("the gateway task panicked: {e}"),
            Err(_) => tracing::warn!("the gateway task did not stop within three seconds"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::handle::{DiscordConfig, Handle};

    /// Starting and immediately dropping a handle has to leave nothing running.
    /// `Handle::drop` sends a Shutdown and joins; if the command loop misses it
    /// this hangs for the three-second grace period and then warns, which is
    /// the failure this is here to catch.
    #[test]
    fn a_core_with_no_token_starts_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("STARCORD_CORE_TEST_DIR", dir.path());
        let paths = Paths::new(
            "starcord",
            "STARCORD_CORE_TEST_DIR",
            "STARCORD_CORE_TEST_DIR",
        );

        let config = DiscordConfig {
            // No stored token, nothing to connect to, and no request to
            // Discord's CDN from a test suite.
            auto_connect: false,
            discover_build: false,
            store: crate::discord::auth::StorePreference::None,
            ..Default::default()
        };

        let started = std::time::Instant::now();
        let handle = Handle::spawn(config, paths).expect("the core thread started");

        // The first thing a core with no token does is say so.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_needs_login = false;
        while std::time::Instant::now() < deadline && !saw_needs_login {
            for event in handle.drain() {
                if matches!(event, Event::Auth(AuthEvent::NeedsLogin)) {
                    saw_needs_login = true;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            saw_needs_login,
            "the core never reported that it needs a login"
        );

        drop(handle);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "shutting down took {:?}",
            started.elapsed()
        );
    }
}
