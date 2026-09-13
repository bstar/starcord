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

use arc_swap::ArcSwap;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::discord::auth::{Token, TokenStore};
use crate::discord::gateway::{self, Bridge, Control, GatewayConfig};
use crate::discord::handle::{
    AuthEvent, Command, Connection, DiscordConfig, Event, EventSink, Note,
};
use crate::discord::http::{Http, HttpError};
use crate::discord::model::PresenceStatus;
use crate::discord::props::{self, ClientProps};
use crate::discord::state::State;
use crate::paths::Paths;

/// How much REST work may be in flight at once.
///
/// Not a rate limit — that is `http/limits.rs` — but a cap on concurrency, so a
/// scroll that asks for forty avatars does not open forty connections and then
/// queue every one of them behind the same bucket anyway.
const REST_CONCURRENCY: usize = 8;

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

        Ok(Self {
            config,
            paths,
            http,
            store,
            bridge,
            gateway: None,
            token: None,
            presence: PresenceStatus::Online,
            rest: Arc::new(Semaphore::new(REST_CONCURRENCY)),
            unhandled: AtomicU64::new(0),
        })
    }

    async fn run(&mut self, mut commands: mpsc::Receiver<Command>) {
        if self.config.discover_build {
            self.start_build_discovery();
        }

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

        while let Some(command) = commands.recv().await {
            if matches!(command, Command::Shutdown) {
                break;
            }
            self.handle(command).await;
        }

        self.disconnect().await;
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
                Some(build) if build != props.build_number() => {
                    tracing::info!("client build number is {build}");
                    http.set_props(Arc::new(props.with_build_number(build)));
                    props::store_cache(
                        &paths,
                        &props::BuildCache {
                            build_number: build,
                            fetched_at: jiff::Timestamp::now(),
                            user_agent: props.user_agent().to_string(),
                        },
                    );
                }
                Some(_) => tracing::debug!("the cached build number is current"),
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
            other => {
                let total = self.unhandled.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::debug!(
                    "{} is not implemented yet ({total} unhandled commands so far)",
                    other.name()
                );
            }
        }
    }

    async fn login(&mut self, token: Token) {
        self.http.set_token(Some(token.clone()));

        let user = {
            let _permit = self.rest.acquire().await;
            crate::discord::auth::validate(&self.http).await
        };

        match user {
            Ok(user) => {
                let stored_in = match self.store.save(&token) {
                    Ok(kind) => kind,
                    Err(e) => {
                        // Failing to store is not failing to log in. The
                        // session works; it just will not survive a restart.
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

        self.gateway = Some(GatewayTask {
            control: control_tx,
            cancel,
            join,
        });
    }

    async fn disconnect(&mut self) {
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
