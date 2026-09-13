//! Signing in by scanning a code with the phone app.
//!
//! This is the login worth recommending, and the reason is not convenience. The
//! alternative — pasting a token out of a browser's developer tools — means the
//! token exists in a clipboard, possibly in a file, and certainly in the user's
//! head as "the thing I copy". Scanning a code means the password is never
//! typed into a terminal, the token never appears anywhere a human can read it,
//! and the phone shows who is asking before it agrees.
//!
//! The handshake, from Discord's side:
//!
//! 1. Connect to the remote-auth gateway. It sends `hello` with a heartbeat
//!    interval and the number of milliseconds the whole exchange may take.
//! 2. Send `init` with an RSA-2048 public key, as base64 of its SPKI DER.
//!    Everything Discord sends from here on is sealed to that key, and the
//!    private half never leaves this process.
//! 3. Discord sends `nonce_proof` with an encrypted nonce. Decrypt it and reply
//!    with a proof, which is how Discord learns the key is really ours.
//! 4. Discord sends `pending_remote_init` with a fingerprint. The QR code is
//!    `https://discord.com/ra/<fingerprint>`.
//! 5. The phone scans it: `pending_ticket` arrives with the scanner's identity,
//!    encrypted, so the terminal can show who is about to log in.
//! 6. The user taps accept: `pending_login` arrives with a ticket, which
//!    `POST /users/@me/remote-auth/login` exchanges for an encrypted token.
//!    They tap cancel instead: `cancel` arrives and nothing happens.
//!
//! **The `nonce_proof` reply was the one open question, and it is settled.**
//! The two descriptions of this protocol that exist disagree about whether the
//! proof is the base64url of the decrypted nonce or of its SHA-256. It is the
//! hash: a live handshake against `remote-auth-gateway.discord.gg` on
//! 2026-09-13 got past step 3 and came back with a fingerprint, which it could
//! not have done had the proof been wrong — a bad proof is a close 4002 and
//! nothing else. [`PROOF_IS_HASHED`] is kept as a constant anyway, so that the
//! day Discord changes its mind the other answer is one line away.
//!
//! What that run did *not* exercise is everything after the scan: the shape of
//! the ticket payload, the login exchange and the token decrypt all need a
//! phone. Those are still from the documentation, and `AGENTS.md` says so.
//!
//! The private key is never printed. [`Keys`] has a hand-written `Debug`, and
//! the nonce, the ticket and the token are never logged at any level.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use rsa::pkcs8::EncodePublicKey as _;
use rsa::{Oaep, RsaPrivateKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use super::Token;
use crate::discord::handle::{AuthEvent, MediaKey};
use crate::discord::http::{api, Http};
use crate::discord::model::User;
use crate::discord::props::ClientProps;
use crate::discord::snowflake::UserId;

/// The remote-auth gateway. Version 2 is what the web client speaks.
pub const REMOTE_AUTH_URL: &str = "wss://remote-auth-gateway.discord.gg/?v=2";

/// What the phone camera is pointed at.
pub const RA_BASE: &str = "https://discord.com/ra";

/// Whether the `nonce_proof` reply is the hash of the nonce or the nonce.
///
/// The hash, confirmed against the live gateway on 2026-09-13: the handshake
/// reached `pending_remote_init`, which a wrong proof cannot do. Kept as a
/// constant rather than inlined so that the other answer stays one line away.
const PROOF_IS_HASHED: bool = true;

/// How large a key to generate.
///
/// 2048 and not less: RSA-OAEP with SHA-256 can carry at most `bits/8 - 66`
/// bytes, and a Discord token is about seventy. A 1024-bit key would complete
/// the handshake and then fail to decrypt the one thing the handshake was for.
const KEY_BITS: usize = 2048;

/// How long to wait for `hello` before giving up on a socket that connected and
/// then said nothing.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the whole exchange may take when the server does not say.
///
/// A fallback only. The live gateway sends `timeout_ms` in `hello`, and what it
/// sent on 2026-09-13 was six minutes.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("could not reach the login gateway: {0}")]
    Socket(String),
    #[error("the login gateway said something unexpected: {0}")]
    Protocol(String),
    #[error("the login could not be decrypted: {0}")]
    Crypto(String),
    #[error("{0}")]
    Closed(String),
    #[error("the code expired before it was scanned")]
    Expired,
    #[error("the login was cancelled")]
    Cancelled,
    #[error("exchanging the ticket failed: {0}")]
    Exchange(String),
}

