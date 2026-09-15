//! Pictures: naming them, fetching them, decoding them.
//!
//! The type that matters is [`MediaKey`], and what matters about it is that it
//! names a picture by **what it is** rather than by where it currently lives.
//! An avatar is a user and a hash; an attachment is a message and an id. A
//! Discord attachment URL is signed and expires within the day, so a cache
//! keyed on the URL would miss on every refresh and a UI keyed on the URL would
//! redraw the same picture as a new one. Keyed on the thing, both stay put while
//! the URL underneath changes.
//!
//! The other half of that is [`cache::canonical`], which strips the signature
//! parameters back off before the bytes are named on disk, so the same
//! attachment fetched on Monday and on Friday is one file.
//!
//! Nothing here decodes anything the moment it arrives. [`decode`] checks the
//! header first — dimensions and allocation — and refuses a small file that
//! claims to be forty thousand pixels on a side before a decoder has a chance
//! to believe it.

pub mod cache;
pub mod decode;
pub mod fetch;

use std::sync::Arc;
use std::time::Duration;

use url::Url;

use crate::discord::snowflake::{EmojiId, GuildId, MessageId, UserId};

/// Where Discord serves pictures from.
pub const CDN_BASE: &str = "https://cdn.discordapp.com";

/// The largest size the CDN will resize to, and the smallest.
const MIN_SIZE: u16 = 16;
const MAX_SIZE: u16 = 4096;

/// A piece of media, identified by what it is rather than by its URL.
///
/// `size` is the CDN's own resize parameter, not a display size: asking for a
/// 32-pixel avatar and getting 32 pixels back is the difference between a few
/// hundred bytes and a megabyte, several hundred times over in a busy guild.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MediaKey {
    Avatar {
        user: UserId,
        /// The hash from the user object. A leading `a_` means it is animated,
        /// which is also what decides the extension.
        hash: String,
        size: u16,
    },
    GuildIcon {
        guild: GuildId,
        hash: String,
        size: u16,
    },
    Emoji {
        id: EmojiId,
        animated: bool,
        size: u16,
    },
    Sticker {
        id: u64,
    },
    /// A file somebody attached. The URL is carried because it is signed and
    /// only Discord can produce it; the message and the attachment id are what
    /// the key is, so a refreshed URL is the same picture.
    Attachment {
        message: MessageId,
        id: u64,
        url: String,
    },
    /// A picture inside a link preview, served from Discord's media proxy.
    EmbedImage {
        url: String,
    },
    /// A GIF from the picker, which is somebody else's host entirely.
    Gif {
        url: String,
    },
}

/// What a key is, for the purpose of deciding how many bytes of it to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// Avatars, guild icons, emoji, stickers: small by construction, and there
    /// are hundreds of them on screen.
    Small,
    /// Link previews and GIFs.
    Embed,
    /// A file somebody chose to send, which is the only one the user controls
    /// the size of.
    Attachment,
}

impl MediaKey {
    /// Where to fetch it from.
    ///
    /// Fallible, unlike the other six accessors, because three of the seven
    /// variants carry a string that arrived over the wire. A malformed URL in
    /// an embed is somebody else's bug and must not be this process's panic.
    pub fn url(&self) -> Result<Url, MediaError> {
        let raw = match self {
            MediaKey::Avatar { user, hash, size } => format!(
                "{CDN_BASE}/avatars/{user}/{hash}.{}?size={}",
                extension(hash),
                clamp_size(*size)
            ),
            MediaKey::GuildIcon { guild, hash, size } => format!(
                "{CDN_BASE}/icons/{guild}/{hash}.{}?size={}",
                extension(hash),
                clamp_size(*size)
            ),
            MediaKey::Emoji { id, animated, size } => format!(
                "{CDN_BASE}/emojis/{id}.{}?size={}&quality=lossless",
                if *animated { "gif" } else { "png" },
                clamp_size(*size)
            ),
            MediaKey::Sticker { id } => format!("{CDN_BASE}/stickers/{id}.png"),
            MediaKey::Attachment { url, .. }
            | MediaKey::EmbedImage { url }
            | MediaKey::Gif { url } => url.clone(),
        };

        // Parsed rather than passed through as a string, because the cache has
        // to canonicalise it and because a URL that will not parse is one no
        // amount of retrying will fetch. Whether the scheme is acceptable is
        // `Http::download_typed`'s question, not this one's: it owns the rule,
        // and a second copy of it here would be a second place to get it wrong.
        Url::parse(&raw).map_err(|e| MediaError::Unsupported(e.to_string()))
    }

