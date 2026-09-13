//! Bytes on disk, named by what they are rather than by where they came from.
//!
//! Two decisions carry this file.
//!
//! **The name is a hash.** A cached file is `<32 hex characters>.<ext>`, where
//! the hex is the first sixteen bytes of a BLAKE3 of the canonical URL. Not
//! because the URL is secret, but because it is an arbitrary string that
//! arrived over a socket: a filename built out of one can contain a `/`, a
//! `..`, a null byte or four kilobytes of query string, and exactly one of
//! those has to be wrong for the cache to write outside its own directory.
//!
//! **The URL is canonicalised first.** Discord's attachment URLs are signed —
//! `ex` is an expiry, `is` an issue time, `hm` the signature — and the
//! signature is regenerated every few hours for bytes that never change.
//! Hashing the URL as it arrived would store the same picture a dozen times a
//! day and never hit once. Stripping the three parameters makes it one file.
//!
//! The directory is 0700, like everything else under `~/.local/starcord`: it
//! holds every picture the user has looked at, which is a reasonable record of
//! what they have been reading.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use url::Url;

/// The query parameters that make one attachment URL differ from the next for
/// the same bytes.
const SIGNATURE_PARAMS: &[&str] = &["ex", "is", "hm"];

/// Extensions the cache knows how to name, and therefore how to find again.
///
/// A lookup has no `Content-Type` to go on, so it probes these in order rather
/// than listing the directory. Seven `stat` calls beats an `O(files)` scan on
/// every avatar in a guild list.
const EXTENSIONS: &[&str] = &["png", "jpg", "gif", "webp", "mp4", "webm", "bin"];

/// Strip the parts of a URL that change while the bytes do not.
///
/// Everything that is not a signature parameter is kept, including the order of
/// what remains, so two genuinely different URLs cannot collide.
pub fn canonical(url: &Url) -> String {
    let mut canonical = url.clone();
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(name, _)| !SIGNATURE_PARAMS.contains(&name.as_ref()))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();

    if kept.is_empty() {
        canonical.set_query(None);
    } else {
        let mut pairs = canonical.query_pairs_mut();
        pairs.clear();
        for (name, value) in &kept {
            pairs.append_pair(name, value);
        }
        drop(pairs);
    }
    // The fragment never reaches the server and so cannot name different bytes.
    canonical.set_fragment(None);
    canonical.into()
}

/// The stem a URL is stored under: sixteen bytes of BLAKE3 as hex.
///
/// Half a BLAKE3 rather than all of it. This is a cache key, not a signature:
/// 128 bits is already far past the point where two URLs collide by accident,
/// and a 64-character filename in a directory listing is unreadable.
pub fn stem(url: &Url) -> String {
    let hash = blake3::hash(canonical(url).as_bytes());
    hash.to_hex()[..32].to_string()
}

/// The extension to store a response under.
///
/// The `Content-Type` first, because it is what the server says it sent; the
/// URL's own extension second, because a CDN that answers
/// `application/octet-stream` is still serving a PNG; `bin` last, so that
/// something unrecognised is still cached rather than fetched forever.
pub fn extension_for(content_type: Option<&str>, url: &Url) -> &'static str {
    let from_type = content_type
        .map(|t| {
            t.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .and_then(|t| match t.as_str() {
            "image/png" | "image/apng" => Some("png"),
            "image/jpeg" | "image/jpg" => Some("jpg"),
            "image/gif" => Some("gif"),
            "image/webp" => Some("webp"),
            "video/mp4" => Some("mp4"),
            "video/webm" => Some("webm"),
            _ => None,
        });
    if let Some(extension) = from_type {
        return extension;
    }

    let tail = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase());

    match tail.as_deref() {
        Some("png" | "apng") => "png",
        Some("jpg" | "jpeg") => "jpg",
        Some("gif") => "gif",
        Some("webp") => "webp",
        Some("mp4" | "m4v") => "mp4",
        Some("webm") => "webm",
        _ => "bin",
    }
}

