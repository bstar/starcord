//! The GIF picker.
//!
//! Three requests — trending, search, suggest — and one rule that matters more
//! than any of them: **the core is what keeps them apart in time**, not the UI.
//!
//! A GIF picker is a search box somebody types into. Left alone, that is one
//! request per keystroke, which on a user account is the single most
//! bot-looking thing a client can do. So every request goes through a
//! [`Spacer`]: requests are at least [`SPACING`] apart, and a request that is
//! still waiting when a newer one arrives is dropped rather than sent, because
//! by the time it would come back nobody wants its answer. The UI may call
//! `GifSearch` on every keystroke; what leaves the machine is one request per
//! three hundred milliseconds, carrying the newest query.
//!
//! The provider is configuration, not a constant. Discord proxies somebody
//! else's service here and has announced a change of provider for 2026; see
//! [`GifProvider`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::discord::handle::{Event, EventSink, RequestId};
use crate::discord::http::api;
use crate::discord::http::Http;

pub use crate::discord::http::route::GifProvider;
// The picker's vocabulary, re-exported so a caller writes `gifs::GifPage`
// rather than reaching into `model`. The UI that consumes them lands later.
#[allow(unused_imports)]
pub use crate::discord::model::gif::{GifCategory, GifPage, GifResult};

/// The least time between two picker requests.
///
/// Three hundred milliseconds is roughly a fast typist's gap between words, so
/// a search runs when somebody pauses rather than on every letter. It is also
/// the number in the account-safety notes, and the two are the same number on
/// purpose.
pub const SPACING: Duration = Duration::from_millis(300);

/// What to do with a request that has just arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Nothing has gone out recently; send it.
    Now,
    /// Wait this long first, then check it is still the newest.
    After(Duration),
    /// A newer request is already queued. This one's answer is not wanted.
    Superseded,
}

/// Keeps requests apart in time, and drops the ones overtaken while waiting.
///
/// Deliberately not async and deliberately holding no clock of its own: every
/// decision is a function of the arguments, so the behaviour can be asserted in
/// a table rather than by sleeping.
#[derive(Debug, Default)]
pub struct Spacer {
    /// When the next request may go out. Advanced past `now` when one is
    /// queued, so a third arrival queues behind the second rather than beside
    /// it.
    next_allowed: Option<Instant>,
    /// The newest request id seen. Anything older has been overtaken.
    newest: u64,
}

impl Spacer {
    /// Decide what happens to a request.
    pub fn claim(&mut self, id: RequestId, now: Instant, spacing: Duration) -> Slot {
        if id.0 < self.newest {
            return Slot::Superseded;
        }
        self.newest = id.0;

        match self.next_allowed {
            Some(at) if at > now => {
                let wait = at - now;
                self.next_allowed = Some(at + spacing);
                Slot::After(wait)
            }
            _ => {
                self.next_allowed = Some(now + spacing);
                Slot::Now
            }
        }
    }

    /// Whether a request that waited is still the one anybody wants.
    pub fn is_current(&self, id: RequestId) -> bool {
        id.0 >= self.newest
    }
}

/// The picker's end of the core.
///
/// Cheap to clone; one is held by the command loop and moved into each task.
#[derive(Clone)]
pub struct Gifs {
    http: Arc<Http>,
    provider: Arc<GifProvider>,
    events: EventSink,
    spacer: Arc<Mutex<Spacer>>,
}

/// Which of the three questions a command asked.
#[derive(Debug, Clone)]
pub enum Ask {
    Trending,
    Search(String),
    Suggest(String),
}

impl Gifs {
    pub fn new(http: Arc<Http>, provider: GifProvider, events: EventSink) -> Self {
        Self {
            http,
            provider: Arc::new(provider),
            events,
            spacer: Arc::new(Mutex::new(Spacer::default())),
        }
    }

    pub fn provider(&self) -> &GifProvider {
        &self.provider
    }

