//! Staying inside Discord's rate limits.
//!
//! The rules are not complicated but they are easy to get subtly wrong, and
//! getting them wrong on a user account is not a 429 — it is an account flag.
//! So this waits *before* sending rather than reacting to a 429:
//!
//! - Every response carries `X-RateLimit-Bucket`, an opaque hash naming the
//!   allowance the request was counted against. Several route templates share
//!   one hash and Discord does not document which, so the mapping from route to
//!   hash is learned from the first response rather than assumed.
//! - `X-RateLimit-Remaining` and `X-RateLimit-Reset-After` say how many
//!   requests are left and how long until the allowance refills. When
//!   remaining reaches zero the next request sleeps until the reset instead of
//!   spending a 429.
//! - A 429 with `"global": true` stops *everything*, not just that bucket.
//! - `Retry-After` and the body's `retry_after` disagree in units — seconds
//!   with a fractional part in the body, whole seconds in the header — and the
//!   body is the precise one.
//!
//! A request is retried at most three times and only for a 429 or a 5xx. A 401
//! is never retried: the token is wrong, and hammering an endpoint with a
//! rejected token is the single most suspicious thing a client can do.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// How long a bucket's learned state is trusted after its reset passes.
const SLACK: Duration = Duration::from_millis(50);

/// What to do when a request has to be repeated.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Attempts in total, not retries: 3 means one try and two more.
    pub attempts: u32,
    /// The first 5xx backoff. Doubles each time — 1 s, 2 s, 4 s.
    pub server_error_backoff: Duration,
    /// A ceiling on how long a 429 may park a request. Beyond this the request
    /// fails and the user is told, rather than the UI appearing to hang.
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            server_error_backoff: Duration::from_secs(1),
            max_wait: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
}

#[derive(Default)]
struct Inner {
    /// Route bucket key → Discord's opaque bucket hash. Learned, never guessed.
    hashes: HashMap<String, String>,
    /// Discord's bucket hash → what is left of that allowance.
    buckets: HashMap<String, Bucket>,
    /// Set by a global 429. Nothing goes out until it passes.
    global_until: Option<Instant>,
}

/// Shared by every request the client makes.
#[derive(Default)]
pub struct RateLimiter {
    inner: Mutex<Inner>,
}

