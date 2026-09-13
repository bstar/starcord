//! Every endpoint this client calls, as data.
//!
//! A route is three things: a method, a path, and the bucket the rate limiter
//! should count it against. The third is why this is an enum rather than
//! formatted strings at the call sites. Discord's limits are per *route
//! template plus major parameter* — every request to
//! `/channels/{id}/messages` for one channel shares an allowance, and the same
//! template for a different channel has its own — so the bucket key has to be
//! built from the shape of the path, not from the path. A formatted string
//! cannot tell you which part was the id.

use std::borrow::Cow;

use crate::discord::snowflake::ChannelId;

/// The API this client speaks. v9 rather than v10: the user-account payloads
/// this client relies on — the READY shape above all — are v9's, and v10
/// changed them in ways no user client has followed.
pub const API_VERSION: &str = "v9";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The current account. The only way to validate a token, and the first
    /// request of every session.
    Me,
    /// Exchanges a scanned QR ticket for a token. Defined here so the bucket
    /// and the path live with every other route; the remote-auth state machine
    /// that calls it arrives at M5.
    RemoteAuthLogin,
    /// A channel's history. Defined for M2; the major parameter is the channel.
    ChannelMessages(ChannelId),
}

impl Route {
    pub fn method(&self) -> reqwest::Method {
        match self {
            Route::Me | Route::ChannelMessages(_) => reqwest::Method::GET,
            Route::RemoteAuthLogin => reqwest::Method::POST,
        }
    }

    /// The path below `/api/v9`.
    pub fn path(&self) -> Cow<'static, str> {
        match self {
            Route::Me => Cow::Borrowed("/users/@me"),
            Route::RemoteAuthLogin => Cow::Borrowed("/users/@me/remote-auth/login"),
            Route::ChannelMessages(id) => Cow::Owned(format!("/channels/{id}/messages")),
        }
    }

    /// The key this route's allowance is counted under.
    ///
    /// Two requests share a bucket when this string matches. It is the method,
    /// the template, and the major parameter — never the whole path, or every
    /// channel would look like a different route and the limiter would learn
    /// nothing.
    pub fn bucket(&self) -> Cow<'static, str> {
        match self {
            Route::Me => Cow::Borrowed("GET /users/@me"),
            Route::RemoteAuthLogin => Cow::Borrowed("POST /users/@me/remote-auth/login"),
            Route::ChannelMessages(id) => Cow::Owned(format!("GET /channels/{id}/messages")),
        }
    }

    /// Whether the token may be sent. Everything here is Discord's own API, so
    /// everything here may; `download()` is the path that must not, and it does
    /// not go through a `Route` at all.
    pub const fn authenticated(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_major_parameter_separates_two_channels() {
        let a = Route::ChannelMessages(ChannelId(1));
        let b = Route::ChannelMessages(ChannelId(2));
        assert_ne!(
            a.bucket(),
            b.bucket(),
            "two channels must not share one allowance"
        );
        assert_eq!(a.bucket(), Route::ChannelMessages(ChannelId(1)).bucket());
    }

    #[test]
    fn the_bucket_carries_the_method() {
        assert!(Route::Me.bucket().starts_with("GET "));
        assert!(Route::RemoteAuthLogin.bucket().starts_with("POST "));
    }

    #[test]
    fn a_path_has_no_version_prefix_of_its_own() {
        for route in [
            Route::Me,
            Route::RemoteAuthLogin,
            Route::ChannelMessages(ChannelId(1)),
        ] {
            let path = route.path();
            assert!(path.starts_with('/'), "{path}");
            assert!(
                !path.contains("/api/"),
                "{path} would be joined onto the base twice"
            );
        }
    }
}
