//! Where STAR/CORD keeps its files.
//!
//! A name for [`starkit::paths::Paths`], and nothing else. The shared type
//! grew `session_file()` and `media_cache_dir()` in 0.2, which were the two
//! reasons this file used to hold a second copy of the layout rather than a
//! name for the shared one.
//!
//! The name is what is kept. `crate::paths::Paths` is taken by value all
//! through `discord/`, and [`PATHS`] is the one place the three strings that
//! identify this application are written down.
//!
//! The layout itself is STAR/KIT's and is documented there: everything under
//! one directory — `~/.local/starcord` by default — rather than scattered
//! across the three XDG roots, so that the whole of a setup can be backed up,
//! moved between machines, or deleted by moving one folder. The directories
//! are mode 0700, and not by taste: what lives under here is a session token,
//! a list of every guild and channel the account is in, and a log that names
//! them.

pub use starkit::paths::{own_dir, Paths};

/// This application's directories.
///
/// `$STARCORD_DIR` moves everything; `$STARCORD_CONFIG_DIR` moves only the
/// configuration, which is what a dotfile manager wants.
pub const PATHS: Paths = Paths::new("starcord", "STARCORD_DIR", "STARCORD_CONFIG_DIR");

#[cfg(test)]
mod tests {
    use super::*;

    /// STAR/KIT's tests cover the layout. What is worth asserting here is that
    /// this application's own three strings reach it: an environment variable
    /// spelled for another program would send a token somewhere nobody meant.
    #[test]
    fn the_directory_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        temp_env("STARCORD_DIR", dir.path().as_os_str(), || {
            assert_eq!(PATHS.app(), "starcord");
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
            assert_eq!(
                PATHS.media_cache_dir().unwrap(),
                dir.path().join("cache").join("media")
            );
        });
    }

    /// `std::env::set_var` is unsafe in edition 2024 and racy in any edition;
    /// this is its only user, and it still runs on the process-wide table.
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