    pub fn kind(&self) -> MediaKind {
        match self {
            MediaKey::Avatar { .. }
            | MediaKey::GuildIcon { .. }
            | MediaKey::Emoji { .. }
            | MediaKey::Sticker { .. } => MediaKind::Small,
            MediaKey::EmbedImage { .. } | MediaKey::Gif { .. } => MediaKind::Embed,
            MediaKey::Attachment { .. } => MediaKind::Attachment,
        }
    }

    /// Whether a failed fetch is worth one `refresh-urls` round trip.
    ///
    /// Only attachments: everything else is addressed by content and a 404 on
    /// one means the picture is gone, not that the link went stale.
    pub fn is_refreshable(&self) -> bool {
        matches!(self, MediaKey::Attachment { .. })
    }

    /// A short name for the log. Never the URL: an attachment URL carries a
    /// signature, and a signature in a log file is a link anybody who reads the
    /// log can open.
    pub fn describe(&self) -> String {
        match self {
            MediaKey::Avatar { user, .. } => format!("avatar of {user}"),
            MediaKey::GuildIcon { guild, .. } => format!("icon of {guild}"),
            MediaKey::Emoji { id, .. } => format!("emoji {id}"),
            MediaKey::Sticker { id } => format!("sticker {id}"),
            MediaKey::Attachment { message, id, .. } => format!("attachment {id} on {message}"),
            MediaKey::EmbedImage { .. } => "an embedded image".into(),
            MediaKey::Gif { .. } => "a gif".into(),
        }
    }

    /// The same key pointed at a refreshed URL.
    pub fn with_url(&self, url: String) -> MediaKey {
        match self {
            MediaKey::Attachment { message, id, .. } => MediaKey::Attachment {
                message: *message,
                id: *id,
                url,
            },
            other => other.clone(),
        }
    }
}

/// `a_` is Discord's marker for an animated hash, and the extension follows it.
fn extension(hash: &str) -> &'static str {
    if hash.starts_with("a_") {
        "gif"
    } else {
        "png"
    }
}

/// The CDN resizes to powers of two between 16 and 4096 and ignores anything
/// else, so a request for 100 comes back at the original size — which for a
/// banner is a megabyte where forty kilobytes was wanted.
fn clamp_size(size: u16) -> u16 {
    let size = size.clamp(MIN_SIZE, MAX_SIZE);
    if size.is_power_of_two() {
        return size;
    }
    size.next_power_of_two().min(MAX_SIZE)
}

/// What the caller wants back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// The bytes, undecoded. What a save-to-disk or an external player needs.
    Bytes,
    /// Pixels, no larger than this.
    Decoded { max_w: u32, max_h: u32 },
}

/// What a fetch is for, which decides the order it is served in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MediaPriority {
    /// Likely to be on screen shortly.
    Prefetch,
    /// On screen now. Ordered after `Prefetch` on purpose: the queue takes the
    /// greatest, and visible work is what should come out first.
    Visible,
}

#[derive(Debug, Clone)]
pub struct MediaRequest {
    pub key: MediaKey,
    pub want: Want,
    pub priority: MediaPriority,
    /// Bumped when the viewport moves. A queued prefetch from an older
    /// generation is dropped rather than fetched: by the time it came up the
    /// scroll had already gone past it.
    pub generation: u64,
}

impl MediaRequest {
    /// A visible request for decoded pixels, which is what almost every call
    /// site wants.
    pub fn visible(key: MediaKey, max_w: u32, max_h: u32, generation: u64) -> Self {
        Self {
            key,
            want: Want::Decoded { max_w, max_h },
            priority: MediaPriority::Visible,
            generation,
        }
    }
}

/// Decoded media.
///
/// `Debug` is written by hand. `RgbaImage`'s derived one prints every pixel,
/// which turns one `{:?}` on a struct that happens to hold a picture into
/// several megabytes of log file.
#[derive(Clone)]
pub enum Decoded {
    /// Undecoded bytes, for [`Want::Bytes`].
    Bytes(Arc<Vec<u8>>),
    Still(Arc<image::RgbaImage>),
    Animated {
        frames: Vec<Arc<image::RgbaImage>>,
        /// One per frame, already floored to something a terminal can keep up
        /// with.
        delays: Vec<Duration>,
        looped: bool,
    },
}

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Decoded::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Decoded::Still(image) => write!(f, "Still({}x{})", image.width(), image.height()),
            Decoded::Animated { frames, looped, .. } => {
                let (w, h) = frames
                    .first()
                    .map(|f| (f.width(), f.height()))
                    .unwrap_or((0, 0));
                write!(
                    f,
                    "Animated({}x{}, {} frames, looped {looped})",
                    w,
                    h,
                    frames.len()
                )
            }
        }
    }
}