/// The private half of the handshake.
///
/// `Debug` is written by hand and says nothing. This is the key every secret in
/// the exchange is sealed to; a derived `Debug` on any struct that came to hold
/// one would be a private key in a log file.
pub struct Keys {
    private: RsaPrivateKey,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(<redacted>)")
    }
}

impl Keys {
    /// Generate a keypair. Slow — hundreds of milliseconds — so callers run it
    /// on a blocking thread.
    pub fn generate() -> Result<Self, RemoteError> {
        Self::with_bits(KEY_BITS)
    }

    pub fn with_bits(bits: usize) -> Result<Self, RemoteError> {
        let private = RsaPrivateKey::new(&mut rand_core::OsRng, bits)
            .map_err(|e| RemoteError::Crypto(format!("no key could be generated: {e}")))?;
        Ok(Self { private })
    }

    /// The public half, as Discord wants it: base64 of the SPKI DER, with no
    /// PEM header and no line breaks.
    pub fn encoded_public_key(&self) -> Result<String, RemoteError> {
        let der = self
            .private
            .to_public_key()
            .to_public_key_der()
            .map_err(|e| RemoteError::Crypto(e.to_string()))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(der.as_bytes()))
    }

    /// Decrypt one base64 payload from the gateway.
    pub fn decrypt(&self, encoded: &str) -> Result<Vec<u8>, RemoteError> {
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|e| RemoteError::Crypto(format!("that was not base64: {e}")))?;
        self.private
            .decrypt(Oaep::new::<Sha256>(), &sealed)
            .map_err(|e| RemoteError::Crypto(e.to_string()))
    }
}

/// The proof that the key in `init` is the one this process holds.
pub fn proof(nonce: &[u8]) -> String {
    let hashed = Sha256::digest(nonce);
    let material: &[u8] = if PROOF_IS_HASHED { &hashed } else { nonce };
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(material)
}

/// What Discord sends.
///
/// `Unknown` rather than a hard error on an op nobody here knows: a gateway
/// that grows a new message must not break a login.
#[derive(Debug, Deserialize)]
#[serde(tag = "op")]
enum Incoming {
    #[serde(rename = "hello")]
    Hello {
        heartbeat_interval: u64,
        #[serde(default)]
        timeout_ms: u64,
    },
    #[serde(rename = "heartbeat_ack")]
    HeartbeatAck,
    #[serde(rename = "nonce_proof")]
    NonceProof { encrypted_nonce: String },
    #[serde(rename = "pending_remote_init")]
    PendingRemoteInit { fingerprint: String },
    #[serde(rename = "pending_ticket")]
    PendingTicket { encrypted_user_payload: String },
    #[serde(rename = "pending_login")]
    PendingLogin { ticket: String },
    /// The person with the phone said no.
    #[serde(rename = "cancel")]
    Cancel,
    #[serde(other)]
    Unknown,
}

/// What this client sends.
#[derive(Debug, Serialize)]
#[serde(tag = "op")]
enum Outgoing<'a> {
    #[serde(rename = "init")]
    Init { encoded_public_key: &'a str },
    #[serde(rename = "nonce_proof")]
    NonceProof { proof: &'a str },
    #[serde(rename = "heartbeat")]
    Heartbeat,
}

/// What the remote-auth gateway's close codes mean.
///
/// Four of them, and none is weather: every one describes something about this
/// exchange rather than about the connection, so the answer to all four is to
/// stop and say why rather than to reconnect.
pub fn close_reason(code: u16) -> &'static str {
    match code {
        4000 => "the login gateway does not speak this client's version",
        4001 => "the login gateway could not decode what this client sent",
        4002 => "the login handshake was refused",
        4003 => "the code expired before it was scanned",
        _ => "the login gateway closed the connection",
    }
}

/// Whether a close code means the code simply ran out of time.
///
/// The only one worth generating a new code for, which is why it is separate
/// from the other three.
pub fn is_timeout(code: u16) -> bool {
    code == 4003
}

/// A signed-in account, before anything has been stored.
#[derive(Debug)]
pub struct Authenticated {
    pub token: Token,
    pub user: User,
}

/// How one attempt ended.
enum Outcome {
    Authenticated(Box<Authenticated>),
    /// The code was never scanned. Worth exactly one more.
    TimedOut,
    Cancelled,
}

