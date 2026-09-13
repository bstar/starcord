//! What the client remembers between runs.
//!
//! Not settings — those are `config.toml`, which a person edits. This is the
//! shape of the last session: which channel was open, which server it was in,
//! what was half-typed into the composer, and where the scroll was left in each
//! channel.
//!
//! **It is written at mode 0600, and the drafts are why.** Everything else here
//! is mildly private — a list of channel ids names the channels somebody reads
//! — but a draft is the user's own unsent words, sitting on disk because they
//! closed the client mid-sentence. On a shared machine the default 0644 makes
//! that readable by every other account on the box. The file is created with
//! the mode rather than chmodded afterwards, so it never exists readable even
//! for an instant, and it is written to a temporary sibling and renamed, so an
//! interrupted write cannot leave a truncated one.
//!
//! A session file that will not parse is not an error worth reporting. It is a
//! cache of conveniences; losing it costs a scroll position.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::discord::snowflake::{ChannelId, GuildId, MessageId};
use crate::paths::Paths;

/// How often a dirty session is written while the client runs.
pub const AUTOSAVE: std::time::Duration = std::time::Duration::from_secs(30);

/// Ids are map keys, and a TOML key is a string. Storing them as strings rather
/// than relying on a serializer to stringify a newtype keeps the file readable
/// and keeps a malformed key from failing the whole parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_channel: Option<ChannelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_guild: Option<GuildId>,
    /// Channel id → the text that was in the composer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub drafts: BTreeMap<String, String>,
    /// Channel id → the message the view was anchored on. Absent means the
    /// bottom, which is where most channels are left.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub anchors: BTreeMap<String, String>,
}

impl Session {
    pub fn draft(&self, channel: ChannelId) -> Option<&str> {
        self.drafts.get(&channel.to_string()).map(String::as_str)
    }

    /// Set or clear a draft. Empty text removes the entry rather than storing
    /// an empty string, so a channel nobody has typed in leaves no trace.
    pub fn set_draft(&mut self, channel: ChannelId, text: &str) -> bool {
        let key = channel.to_string();
        if text.is_empty() {
            return self.drafts.remove(&key).is_some();
        }
        self.drafts.insert(key, text.to_string()).as_deref() != Some(text)
    }

    pub fn anchor(&self, channel: ChannelId) -> Option<MessageId> {
        self.anchors
            .get(&channel.to_string())
            .and_then(|id| id.parse().ok())
            .map(MessageId)
    }

    pub fn set_anchor(&mut self, channel: ChannelId, message: Option<MessageId>) -> bool {
        let key = channel.to_string();
        match message {
            Some(message) => {
                let value = message.to_string();
                self.anchors.insert(key, value.clone()).as_deref() != Some(value.as_str())
            }
            None => self.anchors.remove(&key).is_some(),
        }
    }

    /// Forget everything about channels that are no longer reachable.
    ///
    /// Called after READY: an account that left a server should not carry its
    /// drafts around forever.
    pub fn retain_channels(&mut self, known: impl Fn(ChannelId) -> bool) -> bool {
        let before = self.drafts.len() + self.anchors.len();
        let keep = |key: &String| {
            key.parse::<u64>()
                .map(|id| known(ChannelId(id)))
                .unwrap_or(false)
        };
        self.drafts.retain(|key, _| keep(key));
        self.anchors.retain(|key, _| keep(key));
        if self.last_channel.is_some_and(|c| !known(c)) {
            self.last_channel = None;
            self.last_guild = None;
        }
        before != self.drafts.len() + self.anchors.len()
    }
}

/// The session on disk, and whether it has changed since it was written.
pub struct SessionStore {
    paths: Paths,
    session: Session,
    dirty: bool,
}

impl SessionStore {
    /// Read the session, or start an empty one.
    pub fn load(paths: Paths) -> Self {
        let session = paths
            .session_file()
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| match toml::from_str::<Session>(&text) {
                Ok(session) => Some(session),
                Err(e) => {
                    // A cache of conveniences. Losing it costs a scroll
                    // position, and refusing to start over it would be absurd.
                    tracing::warn!("ignoring an unreadable session file: {e}");
                    None
                }
            })
            .unwrap_or_default();