    /// Run one picker request, spaced.
    ///
    /// Awaits the gap rather than refusing, so the last thing typed is the
    /// thing that gets searched for. Nothing is emitted for a request that was
    /// overtaken: the UI keyed its results on the request id and would discard
    /// a stale answer anyway, and an event nobody wants is still an event that
    /// can push a wanted one out of a bounded channel.
    pub async fn run(&self, id: RequestId, ask: Ask) {
        let slot = {
            let mut spacer = self.spacer.lock().unwrap_or_else(|e| e.into_inner());
            spacer.claim(id, Instant::now(), SPACING)
        };
        match slot {
            Slot::Now => {}
            Slot::Superseded => {
                tracing::trace!("gif request {} was overtaken before it ran", id.0);
                return;
            }
            Slot::After(wait) => {
                tokio::time::sleep(wait).await;
                let current = {
                    let spacer = self.spacer.lock().unwrap_or_else(|e| e.into_inner());
                    spacer.is_current(id)
                };
                if !current {
                    tracing::trace!("gif request {} was overtaken while it waited", id.0);
                    return;
                }
            }
        }

        let result = match &ask {
            Ask::Trending => api::gifs_trending(&self.http, &self.provider).await,
            Ask::Search(query) => api::gifs_search(&self.http, &self.provider, query).await,
            Ask::Suggest(prefix) => api::gifs_suggest(&self.http, &self.provider, prefix).await,
        };

        let result = result.map_err(|e| {
            tracing::debug!("the gif picker asked and got nothing back: {e}");
            e.to_string()
        });
        self.events.send(Event::Gifs { id, result });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::props::ClientProps;

    fn gifs(base: &str) -> (Gifs, crossbeam_channel::Receiver<Event>) {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let http = Arc::new(
            Http::with_base(Arc::new(ClientProps::new("en-US", 1)), base.to_string())
                .expect("a client with no tls work to do"),
        );
        (
            Gifs::new(http, GifProvider::default(), EventSink::detached(tx)),
            rx,
        )
    }

    fn page(events: &crossbeam_channel::Receiver<Event>) -> GifPage {
        match events.try_recv() {
            Ok(Event::Gifs { result, .. }) => result.expect("the request failed"),
            other => panic!("{other:?}"),
        }
    }

    /// Decisions only, with the clock passed in. The async path that sleeps is
    /// the one thing here not worth testing: `tokio::time::sleep` sleeps.
    #[test]
    fn requests_are_kept_three_hundred_milliseconds_apart() {
        let mut spacer = Spacer::default();
        let start = Instant::now();

        assert_eq!(
            spacer.claim(RequestId(1), start, SPACING),
            Slot::Now,
            "the first request has nothing to wait for"
        );
        assert_eq!(
            spacer.claim(RequestId(2), start + Duration::from_millis(50), SPACING),
            Slot::After(Duration::from_millis(250)),
            "a request 50ms later waits out the rest of the gap"
        );
        assert_eq!(
            spacer.claim(RequestId(3), start + Duration::from_millis(400), SPACING),
            Slot::After(Duration::from_millis(200)),
            "the second one moved the gate, so the third queues behind it"
        );
        assert_eq!(
            spacer.claim(RequestId(4), start + Duration::from_secs(5), SPACING),
            Slot::Now,
            "after a pause there is nothing to wait for"
        );
    }

    #[test]
    fn a_request_overtaken_while_it_waited_is_dropped() {
        let mut spacer = Spacer::default();
        let start = Instant::now();

        spacer.claim(RequestId(1), start, SPACING);
        // Typed another letter: request 2 is queued.
        assert!(matches!(
            spacer.claim(RequestId(2), start + Duration::from_millis(10), SPACING),
            Slot::After(_)
        ));
        assert!(spacer.is_current(RequestId(2)));

        // And another. Two is now stale before its sleep is over.
        assert!(matches!(
            spacer.claim(RequestId(3), start + Duration::from_millis(20), SPACING),
            Slot::After(_)
        ));
        assert!(!spacer.is_current(RequestId(2)));
        assert!(spacer.is_current(RequestId(3)));
    }

    /// An answer that arrives out of order must not resurrect itself.
    #[test]
    fn an_id_older_than_one_already_seen_never_runs() {
        let mut spacer = Spacer::default();
        let now = Instant::now();
        spacer.claim(RequestId(9), now, SPACING);
        assert_eq!(
            spacer.claim(RequestId(4), now + Duration::from_secs(1), SPACING),
            Slot::Superseded
        );
    }

    #[tokio::test]
    async fn trending_asks_for_the_configured_provider_and_reads_either_shape() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gifs/trending"))
            .and(query_param("provider", "tenor"))
            .and(query_param("media_format", "gif"))
            .and(query_param("locale", "en-US"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "categories": [{"name": "reaction", "src": "https://c/r.png"}],
                "gifs": [{
                    "id": "1", "title": "a cat",
                    "url": "https://tenor.com/view/cat-1",
                    "gif_src": "https://media.tenor.com/1.gif",
                    "width": 300, "height": 200
                }]
            })))
            .mount(&server)
            .await;

        let (gifs, events) = gifs(&server.uri());
        gifs.run(RequestId(1), Ask::Trending).await;

        let page = page(&events);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].url, "https://tenor.com/view/cat-1");
        assert_eq!(page.categories.len(), 1);
    }

    #[tokio::test]
    async fn a_search_query_is_escaped_rather_than_pasted_into_the_url() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gifs/search"))
            // `wiremock` matches the decoded value, so this asserts that the
            // ampersand arrived as part of the query rather than as a second
            // parameter.
            .and(query_param("q", "cats & dogs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "2", "title": "dog", "url": "https://tenor.com/view/dog-2"}
            ])))
            .mount(&server)
            .await;

        let (gifs, events) = gifs(&server.uri());
        gifs.run(RequestId(1), Ask::Search("cats & dogs".into()))
            .await;

        let page = page(&events);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "dog");
        assert!(page.suggestions.is_empty());
    }

    #[tokio::test]
    async fn suggest_answers_with_search_terms_and_asks_for_no_media_format() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gifs/suggest"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!(["cat", "cat hug"])),
            )
            .mount(&server)
            .await;

        let (gifs, events) = gifs(&server.uri());
        gifs.run(RequestId(1), Ask::Suggest("ca".into())).await;

        let page = page(&events);
        assert_eq!(page.suggestions, vec!["cat", "cat hug"]);
        assert!(page.results.is_empty());

        let asked = &server.received_requests().await.unwrap()[0];
        let query = asked.url.query().unwrap_or_default();
        assert!(
            !query.contains("media_format"),
            "suggest returns words, not pictures: {query}"
        );
        assert!(query.contains("locale=en-US"), "{query}");
    }

    #[tokio::test]
    async fn a_refused_request_is_a_sentence_rather_than_a_panic() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "code": 50001, "message": "Missing Access"
            })))
            .mount(&server)
            .await;

        let (gifs, events) = gifs(&server.uri());
        gifs.run(RequestId(1), Ask::Trending).await;

        match events.try_recv() {
            Ok(Event::Gifs { result: Err(e), .. }) => assert!(e.contains("403"), "{e}"),
            other => panic!("{other:?}"),
        }
    }
}
