//! Where starcord keeps its files.
//!
//! **Temporary.** This is `starkit::paths` with one app's name baked in, and it
//! exists only until that crate lands. The signatures are the ones STAR/KIT
//! will publish, so the swap is deleting this file and changing an import.
//!
//! Everything lives under one directory — `~/.local/starcord` by default —
//! rather than being scattered across the three XDG roots, for the same reason
//! STAR/AMP does it: the whole of a setup can then be backed up, moved between
//! machines, or deleted by moving one folder. `$STARCORD_DIR` overrides the
//! location entirely; `$STARCORD_CONFIG_DIR` moves only the config.
//!
//! The directories are mode 0700 and not by taste. What lives under here is a
//! session token, a list of every guild and channel the account is in, and a
//! log that names them; on a shared machine the default 0755 makes all of that
//! readable by every other account on the box.

#![allow(dead_code)] // some accessors are consumed by later milestones

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The set of directories one application owns.
#[derive(Debug, Clone, Copy)]
pub struct Paths {
    app: &'static str,
    dir_env: &'static str,
    config_dir_env: &'static str,
}

/// starcord's own.
pub const PATHS: Paths = Paths::new("starcord", "STARCORD_DIR", "STARCORD_CONFIG_DIR");

impl Paths {
    pub const fn new(
        app: &'static str,
        dir_env: &'static str,
        config_dir_env: &'static str,
    ) -> Self {
        Self {
            app,
            dir_env,
            config_dir_env,
        }
    }

    pub fn app(&self) -> &'static str {
        self.app
    }

    /// The one directory everything hangs off.
    pub fn base_dir(&self) -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os(self.dir_env) {
            return Ok(PathBuf::from(dir));
        }
        let home = home_dir().context("cannot determine the home directory")?;
        Ok(home.join(".local").join(self.app))
    }

    /// Config lives at the base, unless pointed elsewhere.
    pub fn config_dir(&self) -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os(self.config_dir_env) {
            return Ok(PathBuf::from(dir));
        }
        self.base_dir()
    }

    pub fn config_file(&self) -> Result<PathBuf> {
        Ok(self.config_dir()?.join("config.toml"))
    }

    /// Anything that must not be lost.
    pub fn data_dir(&self) -> Result<PathBuf> {
        self.base_dir()
    }

    /// The session token, when there is no keyring to put it in.
    pub fn credentials_file(&self) -> Result<PathBuf> {
        Ok(self.data_dir()?.join("credentials.toml"))
    }

    /// Last channel, drafts and scroll anchors. Not a secret, but it names
    /// every channel the user reads, so it is written 0600 alongside the
    /// credentials rather than 0644 alongside the config.
    pub fn session_file(&self) -> Result<PathBuf> {
        Ok(self.data_dir()?.join("session.toml"))
    }

    /// Everything here can be deleted without losing anything.
    pub fn cache_dir(&self) -> Result<PathBuf> {
        Ok(self.base_dir()?.join("cache"))
    }

    /// Downloaded attachment, avatar and emoji bytes.
    pub fn media_cache_dir(&self) -> Result<PathBuf> {
        Ok(self.cache_dir()?.join("media"))
    }

    /// The log is cache: losing it costs nothing, and it must not sit next to
    /// the credentials where a backup would sweep it up.
    pub fn log_dir(&self) -> Result<PathBuf> {
        self.cache_dir()
    }

    pub fn themes_dir(&self) -> Result<PathBuf> {
        Ok(self.config_dir()?.join("themes"))
    }

    /// Sockets and pid files, which belong on a tmpfs that the session clears.
    pub fn runtime_dir(&self) -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            return Ok(PathBuf::from(dir).join(self.app));
        }
        self.cache_dir()
    }

    /// Create the directories this application keeps its own files in,
    /// privately. Called once at startup, before anything opens a log.
    pub fn init_private_dirs(&self) {
        for dir in [self.base_dir(), self.config_dir(), self.cache_dir()]
            .into_iter()
            .flatten()
        {
            if let Err(e) = own_dir(&dir) {
                tracing::debug!("could not prepare {}: {e}", dir.display());
            }
        }
    }
}

/// Make a directory readable by nobody else.
///
/// `create_dir_all` takes the umask, which on most systems means 0755. Applied
/// to a directory that already exists as well as to a new one, because an
/// install that predates this runs with the old mode and would otherwise keep
/// it forever. Best-effort by design: a directory that cannot be chmodded —
/// one on a filesystem with no Unix modes, most likely — is not a reason to
/// refuse to start.
pub fn own_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The override has to win over `$HOME`, or a test that sets it writes
    /// into the developer's real configuration.
    #[test]
    fn the_directory_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        temp_env(PATHS.dir_env, dir.path().as_os_str(), || {
            assert_eq!(PATHS.base_dir().unwrap(), dir.path());
            assert_eq!(
                PATHS.credentials_file().unwrap(),
                dir.path().join("credentials.toml")
            );
            assert_eq!(
                PATHS.log_dir().unwrap(),
                dir.path().join("cache"),
                "the log belongs in cache, not next to the credentials"
            );
        });
    }

    #[test]
    fn a_new_directory_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        own_dir(&nested).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&nested).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "created {mode:o}");
        }
    }

    /// `std::env::set_var` is unsafe in edition 2024 and racy in any edition;
    /// these two tests are the only users and neither reads the other's
    /// variable, but they still run on the same process-wide table.
    fn temp_env(key: &str, value: &std::ffi::OsStr, f: impl FnOnce()) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os(key);
        std::env::set_var(key, value);
        f();
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
