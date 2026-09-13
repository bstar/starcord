//! Accounts.

use serde::{Deserialize, Serialize};

use crate::discord::snowflake::UserId;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    #[serde(default)]
    pub username: String,
    /// The display name of the "new username" system. Discord shows this in
    /// preference to `username` everywhere, and it is `null` for accounts that
    /// have not set one, which is why every name goes through
    /// [`User::display_name`] rather than reading a field.
    #[serde(default)]
    pub global_name: Option<String>,
    /// `"0"` for a migrated account. Kept because legacy accounts, webhooks and
    /// bots still carry a real one and it is the only way to tell two identical
    /// usernames apart.
    #[serde(default)]
    pub discriminator: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub public_flags: u64,
    /// Present only on `/users/@me`, which is how a token is validated.
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub mfa_enabled: Option<bool>,
}

impl User {
    /// What a person should be called on screen.
    pub fn display_name(&self) -> &str {
        match self.global_name.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => &self.username,
        }
    }

    /// The unambiguous form, for logs and for telling two `alex`es apart.
    pub fn tag(&self) -> String {
        if self.discriminator.is_empty() || self.discriminator == "0" {
            self.username.clone()
        } else {
            format!("{}#{}", self.username, self.discriminator)
        }
    }

    /// Whether the avatar hash names an animated image. The `a_` prefix is the
    /// only signal; there is no field for it.
    pub fn avatar_is_animated(&self) -> bool {
        self.avatar.as_deref().is_some_and(|h| h.starts_with("a_"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_migrated_account_shows_its_global_name() {
        let user: User = serde_json::from_str(
            r#"{"id":"1","username":"alex","global_name":"Alex","discriminator":"0"}"#,
        )
        .unwrap();
        assert_eq!(user.display_name(), "Alex");
        assert_eq!(
            user.tag(),
            "alex",
            "a zero discriminator is not part of a name"
        );
    }

    #[test]
    fn a_legacy_account_falls_back_to_its_username() {
        let user: User =
            serde_json::from_str(r#"{"id":"2","username":"alex","discriminator":"1234"}"#).unwrap();
        assert_eq!(user.display_name(), "alex");
        assert_eq!(user.tag(), "alex#1234");
        assert!(user.global_name.is_none());
    }

    #[test]
    fn an_empty_global_name_is_not_a_name() {
        let user: User = serde_json::from_str(
            r#"{"id":"3","username":"alex","global_name":"","discriminator":"0"}"#,
        )
        .unwrap();
        assert_eq!(user.display_name(), "alex");
    }

    #[test]
    fn only_the_id_is_required() {
        let user: User = serde_json::from_str(r#"{"id":"4"}"#).unwrap();
        assert_eq!(user.id, UserId(4));
        assert!(!user.bot);
    }

    #[test]
    fn an_unknown_field_is_tuesday() {
        let user: User = serde_json::from_str(
            r#"{"id":"5","username":"a","primary_guild":{"tag":"XYZ"},"collectibles":null}"#,
        )
        .expect("a field nobody announced must not fail a READY");
        assert_eq!(user.username, "a");
    }

    #[test]
    fn an_animated_avatar_is_recognised_by_its_prefix() {
        let animated: User = serde_json::from_str(r#"{"id":"6","avatar":"a_deadbeef"}"#).unwrap();
        let still: User = serde_json::from_str(r#"{"id":"7","avatar":"deadbeef"}"#).unwrap();
        assert!(animated.avatar_is_animated());
        assert!(!still.avatar_is_animated());
    }
}
