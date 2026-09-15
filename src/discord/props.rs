//! What this client claims to be.
//!
//! Every request carries `X-Super-Properties`, a base64 JSON blob describing
//! the browser the session is supposedly running in, and IDENTIFY carries the
//! same object again as `properties`. Discord uses it for feature gating and
//! for its own telemetry; a session whose header, `User-Agent` and IDENTIFY
//! disagree with each other is a session that does not look like any real
//! client, which is the one thing worth avoiding here.
//!
//! So there is one `ClientProps` per process and it produces all three. The
//! test at the bottom asserts that `browser_user_agent` and the `User-Agent`
//! header are the same string, because they drifted apart in every prior art
//! this design was read against.
//!
//! `client_build_number` is the part that goes stale. The web client ships it
//! inside its own JavaScript, and Discord increments it several times a week;
//! a number a year out of date is the single most obvious thing about a client
//! that is not a browser. It is discovered at startup, cached for a day, and
//! falls back to a pinned constant when discovery fails.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::paths::Paths;

/// The build number to use when discovery and the cache both fail.
///
/// Read from `https://discord.com/app` on 2026-09-13: the page's last
/// `/assets/*.js` entry was `web.f34f869e4c648af0.js`, which carries
/// `build_number:"611316"`. It is a floor, not a target — live discovery
/// replaces it within seconds of startup — but a floor that is years old
/// stands out, so it is worth refreshing whenever this file is touched.
pub const PINNED_BUILD_NUMBER: u64 = 611_316;

/// The Chrome release the `User-Agent` claims. Chrome freezes everything below
/// the major version at zero in its reduced user-agent, so this is the whole
/// truth of what a real browser sends.
const BROWSER_VERSION: &str = "152.0.0.0";

/// Where the web client lives, and where the build number is discovered from.
pub const APP_URL: &str = "https://discord.com/app";

/// A build number older than this is refreshed at the next startup.
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long discovery is allowed to delay a login.
pub const DISCOVERY_BUDGET: Duration = Duration::from_secs(5);

/// The object Discord expects, in the order the web client writes it.
///
/// Field order is not required by anything and is preserved anyway: the header
/// is a base64 blob that is easy to diff against a real one, and a diff that is
/// only about ordering wastes the time of whoever is comparing them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuperProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
    pub system_locale: String,
    pub browser_user_agent: String,
    pub browser_version: String,
    pub os_version: String,
    pub referrer: String,
    pub referring_domain: String,
    pub referrer_current: String,
    pub referring_domain_current: String,
    pub release_channel: String,
    pub client_build_number: u64,
    pub client_event_source: Option<String>,
}

/// One per process, shared by the HTTP client, the gateway and remote auth.
#[derive(Debug, Clone)]
pub struct ClientProps {
    props: SuperProperties,
    /// Computed once. It is sent on every single request, and base64 of a
    /// serialisation per request is work for nothing.
    header: String,
}

impl ClientProps {
    pub fn new(locale: impl Into<String>, build_number: u64) -> Self {
        let os = host_os().to_string();
        let os_version = os_version_for(&os).to_string();
        let browser_user_agent = user_agent_for(&os);
        let props = SuperProperties {
            os,
            browser: "Chrome".into(),
            device: String::new(),
            system_locale: locale.into(),
            browser_user_agent,
            browser_version: BROWSER_VERSION.into(),
            os_version,
            referrer: String::new(),
            referring_domain: String::new(),
            referrer_current: String::new(),
            referring_domain_current: String::new(),
            release_channel: "stable".into(),
            client_build_number: build_number,
            client_event_source: None,
        };
        let header = encode(&props);
        Self { props, header }
    }

    /// The same properties with a freshly discovered build number.
    pub fn with_build_number(&self, build_number: u64) -> Self {
        let mut props = self.props.clone();
        props.client_build_number = build_number;
        let header = encode(&props);
        Self { props, header }
    }

    /// The `X-Super-Properties` value.
    pub fn super_properties_header(&self) -> &str {
        &self.header
    }

    /// The `properties` field of IDENTIFY, which is the same object.
    pub fn identify_properties(&self) -> &SuperProperties {
        &self.props
    }

    pub fn user_agent(&self) -> &str {
        &self.props.browser_user_agent
    }

    pub fn locale(&self) -> &str {
        &self.props.system_locale
    }

    pub fn build_number(&self) -> u64 {
        self.props.client_build_number
    }
}

impl Default for ClientProps {
    fn default() -> Self {
        Self::new("en-US", PINNED_BUILD_NUMBER)
    }
}

fn encode(props: &SuperProperties) -> String {
    use base64::Engine as _;
    let json = serde_json::to_vec(props).expect("super properties are plain strings and numbers");
    base64::engine::general_purpose::STANDARD.encode(json)
}

/// What Discord calls the platform this is running on.
fn host_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "Mac OS X"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    }
}

