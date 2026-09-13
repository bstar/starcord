//! What the client does, as opposed to what it knows.
//!
//! Everything under here is a `Command` handler. They share one [`Ops`] value,
//! which is cheap to clone and holds the four things every operation needs: the
//! HTTP client, the bridge to `State` and the event channel, a semaphore
//! bounding how much REST work runs at once, and a way to push a payload up the
//! gateway socket.
//!
//! The socket is behind an `ArcSwapOption` rather than a field, because it is
//! replaced on every reconnect and the tasks holding an `Ops` outlive any one
//! connection. A payload sent while the socket is down is dropped and said so
//! in the log; the alternative is a queue of stale subscriptions delivered to a
//! gateway that has forgotten the session they referred to.
//!
//! [`Shared`] is the small pile of mutable policy that is not `State`: what is
//! subscribed, when typing was last sent, what has been acked, and which
//! channel the user is looking at. It is a `std::sync::Mutex` and nothing
//! `await`s while holding it — the lock is taken to decide, released, and then
//! the request is made.

pub mod ack;
pub mod open;
pub mod send;
pub mod typing;
pub mod upload;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use arc_swap::ArcSwapOption;
use tokio::sync::{mpsc, Semaphore};

use crate::discord::gateway::identify::Subscriptions;
use crate::discord::gateway::{Bridge, Control};
use crate::discord::handle::{Coalescer, Event, Nonce, Note};
use crate::discord::http::Http;
use crate::discord::snowflake::ChannelId;

use ack::AckCoalescer;

/// The policy that is not state.
pub struct Shared {
    /// Member-list subscriptions, and what the gateway has been told.
    pub subscriptions: Subscriptions,
    /// Last typing indicator per channel.
    pub typing: Coalescer,
    pub acks: AckCoalescer,
    /// The channel the user is looking at, from `Command::SetFocus`. An ack is
    /// never sent for anything else.
    pub focus: Option<ChannelId>,
    /// Whether the terminal itself has focus. A message that arrives while the
    /// window is in the background is not read, whatever channel is on screen.
    pub terminal_focused: bool,
    /// Which channel each outstanding send belongs to, so `RetrySend` and
    /// `CancelSend` can find it from a nonce alone.
    pub sends: HashMap<Nonce, ChannelId>,
}

impl Shared {
    pub fn new(legacy_lazy_request: bool) -> Self {
        Self {
            subscriptions: Subscriptions::new(legacy_lazy_request),
            typing: Coalescer::default(),
            acks: AckCoalescer::default(),
            focus: None,
            terminal_focused: true,
            sends: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Ops {
    pub http: Arc<Http>,
    pub bridge: Bridge,
    /// `[media]`. Read here for one thing only: the attachment cap, which is
    /// checked before a file is sent rather than after Discord refuses it.
    pub media: Arc<crate::discord::media::MediaConfig>,
    /// A cap on concurrency, not a rate limit: forty avatars must not open
    /// forty connections that then queue behind the same bucket anyway.
    pub rest: Arc<Semaphore>,
    /// Replaced on every reconnect; `None` while the socket is down.
    pub gateway: Arc<ArcSwapOption<mpsc::Sender<Control>>>,
    pub shared: Arc<Mutex<Shared>>,
}

impl Ops {
    pub fn new(
        http: Arc<Http>,
        bridge: Bridge,
        media: Arc<crate::discord::media::MediaConfig>,
        rest: Arc<Semaphore>,
        gateway: Arc<ArcSwapOption<mpsc::Sender<Control>>>,
        legacy_lazy_request: bool,
    ) -> Self {
        Self {
            http,
            bridge,
            media,
            rest,
            gateway,
            shared: Arc::new(Mutex::new(Shared::new(legacy_lazy_request))),
        }
    }

    /// The largest message-worth of attachments this client will send.
    pub fn attachment_cap(&self) -> u64 {
        self.media.max_attachment_mib * 1024 * 1024
    }

    /// The shared policy. Never held across an `await`.
    pub fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Push a payload up the socket.
    ///
    /// Dropped with a log line when there is no socket. Queuing it would mean
    /// delivering a subscription to a gateway that has forgotten the session it
    /// referred to.
    pub fn to_gateway(&self, payload: String) -> bool {
        let Some(control) = self.gateway.load_full() else {
            tracing::debug!("dropped a gateway payload: the socket is down");
            return false;
        };
        match control.try_send(Control::Send(payload)) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!("could not reach the gateway: {e}");
                false
            }
        }
    }

    pub fn note(&self, note: Note) {
        self.bridge.events.send(Event::Note(note));
    }

    pub fn emit(&self, event: Event) {
        self.bridge.events.send(event);
    }

    /// Run one REST call under the concurrency cap.
    pub async fn rest<T, F>(&self, work: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let _permit = self.rest.acquire().await;
        work.await
    }

    /// Read `State`. The guard is dropped before anything awaits.
    pub fn state(&self) -> std::sync::RwLockReadGuard<'_, crate::discord::state::State> {
        self.bridge.state.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state_mut(&self) -> std::sync::RwLockWriteGuard<'_, crate::discord::state::State> {
        self.bridge.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Which channel the user is looking at.
    pub fn focus(&self) -> Option<ChannelId> {
        self.shared().focus
    }

    /// `Command::SetFocus`.
    ///
    /// Moving focus is what makes an ack legitimate, so it is also where the
    /// channel the user has *left* stops being ackable.
    pub fn set_focus(&self, channel: Option<ChannelId>, terminal_focused: bool) {
        let mut shared = self.shared();
        shared.focus = channel;
        shared.terminal_focused = terminal_focused;
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! A working `Ops` with no network and no gateway, for the tests in this
    //! module's children.

    use super::*;
    use crate::discord::handle::{Connection, EventSink};
    use crate::discord::props::ClientProps;
    use crate::discord::state::State;
    use std::sync::RwLock;

    pub struct Harness {
        pub ops: Ops,
        pub events: crossbeam_channel::Receiver<Event>,
        /// What the gateway would have been sent.
        pub sent: mpsc::Receiver<Control>,
    }

    pub fn harness(base: &str) -> Harness {
        let (event_tx, event_rx) = crossbeam_channel::bounded(256);
        let (control_tx, control_rx) = mpsc::channel(64);
        let sink = EventSink::detached(event_tx);
        let bridge = Bridge {
            state: Arc::new(RwLock::new(State::new())),
            events: sink,
            status: Arc::new(arc_swap::ArcSwap::from_pointee(Connection::LoggedOut)),
        };
        let http = Arc::new(
            Http::with_base(Arc::new(ClientProps::new("en-US", 1)), base.to_string())
                .expect("a client with no tls work to do")
                .with_retry_policy(crate::discord::http::limits::RetryPolicy {
                    attempts: 2,
                    server_error_backoff: std::time::Duration::from_millis(1),
                    max_wait: std::time::Duration::from_secs(1),
                }),
        );
        let ops = Ops::new(
            http,
            bridge,
            Arc::new(crate::discord::media::MediaConfig::default()),
            Arc::new(Semaphore::new(8)),
            Arc::new(ArcSwapOption::from_pointee(control_tx)),
            false,
        );
        Harness {
            ops,
            events: event_rx,
            sent: control_rx,
        }
    }
}
