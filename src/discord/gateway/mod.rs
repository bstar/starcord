//! The gateway connection.
//!
//! One socket, held open for the life of the session, carrying a compressed
//! stream of everything happening to the account. The shape of the thing is a
//! loop around a `select!`, and almost all of the care is in what happens when
//! it ends:
//!
//! - **Resume before reconnect.** A dropped connection with a live session id
//!   and sequence number is resumed, and a resume replays what was missed. A
//!   fresh IDENTIFY does not — it costs a full READY and loses everything
//!   between the two. The session is kept in memory only; the resume window is
//!   a minute or two, and the thing that actually persists is the token.
//! - **The close code decides.** 4004 means the token was rejected and nothing
//!   is retried, ever. 4010 through 4014 are our fault — a malformed IDENTIFY,
//!   a capability that does not exist — and retrying them is a loop. Everything
//!   else is weather.
//! - **A missed heartbeat ACK is a dead socket.** TCP will happily hold a
//!   connection open to a server that stopped answering; the ACK is the only
//!   evidence there is still something at the other end. One missed ACK closes
//!   the socket and resumes.
//! - **Backoff has jitter.** Without it, every client that lost the same
//!   Discord shard redials at the same instant.

pub mod identify;
pub mod inflate;
pub mod payload;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::discord::auth::Token;
use crate::discord::handle::{Connection, Event, EventSink, Note};
use crate::discord::model::PresenceStatus;
use crate::discord::props::ClientProps;
use crate::discord::state::{apply::apply, State};

use identify::{
    Capabilities, ClientState, Heartbeat, Identify, IdentifyPresence, Outgoing, Resume,
};
use inflate::Inflater;
use payload::{decode, Dispatch, Envelope, Hello, OpCode};

/// Where to connect when there is no session to resume.
pub const GATEWAY_URL: &str = "wss://gateway.discord.gg";
/// v9 with a connection-wide deflate stream. See `inflate.rs` for why that is
/// not the same as per-message compression.
pub const GATEWAY_QUERY: &str = "v=9&encoding=json&compress=zlib-stream";

/// The longest a reconnect ever waits.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How long to wait before re-identifying after an INVALID_SESSION, which
/// Discord documents as a one-to-five-second window.
const INVALID_SESSION_MIN: Duration = Duration::from_secs(1);
const INVALID_SESSION_MAX: Duration = Duration::from_secs(5);
/// How long READY may take after IDENTIFY before the socket is treated as dead.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Discord close codes that mean stopping is the correct response.
///
/// 4004 is the token. 4010–4014 are the IDENTIFY: a bad shard, a payload that
/// is too large, a capability or intent that does not exist. Reconnecting
/// changes none of them, and a client that reconnects anyway is a client
/// hammering a socket on a loop.
fn is_fatal(code: u16) -> bool {
    matches!(code, 4004 | 4010 | 4011 | 4012 | 4013 | 4014)
}

/// Codes after which the session cannot be resumed and IDENTIFY must be sent
/// fresh.
fn invalidates_session(code: u16) -> bool {
    matches!(code, 4007 | 4009 | 4003)
}

fn close_reason(code: u16) -> &'static str {
    match code {
        4000 => "unknown error",
        4001 => "unknown opcode",
        4002 => "decode error",
        4003 => "not authenticated",
        4004 => "the token was rejected",
        4005 => "already authenticated",
        4007 => "the resume sequence was invalid",
        4008 => "rate limited",
        4009 => "the session timed out",
        4010 => "invalid shard",
        4011 => "sharding required",
        4012 => "invalid api version",
        4013 => "invalid intents",
        4014 => "disallowed intents",
        _ => "closed",
    }
}

/// How the gateway is configured for one login.
#[derive(Clone)]
pub struct GatewayConfig {
    pub token: Token,
    pub props: Arc<ClientProps>,
    pub presence: PresenceStatus,
    /// Write every raw dispatch here, for fixture capture.
    pub record_dir: Option<PathBuf>,
}

/// What the core can ask of a live connection.
#[derive(Debug)]
pub enum Control {
    /// A payload built elsewhere — a presence update, a member-list
    /// subscription. Already serialised, because the gateway does not need to
    /// know what any of them mean.
    Send(String),
    /// Drop the socket and come back. The status line's reconnect word.
    Reconnect,
}