/// What the web client puts in `os_version`, which it parses out of the user
/// agent: the frozen `10_15_7` on a Mac, `10` on Windows, and nothing at all
/// on Linux, where the UA carries no version. A Mac with an empty version is
/// a pair no real browser produces.
fn os_version_for(os: &str) -> &'static str {
    match os {
        "Mac OS X" => "10.15.7",
        "Windows" => "10",
        _ => "",
    }
}

/// The `User-Agent` a current Chrome sends on that platform.
///
/// Chrome's reduced user-agent froze the platform string years ago, so these
/// are constants rather than anything read off the running system: a real
/// Chrome on any Linux sends `X11; Linux x86_64` whatever the distribution and
/// whatever the architecture.
fn user_agent_for(os: &str) -> String {
    let platform = match os {
        "Mac OS X" => "Macintosh; Intel Mac OS X 10_15_7",
        "Windows" => "Windows NT 10.0; Win64; x64",
        _ => "X11; Linux x86_64",
    };
    format!(
        "Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{BROWSER_VERSION} Safari/537.36"
    )
}

// ---------------------------------------------------------------------------
// Build-number discovery
// ---------------------------------------------------------------------------

/// What the cache file holds.
///
/// The user agent is recorded with the number because the two are a pair: a
/// build number from a page fetched as one browser does not necessarily
/// describe the client another browser would be served.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildCache {
    pub build_number: u64,
    pub fetched_at: jiff::Timestamp,
    pub user_agent: String,
}

impl BuildCache {
    fn fresh(&self, user_agent: &str) -> bool {
        if self.user_agent != user_agent {
            return false;
        }
        let age = jiff::Timestamp::now().as_second() - self.fetched_at.as_second();
        age >= 0 && (age as u64) < CACHE_TTL.as_secs()
    }
}

fn cache_path(paths: &Paths) -> anyhow::Result<std::path::PathBuf> {
    Ok(paths.cache_dir()?.join("client_build.toml"))
}

/// The cached number, if there is one and it is not stale.
pub fn load_cache(paths: &Paths, user_agent: &str) -> Option<BuildCache> {
    let text = std::fs::read_to_string(cache_path(paths).ok()?).ok()?;
    let cache: BuildCache = toml::from_str(&text).ok()?;
    cache.fresh(user_agent).then_some(cache)
}

/// Best effort. A cache that cannot be written costs a request next startup.
pub fn store_cache(paths: &Paths, cache: &BuildCache) {
    let Ok(path) = cache_path(paths) else { return };
    if let Some(parent) = path.parent() {
        let _ = crate::paths::own_dir(parent);
    }
    match toml::to_string_pretty(cache) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                tracing::debug!("could not cache the build number: {e}");
            }
        }
        Err(e) => tracing::debug!("could not serialise the build cache: {e}"),
    }
}

/// Fetch `https://discord.com/app` and read the build number out of its assets.
///
/// The number lives in one of the three hundred-odd chunks the page loads, and
/// which one changes between deploys. Scanning from the last script tag back is
/// not a guess: the entry chunk is emitted last and has carried it every time
/// this was checked, so the common case is one extra request rather than three
/// hundred. `budget` caps the whole walk, because this runs before a login and
/// a login must not wait on Discord's CDN.
pub async fn discover_build_number(
    client: &reqwest::Client,
    user_agent: &str,
    budget: Duration,
) -> Option<u64> {
    let deadline = tokio::time::Instant::now() + budget;
    let fetch = |url: String| {
        let req = client
            .get(url)
            .header(reqwest::header::USER_AGENT, user_agent);
        async move { req.send().await.ok()?.text().await.ok() }
    };

    let html = tokio::time::timeout_at(deadline, fetch(APP_URL.to_string()))
        .await
        .ok()??;

    for path in asset_paths(&html).into_iter().rev() {
        let url = format!("https://discord.com{path}");
        let Ok(Some(js)) = tokio::time::timeout_at(deadline, fetch(url)).await else {
            break;
        };
        if let Some(n) = scan_build_number(&js) {
            return Some(n);
        }
    }
    None
}

/// Every `/assets/*.js` the page loads, in document order.
///
/// Hand-written rather than a regex or an HTML parser: the shape is
/// `src="/assets/…​.js"` and nothing else on the page looks like it, so a
/// parser would be a dependency bought for one line of work.
pub fn asset_paths(html: &str) -> Vec<&str> {
    const OPEN: &str = "src=\"/assets/";
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len() - "/assets/".len()..];
        match after.find('"') {
            Some(end) if after[..end].ends_with(".js") => {
                out.push(&after[..end]);
                rest = &after[end..];
            }
            Some(end) => rest = &after[end..],
            None => break,
        }
    }
    out
}