/// Turn a login URL into the squares a phone camera reads.
///
/// The core does not draw; it produces the matrix and the UI decides what a
/// dark module looks like. Generating it here is also what proves the URL
/// encodes at all before it is shown to anybody.
pub fn matrix(url: &str) -> Result<Vec<Vec<bool>>, RemoteError> {
    let code = qrcode::QrCode::new(url.as_bytes())
        .map_err(|e| RemoteError::Protocol(format!("the login url will not encode: {e}")))?;
    let width = code.width();
    let colors = code.to_colors();
    Ok(colors
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|colour| *colour == qrcode::Color::Dark)
                .collect()
        })
        .collect())
}

/// Run the whole flow, emitting each step as it happens.
///
/// One automatic retry, and only for a code that expired unscanned: that is a
/// user who walked away from the terminal for three minutes, and regenerating
/// once is what a person would have pressed a key for. A second expiry needs
/// the key press, because at that point nobody is there.
pub async fn run(
    http: Arc<Http>,
    props: Arc<ClientProps>,
    cancel: CancellationToken,
    emit: &mut impl FnMut(AuthEvent),
) -> Result<Authenticated, RemoteError> {
    for attempt in 0..2 {
        if cancel.is_cancelled() {
            return Err(RemoteError::Cancelled);
        }

        // Key generation is the one genuinely slow thing here, and a fresh key
        // per attempt rather than a reused one: the first key was published to
        // a gateway that has since closed the connection.
        let keys = tokio::task::spawn_blocking(Keys::generate)
            .await
            .map_err(|e| RemoteError::Crypto(format!("the key task stopped: {e}")))??;

        let mut socket = connect(&props).await?;
        let outcome = drive(&mut socket, &keys, &http, &cancel, emit).await;
        // Best effort; the answer is already in hand either way.
        let _ = socket.close(None).await;

        match outcome? {
            Outcome::Authenticated(authenticated) => return Ok(*authenticated),
            Outcome::Cancelled => return Err(RemoteError::Cancelled),
            Outcome::TimedOut if attempt == 0 => {
                tracing::info!("the login code expired unscanned; generating one more");
                continue;
            }
            Outcome::TimedOut => return Err(RemoteError::Expired),
        }
    }
    Err(RemoteError::Expired)
}

/// Open the socket, with the same identity every other connection uses.
///
/// `Origin` matters here as much as it does on the main gateway: the whole
/// point of one `ClientProps` is that every connection this client makes
/// describes the same browser.
async fn connect(
    props: &ClientProps,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    RemoteError,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

    let mut request = REMOTE_AUTH_URL
        .into_client_request()
        .map_err(|e| RemoteError::Socket(e.to_string()))?;
    {
        let headers = request.headers_mut();
        let user_agent = props
            .user_agent()
            .parse()
            .map_err(|_| RemoteError::Socket("the user agent is not a header value".into()))?;
        headers.insert("User-Agent", user_agent);
        headers.insert(
            "Origin",
            "https://discord.com"
                .parse()
                .map_err(|_| RemoteError::Socket("the origin is not a header value".into()))?,
        );
    }

    let (socket, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| RemoteError::Socket(e.to_string()))?;
    Ok(socket)
}

