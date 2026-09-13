//! The task that turns a request for a picture into pixels.
//!
//! One task owns the queue; four downloads run at once; decoding happens on a
//! blocking thread. The interesting parts are all about what *not* to do.
//!
//! **Order matters more than throughput.** A scroll asks for forty pictures at
//! once and then moves on before half of them arrive. The queue is ordered by
//! whether something is on screen and then by how recently it was asked for, so
//! the newest visible request is served first and a prefetch from three screens
//! ago is dropped rather than fetched. That is what `generation` is: a number
//! the UI bumps when the viewport moves.
//!
//! **The same picture is fetched once.** Forty messages from one person are one
//! avatar. A key already in flight is not queued again; the answer goes out as
//! an `Event`, which everybody waiting for that key sees.
//!
//! **An expired attachment is retried exactly once.** Discord signs attachment
//! URLs and the signature lapses within the day, so a 403 or a 404 on an
//! attachment is usually not a missing file: it is a stale link, and
//! `POST /attachments/refresh-urls` re-signs it. Once, though — a second 403 on
//! a freshly signed URL means the file is genuinely gone, and a client that
//! keeps asking is a client in a loop.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};

use super::cache::Cache;
use super::decode::{self, Decoding};
use super::{Decoded, MediaConfig, MediaError, MediaKey, MediaPriority, MediaRequest, Want};
use crate::discord::handle::{Event, EventSink, ExternalKind, Note};
use crate::discord::http::{Http, HttpError};

/// How many downloads run at once.
///
/// Four rather than the eight the REST semaphore allows. These are CDN
/// transfers of up to twenty-five megabytes each, and the thing being optimised
/// is the time until the *first* picture on screen appears, not the time until
/// the last one does.
const CONCURRENCY: usize = 4;

/// How many requests may be waiting to be handed to the task.
const CHANNEL: usize = 256;

/// What the media task is asked to do.
#[derive(Debug)]
enum Job {
    Fetch(Box<MediaRequest>),
    Cancel(MediaKey),
    Open {
        url: String,
        kind: ExternalKind,
    },
    /// A worker finished, so another may start. Sent by the workers themselves.
    Done(MediaKey),
}

/// The core's handle on the media task.
#[derive(Clone)]
pub struct Media {
    jobs: mpsc::Sender<Job>,
}

impl Media {
    pub fn fetch(&self, request: MediaRequest) {
        self.send(Job::Fetch(Box::new(request)));
    }

    pub fn cancel(&self, key: MediaKey) {
        self.send(Job::Cancel(key));
    }

    pub fn open_external(&self, url: String, kind: ExternalKind) {
        self.send(Job::Open { url, kind });
    }

    fn send(&self, job: Job) {
        if self.jobs.try_send(job).is_err() {
            // A dropped fetch is a picture that stays a placeholder until the
            // next frame asks again, which the UI does. Blocking the command
            // loop for one would be worse.
            tracing::debug!("the media queue is full; dropped a request");
        }
    }
}

/// Everything a worker needs. Cheap to clone; one per running download.
#[derive(Clone)]
pub struct Context {
    pub http: Arc<Http>,
    pub cache: Arc<Cache>,
    pub config: Arc<MediaConfig>,
    pub events: EventSink,
}

/// Start the media task and return the handle the core keeps.
pub fn spawn(context: Context) -> Media {
    let (tx, rx) = mpsc::channel(CHANNEL);
    let jobs = tx.clone();
    tokio::spawn(async move { run(context, rx, tx).await });
    Media { jobs }
}