/// Everything the gateway writes to.
#[derive(Clone)]
pub struct Bridge {
    pub state: Arc<RwLock<State>>,
    pub events: EventSink,
    pub status: Arc<ArcSwap<Connection>>,
}

impl Bridge {
    pub fn set_status(&self, connection: Connection) {
        let connection = Arc::new(connection);
        self.status.store(Arc::clone(&connection));
        self.events.send(Event::Status(connection));
    }

    pub fn note(&self, note: Note) {
        self.events.send(Event::Note(note));
    }

    /// Apply one dispatch and publish what it changed.
    ///
    /// The write lock is taken, used and dropped with no `await` in between.
    /// Holding it across one would park the UI on a socket read.
    fn apply(&self, dispatch: Dispatch) {
        let events = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            apply(&mut state, dispatch)
        };
        for event in events {
            self.events.send(event);
        }
    }
}

/// A live session, for RESUME.
#[derive(Debug, Clone)]
struct Session {
    id: String,
    seq: u64,
    resume_url: Option<String>,
}

/// Run until cancelled.
pub async fn run(
    config: GatewayConfig,
    bridge: Bridge,
    mut control: mpsc::Receiver<Control>,
    cancel: CancellationToken,
) {
    let recorded = AtomicU64::new(0);
    let mut session: Option<Session> = None;
    let mut attempt: u32 = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        let resuming = session.is_some();
        bridge.set_status(if resuming {
            Connection::Resuming
        } else {
            Connection::Connecting
        });

        let outcome = connect_once(
            &config,
            &bridge,
            &mut control,
            &cancel,
            &mut session,
            &recorded,
        )
        .await;

        match outcome {
            Outcome::Cancelled => break,
            Outcome::Fatal(reason) => {
                bridge.set_status(Connection::AuthFailed(reason.clone()));
                bridge.note(Note::error("gateway-fatal", reason));
                break;
            }
            Outcome::Ready => {
                // The connection lived long enough to identify, so the next
                // failure starts its backoff from zero rather than from
                // wherever the last one left it.
                attempt = 0;
                continue;
            }
            Outcome::Retry(reason) => {
                attempt = attempt.saturating_add(1);
                let wait = backoff(attempt);
                bridge.set_status(Connection::Reconnecting {
                    attempt,
                    next_in: wait,
                    reason: reason.clone(),
                });
                tracing::info!("gateway: {reason}; reconnecting in {wait:?} (attempt {attempt})");
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }

    bridge.set_status(Connection::LoggedOut);
}

enum Outcome {
    /// The socket closed after a successful identify or resume.
    Ready,
    /// It never got that far, or failed in a way worth waiting before retrying.
    Retry(String),
    /// Stop. Retrying cannot help.
    Fatal(String),
    Cancelled,
}

/// `min(60 s, 2^attempt s)` with up to a quarter of that again in jitter.
fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_secs(1u64 << attempt.min(6));
    let base = base.min(MAX_BACKOFF);
    let jitter = rand::rng().random_range(0.0..0.25);
    base + base.mul_f64(jitter)
}