        Self {
            paths,
            session,
            dirty: false,
        }
    }

    pub fn get(&self) -> &Session {
        &self.session
    }

    /// Change the session. The closure says whether anything actually changed,
    /// so that an autosave does not rewrite an unchanged file every thirty
    /// seconds.
    pub fn update(&mut self, f: impl FnOnce(&mut Session) -> bool) {
        if f(&mut self.session) {
            self.dirty = true;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Write it, if there is anything to write.
    pub fn save_if_dirty(&mut self) {
        if self.dirty {
            self.save();
        }
    }

    pub fn save(&mut self) {
        let Ok(path) = self.paths.session_file() else {
            return;
        };
        let Ok(text) = toml::to_string_pretty(&self.session) else {
            return;
        };
        match write_private(&path, &text) {
            Ok(()) => self.dirty = false,
            // Not fatal and not worth a note in the status line: the session
            // still works, it just will not survive a restart.
            Err(e) => tracing::warn!("could not write the session file: {e}"),
        }
    }
}

/// Write a file only its owner can read.
///
/// The same discipline as `discord::auth`'s credentials file, and for the same
/// reason: created with the mode rather than chmodded afterwards, written to a
/// temporary sibling and renamed so an interrupted write leaves the old one
/// intact, and the mode set again on the destination because a rename does not
/// carry it on every filesystem.
///
/// STAR/KIT has this as `fs::write_private`; it lives here until that
/// dependency is switched on. See `AGENTS.md`.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        crate::paths::own_dir(parent)?;
    }
    let tmp = path.with_extension(format!("toml.{}", std::process::id()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, text)?;

    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Paths` reads the directory out of the environment, which is
    /// process-wide, and `cargo test` runs these in parallel threads of one
    /// process. So every test that points `Paths` at a temporary directory
    /// holds this first; without it two of them race and one reads the other's
    /// directory.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        ENV.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A `Paths` pointing at a temporary directory. Only call it while holding
    /// [`exclusive`].
    fn store(dir: &std::path::Path) -> SessionStore {
        std::env::set_var("STARCORD_SESSION_TEST_DIR", dir);
        SessionStore::load(Paths::new(
            "starcord",
            "STARCORD_SESSION_TEST_DIR",
            "STARCORD_SESSION_TEST_DIR",
        ))
    }

    #[test]
    fn a_session_round_trips_through_the_file() {
        let _env = exclusive();
        let dir = tempfile::tempdir().unwrap();
        let mut session = store(dir.path());

        session.update(|s| {
            s.last_channel = Some(ChannelId(7));
            s.last_guild = Some(GuildId(3));
            true
        });
        session.update(|s| s.set_draft(ChannelId(7), "half a sentence"));
        session.update(|s| s.set_anchor(ChannelId(7), Some(MessageId(500))));
        assert!(session.is_dirty());
        session.save();
        assert!(!session.is_dirty());

        let read = store(dir.path());
        assert_eq!(read.get().last_channel, Some(ChannelId(7)));
        assert_eq!(read.get().last_guild, Some(GuildId(3)));
        assert_eq!(read.get().draft(ChannelId(7)), Some("half a sentence"));
        assert_eq!(read.get().anchor(ChannelId(7)), Some(MessageId(500)));
    }

    /// The reason the mode matters: a draft is the user's own unsent words.
    #[test]
    #[cfg(unix)]
    fn the_session_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let _env = exclusive();
        let dir = tempfile::tempdir().unwrap();
        let mut session = store(dir.path());
        session.update(|s| s.set_draft(ChannelId(1), "something private"));
        session.save();

        let path = dir.path().join("session.toml");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the session file is {mode:o}");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("something private"),
            "the draft was not written at all"
        );
    }

    #[test]
    fn an_unreadable_session_file_is_ignored_rather_than_fatal() {
        let _env = exclusive();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("session.toml"), "this is not [ toml").unwrap();

        let session = store(dir.path());
        assert_eq!(session.get(), &Session::default());
        assert!(!session.is_dirty());
    }

    #[test]
    fn an_empty_draft_leaves_no_trace() {
        let mut session = Session::default();
        assert!(session.set_draft(ChannelId(1), "typing"));
        assert!(!session.set_draft(ChannelId(1), "typing"), "no change");
        assert!(session.set_draft(ChannelId(1), ""));
        assert!(session.drafts.is_empty());
        assert_eq!(session.draft(ChannelId(1)), None);
    }

    #[test]
    fn an_anchor_can_be_set_and_cleared() {
        let mut session = Session::default();
        assert!(session.set_anchor(ChannelId(1), Some(MessageId(9))));
        assert!(!session.set_anchor(ChannelId(1), Some(MessageId(9))));
        assert_eq!(session.anchor(ChannelId(1)), Some(MessageId(9)));
        assert!(session.set_anchor(ChannelId(1), None));
        assert_eq!(session.anchor(ChannelId(1)), None);
    }

    /// An account that left a server should not carry its drafts around.
    #[test]
    fn channels_that_are_gone_are_forgotten() {
        let mut session = Session::default();
        session.set_draft(ChannelId(1), "kept");
        session.set_draft(ChannelId(2), "gone");
        session.set_anchor(ChannelId(2), Some(MessageId(5)));
        session.last_channel = Some(ChannelId(2));
        session.last_guild = Some(GuildId(9));

        assert!(session.retain_channels(|id| id == ChannelId(1)));
        assert_eq!(session.draft(ChannelId(1)), Some("kept"));
        assert_eq!(session.draft(ChannelId(2)), None);
        assert_eq!(session.anchor(ChannelId(2)), None);
        assert_eq!(session.last_channel, None);
        assert_eq!(
            session.last_guild, None,
            "a channel that is gone takes its server with it"
        );
    }

    #[test]
    fn an_unchanged_session_is_not_rewritten() {
        let _env = exclusive();
        let dir = tempfile::tempdir().unwrap();
        let mut session = store(dir.path());
        session.update(|s| s.set_draft(ChannelId(1), "a"));
        session.save();

        session.update(|s| s.set_draft(ChannelId(1), "a"));
        assert!(
            !session.is_dirty(),
            "an autosave would rewrite the file every thirty seconds"
        );
        session.save_if_dirty();
    }
}