async fn run(context: Context, mut rx: mpsc::Receiver<Job>, tx: mpsc::Sender<Job>) {
    // The cache is swept once at startup rather than only when it grows past
    // the cap, because the cap may have been lowered since the last run and
    // because a client that is never left open long enough to cross it would
    // otherwise never sweep at all.
    {
        let cache = Arc::clone(&context.cache);
        let max = context.config.cache_max_bytes();
        let _ = tokio::task::spawn_blocking(move || cache.sweep(max)).await;
    }

    let permits = Arc::new(Semaphore::new(CONCURRENCY));
    let mut queue = Queue::default();
    let mut inflight: HashSet<MediaKey> = HashSet::new();

    while let Some(job) = rx.recv().await {
        match job {
            Job::Fetch(request) => {
                if inflight.contains(&request.key) {
                    // Somebody else is already fetching it; the answer goes out
                    // as an event, which every waiter sees.
                    continue;
                }
                queue.push(*request);
            }
            Job::Cancel(key) => {
                queue.cancel(&key);
            }
            Job::Done(key) => {
                inflight.remove(&key);
            }
            Job::Open { url, kind } => {
                let context = context.clone();
                tokio::spawn(async move { open_external(&context, &url, kind).await });
                continue;
            }
        }

        // Start as much as the permits allow, in priority order.
        while let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
            let Some(request) = queue.take_best() else {
                break;
            };
            inflight.insert(request.key.clone());
            let context = context.clone();
            let done = tx.clone();
            tokio::spawn(async move {
                let key = request.key.clone();
                let result = fetch_one(&context, request).await;
                context.events.send(Event::Media {
                    key: key.clone(),
                    result,
                });
                drop(permit);
                // Best effort: if the task is gone there is nothing to tell.
                let _ = done.send(Job::Done(key)).await;
            });
        }
    }
}

/// The ordered pile of requests that have not started yet.
///
/// A `Vec` rather than a heap. It holds what one viewport asked for — tens of
/// entries, not thousands — and a linear scan that can also replace an existing
/// entry in place is simpler than a heap plus a side table to find things in it.
#[derive(Debug, Default)]
pub struct Queue {
    items: Vec<MediaRequest>,
    /// The newest generation anything has been queued at.
    generation: u64,
}

impl Queue {
    /// Add a request, replacing any earlier one for the same picture.
    pub fn push(&mut self, request: MediaRequest) {
        self.generation = self.generation.max(request.generation);
        match self.items.iter_mut().find(|item| item.key == request.key) {
            // The same picture asked for again, more urgently or more recently.
            Some(existing) => {
                existing.priority = existing.priority.max(request.priority);
                existing.generation = existing.generation.max(request.generation);
                existing.want = request.want;
            }
            None => self.items.push(request),
        }
    }

    /// Drop a queued request. Says whether there was one.
    pub fn cancel(&mut self, key: &MediaKey) -> bool {
        let before = self.items.len();
        self.items.retain(|item| &item.key != key);
        self.items.len() != before
    }