async fn connect_once(
    config: &GatewayConfig,
    bridge: &Bridge,
    control: &mut mpsc::Receiver<Control>,
    cancel: &CancellationToken,
    session: &mut Option<Session>,
    recorded: &AtomicU64,
) -> Outcome {
    let base = session
        .as_ref()
        .and_then(|s| s.resume_url.clone())
        .unwrap_or_else(|| GATEWAY_URL.to_string());
    let url = format!("{}/?{GATEWAY_QUERY}", base.trim_end_matches('/'));

    let request = match build_request(&url, config.props.user_agent()) {
        Ok(request) => request,
        Err(e) => return Outcome::Fatal(format!("the gateway url is not usable: {e}")),
    };

    let socket = tokio::select! {
        _ = cancel.cancelled() => return Outcome::Cancelled,
        result = tokio_tungstenite::connect_async(request) => result,
    };

    let (mut socket, _response) = match socket {
        Ok(pair) => pair,
        Err(e) => {
            // A resume URL that no longer answers is not a reason to keep
            // trying the same dead host.
            if let Some(existing) = session.as_mut() {
                existing.resume_url = None;
            }
            return Outcome::Retry(format!("could not connect: {e}"));
        }
    };

    let mut inflater = Inflater::new();
    let mut heartbeat: Option<tokio::time::Interval> = None;
    let mut awaiting_ack = false;
    let mut identified = false;
    let ready_deadline = tokio::time::Instant::now() + READY_TIMEOUT;

    loop {
        let tick = async {
            match heartbeat.as_mut() {
                Some(interval) => interval.tick().await,
                // Before HELLO there is no interval; parking forever here is
                // correct, because the `select!` has other arms.
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = socket.close(None).await;
                return Outcome::Cancelled;
            }

            _ = tokio::time::sleep_until(ready_deadline), if !identified => {
                let _ = socket.close(None).await;
                return Outcome::Retry("the gateway did not answer the handshake".into());
            }

            command = control.recv() => {
                match command {
                    Some(Control::Send(text)) => {
                        if let Err(e) = socket.send(Message::Text(text.into())).await {
                            return Outcome::Retry(format!("send failed: {e}"));
                        }
                    }
                    Some(Control::Reconnect) => {
                        let _ = socket.close(None).await;
                        return Outcome::Retry("reconnect requested".into());
                    }
                    // The core dropped its end, which happens on shutdown.
                    None => {
                        let _ = socket.close(None).await;
                        return Outcome::Cancelled;
                    }
                }
            }

            _ = tick => {
                if awaiting_ack {
                    // The previous beat was never acknowledged. TCP would hold
                    // this socket open indefinitely; the ACK is the only proof
                    // anybody is still there.
                    let _ = socket.close(None).await;
                    return Outcome::Retry("the gateway stopped acknowledging heartbeats".into());
                }
                let seq = session.as_ref().map(|s| s.seq).filter(|s| *s > 0);
                let beat = serde_json::to_string(&Outgoing::new(OpCode::Heartbeat, Heartbeat(seq)))
                    .expect("a heartbeat is two numbers");
                if let Err(e) = socket.send(Message::Text(beat.into())).await {
                    return Outcome::Retry(format!("heartbeat failed: {e}"));
                }
                awaiting_ack = true;
            }

            message = socket.next() => {
                let message = match message {
                    Some(Ok(message)) => message,
                    Some(Err(e)) => return Outcome::Retry(format!("socket error: {e}")),
                    None => return Outcome::Retry("the gateway closed the connection".into()),
                };

                let bytes = match message {
                    Message::Binary(bytes) => bytes,
                    // The gateway sends binary under zlib-stream; a text frame
                    // would be a protocol change worth noticing rather than
                    // guessing at.
                    Message::Text(text) => {
                        tracing::debug!("ignoring an unexpected text frame of {} bytes", text.len());
                        continue;
                    }
                    Message::Close(frame) => {
                        let code = frame
                            .as_ref()
                            .map(|f| u16::from(f.code))
                            .unwrap_or(1006);
                        let reason = close_reason(code);
                        if is_fatal(code) {
                            *session = None;
                            return Outcome::Fatal(format!("{reason} (close {code})"));
                        }
                        if invalidates_session(code) {
                            *session = None;
                        }
                        return Outcome::Retry(format!("{reason} (close {code})"));
                    }
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                };

                let inflated = match inflater.push(&bytes) {
                    Ok(Some(inflated)) => inflated,
                    Ok(None) => continue,
                    Err(e) => {
                        // A corrupt stream cannot be recovered: the shared
                        // dictionary is now out of step with the sender's.
                        return Outcome::Retry(format!("the compressed stream broke: {e}"));
                    }
                };

                let envelope: Envelope<'_> = match serde_json::from_slice(inflated) {
                    Ok(envelope) => envelope,
                    Err(e) => {
                        tracing::warn!("a gateway message did not parse as an envelope: {e}");
                        continue;
                    }
                };

                match envelope.opcode() {
                    OpCode::Hello => {
                        let Some(hello) = envelope
                            .d
                            .and_then(|d| serde_json::from_str::<Hello>(d.get()).ok())
                        else {
                            return Outcome::Retry("HELLO carried no heartbeat interval".into());
                        };

                        let period = Duration::from_millis(hello.heartbeat_interval.max(1));
                        // The first beat is jittered across the interval, which
                        // is what Discord asks for and what stops every client
                        // that reconnected together from beating in lockstep.
                        let offset = period.mul_f64(rand::rng().random_range(0.0..1.0));
                        let mut interval = tokio::time::interval_at(
                            tokio::time::Instant::now() + offset,
                            period,
                        );
                        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        heartbeat = Some(interval);

                        let text = match session.as_ref() {
                            Some(existing) => {
                                serde_json::to_string(&Outgoing::new(
                                    OpCode::Resume,
                                    Resume {
                                        token: &config.token,
                                        session_id: &existing.id,
                                        seq: existing.seq,
                                    },
                                ))
                            }
                            None => {
                                serde_json::to_string(&Outgoing::new(
                                    OpCode::Identify,
                                    Identify {
                                        token: &config.token,
                                        capabilities: Capabilities::client(),
                                        properties: config.props.identify_properties(),
                                        presence: IdentifyPresence::new(config.presence),
                                        compress: false,
                                        client_state: ClientState::default(),
                                    },
                                ))
                            }
                        };

                        let text = match text {
                            Ok(text) => text,
                            Err(e) => return Outcome::Fatal(format!("could not build IDENTIFY: {e}")),
                        };
                        bridge.set_status(Connection::Identifying);
                        if let Err(e) = socket.send(Message::Text(text.into())).await {
                            return Outcome::Retry(format!("could not send IDENTIFY: {e}"));
                        }
                    }

                    OpCode::HeartbeatAck => awaiting_ack = false,

                    // The gateway asking for a beat out of band, which it does
                    // when it is about to go away.
                    OpCode::Heartbeat => {
                        let seq = session.as_ref().map(|s| s.seq).filter(|s| *s > 0);
                        let beat = serde_json::to_string(
                            &Outgoing::new(OpCode::Heartbeat, Heartbeat(seq)),
                        )
                        .expect("a heartbeat is two numbers");
                        if let Err(e) = socket.send(Message::Text(beat.into())).await {
                            return Outcome::Retry(format!("heartbeat failed: {e}"));
                        }
                        awaiting_ack = true;
                    }

                    OpCode::Reconnect => {
                        // Discord is moving this session. The session id stays
                        // so the next connection resumes rather than
                        // re-READYing.
                        let _ = socket.close(None).await;
                        return Outcome::Retry("the gateway asked for a reconnect".into());
                    }

                    OpCode::InvalidSession => {
                        let resumable = envelope
                            .d
                            .and_then(|d| serde_json::from_str::<bool>(d.get()).ok())
                            .unwrap_or(false);
                        if !resumable {
                            *session = None;
                        }
                        let wait = Duration::from_millis(
                            rand::rng().random_range(
                                INVALID_SESSION_MIN.as_millis() as u64
                                    ..INVALID_SESSION_MAX.as_millis() as u64,
                            ),
                        );
                        let _ = socket.close(None).await;
                        tokio::time::sleep(wait).await;
                        return Outcome::Retry("the session was invalidated".into());
                    }

                    OpCode::Dispatch => {
                        if let Some(seq) = envelope.s {
                            match session.as_mut() {
                                Some(existing) => existing.seq = seq,
                                None => {
                                    // A sequence before READY means a resumed
                                    // session's replay; there is nothing to
                                    // store it in yet, and READY will.
                                }
                            }
                        }

                        let event = envelope.t.unwrap_or("");
                        if let Some(dir) = config.record_dir.as_deref() {
                            record(dir, envelope.s.unwrap_or(0), event, inflated, recorded);
                        }

                        let dispatch = decode(event, envelope.d);
                        match &dispatch {
                            Dispatch::Ready(ready) => {
                                *session = Some(Session {
                                    id: ready.session_id.clone(),
                                    seq: envelope.s.unwrap_or(0),
                                    resume_url: ready.resume_gateway_url.clone(),
                                });
                                identified = true;
                                bridge.apply(dispatch);
                                bridge.set_status(Connection::Ready {
                                    since: std::time::Instant::now(),
                                    resumed: false,
                                });
                                continue;
                            }
                            Dispatch::Resumed => {
                                identified = true;
                                bridge.set_status(Connection::Ready {
                                    since: std::time::Instant::now(),
                                    resumed: true,
                                });
                                tracing::info!("gateway session resumed");
                                continue;
                            }
                            _ => {}
                        }

                        // A resume replays what was missed before RESUMED
                        // arrives, so a dispatch before the handshake finished
                        // is real and is applied like any other.
                        bridge.apply(dispatch);
                    }

                    other => {
                        tracing::trace!("ignoring gateway op {:?}", other);
                    }
                }
            }
        }
    }
}