/// The media cache directory.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// Open — or rather create — the cache at `dir`, privately.
    pub fn new(dir: PathBuf) -> Self {
        if let Err(e) = crate::paths::own_dir(&dir) {
            // Not fatal. A cache that cannot be written is a client that
            // fetches more than it should, not a client that cannot run.
            tracing::warn!(
                "could not prepare the media cache at {}: {e}",
                dir.display()
            );
        }
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file a URL would be stored at, given an extension.
    pub fn path(&self, url: &Url, extension: &str) -> PathBuf {
        self.dir.join(format!("{}.{extension}", stem(url)))
    }

    /// The cached file for a URL, if there is one.
    ///
    /// Touches the file's modified time on a hit, because that is what the
    /// sweep orders by: a picture looked at every day should outlive one
    /// fetched once a month ago.
    pub fn find(&self, url: &Url) -> Option<PathBuf> {
        let stem = stem(url);
        for extension in EXTENSIONS {
            let path = self.dir.join(format!("{stem}.{extension}"));
            if path.is_file() {
                touch(&path);
                return Some(path);
            }
        }
        None
    }

    /// Write bytes into the cache and return where they went.
    ///
    /// Written to a temporary sibling and renamed, so a client killed
    /// mid-download cannot leave a truncated picture that decodes to garbage
    /// forever after.
    pub fn store(
        &self,
        url: &Url,
        content_type: Option<&str>,
        bytes: &[u8],
    ) -> std::io::Result<PathBuf> {
        let extension = extension_for(content_type, url);
        let path = self.path(url, extension);
        let tmp = path.with_extension(format!("{extension}.{}.part", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Total size of everything cached.
    pub fn size(&self) -> u64 {
        self.entries().iter().map(|e| e.size).sum()
    }

    /// Delete the oldest files until the cache is under `max_bytes`.
    ///
    /// By modified time rather than by a recorded access count, because the
    /// filesystem already keeps one and a sidecar index is a second thing to
    /// get out of step with the directory. Returns how many bytes went.
    pub fn sweep(&self, max_bytes: u64) -> u64 {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.size).sum();
        if total <= max_bytes {
            return 0;
        }

        entries.sort_by_key(|e| e.modified);
        let mut freed = 0;
        for entry in entries {
            if total <= max_bytes {
                break;
            }
            match std::fs::remove_file(&entry.path) {
                Ok(()) => {
                    total = total.saturating_sub(entry.size);
                    freed += entry.size;
                }
                Err(e) => tracing::debug!("could not sweep {}: {e}", entry.path.display()),
            }
        }
        if freed > 0 {
            tracing::debug!("swept {freed} bytes out of the media cache");
        }
        freed
    }

    fn entries(&self) -> Vec<Entry> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let meta = entry.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                Some(Entry {
                    path,
                    size: meta.len(),
                    modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                })
            })
            .collect()
    }
}

struct Entry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