    /// The next thing worth fetching.
    ///
    /// Stale prefetches go first — a prefetch queued two viewports ago is for
    /// something the reader has already scrolled past. A stale *visible*
    /// request is kept: it was on screen when it was asked for, and the cheapest
    /// way to be wrong about that is to fetch a picture nobody sees.
    pub fn take_best(&mut self) -> Option<MediaRequest> {
        let generation = self.generation;
        self.items.retain(|item| {
            item.priority != MediaPriority::Prefetch || item.generation >= generation
        });

        let (index, _) = self
            .items
            .iter()
            .enumerate()
            .max_by_key(|(_, item)| (item.priority, item.generation))?;
        Some(self.items.swap_remove(index))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Fetch and decode one thing.
///
/// Public because `probe --media` drives exactly this, with no queue and no
/// core behind it: a defect in the fetch path should reproduce from one command
/// line rather than from a logged-in session that happened to scroll past the
/// wrong picture.
pub async fn fetch_one(
    context: &Context,
    request: MediaRequest,
) -> Result<Arc<Decoded>, MediaError> {
    let MediaRequest { key, want, .. } = request;
    let bytes = bytes_for(context, &key).await?;

    match want {
        Want::Bytes => Ok(Arc::new(Decoded::Bytes(Arc::new(bytes)))),
        Want::Decoded { max_w, max_h } => {
            let described = key.describe();
            let Decoding { decoded, note } = decode_off_thread(bytes, max_w, max_h).await?;
            if let Some(note) = note {
                tracing::debug!("{described}: {note}");
                context.events.send(Event::Note(Note::info(note)));
            }
            Ok(Arc::new(decoded))
        }
    }
}

/// Decoding is the one genuinely CPU-bound thing the core does.
///
/// A 4000-pixel JPEG takes tens of milliseconds, which on the runtime's two
/// worker threads is tens of milliseconds during which no gateway frame is
/// read.
async fn decode_off_thread(bytes: Vec<u8>, max_w: u32, max_h: u32) -> Result<Decoding, MediaError> {
    match tokio::task::spawn_blocking(move || decode::decode(&bytes, max_w, max_h)).await {
        Ok(result) => result,
        Err(e) => Err(MediaError::Decode(format!("the decoder stopped: {e}"))),
    }
}

/// The bytes for a key: from the cache if they are there, from the network if
/// they are not.
pub async fn bytes_for(context: &Context, key: &MediaKey) -> Result<Vec<u8>, MediaError> {
    let url = key.url()?;
    let cap = context.config.cap(key.kind());

    if let Some(path) = context.cache.find(&url) {
        match tokio::fs::read(&path).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) => {
                // A cache entry that cannot be read is a cache entry that is not
                // there. Swept or half-written; either way, fetch it again.
                tracing::debug!("could not read {}: {e}", path.display());
            }
        }
    }

    let download = match context.http.download_typed(url.as_str(), cap).await {
        Ok(download) => download,
        Err(e) if e.is_gone() && key.is_refreshable() => {
            // A signed link that lapsed. One refresh, one retry.
            let refreshed = refresh(context, url.as_str()).await?;
            context
                .http
                .download_typed(refreshed.as_str(), cap)
                .await
                .map_err(|e| media_error(e, cap))?
        }
        Err(e) => return Err(media_error(e, cap)),
    };

    // Stored under the *original* URL, not the refreshed one: they canonicalise
    // to the same name, and the original is what the next lookup will ask for.
    match context
        .cache
        .store(&url, download.content_type.as_deref(), &download.bytes)
    {
        Ok(_) => {
            let max = context.config.cache_max_bytes();
            let cache = Arc::clone(&context.cache);
            // Sweeping walks a directory, so it does not belong on a worker
            // thread that something else is waiting on.
            tokio::spawn(async move {
                let _ = tokio::task::spawn_blocking(move || {
                    if cache.size() > max {
                        cache.sweep(max);
                    }
                })
                .await;
            });
        }
        Err(e) => tracing::debug!("could not cache {}: {e}", key.describe()),
    }

    Ok(download.bytes)
}

/// Ask Discord to re-sign one attachment URL.
async fn refresh(context: &Context, url: &str) -> Result<url::Url, MediaError> {
    let answer = crate::discord::http::api::refresh_attachment_urls(
        &context.http,
        std::slice::from_ref(&url.to_string()),
    )
    .await
    .map_err(|e| {
        tracing::debug!("refreshing an attachment url failed: {e}");
        MediaError::Expired
    })?;

    let refreshed = answer
        .refreshed_urls
        .into_iter()
        .next()
        .ok_or(MediaError::Expired)?;
    url::Url::parse(&refreshed.refreshed).map_err(|_| MediaError::Expired)
}

fn media_error(error: HttpError, cap: u64) -> MediaError {
    match error {
        HttpError::TooLarge { .. } => MediaError::TooLarge { limit: cap },
        HttpError::NotHttps { url } => MediaError::Unsupported(format!("{url} is not https")),
        other if other.is_gone() => MediaError::Expired,
        other => MediaError::Network(other.to_string()),
    }
}