impl Decoded {
    /// The size of the first frame, for the probe and for the tests.
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match self {
            Decoded::Bytes(_) => None,
            Decoded::Still(image) => Some((image.width(), image.height())),
            Decoded::Animated { frames, .. } => frames.first().map(|f| (f.width(), f.height())),
        }
    }

    pub fn frame_count(&self) -> usize {
        match self {
            Decoded::Bytes(_) => 0,
            Decoded::Still(_) => 1,
            Decoded::Animated { frames, .. } => frames.len(),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum MediaError {
    #[error("it is larger than the {limit} byte cap")]
    TooLarge { limit: u64 },
    #[error("it is not something this client can show: {0}")]
    Unsupported(String),
    #[error("the download failed: {0}")]
    Network(String),
    #[error("it could not be decoded: {0}")]
    Decode(String),
    /// A signed URL that has expired and could not be refreshed.
    #[error("the link has expired")]
    Expired,
    #[error("the request was dropped before it ran")]
    Cancelled,
}

/// What the user put in `[media]`.
///
/// Read from [`crate::discord::handle::DiscordConfig`] rather than from the
/// UI's own `Config`, because the core has to work with no UI at all: `probe`
/// fetches media and opens players, and it has never drawn a frame.
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// How large the on-disk cache may grow before the oldest files are swept.
    pub cache_max_mib: u64,
    /// The largest attachment worth downloading. Everything else has a cap
    /// fixed in the code; this is the one the user chooses, because it is the
    /// one somebody else decides the size of.
    pub max_attachment_mib: u64,
    /// The program that plays a video, as argv. Never a shell command: a
    /// filename with a space or a semicolon in it is somebody else's filename.
    pub player: Vec<String>,
    /// The program that shows a picture, as argv. Empty means whatever the
    /// desktop opens pictures with: `open` on macOS, `xdg-open` elsewhere.
    pub viewer: Vec<String>,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            cache_max_mib: 512,
            max_attachment_mib: 25,
            // `--` so that a file called `-x` is a file rather than an option.
            player: vec!["mpv".into(), "--".into()],
            viewer: Vec::new(),
        }
    }
}

impl MediaConfig {
    /// The byte cap for one key.
    pub fn cap(&self, kind: MediaKind) -> u64 {
        match kind {
            MediaKind::Small => 2 * 1024 * 1024,
            MediaKind::Embed => 10 * 1024 * 1024,
            MediaKind::Attachment => self.max_attachment_mib * 1024 * 1024,
        }
    }