/// The state machine, over any stream of websocket messages.
///
/// Generic so that a test can drive the whole handshake through a pair of
/// in-process channels: the interesting parts of this file are the crypto and
/// the ordering, and neither should need a network to exercise.
async fn drive<S, E>(
    socket: &mut S,
    keys: &Keys,
    http: &Http,
    cancel: &CancellationToken,
    emit: &mut impl FnMut(AuthEvent),
) -> Result<Outcome, RemoteError>
where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let mut heartbeat: Option<tokio::time::Interval> = None;
    let mut awaiting_ack = false;
    // Replaced by the server's own `timeout_ms` the moment `hello` arrives.
    let mut deadline = tokio::time::Instant::now() + HELLO_TIMEOUT;

    loop {
        let tick = async {
            match heartbeat.as_mut() {
                Some(interval) => interval.tick().await,
                // No interval before `hello`. Parking forever is correct: the
                // other arms of the `select!` are what move this along.
                None => std::future::pending().await,
            }
        };

        let message = tokio::select! {
            _ = cancel.cancelled() => return Ok(Outcome::Cancelled),

            _ = tokio::time::sleep_until(deadline) => {
                return Ok(Outcome::TimedOut);
            }

            _ = tick => {
                if awaiting_ack {
                    return Err(RemoteError::Socket(
                        "the login gateway stopped acknowledging heartbeats".into(),
                    ));
                }
                send(socket, &Outgoing::Heartbeat).await?;
                awaiting_ack = true;
                continue;
            }

            message = socket.next() => message,
        };

        let message = match message {
            Some(Ok(message)) => message,
            Some(Err(e)) => return Err(RemoteError::Socket(e.to_string())),
            None => {
                return Err(RemoteError::Closed(
                    "the login gateway closed the connection".into(),
                ))
            }
        };

        let text = match message {
            Message::Text(text) => text.to_string(),
            // The remote-auth gateway is plain JSON text, uncompressed, unlike
            // the main one.
            Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => text,
                Err(_) => continue,
            },
            Message::Close(frame) => {
                let code = frame.as_ref().map(|f| u16::from(f.code)).unwrap_or(1006);
                if is_timeout(code) {
                    return Ok(Outcome::TimedOut);
                }
                return Err(RemoteError::Closed(format!(
                    "{} (close {code})",
                    close_reason(code)
                )));
            }
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        };

        let incoming: Incoming = match serde_json::from_str(&text) {
            Ok(incoming) => incoming,
            Err(e) => {
                // Never the payload: it carries a nonce or a ticket.
                tracing::debug!("a remote-auth message did not parse: {e}");
                continue;
            }
        };

        match incoming {
            Incoming::Hello {
                heartbeat_interval,
                timeout_ms,
            } => {
                let period = Duration::from_millis(heartbeat_interval.max(1));
                let mut interval =
                    tokio::time::interval_at(tokio::time::Instant::now() + period, period);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                heartbeat = Some(interval);

                let lifetime = if timeout_ms == 0 {
                    DEFAULT_TIMEOUT
                } else {
                    Duration::from_millis(timeout_ms)
                };
                deadline = tokio::time::Instant::now() + lifetime;

                let encoded = keys.encoded_public_key()?;
                send(
                    socket,
                    &Outgoing::Init {
                        encoded_public_key: &encoded,
                    },
                )
                .await?;
            }

            Incoming::HeartbeatAck => awaiting_ack = false,

            Incoming::NonceProof { encrypted_nonce } => {
                let nonce = keys.decrypt(&encrypted_nonce)?;
                let proof = proof(&nonce);
                send(socket, &Outgoing::NonceProof { proof: &proof }).await?;
            }

            Incoming::PendingRemoteInit { fingerprint } => {
                let url = format!("{RA_BASE}/{fingerprint}");
                let matrix = matrix(&url)?;
                let expires_in = deadline.saturating_duration_since(tokio::time::Instant::now());
                emit(AuthEvent::QrReady {
                    url,
                    fingerprint,
                    expires_in,
                    matrix,
                });
            }

            Incoming::PendingTicket {
                encrypted_user_payload,
            } => {
                let payload = keys.decrypt(&encrypted_user_payload)?;
                let scanned = Scanned::parse(&payload)?;
                emit(AuthEvent::QrScanned {
                    username: scanned.username,
                    avatar: scanned.avatar,
                });
            }

            Incoming::PendingLogin { ticket } => {
                let authenticated = exchange(http, keys, &ticket).await?;
                return Ok(Outcome::Authenticated(Box::new(authenticated)));
            }

            Incoming::Cancel => return Ok(Outcome::Cancelled),

            Incoming::Unknown => tracing::trace!("ignoring an unknown remote-auth message"),
        }
    }
}

/// Turn a ticket into a session.
async fn exchange(http: &Http, keys: &Keys, ticket: &str) -> Result<Authenticated, RemoteError> {
    let answer = api::remote_auth_login(http, ticket)
        .await
        .map_err(|e| RemoteError::Exchange(e.to_string()))?;

    let raw = keys.decrypt(&answer.encrypted_token)?;
    let raw =
        String::from_utf8(raw).map_err(|_| RemoteError::Crypto("the token was not text".into()))?;
    let token = Token::new(raw).map_err(|e| RemoteError::Crypto(e.to_string()))?;

    // Validated by using it, as every other login is: a token that came back
    // from the right endpoint and does not work is worth discovering here
    // rather than as a close 4004 a minute later.
    http.set_token(Some(token.clone()));
    match api::me(http).await {
        Ok(user) => Ok(Authenticated { token, user }),
        Err(e) => {
            http.set_token(None);
            Err(RemoteError::Exchange(format!(
                "the new token was refused: {e}"
            )))
        }
    }
}