/// Hand something to a program that can show it.
///
/// A plain link goes to whatever the desktop opens links with. A picture or a
/// video is downloaded into the cache first and the configured player is given
/// the **path**, as `argv` — never a shell command line. A filename that came
/// off a stranger's computer can contain a space, a quote or a semicolon, and
/// the only way that is safe is for it never to be parsed by anything.
async fn open_external(context: &Context, url: &str, kind: ExternalKind) {
    if kind == ExternalKind::Link {
        open_in_browser(context, url.to_string()).await;
        return;
    }

    let key = MediaKey::EmbedImage {
        url: url.to_string(),
    };
    let Ok(parsed) = key.url() else {
        context.events.send(Event::Note(Note::warning(
            "open-external",
            "that link cannot be opened",
        )));
        return;
    };

    // The same caps as everything else: an external player does not make a
    // half-gigabyte download reasonable.
    let cap = context.config.cap(super::MediaKind::Attachment);
    let path = match context.cache.find(&parsed) {
        Some(path) => path,
        None => match context.http.download_typed(parsed.as_str(), cap).await {
            Ok(download) => match context.cache.store(
                &parsed,
                download.content_type.as_deref(),
                &download.bytes,
            ) {
                Ok(path) => path,
                Err(e) => {
                    context.events.send(Event::Note(Note::warning(
                        "open-external",
                        format!("could not save it to open it: {e}"),
                    )));
                    return;
                }
            },
            Err(e) => {
                context.events.send(Event::Note(Note::warning(
                    "open-external",
                    format!("could not download it: {e}"),
                )));
                return;
            }
        },
    };

    if context.config.player.is_empty() {
        // No player configured: the browser is a better answer than nothing,
        // and it is what the user gets for a link anyway.
        open_in_browser(context, url.to_string()).await;
        return;
    }

    match play(&context.config.player, &path).await {
        Ok(()) => {}
        Err(e) => context.events.send(Event::Note(Note::warning(
            "open-external",
            format!("{} could not be started: {e}", context.config.player[0]),
        ))),
    }
}

