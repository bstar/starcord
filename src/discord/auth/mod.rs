//! The session token: holding one, storing one, and not leaking one.
//!
//! There is exactly one secret in this program. It is worth being tedious
//! about, because a Discord user token is not a password — it is the session
//! itself, it does not expire on its own, and it is not protected by the
//! account's two-factor setting. Somebody who reads one out of a log file is
//! logged in.
//!
//! So: [`Token`]'s `Debug` is redacted, which means a `{:?}` on any struct that
//! transitively holds one is safe; the only two places it is ever written are
//! the OS keyring and a mode-0600 file; and it reaches the process through
//! standard input rather than through `argv`, which is world-readable in
//! `/proc` on Linux.

use std::fmt;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;

/// The keyring service name. Alongside [`KEYRING_USER`] it is the whole key.
const KEYRING_SERVICE: &str = "starcord";
const KEYRING_USER: &str = "token";

/// A Discord session token.
///
/// Not `Copy`, not `Display`, and its `Debug` says nothing. Pulling the string
/// out takes [`Token::expose`], which is deliberately an ugly name: every call
/// site is a place to ask whether this is one of the two that should exist.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Take a token from the user.
    ///
    /// Surrounding whitespace is stripped, because the overwhelmingly common
    /// way to obtain one is a copy out of a browser's developer tools and it
    /// arrives with a newline on it, sometimes with the quotes still attached.
    /// Anything with an interior control character or space is refused rather
    /// than sent: it would be rejected anyway, and a header value containing a
    /// newline is a request-splitting bug waiting for a worse day.
    pub fn new(raw: impl AsRef<str>) -> Result<Self> {
        let trimmed = raw.as_ref().trim().trim_matches(['"', '\'']).trim();
        if trimmed.is_empty() {
            anyhow::bail!("the token is empty");
        }
        if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
            anyhow::bail!("the token contains whitespace, so it is not a token");
        }
        if trimmed.len() < 16 {
            anyhow::bail!("the token is too short to be one");
        }
        Ok(Self(trimmed.to_string()))
    }

    /// The header value. Called in two places: the `Authorization` header and
    /// IDENTIFY.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// Where a token came from, or went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStoreKind {
    /// The OS keyring: secret-service on Linux, the Keychain on macOS.
    Keyring,
    /// `credentials.toml`, mode 0600.
    File,
    /// Nowhere. `--no-store` on the command line, or a session that was told
    /// not to persist.
    Memory,
}

impl TokenStoreKind {
    pub const fn describe(self) -> &'static str {
        match self {
            TokenStoreKind::Keyring => "the system keyring",
            TokenStoreKind::File => "credentials.toml",
            TokenStoreKind::Memory => "memory only",
        }
    }
}

/// What the user asked for in `[auth] store`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorePreference {
    /// Keyring if there is one, file if there is not.
    #[default]
    Auto,
    Keyring,
    File,
    /// Ask every time.
    None,
}

/// The mode-0600 file, when there is no keyring.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Credentials {
    #[serde(default)]
    token: String,
}

/// Reading and writing the one secret.
#[derive(Debug, Clone)]
pub struct TokenStore {
    paths: Paths,
    prefer: StorePreference,
}

impl TokenStore {
    pub fn new(paths: Paths, prefer: StorePreference) -> Self {
        Self { paths, prefer }
    }

    /// The stored token, and where it was found.
    ///
    /// Only the configured store is read. Trying both would be friendlier right
    /// up to the moment somebody sets `store = "file"` to get away from a
    /// keyring that is misbehaving and finds it still being used.
    /// [`TokenStore::clear`] is the asymmetric one: it clears everything.
    pub fn load(&self) -> Option<(Token, TokenStoreKind)> {
        match self.prefer {
            StorePreference::None => None,
            StorePreference::Keyring => self.load_keyring().map(|t| (t, TokenStoreKind::Keyring)),
            StorePreference::File => self.load_file().map(|t| (t, TokenStoreKind::File)),
            StorePreference::Auto => self
                .load_keyring()
                .map(|t| (t, TokenStoreKind::Keyring))
                .or_else(|| self.load_file().map(|t| (t, TokenStoreKind::File))),
        }
    }