/// The first `build_number:"123456"` in a chunk.
///
/// Also matches the `buildNumber:` spelling, which the same chunk carries: the
/// two are emitted from one constant and have never disagreed, but reading
/// whichever comes first costs nothing and removes a way to miss it.
pub fn scan_build_number(js: &str) -> Option<u64> {
    for key in ["build_number:\"", "buildNumber:\""] {
        let mut rest = js;
        while let Some(at) = rest.find(key) {
            let after = &rest[at + key.len()..];
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            if digits.len() >= 6 {
                if let Ok(n) = digits.parse() {
                    return Some(n);
                }
            }
            rest = after;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header, the `User-Agent` and IDENTIFY all describe one client, or
    /// they describe a client that does not exist.
    #[test]
    fn the_header_and_the_user_agent_agree() {
        let props = ClientProps::new("en-GB", 611_316);
        assert_eq!(
            props.user_agent(),
            props.identify_properties().browser_user_agent,
            "the User-Agent header and browser_user_agent must be one string"
        );
        assert!(props.user_agent().contains(BROWSER_VERSION));
        assert_eq!(props.identify_properties().browser_version, BROWSER_VERSION);
    }

    #[test]
    fn the_header_is_base64_of_the_identify_properties() {
        use base64::Engine as _;
        let props = ClientProps::new("en-US", 42);
        let raw = base64::engine::general_purpose::STANDARD
            .decode(props.super_properties_header())
            .expect("the header is base64");
        let back: SuperProperties = serde_json::from_slice(&raw).expect("the header is JSON");
        assert_eq!(&back, props.identify_properties());
        assert_eq!(back.client_build_number, 42);
        assert_eq!(back.release_channel, "stable");
        assert_eq!(back.browser, "Chrome");
        assert!(
            back.client_event_source.is_none(),
            "client_event_source is null in a browser session, not a string"
        );
    }

    #[test]
    fn a_rediscovered_build_number_rewrites_the_header() {
        let props = ClientProps::new("en-US", 1);
        let newer = props.with_build_number(2);
        assert_ne!(
            props.super_properties_header(),
            newer.super_properties_header()
        );
        assert_eq!(newer.identify_properties().client_build_number, 2);
        assert_eq!(
            newer.user_agent(),
            props.user_agent(),
            "only the build number changes"
        );
    }

    /// Trimmed from the real `/assets/web.*.js` fetched on 2026-09-13, which is
    /// the shape that matters: minified, no whitespace, the number buried in a
    /// request body next to a millisecond timestamp that is also six digits or
    /// more.
    const ASSET_EXCERPT: &str = concat!(
        "ush(i),(n||this._metrics.length>=100)&&this._flush()}_flush(){if(this._metrics.length>0)",
        "{let e=[...this._metrics];r.Bo.post({url:o.Rsh.METRICS_V2,body:{metrics:e,client_info:",
        "{built_at:\"1789111143211\",build_number:\"611316\"}},retries:1,rejectWithError:!0})",
        ".catch(t=>{this._metrics.length+e.length<100&&(this._metrics=[...this._metrics,...e])})}",
        "this._metrics=[]}_metrics;_intervalId}},573879(e,t,n){\"use strict\";n.d(t,{Gl:()"
    );

    #[test]
    fn the_build_number_is_found_in_a_real_chunk() {
        assert_eq!(scan_build_number(ASSET_EXCERPT), Some(611_316));
    }

    #[test]
    fn a_chunk_without_one_yields_nothing() {
        assert_eq!(scan_build_number(""), None);
        assert_eq!(scan_build_number("build_number:\"123\""), None, "too short");
        assert_eq!(scan_build_number("build_number:123456"), None, "unquoted");
        assert_eq!(scan_build_number(&"x".repeat(10_000)), None);
    }

    #[test]
    fn the_asset_list_is_in_document_order() {
        let html = concat!(
            "<html><head><script src=\"/assets/533077.aaffa7a5.js\" defer></script>",
            "<link rel=stylesheet href=\"/assets/web.1234.css\">",
            "<script src=\"/assets/web.f34f869e.js\" defer></script></head></html>"
        );
        assert_eq!(
            asset_paths(html),
            vec!["/assets/533077.aaffa7a5.js", "/assets/web.f34f869e.js"],
            "stylesheets are not chunks, and the entry chunk must stay last"
        );
    }

    #[test]
    fn a_stale_or_foreign_cache_is_not_used() {
        let now = jiff::Timestamp::now();
        let fresh = BuildCache {
            build_number: 1,
            fetched_at: now,
            user_agent: "ua".into(),
        };
        assert!(fresh.fresh("ua"));
        assert!(
            !fresh.fresh("other ua"),
            "a different browser, a different build"
        );

        let old = BuildCache {
            fetched_at: now - jiff::SignedDuration::from_hours(25),
            ..fresh.clone()
        };
        assert!(!old.fresh("ua"));
    }

    proptest::proptest! {
        /// The scanner reads bytes served by somebody else. It may find
        /// nothing; it may not panic and may not run away.
        #[test]
        fn the_scanner_never_panics(s in ".{0,2000}") {
            let _ = scan_build_number(&s);
            let _ = asset_paths(&s);
        }

        #[test]
        fn a_number_of_any_length_is_read_correctly(n in 100_000u64..u64::MAX) {
            let js = format!("x={{build_number:\"{n}\"}};");
            proptest::prop_assert_eq!(scan_build_number(&js), Some(n));
        }
    }
}
