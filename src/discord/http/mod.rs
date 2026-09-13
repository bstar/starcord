//! The REST client.
//!
//! Three things here are load-bearing and none of them is the request builder.
//!
//! **The crypto provider is chosen explicitly.** rustls 0.23 can be backed by
//! `ring` or by `aws-lc-rs`, and which one you get depends on which crate in
//! the graph enabled which feature. This client installs `ring` as the process
//! default before it builds anything, `reqwest` is taken with
//! `rustls-no-provider` so it picks that default up rather than dragging in its
//! own, and a test fails the build if `aws-lc` appears in the dependency graph
//! at all. The reason is not a preference between two good libraries: it is
//! that a TLS backend that arrives by accident is one nobody reviewed.
//!
//! **`download()` never carries the token.** Attachments, avatars and emoji
//! live on `cdn.discordapp.com` and on media proxies, and an `Authorization`
//! header on a request to a host Discord does not control is how a session gets
//! handed to somebody else. It is a separate client with no token in reach, it
//! refuses anything that is not `https`, and it stops reading at a byte cap
//! rather than believing a `Content-Length`.
//!
//! **Every request waits its turn.** See `limits.rs`; the summary is that the
//! limiter sleeps before sending rather than reacting to a 429, because on a
//! user account a 429 is not a retry, it is a note in somebody's ledger.

pub mod api;
pub mod limits;
pub mod route;

use std::sync::Arc;
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::discord::auth::Token;
use crate::discord::props::ClientProps;
use limits::{observe_headers, retry_after, RateLimiter, RetryPolicy};
use route::{Route, API_VERSION};

/// Where the API lives.
pub const API_BASE: &str = "https://discord.com/api";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a request did not produce what was asked for.
///
/// `Unauthorized` is separate from every other status because it is the only
/// one that must stop the client rather than delay it: a rejected token does
/// not become accepted by being sent again, and a client that keeps trying is
/// the most conspicuous thing on Discord's side of the connection.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("the session token was rejected")]
    Unauthorized,
    #[error("rate limited; gave up after {attempts} attempts")]
    RateLimited { attempts: u32 },
    #[error("discord returned {status}: {message}")]
    Status {
        status: u16,
        code: u64,
        message: String,
    },
    #[error("network error: {0}")]
    Network(#[source] reqwest::Error),
    #[error("could not decode the response: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("{url} is not an https url this client will fetch")]
    NotHttps { url: String },
    #[error("the response was larger than the {limit} byte cap")]
    TooLarge { limit: u64 },
    #[error("the client could not be built: {0}")]
    Build(String),
}

impl HttpError {
    /// Whether this ends the session rather than one request.
    pub fn is_fatal(&self) -> bool {
        matches!(self, HttpError::Unauthorized)
    }

    /// Whether the resource is gone, which for an attachment means the signed
    /// URL expired and is worth refreshing once.
    pub fn is_gone(&self) -> bool {
        matches!(
            self,
            HttpError::Status {
                status: 403 | 404,
                ..
            }
        )
    }
}