/// Launch the configured player with the file appended to its argv.
async fn play(player: &[String], path: &std::path::Path) -> std::io::Result<()> {
    let program = player[0].clone();
    let args: Vec<String> = player[1..].to_vec();
    let path: PathBuf = path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(&program);
        command.args(&args).arg(&path);
        // Detached from this process's terminal: the player must not draw over
        // the client, and the client must not wait for it to exit.
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        command.spawn().map(|child| {
            tracing::debug!("started {program} as pid {}", child.id());
        })
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

async fn open_in_browser(context: &Context, url: String) {
    let reported = url.clone();
    let opened = tokio::task::spawn_blocking(move || webbrowser::open(&url)).await;
    match opened {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::debug!("could not open {reported}: {e}");
            context.events.send(Event::Note(Note::warning(
                "open-external",
                format!("nothing on this system opened that link: {e}"),
            )));
        }
        Err(e) => tracing::debug!("the browser task stopped: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::props::ClientProps;
    use crate::discord::snowflake::{MessageId, UserId};

    fn avatar(user: u64) -> MediaKey {
        MediaKey::Avatar {
            user: UserId(user),
            hash: format!("{user:032x}"),
            size: 64,
        }
    }

    fn request(key: MediaKey, priority: MediaPriority, generation: u64) -> MediaRequest {
        MediaRequest {
            key,
            want: Want::Decoded {
                max_w: 32,
                max_h: 32,
            },
            priority,
            generation,
        }
    }

    #[test]
    fn what_is_on_screen_comes_out_first() {
        let mut queue = Queue::default();
        queue.push(request(avatar(1), MediaPriority::Prefetch, 1));
        queue.push(request(avatar(2), MediaPriority::Visible, 1));
        queue.push(request(avatar(3), MediaPriority::Prefetch, 1));

        assert_eq!(queue.take_best().unwrap().key, avatar(2));
        assert_eq!(queue.len(), 2);
    }

    /// Within one priority, the most recently asked for wins: it is the one the
    /// reader is looking at now.
    #[test]
    fn the_newest_request_wins_a_tie() {
        let mut queue = Queue::default();
        queue.push(request(avatar(1), MediaPriority::Visible, 1));
        queue.push(request(avatar(2), MediaPriority::Visible, 7));
        queue.push(request(avatar(3), MediaPriority::Visible, 4));

        assert_eq!(queue.take_best().unwrap().key, avatar(2));
        assert_eq!(queue.take_best().unwrap().key, avatar(3));
        assert_eq!(queue.take_best().unwrap().key, avatar(1));
        assert!(queue.take_best().is_none());
    }

    /// The point of `generation`: a prefetch for something three screens back is
    /// work nobody is waiting for.
    #[test]
    fn a_stale_prefetch_is_dropped_and_a_stale_visible_request_is_not() {
        let mut queue = Queue::default();
        queue.push(request(avatar(1), MediaPriority::Prefetch, 1));
        queue.push(request(avatar(2), MediaPriority::Visible, 1));
        // The viewport moved.
        queue.push(request(avatar(3), MediaPriority::Prefetch, 9));

        let first = queue.take_best().unwrap();
        assert_eq!(
            first.key,
            avatar(2),
            "visible still outranks a fresh prefetch"
        );
        assert_eq!(queue.take_best().unwrap().key, avatar(3));
        assert!(
            queue.is_empty(),
            "the prefetch from the older generation should have been dropped"
        );
    }

    #[test]
    fn asking_twice_queues_once_and_keeps_the_stronger_claim() {
        let mut queue = Queue::default();
        queue.push(request(avatar(1), MediaPriority::Prefetch, 1));
        queue.push(request(avatar(1), MediaPriority::Visible, 3));
        assert_eq!(queue.len(), 1);

        let taken = queue.take_best().unwrap();
        assert_eq!(taken.priority, MediaPriority::Visible);
        assert_eq!(taken.generation, 3);
    }

    #[test]
    fn cancelling_takes_it_out_of_the_queue() {
        let mut queue = Queue::default();
        queue.push(request(avatar(1), MediaPriority::Visible, 1));
        queue.push(request(avatar(2), MediaPriority::Visible, 1));

        assert!(queue.cancel(&avatar(1)));
        assert!(
            !queue.cancel(&avatar(1)),
            "cancelling twice is not an error"
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.take_best().unwrap().key, avatar(2));
    }

    fn context(base: &str, dir: &std::path::Path) -> (Context, crossbeam_channel::Receiver<Event>) {
        let (tx, rx) = crossbeam_channel::bounded(64);
        let http = Http::with_base(Arc::new(ClientProps::new("en-US", 1)), base.to_string())
            .expect("a client pointed at the mock server");
        (
            Context {
                http: Arc::new(http),
                cache: Arc::new(Cache::new(dir.to_path_buf())),
                config: Arc::new(MediaConfig::default()),
                events: EventSink::detached(tx),
            },
            rx,
        )
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// A signed attachment URL that has lapsed: one refresh, one retry, and the
    /// picture arrives.
    #[tokio::test]
    async fn an_expired_attachment_is_refreshed_once_and_then_fetched() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();

        // The stale link, which Discord answers with a 403.
        Mock::given(method("GET"))
            .and(path("/attachments/1/2/cat.png"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;

        let refreshed = format!("{}/attachments/1/2/cat.png-refreshed", server.uri());
        Mock::given(method("POST"))
            .and(path("/attachments/refresh-urls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "refreshed_urls": [{"original": "x", "refreshed": refreshed}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/attachments/1/2/cat.png-refreshed"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(png(8, 4)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let (context, _events) = context(&server.uri(), dir.path());
        let key = MediaKey::Attachment {
            message: MessageId(9),
            id: 2,
            url: format!("{}/attachments/1/2/cat.png?ex=old&hm=stale", server.uri()),
        };

        let decoded = fetch_one(&context, MediaRequest::visible(key.clone(), 0, 0, 1))
            .await
            .expect("the refreshed url was not fetched");
        assert_eq!(decoded.dimensions(), Some((8, 4)));

        // Cached under the canonical url, so the second ask makes no request at
        // all — which is what the `expect(1)`s above are checking.
        let again = fetch_one(&context, MediaRequest::visible(key, 0, 0, 2))
            .await
            .unwrap();
        assert_eq!(again.dimensions(), Some((8, 4)));
    }

    /// A 403 on something that is not an attachment is a missing file, and
    /// asking Discord to re-sign an emoji would be nonsense.
    #[tokio::test]
    async fn only_an_attachment_is_worth_refreshing() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let (context, _events) = context(&server.uri(), dir.path());
        let key = MediaKey::EmbedImage {
            url: format!("{}/preview.png", server.uri()),
        };
        let error = fetch_one(&context, MediaRequest::visible(key, 32, 32, 1))
            .await
            .unwrap_err();
        assert!(matches!(error, MediaError::Expired), "{error}");

        let posts = server.received_requests().await.unwrap();
        assert!(
            posts
                .iter()
                .all(|r| r.method != wiremock::http::Method::POST),
            "an embed image asked for a refresh"
        );
    }

    /// The cap is per kind, and it is enforced while the body is read rather
    /// than from a `Content-Length` a server is free to lie about.
    #[tokio::test]
    async fn something_over_the_cap_is_refused_rather_than_decoded() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![0u8; 3 * 1024 * 1024]),
            )
            .mount(&server)
            .await;

        let (generous, _events) = context(&server.uri(), dir.path());
        let key = MediaKey::Attachment {
            message: MessageId(1),
            id: 2,
            url: format!("{}/big.png", server.uri()),
        };
        let bytes = MediaRequest {
            key: key.clone(),
            want: Want::Bytes,
            priority: MediaPriority::Visible,
            generation: 1,
        };

        // The user's own cap, which is the only one they set.
        let tight = Context {
            config: Arc::new(MediaConfig {
                max_attachment_mib: 1,
                ..MediaConfig::default()
            }),
            ..generous.clone()
        };
        let error = fetch_one(&tight, bytes.clone()).await.unwrap_err();
        assert!(
            matches!(error, MediaError::TooLarge { .. }),
            "three megabytes came back under a one-megabyte cap: {error}"
        );
        assert_eq!(
            tight.cache.size(),
            0,
            "a refused download must not be cached; nothing whole arrived"
        );

        // The same bytes under the default cap are fine, and are never decoded,
        // because nothing asked for pixels.
        let decoded = fetch_one(&generous, bytes)
            .await
            .expect("25 MiB should have been enough");
        assert!(matches!(&*decoded, Decoded::Bytes(_)), "{decoded:?}");

        // And with pixels asked for, the caps let it through and the decoder
        // refuses it, which is a different error entirely.
        let dir = tempfile::tempdir().unwrap();
        let (fresh, _events) = context(&server.uri(), dir.path());
        let error = fetch_one(&fresh, MediaRequest::visible(key, 32, 32, 1))
            .await
            .unwrap_err();
        assert!(
            matches!(error, MediaError::Unsupported(_) | MediaError::Decode(_)),
            "three megabytes of zeroes is not a picture: {error}"
        );
    }

    /// Bytes are bytes: `Want::Bytes` must not decode, so a video that this
    /// client cannot decode at all still comes back.
    #[tokio::test]
    async fn asking_for_bytes_does_not_decode_them() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "video/mp4")
                    .set_body_bytes(b"not a video either".to_vec()),
            )
            .mount(&server)
            .await;

        let (context, _events) = context(&server.uri(), dir.path());
        let request = MediaRequest {
            key: MediaKey::Attachment {
                message: MessageId(1),
                id: 2,
                url: format!("{}/clip.mp4", server.uri()),
            },
            want: Want::Bytes,
            priority: MediaPriority::Visible,
            generation: 1,
        };
        let decoded = fetch_one(&context, request).await.unwrap();
        match &*decoded {
            Decoded::Bytes(bytes) => assert_eq!(&**bytes, b"not a video either"),
            other => panic!("{other:?}"),
        }
    }
}