/// Mark a file as used, so the sweep sees it as recent.
///
/// Best effort: a filesystem mounted `noatime` or a read-only cache is not a
/// reason to fail a fetch that has already succeeded.
fn touch(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    /// The reason the whole file exists: the same attachment, signed twice.
    #[test]
    fn a_signature_is_not_part_of_a_pictures_identity() {
        let monday = url("https://cdn.discordapp.com/attachments/1/2/cat.png\
             ?ex=66f0&is=66ef&hm=deadbeef");
        let friday = url("https://cdn.discordapp.com/attachments/1/2/cat.png\
             ?ex=6700&is=66ff&hm=cafebabe");
        assert_eq!(canonical(&monday), canonical(&friday));
        assert_eq!(stem(&monday), stem(&friday));
        assert_eq!(
            canonical(&monday),
            "https://cdn.discordapp.com/attachments/1/2/cat.png"
        );
    }

    /// Everything that is not a signature is kept, or two different pictures
    /// would share a file.
    #[test]
    fn anything_that_is_not_a_signature_survives() {
        let small = url("https://cdn.discordapp.com/avatars/1/abc.png?size=64");
        let large = url("https://cdn.discordapp.com/avatars/1/abc.png?size=256");
        assert_ne!(stem(&small), stem(&large));
        assert!(canonical(&small).contains("size=64"));

        let mixed = url("https://cdn.discordapp.com/attachments/1/2/c.png?size=64&ex=aa&hm=bb");
        assert_eq!(
            canonical(&mixed),
            "https://cdn.discordapp.com/attachments/1/2/c.png?size=64"
        );

        // Two different files in one channel are two entries.
        assert_ne!(
            stem(&url("https://cdn.discordapp.com/attachments/1/2/a.png")),
            stem(&url("https://cdn.discordapp.com/attachments/1/3/a.png"))
        );
    }

    /// A name is a hash and nothing else: a URL cannot steer where the bytes
    /// land.
    #[test]
    fn a_hostile_url_cannot_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("media"));
        let nasty = url("https://cdn.discordapp.com/a/..%2F..%2F..%2Fetc%2Fpasswd?x=%2F%2F");
        let path = cache.path(&nasty, "png");

        assert_eq!(path.parent().unwrap(), cache.dir());
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 32 + 4, "{name}");
        assert!(name.ends_with(".png"));
        assert!(
            name[..32].chars().all(|c| c.is_ascii_hexdigit()),
            "{name} is not a hash"
        );
    }

    #[test]
    fn the_extension_comes_from_the_server_then_from_the_url() {
        let png = url("https://cdn.discordapp.com/attachments/1/2/cat.png?ex=aa");
        assert_eq!(extension_for(Some("image/png"), &png), "png");
        assert_eq!(
            extension_for(Some("image/gif; charset=binary"), &png),
            "gif",
            "the server outranks the file name"
        );
        assert_eq!(
            extension_for(Some("application/octet-stream"), &png),
            "png",
            "an unhelpful content type falls through to the url"
        );
        assert_eq!(extension_for(None, &png), "png");
        assert_eq!(
            extension_for(None, &url("https://media.tenor.com/abc")),
            "bin"
        );
        assert_eq!(
            extension_for(None, &url("https://x.invalid/a/b.JPEG")),
            "jpg"
        );
        assert_eq!(extension_for(Some("video/mp4"), &png), "mp4");
    }

    #[test]
    fn what_is_stored_is_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("media"));
        let fresh = url("https://cdn.discordapp.com/attachments/1/2/cat.png?ex=1&hm=a");
        let stale = url("https://cdn.discordapp.com/attachments/1/2/cat.png?ex=2&hm=b");

        assert!(cache.find(&fresh).is_none());
        let path = cache
            .store(&fresh, Some("image/png"), b"not really a png")
            .unwrap();
        assert_eq!(cache.find(&fresh).as_deref(), Some(path.as_path()));
        assert_eq!(
            cache.find(&stale).as_deref(),
            Some(path.as_path()),
            "a re-signed url must hit the file it already has"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"not really a png");

        // Nothing left behind by the temporary-then-rename write.
        let names: Vec<String> = std::fs::read_dir(cache.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_cache_directory_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("media"));
        let mode = std::fs::metadata(cache.dir()).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "the cache lists every picture the user has looked at"
        );
    }

    #[test]
    fn a_sweep_takes_the_oldest_first_and_then_stops() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("media"));

        // Three files of a thousand bytes, oldest to newest.
        let mut paths = Vec::new();
        for (n, age) in [(0u8, 3000u64), (1, 2000), (2, 1000)] {
            let target = url(&format!(
                "https://cdn.discordapp.com/attachments/1/{n}/x.png"
            ));
            let path = cache
                .store(&target, Some("image/png"), &vec![n; 1000])
                .unwrap();
            let when = SystemTime::now() - std::time::Duration::from_secs(age);
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
            paths.push(path);
        }

        assert_eq!(cache.size(), 3000);
        assert_eq!(cache.sweep(4000), 0, "a cache under the cap is left alone");
        assert_eq!(cache.size(), 3000);

        assert_eq!(cache.sweep(1500), 2000);
        assert!(!paths[0].exists(), "the oldest should have gone first");
        assert!(!paths[1].exists());
        assert!(paths[2].exists(), "the newest should have survived");
        assert_eq!(cache.size(), 1000);
    }

    /// A cache pointed at somewhere unusable answers rather than panics.
    #[test]
    fn a_cache_that_cannot_be_written_still_answers() {
        let cache = Cache::new(PathBuf::from("/proc/starcord-cannot-exist/media"));
        assert!(cache.find(&url("https://x.invalid/a.png")).is_none());
        assert_eq!(cache.size(), 0);
        assert_eq!(cache.sweep(0), 0);
        assert!(cache
            .store(&url("https://x.invalid/a.png"), None, b"x")
            .is_err());
    }
}