/// Who scanned the code.
struct Scanned {
    username: String,
    avatar: Option<MediaKey>,
}

impl Scanned {
    /// The payload is `id:discriminator:avatar:username`, colon-separated.
    ///
    /// Split into exactly four, because a username may contain anything at all
    /// and the fourth field runs to the end.
    fn parse(payload: &[u8]) -> Result<Self, RemoteError> {
        let text = std::str::from_utf8(payload)
            .map_err(|_| RemoteError::Crypto("the scanned payload was not text".into()))?;
        let mut parts = text.splitn(4, ':');
        let id = parts.next().unwrap_or("");
        let _discriminator = parts.next().unwrap_or("");
        let avatar = parts.next().unwrap_or("");
        let username = parts.next().unwrap_or("");

        if username.is_empty() {
            return Err(RemoteError::Protocol(
                "the scanned payload had no username in it".into(),
            ));
        }

        let avatar = match (id.parse::<u64>(), avatar) {
            (Ok(id), hash) if !hash.is_empty() => Some(MediaKey::Avatar {
                user: UserId(id),
                hash: hash.to_string(),
                size: 128,
            }),
            _ => None,
        };

        Ok(Self {
            username: username.to_string(),
            avatar,
        })
    }
}

async fn send<S, E>(socket: &mut S, outgoing: &Outgoing<'_>) -> Result<(), RemoteError>
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let text = serde_json::to_string(outgoing)
        .map_err(|e| RemoteError::Protocol(format!("could not build a message: {e}")))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|e| RemoteError::Socket(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::DecodePublicKey as _;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::sync::mpsc;

    /// A stand-in for a websocket error, so `drive` can be generic over one.
    #[derive(Debug)]
    struct Broken(String);

    impl std::fmt::Display for Broken {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    /// The client's end of an in-process socket.
    struct Wire {
        incoming: mpsc::UnboundedReceiver<Result<Message, Broken>>,
        outgoing: mpsc::UnboundedSender<Message>,
    }

    /// The gateway's end of the same socket.
    struct Peer {
        to_client: mpsc::UnboundedSender<Result<Message, Broken>>,
        from_client: mpsc::UnboundedReceiver<Message>,
    }

    impl Peer {
        fn send(&self, value: serde_json::Value) {
            let _ = self
                .to_client
                .send(Ok(Message::Text(value.to_string().into())));
        }

        async fn next(&mut self) -> serde_json::Value {
            let message = self
                .from_client
                .recv()
                .await
                .expect("the client hung up mid-handshake");
            match message {
                Message::Text(text) => serde_json::from_str(&text).expect("not json"),
                other => panic!("the client sent {other:?}"),
            }
        }
    }

    fn wire() -> (Wire, Peer) {
        let (to_client, incoming) = mpsc::unbounded_channel();
        let (outgoing, from_client) = mpsc::unbounded_channel();
        (
            Wire { incoming, outgoing },
            Peer {
                to_client,
                from_client,
            },
        )
    }

    impl Stream for Wire {
        type Item = Result<Message, Broken>;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.incoming.poll_recv(cx)
        }
    }

    impl Sink<Message> for Wire {
        type Error = Broken;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Broken>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Broken> {
            self.outgoing
                .send(item)
                .map_err(|_| Broken("the peer is gone".into()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Broken>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Broken>> {
            Poll::Ready(Ok(()))
        }
    }

    fn seal(public_key_base64: &str, plain: &[u8]) -> String {
        let der = base64::engine::general_purpose::STANDARD
            .decode(public_key_base64)
            .expect("the public key was not base64");
        let key =
            rsa::RsaPublicKey::from_public_key_der(&der).expect("the public key was not SPKI DER");
        let sealed = key
            .encrypt(&mut rand_core::OsRng, Oaep::new::<Sha256>(), plain)
            .expect("could not seal");
        base64::engine::general_purpose::STANDARD.encode(sealed)
    }

    /// One key for the whole module's tests. Generating a 2048-bit key is the
    /// slowest thing here by two orders of magnitude, and every test wants the
    /// same one.
    fn shared_keys() -> &'static Keys {
        static KEYS: std::sync::OnceLock<Keys> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| Keys::generate().expect("a keypair"))
    }

    #[test]
    fn a_private_key_never_prints_itself() {
        let keys = shared_keys();
        assert_eq!(format!("{keys:?}"), "Keys(<redacted>)");

        #[derive(Debug)]
        struct Holder<'a> {
            keys: &'a Keys,
            stage: &'static str,
        }
        let printed = format!(
            "{:?}",
            Holder {
                keys,
                stage: "awaiting scan"
            }
        );
        assert!(printed.contains("Keys(<redacted>)"), "{printed}");
        assert!(printed.len() < 80, "{printed}");
    }

    /// The round trip the whole handshake rests on: Discord seals to the public
    /// key this process published, and only this process can open it.
    #[test]
    fn what_discord_seals_to_the_published_key_comes_back() {
        let keys = shared_keys();
        let encoded = keys.encoded_public_key().unwrap();
        assert!(
            !encoded.contains("BEGIN"),
            "the key is base64 of DER, not PEM"
        );

        let secret = b"a nonce, or a ticket, or a token";
        let sealed = seal(&encoded, secret);
        assert_eq!(keys.decrypt(&sealed).unwrap(), secret);

        // Rubbish is an error, not a panic.
        assert!(keys.decrypt("not base64 at all!!").is_err());
        assert!(keys.decrypt("aGVsbG8=").is_err());
    }

    /// The part of the protocol the two descriptions of it disagree about.
    /// The hash is what the live gateway accepted; this is what makes changing
    /// it deliberate.
    #[test]
    fn the_proof_is_the_base64url_of_the_hash_of_the_nonce() {
        let nonce = b"0123456789abcdef";
        let expected =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(nonce));
        assert_eq!(proof(nonce), expected);

        // Base64url with no padding: `+` and `/` would be rejected by a server
        // reading it out of a URL-safe field, and `=` is not sent.
        assert!(!proof(nonce).contains('='));
        assert!(!proof(nonce).contains('+'));
        assert!(!proof(nonce).contains('/'));
        assert_eq!(
            proof(nonce).len(),
            43,
            "a SHA-256 is 32 bytes, which is 43 base64 characters unpadded"
        );
    }

    #[test]
    fn every_close_code_says_what_went_wrong_and_only_one_is_worth_retrying() {
        for (code, fragment) in [
            (4000u16, "version"),
            (4001, "decode"),
            (4002, "refused"),
            (4003, "expired"),
        ] {
            assert!(
                close_reason(code).contains(fragment),
                "close {code} says {:?}",
                close_reason(code)
            );
        }
        assert_eq!(
            close_reason(1006),
            "the login gateway closed the connection"
        );

        assert!(is_timeout(4003));
        for code in [4000u16, 4001, 4002, 1006, 1000] {
            assert!(!is_timeout(code), "{code} is not a timeout");
        }
    }

    #[test]
    fn a_login_url_encodes_to_something_a_camera_can_read() {
        let matrix = matrix("https://discord.com/ra/abcdef0123456789").unwrap();
        assert!(!matrix.is_empty(), "the matrix is empty");
        assert!(
            matrix.iter().all(|row| row.len() == matrix.len()),
            "a QR code is square"
        );
        assert!(
            matrix.iter().any(|row| row.iter().any(|dark| *dark)),
            "every module was light, which is not a QR code"
        );

        // The three finder patterns: the top-left corner is always dark.
        assert!(matrix[0][0]);
        assert!(matrix[0][matrix.len() - 1]);
        assert!(matrix[matrix.len() - 1][0]);
    }

    #[test]
    fn a_scanned_payload_becomes_a_name_and_an_avatar() {
        let scanned = Scanned::parse(b"80351110224678912:0:8342729096ea3675:alex").unwrap();
        assert_eq!(scanned.username, "alex");
        assert_eq!(
            scanned.avatar,
            Some(MediaKey::Avatar {
                user: UserId(80351110224678912),
                hash: "8342729096ea3675".into(),
                size: 128,
            })
        );

        // No avatar set.
        let plain = Scanned::parse(b"1:0::sam").unwrap();
        assert_eq!(plain.username, "sam");
        assert!(plain.avatar.is_none());

        // A username with a colon in it: the fourth field runs to the end.
        let odd = Scanned::parse(b"1:0::a:b:c").unwrap();
        assert_eq!(odd.username, "a:b:c");

        for bad in [&b""[..], b"1", b"1:0:hash:", b"\xff\xfe"] {
            assert!(Scanned::parse(bad).is_err(), "{bad:?} was accepted");
        }
    }

    /// The whole handshake, driven in process, against a mock ticket endpoint.
    #[tokio::test]
    async fn the_handshake_walks_from_hello_to_a_signed_in_account() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let keys = shared_keys();
        let public = keys.encoded_public_key().unwrap();
        let nonce: Vec<u8> = (0..32u8).collect();
        let token = "mfa.a_token_long_enough_to_be_believed_by_the_parser";

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/users/@me/remote-auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "encrypted_token": seal(&public, token.as_bytes())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "80351110224678912",
                "username": "alex",
                "discriminator": "0"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = Http::with_base(Arc::new(ClientProps::new("en-US", 1)), server.uri()).unwrap();

        let (mut wire, mut peer) = wire();
        let cancel = CancellationToken::new();
        let mut steps: Vec<AuthEvent> = Vec::new();

        let client = async {
            drive(&mut wire, keys, &http, &cancel, &mut |event| {
                steps.push(event)
            })
            .await
        };

        let gateway = async {
            // A long heartbeat interval, so nothing ticks during the test, and a
            // generous lifetime.
            peer.send(serde_json::json!({
                "op": "hello",
                "heartbeat_interval": 60_000,
                "timeout_ms": 60_000
            }));

            let init = peer.next().await;
            assert_eq!(init["op"], "init");
            assert_eq!(
                init["encoded_public_key"], public,
                "the key that went out is not the one this process holds"
            );

            peer.send(serde_json::json!({
                "op": "nonce_proof",
                "encrypted_nonce": seal(&public, &nonce)
            }));

            let reply = peer.next().await;
            assert_eq!(reply["op"], "nonce_proof");
            assert_eq!(
                reply["proof"],
                proof(&nonce),
                "the proof did not match what the nonce should have produced"
            );

            peer.send(serde_json::json!({
                "op": "pending_remote_init",
                "fingerprint": "abcdef0123456789"
            }));
            peer.send(serde_json::json!({
                "op": "pending_ticket",
                "encrypted_user_payload": seal(&public, b"80351110224678912:0:hash:alex")
            }));
            peer.send(serde_json::json!({
                "op": "pending_login",
                "ticket": "a-ticket"
            }));
        };

        let (outcome, ()) = tokio::join!(client, gateway);
        let outcome = outcome.expect("the handshake failed");

        match outcome {
            Outcome::Authenticated(authenticated) => {
                assert_eq!(authenticated.token.expose(), token);
                assert_eq!(authenticated.user.username, "alex");
            }
            _ => panic!("the handshake did not produce an account"),
        }

        assert_eq!(steps.len(), 2, "{steps:?}");
        match &steps[0] {
            AuthEvent::QrReady {
                url,
                fingerprint,
                matrix,
                expires_in,
            } => {
                assert_eq!(url, "https://discord.com/ra/abcdef0123456789");
                assert_eq!(fingerprint, "abcdef0123456789");
                assert!(!matrix.is_empty(), "the UI was given nothing to draw");
                assert!(
                    *expires_in <= Duration::from_secs(60) && *expires_in > Duration::from_secs(50),
                    "{expires_in:?} does not match the server's timeout"
                );
            }
            other => panic!("{other:?}"),
        }
        match &steps[1] {
            AuthEvent::QrScanned { username, avatar } => {
                assert_eq!(username, "alex");
                assert!(avatar.is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    /// The person with the phone said no.
    #[tokio::test]
    async fn a_cancelled_scan_ends_the_exchange() {
        let keys = shared_keys();
        let http = Http::with_base(
            Arc::new(ClientProps::new("en-US", 1)),
            "http://unused.invalid",
        )
        .unwrap();
        let (mut wire, mut peer) = wire();
        let cancel = CancellationToken::new();

        let client = async { drive(&mut wire, keys, &http, &cancel, &mut |_| {}).await };
        let gateway = async {
            peer.send(serde_json::json!({
                "op": "hello", "heartbeat_interval": 60_000, "timeout_ms": 60_000
            }));
            let _ = peer.next().await;
            peer.send(serde_json::json!({"op": "cancel"}));
        };

        let (outcome, ()) = tokio::join!(client, gateway);
        assert!(matches!(outcome.unwrap(), Outcome::Cancelled));
    }

    /// A code nobody scanned. The gateway closes with 4003, and that is the one
    /// close worth generating a new code for.
    #[tokio::test]
    async fn an_expired_code_is_a_timeout_rather_than_a_failure() {
        let keys = shared_keys();
        let http = Http::with_base(
            Arc::new(ClientProps::new("en-US", 1)),
            "http://unused.invalid",
        )
        .unwrap();
        let (mut wire, mut peer) = wire();
        let cancel = CancellationToken::new();

        let client = async { drive(&mut wire, keys, &http, &cancel, &mut |_| {}).await };
        let gateway = async {
            peer.send(serde_json::json!({
                "op": "hello", "heartbeat_interval": 60_000, "timeout_ms": 60_000
            }));
            let _ = peer.next().await;
            let _ = peer.to_client.send(Ok(Message::Close(Some(
                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code: 4003u16.into(),
                    reason: "timeout".into(),
                },
            ))));
        };

        let (outcome, ()) = tokio::join!(client, gateway);
        assert!(matches!(outcome.unwrap(), Outcome::TimedOut));
    }

    /// Any other close is a failure that says which one it was.
    #[tokio::test]
    async fn a_handshake_failure_names_its_close_code() {
        let keys = shared_keys();
        let http = Http::with_base(
            Arc::new(ClientProps::new("en-US", 1)),
            "http://unused.invalid",
        )
        .unwrap();
        let (mut wire, mut peer) = wire();
        let cancel = CancellationToken::new();

        let client = async { drive(&mut wire, keys, &http, &cancel, &mut |_| {}).await };
        let gateway = async {
            peer.send(serde_json::json!({
                "op": "hello", "heartbeat_interval": 60_000, "timeout_ms": 60_000
            }));
            let _ = peer.next().await;
            let _ = peer.to_client.send(Ok(Message::Close(Some(
                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code: 4002u16.into(),
                    reason: "handshake".into(),
                },
            ))));
        };

        let (outcome, ()) = tokio::join!(client, gateway);
        match outcome {
            Err(RemoteError::Closed(reason)) => {
                assert!(reason.contains("4002"), "{reason}");
                assert!(reason.contains("refused"), "{reason}");
            }
            other => panic!("{other:?}", other = other.map(|_| "an outcome")),
        }
    }

    /// Cancelling from this side stops it wherever it is.
    #[tokio::test]
    async fn cancelling_stops_a_handshake_that_is_waiting() {
        let keys = shared_keys();
        let http = Http::with_base(
            Arc::new(ClientProps::new("en-US", 1)),
            "http://unused.invalid",
        )
        .unwrap();
        let (mut wire, mut peer) = wire();
        let cancel = CancellationToken::new();
        let child = cancel.clone();

        let client = async { drive(&mut wire, keys, &http, &cancel, &mut |_| {}).await };
        let gateway = async {
            peer.send(serde_json::json!({
                "op": "hello", "heartbeat_interval": 60_000, "timeout_ms": 60_000
            }));
            let _ = peer.next().await;
            // Nothing more arrives; the user pressed escape.
            child.cancel();
        };

        let (outcome, ()) = tokio::join!(client, gateway);
        assert!(matches!(outcome.unwrap(), Outcome::Cancelled));
    }

    /// A gateway that grows a new message must not break a login.
    #[tokio::test]
    async fn an_unknown_message_is_ignored() {
        let keys = shared_keys();
        let http = Http::with_base(
            Arc::new(ClientProps::new("en-US", 1)),
            "http://unused.invalid",
        )
        .unwrap();
        let (mut wire, mut peer) = wire();
        let cancel = CancellationToken::new();

        let client = async { drive(&mut wire, keys, &http, &cancel, &mut |_| {}).await };
        let gateway = async {
            peer.send(serde_json::json!({
                "op": "hello", "heartbeat_interval": 60_000, "timeout_ms": 60_000
            }));
            let _ = peer.next().await;
            peer.send(serde_json::json!({"op": "something_new", "data": 1}));
            peer.send(serde_json::json!({"not even an op": true}));
            let _ = peer.to_client.send(Ok(Message::Text("{".into())));
            peer.send(serde_json::json!({"op": "cancel"}));
        };

        let (outcome, ()) = tokio::join!(client, gateway);
        assert!(
            matches!(outcome.unwrap(), Outcome::Cancelled),
            "an unknown message should have been skipped rather than fatal"
        );
    }
}