    pub fn cache_max_bytes(&self) -> u64 {
        self.cache_max_mib * 1024 * 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_of(key: &MediaKey) -> String {
        key.url().expect("the key produced no url").to_string()
    }

    #[test]
    fn every_key_produces_the_url_discord_documents() {
        let cases: Vec<(MediaKey, &str)> = vec![
            (
                MediaKey::Avatar {
                    user: UserId(80351110224678912),
                    hash: "8342729096ea3675442027381ff50dfe".into(),
                    size: 64,
                },
                "https://cdn.discordapp.com/avatars/80351110224678912/\
                 8342729096ea3675442027381ff50dfe.png?size=64",
            ),
            (
                MediaKey::Avatar {
                    user: UserId(1),
                    hash: "a_1269e74af4df7417b13759eae50c83dc".into(),
                    size: 128,
                },
                "https://cdn.discordapp.com/avatars/1/\
                 a_1269e74af4df7417b13759eae50c83dc.gif?size=128",
            ),
            (
                MediaKey::GuildIcon {
                    guild: GuildId(2),
                    hash: "abc".into(),
                    size: 256,
                },
                "https://cdn.discordapp.com/icons/2/abc.png?size=256",
            ),
            (
                MediaKey::Emoji {
                    id: EmojiId(3),
                    animated: false,
                    size: 48,
                },
                "https://cdn.discordapp.com/emojis/3.png?size=64&quality=lossless",
            ),
            (
                MediaKey::Emoji {
                    id: EmojiId(4),
                    animated: true,
                    size: 64,
                },
                "https://cdn.discordapp.com/emojis/4.gif?size=64&quality=lossless",
            ),
            (
                MediaKey::Sticker { id: 5 },
                "https://cdn.discordapp.com/stickers/5.png",
            ),
        ];

        for (key, expected) in cases {
            let expected: String = expected.split_whitespace().collect();
            assert_eq!(url_of(&key), expected, "{key:?}");
        }
    }

    /// The three variants that carry a URL hand it back untouched, signature
    /// and all: only the fetcher may strip anything, and only for the cache.
    #[test]
    fn a_carried_url_is_used_as_it_arrived() {
        let signed = "https://cdn.discordapp.com/attachments/1/2/cat.png?ex=aa&is=bb&hm=cc";
        let key = MediaKey::Attachment {
            message: MessageId(9),
            id: 2,
            url: signed.into(),
        };
        assert_eq!(url_of(&key), signed);
        assert_eq!(
            url_of(&MediaKey::Gif {
                url: "https://media.tenor.com/x.gif".into()
            }),
            "https://media.tenor.com/x.gif"
        );
    }

    /// Nothing that is not a URL reaches the queue, let alone a socket. The
    /// scheme is checked one layer down, where `Http` already owns that rule.
    #[test]
    fn something_that_is_not_a_url_never_becomes_a_request() {
        for raw in ["not a url at all", "", "://", "https://"] {
            let key = MediaKey::EmbedImage { url: raw.into() };
            assert!(
                matches!(key.url(), Err(MediaError::Unsupported(_))),
                "{raw:?} was accepted"
            );
        }
    }

    /// The CDN only honours powers of two, and silently serves the original
    /// otherwise — which for an avatar is a megabyte instead of two kilobytes.
    #[test]
    fn a_size_is_rounded_to_something_the_cdn_honours() {
        assert_eq!(clamp_size(64), 64);
        assert_eq!(clamp_size(48), 64);
        assert_eq!(clamp_size(0), 16);
        assert_eq!(clamp_size(1), 16);
        assert_eq!(clamp_size(9000), 4096);
        assert_eq!(clamp_size(4095), 4096);
    }

    #[test]
    fn the_caps_follow_what_the_user_did_not_choose_the_size_of() {
        let config = MediaConfig::default();
        assert_eq!(config.cap(MediaKind::Small), 2 * 1024 * 1024);
        assert_eq!(config.cap(MediaKind::Embed), 10 * 1024 * 1024);
        assert_eq!(config.cap(MediaKind::Attachment), 25 * 1024 * 1024);

        let bigger = MediaConfig {
            max_attachment_mib: 100,
            ..MediaConfig::default()
        };
        assert_eq!(bigger.cap(MediaKind::Attachment), 100 * 1024 * 1024);
        assert_eq!(
            bigger.cap(MediaKind::Small),
            2 * 1024 * 1024,
            "an avatar cap is not the user's to raise"
        );
    }

    #[test]
    fn only_an_attachment_url_is_worth_refreshing() {
        assert!(MediaKey::Attachment {
            message: MessageId(1),
            id: 2,
            url: "https://x/y".into()
        }
        .is_refreshable());
        assert!(!MediaKey::Sticker { id: 1 }.is_refreshable());
        assert!(!MediaKey::EmbedImage {
            url: "https://x/y".into()
        }
        .is_refreshable());
    }

    /// A refreshed URL is the same picture, so the key it produces must match
    /// the one the UI is still holding.
    #[test]
    fn refreshing_a_url_keeps_the_identity() {
        let before = MediaKey::Attachment {
            message: MessageId(1),
            id: 2,
            url: "https://cdn.discordapp.com/attachments/1/2/cat.png?ex=old".into(),
        };
        let after =
            before.with_url("https://cdn.discordapp.com/attachments/1/2/cat.png?ex=new".into());
        assert_eq!(before.describe(), after.describe());
        assert_ne!(
            before, after,
            "the url is part of the key, as the type says"
        );
    }

    /// Visible work comes out of the queue first, so the ordering has to say so.
    #[test]
    fn visible_outranks_prefetch() {
        assert!(MediaPriority::Visible > MediaPriority::Prefetch);
    }

    /// A `{:?}` on a decoded picture must not print the picture.
    #[test]
    fn debugging_a_picture_does_not_print_its_pixels() {
        let image = Arc::new(image::RgbaImage::new(64, 32));
        let printed = format!("{:?}", Decoded::Still(Arc::clone(&image)));
        assert_eq!(printed, "Still(64x32)");
        assert!(printed.len() < 64, "{printed}");

        let animated = Decoded::Animated {
            frames: vec![image],
            delays: vec![Duration::from_millis(100)],
            looped: true,
        };
        assert_eq!(animated.frame_count(), 1);
        assert_eq!(animated.dimensions(), Some((64, 32)));
        assert!(format!("{animated:?}").len() < 80);
    }

    /// A key must never put a signature in the log.
    #[test]
    fn a_description_never_carries_the_signature() {
        let key = MediaKey::Attachment {
            message: MessageId(1),
            id: 2,
            url: "https://cdn.discordapp.com/attachments/1/2/cat.png?ex=aa&is=bb&hm=SECRET".into(),
        };
        assert!(!key.describe().contains("SECRET"));
    }
}