/// What the response headers said, parsed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    pub bucket: Option<String>,
    pub remaining: Option<u32>,
    pub reset_after: Option<Duration>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wait until a request on `key` may be sent.
    ///
    /// Returns how long it waited, which is used by the tests and by a debug
    /// log: a limiter that is silently sleeping for thirty seconds is
    /// indistinguishable from a hang.
    pub async fn acquire(&self, key: &str) -> Duration {
        let start = Instant::now();
        while let Some(wait) = self.next_wait(key).await {
            tracing::debug!("rate limiter holding {key} for {wait:?}");
            tokio::time::sleep(wait).await;
        }
        start.elapsed()
    }

    /// How long `key` must wait right now, or `None` if it may go.
    async fn next_wait(&self, key: &str) -> Option<Duration> {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();

        // Global first. It outranks every bucket, and a bucket that still has
        // an allowance must not slip past it.
        if let Some(until) = inner.global_until {
            if until > now {
                return Some(until - now);
            }
            inner.global_until = None;
        }

        let hash = inner.hashes.get(key).cloned()?;
        let bucket = *inner.buckets.get(&hash)?;
        if bucket.reset_at <= now {
            // The allowance has refilled. Forget what was known rather than
            // carrying a stale `remaining` across the reset.
            inner.buckets.remove(&hash);
            return None;
        }
        (bucket.remaining == 0).then(|| bucket.reset_at - now + SLACK)
    }

    /// Spend one request from `key`'s allowance before it is sent.
    ///
    /// Decrementing on the way out rather than on the way back is what makes
    /// concurrent requests safe: two tasks that both read `remaining == 1` and
    /// both send would otherwise spend the same one.
    pub async fn consume(&self, key: &str) {
        let mut inner = self.inner.lock().await;
        let Some(hash) = inner.hashes.get(key).cloned() else {
            return;
        };
        if let Some(bucket) = inner.buckets.get_mut(&hash) {
            bucket.remaining = bucket.remaining.saturating_sub(1);
        }
    }

    /// Record what a response said about the allowance it came from.
    pub async fn observe(&self, key: &str, observed: &Observed) {
        let Some(hash) = observed.bucket.clone() else {
            return;
        };
        let mut inner = self.inner.lock().await;
        inner.hashes.insert(key.to_string(), hash.clone());
        if let (Some(remaining), Some(reset_after)) = (observed.remaining, observed.reset_after) {
            inner.buckets.insert(
                hash,
                Bucket {
                    remaining,
                    reset_at: Instant::now() + reset_after,
                },
            );
        }
    }

    /// A 429 on one bucket. Park that bucket until it resets.
    pub async fn limited(&self, key: &str, retry_after: Duration) {
        let mut inner = self.inner.lock().await;
        let hash = inner
            .hashes
            .get(key)
            .cloned()
            // A 429 before the bucket hash is known still has to park
            // something, so the route key stands in for it.
            .unwrap_or_else(|| key.to_string());
        inner.hashes.insert(key.to_string(), hash.clone());
        inner.buckets.insert(
            hash,
            Bucket {
                remaining: 0,
                reset_at: Instant::now() + retry_after,
            },
        );
    }

    /// A global 429. Nothing else goes out until it passes.
    pub async fn limited_globally(&self, retry_after: Duration) {
        let mut inner = self.inner.lock().await;
        let until = Instant::now() + retry_after;
        inner.global_until = Some(match inner.global_until {
            Some(existing) if existing > until => existing,
            _ => until,
        });
        tracing::warn!("globally rate limited for {retry_after:?}");
    }

    /// Whether a global limit is in force, for the status line.
    pub async fn globally_limited(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.global_until.is_some_and(|t| t > Instant::now())
    }
}

/// Read the rate-limit headers off a response.
pub fn observe_headers(headers: &reqwest::header::HeaderMap) -> Observed {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    Observed {
        bucket: text("x-ratelimit-bucket").map(str::to_owned),
        remaining: text("x-ratelimit-remaining").and_then(|v| v.parse().ok()),
        reset_after: text("x-ratelimit-reset-after")
            .and_then(|v| v.parse::<f64>().ok())
            .and_then(seconds),
    }
}

/// Whether a 429 was global, and how long to wait.
///
/// The body is preferred because it carries fractional seconds; the header is
/// whole seconds and rounds a 0.4-second wait up to 1 or down to 0 depending on
/// which gateway answered.
pub fn retry_after(
    headers: &reqwest::header::HeaderMap,
    body: &serde_json::Value,
) -> (bool, Duration) {
    let global = body
        .get("global")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or_else(|| {
            headers
                .get("x-ratelimit-global")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        });

    let from_body = body
        .get("retry_after")
        .and_then(serde_json::Value::as_f64)
        .and_then(seconds);
    let from_header = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .and_then(seconds);

    // A 429 with no wait at all is a malformed 429; a second is a guess, and it
    // is the guess that cannot make things worse.
    (
        global,
        from_body.or(from_header).unwrap_or(Duration::from_secs(1)),
    )
}