/// Install `ring` as the process-wide rustls provider.
///
/// Idempotent, and deliberately ignores the error: `install_default` fails only
/// when a provider is already installed, which in a test binary means another
/// test got there first. The test below is what makes sure the installed one is
/// the right one.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The TLS configuration both clients use.
///
/// Built here rather than left to `reqwest` for two reasons. The roots are
/// Mozilla's compiled-in list, the same ones the gateway socket uses, so a
/// container with no `ca-certificates` package still connects and the two
/// transports cannot disagree about what they trust. And `reqwest` does not set
/// ALPN on a configuration it was handed, so h2 has to be offered here or every
/// request negotiates HTTP/1.1.
fn tls_config() -> Result<rustls::ClientConfig, HttpError> {
    install_crypto_provider();
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| HttpError::Build(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

pub struct Http {
    client: reqwest::Client,
    /// A second client for media, with a longer timeout and — the point — no
    /// path by which a token could reach it.
    media: reqwest::Client,
    props: ArcSwap<ClientProps>,
    token: ArcSwapOption<Token>,
    limits: RateLimiter,
    base: String,
    retry: RetryPolicy,
    /// Whether this client may fetch media over plain http.
    ///
    /// False for every client the program builds. It is true only when the base
    /// was deliberately pointed somewhere that is not https, which nothing but a
    /// test against a local mock server ever does — and a local mock server
    /// cannot serve https. The field exists so that the rule has one place
    /// rather than an argument threaded through every call site.
    insecure: bool,
}

impl Http {
    pub fn new(props: Arc<ClientProps>) -> Result<Self, HttpError> {
        Self::with_base(props, format!("{API_BASE}/{API_VERSION}"))
    }

    /// The same client pointed somewhere else, which is how the rate limiter
    /// is tested against a real server.
    pub fn with_base(props: Arc<ClientProps>, base: impl Into<String>) -> Result<Self, HttpError> {
        let tls = tls_config()?;
        let base = base.into();
        // Discord is https and so is its CDN, so a redirect to http cannot
        // silently downgrade a request that carries a token. The only thing
        // that ever points this somewhere else is a test against a local
        // server, which cannot serve https; `download` refuses a non-https URL
        // regardless of what the client would allow.
        let https_only = base.starts_with("https://");
        let build = |timeout: Duration| {
            reqwest::Client::builder()
                .user_agent(props.user_agent())
                .https_only(https_only)
                .redirect(reqwest::redirect::Policy::limited(5))
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(timeout)
                .use_preconfigured_tls(tls.clone())
                .build()
                .map_err(|e| HttpError::Build(e.to_string()))
        };

        Ok(Self {
            client: build(REQUEST_TIMEOUT)?,
            media: build(DOWNLOAD_TIMEOUT)?,
            props: ArcSwap::from(props),
            token: ArcSwapOption::empty(),
            limits: RateLimiter::new(),
            base,
            retry: RetryPolicy::default(),
            insecure: !https_only,
        })
    }

    /// Shorter waits, for tests that would otherwise sleep for seconds.
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn set_token(&self, token: Option<Token>) {
        self.token.store(token.map(Arc::new));
    }

    pub fn has_token(&self) -> bool {
        self.token.load().is_some()
    }

    pub fn set_props(&self, props: Arc<ClientProps>) {
        self.props.store(props);
    }

    pub fn props(&self) -> Arc<ClientProps> {
        self.props.load_full()
    }

    pub fn limits(&self) -> &RateLimiter {
        &self.limits
    }

    /// Send a request and decode its body.
    pub async fn request<T: DeserializeOwned>(
        &self,
        route: Route,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<T, HttpError> {
        let bytes = self.request_bytes(route, body).await?;
        if bytes.is_empty() {
            // 204 No Content. `null` is the only JSON an empty body can mean,
            // and it deserialises into `()` and into every `Option`.
            return serde_json::from_str("null").map_err(HttpError::Decode);
        }
        serde_json::from_slice(&bytes).map_err(HttpError::Decode)
    }

    async fn request_bytes(
        &self,
        route: Route,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<Vec<u8>, HttpError> {
        let key = route.bucket().into_owned();
        let url = format!("{}{}", self.base, route.path());
        let mut backoff = self.retry.server_error_backoff;

        for attempt in 1..=self.retry.attempts {
            self.limits.acquire(&key).await;
            self.limits.consume(&key).await;

            let props = self.props.load();
            let mut req = self
                .client
                .request(route.method(), &url)
                .header("X-Super-Properties", props.super_properties_header())
                .header("X-Discord-Locale", props.locale())
                .header(reqwest::header::ACCEPT, "*/*");
            if route.authenticated() {
                if let Some(token) = self.token.load_full() {
                    // No `Bearer` and no `Bot`: a user token is sent bare, which
                    // is the one place Discord's API differs from every other.
                    req = req.header(reqwest::header::AUTHORIZATION, token.expose());
                }
            }
            if let Some(body) = body {
                req = req.json(body);
            }

            let response = match req.send().await {
                Ok(response) => response,
                Err(e) if attempt < self.retry.attempts && is_transient(&e) => {
                    tracing::debug!("{key} failed transiently ({e}); retrying in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                    continue;
                }
                Err(e) => return Err(HttpError::Network(e)),
            };

            let status = response.status();
            let headers = response.headers().clone();
            self.limits.observe(&key, &observe_headers(&headers)).await;

            if status == reqwest::StatusCode::UNAUTHORIZED {
                // Never retried, at any attempt count.
                return Err(HttpError::Unauthorized);
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                let (global, wait) = retry_after(&headers, &body);
                if global {
                    self.limits.limited_globally(wait).await;
                } else {
                    self.limits.limited(&key, wait).await;
                }
                if attempt == self.retry.attempts || wait > self.retry.max_wait {
                    return Err(HttpError::RateLimited { attempts: attempt });
                }
                continue;
            }

            if status.is_server_error() && attempt < self.retry.attempts {
                tracing::debug!("{key} returned {status}; retrying in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }

            if !status.is_success() {
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                return Err(HttpError::Status {
                    status: status.as_u16(),
                    code: body
                        .get("code")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    message: body
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_else(|| status.canonical_reason().unwrap_or("error"))
                        .to_string(),
                });
            }

            return response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(HttpError::Network);
        }

        Err(HttpError::RateLimited {
            attempts: self.retry.attempts,
        })
    }

    /// Fetch bytes from a CDN, with no token and a hard size cap.
    ///
    /// The cap is enforced while reading rather than from `Content-Length`,
    /// because a length header is a claim and the bytes are the fact.
    pub async fn download(&self, url: &str, limit: u64) -> Result<Vec<u8>, HttpError> {
        self.download_typed(url, limit).await.map(|d| d.bytes)
    }

    /// The same, keeping what the server said the bytes were.
    ///
    /// The media cache names a file by its type, and a `Content-Type` from the
    /// server is a better answer than an extension on a URL that may not have
    /// one at all.
    pub async fn download_typed(&self, url: &str, limit: u64) -> Result<Download, HttpError> {
        if !url.starts_with("https://") && !self.insecure {
            return Err(HttpError::NotHttps {
                url: url.to_string(),
            });
        }
        self.get_capped(url, limit).await
    }

    /// The reading half of `download`, separated so the cap can be exercised
    /// against a local server that cannot serve https.
    async fn get_capped(&self, url: &str, limit: u64) -> Result<Download, HttpError> {
        let response = self
            .media
            .get(url)
            .send()
            .await
            .map_err(HttpError::Network)?;

        let status = response.status();
        if !status.is_success() {
            return Err(HttpError::Status {
                status: status.as_u16(),
                code: 0,
                message: status.canonical_reason().unwrap_or("error").to_string(),
            });
        }

        if response.content_length().is_some_and(|len| len > limit) {
            return Err(HttpError::TooLarge { limit });
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        let mut out = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(HttpError::Network)? {
            if out.len() as u64 + chunk.len() as u64 > limit {
                return Err(HttpError::TooLarge { limit });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(Download {
            bytes: out,
            content_type,
        })
    }
}

/// Bytes from a CDN, and what the server called them.
#[derive(Debug, Clone)]
pub struct Download {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

/// Whether a transport failure is worth one more try.
///
/// A timeout or a refused connection is weather. A body that failed to decode
/// or a URL that will not parse will fail identically next time.
fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::{ChannelId, MessageId};

    fn props() -> Arc<ClientProps> {
        Arc::new(ClientProps::new("en-US", 1))
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            attempts: 3,
            server_error_backoff: Duration::from_millis(1),
            max_wait: Duration::from_secs(5),
        }
    }

    /// Every crate the resolver actually selected.
    ///
    /// `cargo metadata` is the resolver's own answer and is used when it can be
    /// run. Its *package names* are what matters, not the raw JSON: the
    /// document also carries every optional dependency and feature name every
    /// manifest declares, so a substring search over it matches `aws-lc-rs` in
    /// `reqwest`'s feature table whether or not anything enabled it.
    ///
    /// In a sandboxed build — `nix flake check`, most usefully — `Cargo.lock`
    /// beside the manifest says the same thing without a subprocess, and it is
    /// the file a reviewer would read anyway.
    fn resolved_crates() -> Vec<String> {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let metadata = std::process::Command::new(cargo)
            .args(["metadata", "--format-version", "1"])
            .current_dir(manifest_dir)
            .output();

        if let Ok(out) = metadata {
            if out.status.success() {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(packages) = json["packages"].as_array() {
                        return packages
                            .iter()
                            .filter_map(|p| p["name"].as_str().map(str::to_owned))
                            .collect();
                    }
                }
            }
        }

        let lock = std::fs::read_to_string(std::path::Path::new(manifest_dir).join("Cargo.lock"))
            .expect("neither cargo metadata nor Cargo.lock could be read");
        lock.lines()
            .filter_map(|line| line.strip_prefix("name = \""))
            .filter_map(|rest| rest.strip_suffix('"'))
            .map(str::to_owned)
            .collect()
    }

    /// aws-lc-rs is a perfectly good TLS backend. The objection is to getting
    /// one because `reqwest`'s `rustls` feature happens to imply it, in a client
    /// whose whole job is carrying somebody's session token.
    #[test]
    fn the_tls_backend_is_ring_and_aws_lc_is_not_in_the_graph() {
        let crates = resolved_crates();
        assert!(crates.len() > 50, "only {} crates were found", crates.len());

        let aws: Vec<&String> = crates.iter().filter(|c| c.contains("aws-lc")).collect();
        assert!(
            aws.is_empty(),
            "{aws:?} reached the dependency graph; check reqwest's features \
             (it must be rustls-no-provider, not rustls)"
        );
        assert!(
            crates.iter().any(|c| c == "ring"),
            "ring is not in the graph, so nothing would provide crypto"
        );
    }

    #[test]
    fn installing_the_provider_twice_is_not_a_panic() {
        install_crypto_provider();
        install_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    /// The trust anchors are compiled in, so a machine with no system CA store
    /// still connects — which is also what makes the test suite run in a build
    /// sandbox.
    #[test]
    fn the_roots_are_compiled_in_and_h2_is_offered() {
        let config = tls_config().expect("a TLS configuration");
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()],
            "reqwest does not set ALPN on a configuration it was handed"
        );
        assert!(
            webpki_roots::TLS_SERVER_ROOTS.len() > 50,
            "only {} trust anchors were compiled in",
            webpki_roots::TLS_SERVER_ROOTS.len()
        );
    }

    #[tokio::test]
    async fn a_download_refuses_anything_that_is_not_https() {
        let http = Http::new(props()).unwrap();
        for url in [
            "http://cdn.discordapp.com/attachments/1/2/a.png",
            "file:///etc/passwd",
            "ftp://example.invalid/x",
        ] {
            let err = http.download(url, 1024).await.unwrap_err();
            assert!(matches!(err, HttpError::NotHttps { .. }), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn a_401_is_never_retried() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"message": "401: Unauthorized", "code": 0})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri())
            .unwrap()
            .with_retry_policy(fast());
        http.set_token(Some(Token::new("fake-token-for-a-mock-server").unwrap()));

        let err = http
            .request::<serde_json::Value>(Route::Me, None::<&()>)
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::Unauthorized));
        assert!(err.is_fatal());
        // `expect(1)` on the mock is checked when the server drops: a second
        // attempt fails the test rather than merely being slow.
    }

    #[tokio::test]
    async fn a_429_is_waited_out_and_then_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("x-ratelimit-bucket", "me")
                    .set_body_json(serde_json::json!({"retry_after": 0.05, "global": false})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-ratelimit-bucket", "me")
                    .insert_header("x-ratelimit-remaining", "4")
                    .insert_header("x-ratelimit-reset-after", "1.0")
                    .set_body_json(serde_json::json!({"id": "1", "username": "me"})),
            )
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri())
            .unwrap()
            .with_retry_policy(fast());
        http.set_token(Some(Token::new("fake-token-for-a-mock-server").unwrap()));

        let user: crate::discord::model::User = http.request(Route::Me, None::<&()>).await.unwrap();
        assert_eq!(user.username, "me");
    }

    #[tokio::test]
    async fn a_global_429_parks_every_other_route() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_json(serde_json::json!({"retry_after": 30.0, "global": true})),
            )
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri())
            .unwrap()
            .with_retry_policy(RetryPolicy {
                attempts: 1,
                server_error_backoff: Duration::from_millis(1),
                max_wait: Duration::from_secs(5),
            });

        let err = http
            .request::<serde_json::Value>(Route::Me, None::<&()>)
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::RateLimited { .. }), "{err}");
        assert!(
            http.limits().globally_limited().await,
            "a global 429 must hold the whole client, not one bucket"
        );
    }

    #[tokio::test]
    async fn a_5xx_is_retried_and_a_4xx_is_not() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "9"})))
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri())
            .unwrap()
            .with_retry_policy(fast());
        let user: crate::discord::model::User = http.request(Route::Me, None::<&()>).await.unwrap();
        assert_eq!(user.id.get(), 9);

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"message": "Missing Access", "code": 50001})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let http = Http::with_base(props(), server.uri())
            .unwrap()
            .with_retry_policy(fast());
        let err = http
            .request::<serde_json::Value>(
                Route::ChannelMessages {
                    channel: ChannelId(1),
                    history: route::History::Latest,
                    limit: route::PAGE,
                },
                None::<&()>,
            )
            .await
            .unwrap_err();
        match err {
            HttpError::Status { status, code, .. } => {
                assert_eq!((status, code), (403, 50001));
            }
            other => panic!("expected a status error, got {other}"),
        }
    }

    #[tokio::test]
    async fn the_super_properties_and_locale_go_out_on_every_request() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let props = props();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "1"})))
            .mount(&server)
            .await;

        let http = Http::with_base(Arc::clone(&props), server.uri())
            .unwrap()
            .with_retry_policy(fast());
        http.set_token(Some(Token::new("a-token-long-enough-to-pass").unwrap()));
        let _: crate::discord::model::User = http.request(Route::Me, None::<&()>).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let sent = requests.last().expect("the request was recorded");
        let header = |name: &str| {
            sent.headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };

        assert_eq!(
            header("x-super-properties"),
            props.super_properties_header()
        );
        assert_eq!(header("x-discord-locale"), props.locale());
        assert_eq!(header("user-agent"), props.user_agent());
        assert_eq!(
            header("authorization"),
            "a-token-long-enough-to-pass",
            "a user token is sent bare, with no Bearer or Bot prefix"
        );
    }

    #[tokio::test]
    async fn a_download_stops_at_the_cap() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 4096]))
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri()).unwrap();
        let url = format!("{}/big", server.uri());
        assert!(
            matches!(
                http.get_capped(&url, 1024).await,
                Err(HttpError::TooLarge { .. })
            ),
            "4 KiB came back under a 1 KiB cap"
        );
        assert_eq!(http.get_capped(&url, 8192).await.unwrap().bytes.len(), 4096);
    }

    /// A download carries a User-Agent and nothing else. An Authorization
    /// header on a request to a host Discord does not control is how a session
    /// gets handed to somebody else.
    #[tokio::test]
    async fn a_download_never_carries_the_token() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8; 8]))
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri()).unwrap();
        http.set_token(Some(Token::new("super-secret-token").unwrap()));
        let url = format!("{}/avatar.png", server.uri());
        http.get_capped(&url, 1024).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let sent: &Request = requests.last().expect("the download was recorded");
        assert!(
            sent.headers.get("authorization").is_none(),
            "a media request carried an Authorization header"
        );
        let all = format!("{:?}", sent.headers);
        assert!(
            !all.contains("super-secret-token"),
            "the token appeared in a media request's headers"
        );
    }

    /// Paging through history: the first request asks for the newest page, the
    /// second asks for what is before the oldest of it, and both come out of
    /// one allowance.
    #[tokio::test]
    async fn history_pages_backwards_through_a_channel() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn page(from: u64, count: u64) -> serde_json::Value {
            // Newest first, as Discord returns it.
            serde_json::Value::Array(
                (0..count)
                    .map(|n| {
                        serde_json::json!({
                            "id": (from - n).to_string(),
                            "channel_id": "7",
                            "content": format!("m{}", from - n),
                            "author": {"id": "2", "username": "alex"}
                        })
                    })
                    .collect(),
            )
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/7/messages"))
            .and(query_param("limit", "50"))
            .and(query_param("before", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(99, 3)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/channels/7/messages"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(150, 3)))
            .expect(1)
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri())
            .unwrap()
            .with_retry_policy(fast());

        let newest = api::messages(&http, ChannelId(7), route::History::Latest)
            .await
            .unwrap();
        assert_eq!(newest.len(), 3);
        assert_eq!(newest[0].id.get(), 150, "Discord returns newest first");

        let older = api::messages(&http, ChannelId(7), route::History::Before(MessageId(100)))
            .await
            .unwrap();
        assert_eq!(older.len(), 3);
        assert_eq!(older[0].id.get(), 99);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.query().unwrap().contains("limit=50"));
        assert!(requests[1].url.query().unwrap().contains("before=100"));
    }

    /// The send path, which is the one place a nonce has to survive
    /// serialisation exactly: without it the gateway echo cannot find the row
    /// it belongs to and the sender sees their message twice.
    #[tokio::test]
    async fn a_sent_message_carries_its_nonce_and_its_reply() {
        use wiremock::matchers::{body_json_schema, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/channels/7/messages"))
            .and(body_json_schema::<serde_json::Value>)
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "500",
                "channel_id": "7",
                "content": "hello",
                "nonce": "81237712343",
                "author": {"id": "1", "username": "sam"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri())
            .unwrap()
            .with_retry_policy(fast());

        let sent = api::create_message(
            &http,
            ChannelId(7),
            &api::CreateMessage {
                content: "hello",
                nonce: "81237712343".into(),
                message_reference: Some(api::ReplyTo {
                    message_id: MessageId(499),
                    channel_id: ChannelId(7),
                    fail_if_not_exists: false,
                }),
                allowed_mentions: api::AllowedMentions::new(true),
                tts: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(sent.id, MessageId(500));
        assert_eq!(sent.nonce.as_deref(), Some("81237712343"));

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["nonce"], "81237712343");
        assert_eq!(body["content"], "hello");
        assert_eq!(body["message_reference"]["message_id"], "499");
        assert_eq!(body["allowed_mentions"]["replied_user"], true);
    }

    /// Too long is refused here rather than by Discord. A refused request is
    /// still a request, and on a user account that is a line in somebody's
    /// ledger.
    #[tokio::test]
    async fn an_overlong_message_never_reaches_the_network() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let http = Http::with_base(props(), server.uri()).unwrap();
        let long = "x".repeat(api::MAX_CONTENT + 1);
        let err = api::create_message(
            &http,
            ChannelId(7),
            &api::CreateMessage {
                content: &long,
                nonce: "1".into(),
                message_reference: None,
                allowed_mentions: api::AllowedMentions::new(false),
                tts: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, HttpError::Status { status: 400, .. }));
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