/// Build the upgrade request with this client's own headers.
///
/// `Origin` matters: Discord's gateway treats a browser session and a bare
/// websocket differently, and the whole point of `ClientProps` is that every
/// connection this client makes looks like the same browser.
fn build_request(
    url: &str,
    user_agent: &str,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, anyhow::Error> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut request = url.into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("User-Agent", user_agent.parse()?);
    headers.insert("Origin", "https://discord.com".parse()?);
    headers.insert("Accept-Language", "en-US,en;q=0.9".parse()?);
    Ok(request)
}

/// Write one raw dispatch out for fixture capture.
///
/// Best effort and never fatal: a recording that fails is a recording, not a
/// session. The counter makes the filenames unique when a resumed session
/// replays sequence numbers it has already used.
fn record(dir: &std::path::Path, seq: u64, event: &str, raw: &[u8], counter: &AtomicU64) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::debug!("could not create the recording directory: {e}");
        return;
    }
    let n = counter.fetch_add(1, Ordering::Relaxed);
    let safe: String = event
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = dir.join(format!("{seq:06}_{n:04}_{safe}.json"));
    if let Err(e) = std::fs::write(&path, raw) {
        tracing::debug!("could not write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_close_codes_that_must_not_be_retried() {
        for code in [4004, 4010, 4011, 4012, 4013, 4014] {
            assert!(is_fatal(code), "{code} should have stopped the client");
        }
        // Weather, all of it.
        for code in [1000, 1001, 1006, 4000, 4001, 4002, 4005, 4007, 4008, 4009] {
            assert!(!is_fatal(code), "{code} should have been retried");
        }
    }

    #[test]
    fn the_close_codes_that_burn_the_session() {
        for code in [4003, 4007, 4009] {
            assert!(
                invalidates_session(code),
                "{code} leaves a session that cannot be resumed"
            );
        }
        assert!(
            !invalidates_session(4000),
            "an unknown error is resumable, and resuming is how nothing is missed"
        );
    }

    #[test]
    fn every_close_code_has_something_to_say() {
        for code in [
            4000u16, 4001, 4002, 4003, 4004, 4005, 4007, 4008, 4009, 4010, 4011, 4012, 4013, 4014,
        ] {
            assert_ne!(close_reason(code), "closed", "{code} has no message");
        }
        assert_eq!(close_reason(1006), "closed");
    }

    #[test]
    fn backoff_climbs_jitters_and_stops_climbing() {
        let mut previous = Duration::ZERO;
        for attempt in 1..=6 {
            let wait = backoff(attempt);
            assert!(
                wait > previous,
                "attempt {attempt} did not back off further"
            );
            assert!(wait <= MAX_BACKOFF.mul_f64(1.25));
            previous = wait;
        }
        for attempt in 7..20 {
            assert!(backoff(attempt) <= MAX_BACKOFF.mul_f64(1.25));
        }

        // Jitter is the point: two clients that lost the same shard must not
        // redial together.
        let samples: std::collections::HashSet<u128> =
            (0..32).map(|_| backoff(4).as_nanos()).collect();
        assert!(samples.len() > 1, "the backoff has no jitter");
    }

    #[test]
    fn the_gateway_url_asks_for_a_compressed_v9_stream() {
        let url = format!("{GATEWAY_URL}/?{GATEWAY_QUERY}");
        assert!(url.starts_with("wss://"), "{url}");
        assert!(url.contains("v=9"));
        assert!(url.contains("encoding=json"));
        assert!(
            url.contains("compress=zlib-stream"),
            "per-message compression is not what the inflater implements"
        );
    }

    #[test]
    fn the_upgrade_request_carries_this_clients_identity() {
        let request =
            build_request(&format!("{GATEWAY_URL}/?{GATEWAY_QUERY}"), "a-user-agent").unwrap();
        assert_eq!(request.headers()["User-Agent"], "a-user-agent");
        assert_eq!(request.headers()["Origin"], "https://discord.com");
    }

    #[test]
    fn a_recording_is_named_by_sequence_and_event() {
        let dir = tempfile::tempdir().unwrap();
        let counter = AtomicU64::new(0);
        record(dir.path(), 7, "MESSAGE_CREATE", b"{\"op\":0}", &counter);
        record(dir.path(), 7, "MESSAGE/CREATE", b"{\"op\":0}", &counter);

        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "000007_0000_MESSAGE_CREATE.json",
                "000007_0001_MESSAGE_CREATE.json"
            ],
            "a replayed sequence number must not overwrite an earlier recording, \
             and an event name must not be able to escape the directory"
        );
    }
}