/// Seconds off the wire, refused when they are not a duration.
///
/// A negative or non-finite value is Discord's problem and a panic would be
/// ours: `Duration::from_secs_f64` panics on both.
fn seconds(v: f64) -> Option<Duration> {
    (v.is_finite() && (0.0..=86_400.0).contains(&v)).then(|| Duration::from_secs_f64(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        let mut map = reqwest::header::HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn the_headers_are_read_as_a_bucket_and_a_reset() {
        let observed = observe_headers(&headers(&[
            ("x-ratelimit-bucket", "abcdef"),
            ("x-ratelimit-remaining", "4"),
            ("x-ratelimit-reset-after", "0.483"),
        ]));
        assert_eq!(observed.bucket.as_deref(), Some("abcdef"));
        assert_eq!(observed.remaining, Some(4));
        assert_eq!(observed.reset_after, Some(Duration::from_secs_f64(0.483)));
    }

    #[test]
    fn a_response_with_no_rate_limit_headers_says_nothing() {
        assert_eq!(observe_headers(&headers(&[])), Observed::default());
    }

    #[test]
    fn the_body_wins_over_the_header_because_it_has_the_fraction() {
        let (global, wait) = retry_after(
            &headers(&[("retry-after", "1")]),
            &serde_json::json!({"retry_after": 0.472, "global": false}),
        );
        assert!(!global);
        assert_eq!(wait, Duration::from_secs_f64(0.472));
    }

    #[test]
    fn a_global_limit_is_recognised_from_either_place() {
        let (from_body, _) = retry_after(&headers(&[]), &serde_json::json!({"global": true}));
        let (from_header, _) = retry_after(
            &headers(&[("x-ratelimit-global", "true")]),
            &serde_json::json!({}),
        );
        assert!(from_body);
        assert!(from_header);
    }

    /// Every one of these has appeared in the wild or is one refresh away from
    /// it, and `Duration::from_secs_f64` panics on three of them.
    #[test]
    fn a_nonsense_retry_after_is_a_second_rather_than_a_panic() {
        for body in [
            serde_json::json!({"retry_after": -1.0}),
            serde_json::json!({"retry_after": f64::INFINITY}),
            serde_json::json!({"retry_after": 1e30}),
            serde_json::json!({"retry_after": "soon"}),
            serde_json::json!({}),
        ] {
            let (_, wait) = retry_after(&headers(&[]), &body);
            assert_eq!(wait, Duration::from_secs(1), "{body}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_exhausted_bucket_sleeps_until_it_refills() {
        let limiter = RateLimiter::new();
        limiter
            .observe(
                "GET /x",
                &Observed {
                    bucket: Some("b".into()),
                    remaining: Some(0),
                    reset_after: Some(Duration::from_secs(2)),
                },
            )
            .await;

        let waited = limiter.acquire("GET /x").await;
        assert!(
            waited >= Duration::from_secs(2),
            "waited only {waited:?} for a two-second reset"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn two_routes_that_share_a_hash_share_the_allowance() {
        let limiter = RateLimiter::new();
        let exhausted = Observed {
            bucket: Some("shared".into()),
            remaining: Some(0),
            reset_after: Some(Duration::from_secs(3)),
        };
        limiter.observe("GET /a", &exhausted).await;
        // The second route has never been sent, but Discord told us it counts
        // against the same hash.
        limiter.observe("GET /b", &exhausted).await;

        let waited = limiter.acquire("GET /b").await;
        assert!(waited >= Duration::from_secs(3), "waited {waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_global_limit_holds_a_bucket_that_still_has_room() {
        let limiter = RateLimiter::new();
        limiter
            .observe(
                "GET /x",
                &Observed {
                    bucket: Some("b".into()),
                    remaining: Some(10),
                    reset_after: Some(Duration::from_secs(60)),
                },
            )
            .await;
        limiter.limited_globally(Duration::from_secs(5)).await;
        assert!(limiter.globally_limited().await);

        let waited = limiter.acquire("GET /x").await;
        assert!(waited >= Duration::from_secs(5), "waited {waited:?}");
        assert!(!limiter.globally_limited().await);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_route_does_not_wait() {
        let limiter = RateLimiter::new();
        assert_eq!(limiter.acquire("GET /never-seen").await, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn consuming_the_last_of_an_allowance_makes_the_next_call_wait() {
        let limiter = RateLimiter::new();
        limiter
            .observe(
                "GET /x",
                &Observed {
                    bucket: Some("b".into()),
                    remaining: Some(1),
                    reset_after: Some(Duration::from_secs(4)),
                },
            )
            .await;

        assert_eq!(limiter.acquire("GET /x").await, Duration::ZERO);
        limiter.consume("GET /x").await;
        let waited = limiter.acquire("GET /x").await;
        assert!(waited >= Duration::from_secs(4), "waited {waited:?}");
    }
}