    fn load_keyring(&self) -> Option<Token> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).ok()?;
        match entry.get_password() {
            Ok(secret) => Token::new(secret).ok(),
            Err(keyring::Error::NoEntry) => None,
            Err(e) => {
                // A locked keyring, a headless session with no secret-service,
                // a Keychain that refused. All of them mean "fall through to
                // the file", none of them means "stop".
                tracing::debug!("the keyring did not answer: {e}");
                None
            }
        }
    }

    fn load_file(&self) -> Option<Token> {
        let path = self.paths.credentials_file().ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let creds: Credentials = toml::from_str(&text).ok()?;
        Token::new(creds.token).ok()
    }

    /// Store a token, and say where it went.
    pub fn save(&self, token: &Token) -> Result<TokenStoreKind> {
        match self.prefer {
            StorePreference::None => Ok(TokenStoreKind::Memory),
            StorePreference::File => {
                self.save_file(token)?;
                Ok(TokenStoreKind::File)
            }
            StorePreference::Keyring => {
                self.save_keyring(token)
                    .context("the keyring was required by configuration and refused")?;
                Ok(TokenStoreKind::Keyring)
            }
            StorePreference::Auto => match self.save_keyring(token) {
                Ok(()) => Ok(TokenStoreKind::Keyring),
                Err(e) => {
                    tracing::info!("no keyring available ({e}); using credentials.toml");
                    self.save_file(token)?;
                    Ok(TokenStoreKind::File)
                }
            },
        }
    }

    fn save_keyring(&self, token: &Token) -> Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?;
        entry.set_password(token.expose())?;
        Ok(())
    }

    /// Write `credentials.toml` at mode 0600.
    ///
    /// Created with the mode rather than chmodded afterwards, so the file never
    /// exists readable even for an instant, and written to a temporary sibling
    /// and renamed so an interrupted write cannot leave a truncated one. The
    /// rename does not carry the mode on every filesystem, so it is set again
    /// on the destination.
    fn save_file(&self, token: &Token) -> Result<()> {
        use std::io::Write as _;

        let path = self.paths.credentials_file()?;
        if let Some(parent) = path.parent() {
            crate::paths::own_dir(parent)?;
        }
        let text = toml::to_string_pretty(&Credentials {
            token: token.expose().to_string(),
        })?;
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
        std::fs::write(&tmp, &text)?;

        std::fs::rename(&tmp, &path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Forget the token everywhere, whatever the preference says.
    ///
    /// Both stores are cleared rather than the configured one, because a logout
    /// that leaves a working credential in the other place is not a logout.
    pub fn clear(&self) {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => tracing::debug!("could not clear the keyring entry: {e}"),
            }
        }
        if let Ok(path) = self.paths.credentials_file() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("could not remove {}: {e}", path.display()),
            }
        }
    }
}

/// Check a token by using it.
///
/// There is no endpoint that says whether a token is valid; `GET /users/@me`
/// either answers with the account or comes back 401, and that is the whole
/// test. Doing it before the gateway connects turns "the socket closed with
/// 4004 for some reason" into "this token was rejected".
pub async fn validate(
    http: &crate::discord::http::Http,
) -> Result<crate::discord::model::User, crate::discord::http::HttpError> {
    crate::discord::http::api::me(http).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the type.
    #[test]
    fn a_token_never_prints_itself() {
        let token = Token::new("mfa.aVeryRealLookingSecret").unwrap();
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");

        // The case that actually bites: a struct that derives Debug and
        // happens to contain one.
        #[derive(Debug)]
        struct Session {
            token: Token,
            user: &'static str,
        }
        let printed = format!(
            "{:?}",
            Session {
                token,
                user: "alex"
            }
        );
        assert!(!printed.contains("aVeryRealLookingSecret"), "{printed}");
        assert!(printed.contains("Token(<redacted>)"));
    }

    #[test]
    fn a_pasted_token_is_cleaned_up() {
        let expected = "mfa.aVeryRealLookingSecret";
        for raw in [
            "mfa.aVeryRealLookingSecret",
            "  mfa.aVeryRealLookingSecret\n",
            "\"mfa.aVeryRealLookingSecret\"",
            "'mfa.aVeryRealLookingSecret'\r\n",
        ] {
            assert_eq!(Token::new(raw).unwrap().expose(), expected, "{raw:?}");
        }
    }

    #[test]
    fn something_that_is_not_a_token_is_refused() {
        for raw in [
            "",
            "   ",
            "short",
            "has a space in the middle of it",
            "carries\na\nnewline\ninto\na\nheader",
        ] {
            assert!(Token::new(raw).is_err(), "{raw:?} was accepted");
        }
    }

    #[test]
    fn the_credentials_file_is_private_and_is_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore {
            paths: test_paths(dir.path()),
            prefer: StorePreference::File,
        };
        let token = Token::new("mfa.aVeryRealLookingSecret").unwrap();
        assert_eq!(store.save(&token).unwrap(), TokenStoreKind::File);

        let path = store.paths.credentials_file().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "credentials.toml is {mode:o}");
        }

        let (back, kind) = store.load().expect("the token was not read back");
        assert_eq!(back, token);
        assert_eq!(kind, TokenStoreKind::File);

        store.clear();
        assert!(!path.exists());
        assert!(store.load().is_none());
    }

    #[test]
    fn no_store_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore {
            paths: test_paths(dir.path()),
            prefer: StorePreference::None,
        };
        let token = Token::new("mfa.aVeryRealLookingSecret").unwrap();
        assert_eq!(store.save(&token).unwrap(), TokenStoreKind::Memory);
        assert!(!store.paths.credentials_file().unwrap().exists());
        assert!(
            store.load().is_none(),
            "a `none` store must never load either"
        );
    }

    #[test]
    fn the_store_preference_round_trips_through_config() {
        for (text, expected) in [
            ("auto", StorePreference::Auto),
            ("keyring", StorePreference::Keyring),
            ("file", StorePreference::File),
            ("none", StorePreference::None),
        ] {
            let parsed: StorePreference = toml::from_str(&format!("v = \"{text}\""))
                .map(|w: Wrapper| w.v)
                .unwrap();
            assert_eq!(parsed, expected);
        }
        #[derive(serde::Deserialize)]
        struct Wrapper {
            v: StorePreference,
        }
    }

    /// A `Paths` whose base is a temporary directory, without touching the
    /// process environment that other tests read.
    fn test_paths(dir: &std::path::Path) -> Paths {
        // `Paths` resolves its base from an environment variable, so the test
        // gives it one of its own rather than sharing `STARCORD_DIR`.
        let key: &'static str = Box::leak(
            format!(
                "STARCORD_TEST_DIR_{}",
                COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
            .into_boxed_str(),
        );
        std::env::set_var(key, dir);
        Paths::new("starcord", key, key)
    }

    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
}
