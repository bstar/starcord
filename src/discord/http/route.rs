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
//!
//! The major parameter is the channel, never the message: editing two messages
//! in one channel shares one allowance, and a bucket key that carried the
//! message id would be a fresh, empty allowance for every request and would
//! learn nothing at all.

use std::borrow::Cow;

use crate::discord::snowflake::{ChannelId, MessageId};

/// The API this client speaks. v9 rather than v10: the user-account payloads
/// this client relies on — the READY shape above all — are v9's, and v10
/// changed them in ways no user client has followed.
pub const API_VERSION: &str = "v9";

/// What one history request asks for, and Discord's own maximum.
pub const PAGE: u8 = 50;

/// Which end of a channel's history to read from.
///
/// `Around` is the one that is not like the others: it returns the page
/// centred on a message rather than the page after it, which is what a jump to
/// a search result or to a reply needs, and it is also why a jump leaves
/// `at_latest` false — there is history on both sides of what came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum History {
    /// The newest messages in the channel.
    Latest,
    /// Older than this one.
    Before(MessageId),
    /// Newer than this one.
    After(MessageId),
    /// Centred on this one.
    Around(MessageId),
}

impl History {
    fn query(&self) -> Option<(&'static str, MessageId)> {
        match self {
            History::Latest => None,
            History::Before(id) => Some(("before", *id)),
            History::After(id) => Some(("after", *id)),
            History::Around(id) => Some(("around", *id)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The current account. The only way to validate a token, and the first
    /// request of every session.
    Me,
    /// Exchanges a scanned QR ticket for a token. Defined here so the bucket
    /// and the path live with every other route; the remote-auth state machine
    /// that calls it arrives at M5.
    RemoteAuthLogin,
    /// A channel's history.
    ChannelMessages {
        channel: ChannelId,
        history: History,
        limit: u8,
    },
    /// Post a message.
    CreateMessage(ChannelId),
    EditMessage(ChannelId, MessageId),
    DeleteMessage(ChannelId, MessageId),
    /// The typing indicator, which Discord expects roughly every eight to ten
    /// seconds while somebody is actually typing.
    Typing(ChannelId),
    /// Mark a message, and everything before it, read.
    Ack(ChannelId, MessageId),
}

impl Route {
    pub fn method(&self) -> reqwest::Method {
        match self {
            Route::Me | Route::ChannelMessages { .. } => reqwest::Method::GET,
            Route::RemoteAuthLogin
            | Route::CreateMessage(_)
            | Route::Typing(_)
            | Route::Ack(_, _) => reqwest::Method::POST,
            Route::EditMessage(_, _) => reqwest::Method::PATCH,
            Route::DeleteMessage(_, _) => reqwest::Method::DELETE,
        }
    }

    /// The path below `/api/v9`, query string included.
    pub fn path(&self) -> Cow<'static, str> {
        match self {
            Route::Me => Cow::Borrowed("/users/@me"),
            Route::RemoteAuthLogin => Cow::Borrowed("/users/@me/remote-auth/login"),
            Route::ChannelMessages {
                channel,
                history,
                limit,
            } => Cow::Owned(match history.query() {
                Some((name, id)) => {
                    format!("/channels/{channel}/messages?limit={limit}&{name}={id}")
                }
                None => format!("/channels/{channel}/messages?limit={limit}"),
            }),
            Route::CreateMessage(channel) => Cow::Owned(format!("/channels/{channel}/messages")),
            Route::EditMessage(channel, message) => {
                Cow::Owned(format!("/channels/{channel}/messages/{message}"))
            }
            Route::DeleteMessage(channel, message) => {
                Cow::Owned(format!("/channels/{channel}/messages/{message}"))
            }
            Route::Typing(channel) => Cow::Owned(format!("/channels/{channel}/typing")),
            Route::Ack(channel, message) => {
                Cow::Owned(format!("/channels/{channel}/messages/{message}/ack"))
            }
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
            Route::ChannelMessages { channel, .. } => {
                Cow::Owned(format!("GET /channels/{channel}/messages"))
            }
            Route::CreateMessage(channel) => {
                Cow::Owned(format!("POST /channels/{channel}/messages"))
            }
            Route::EditMessage(channel, _) => {
                Cow::Owned(format!("PATCH /channels/{channel}/messages/:id"))
            }
            Route::DeleteMessage(channel, _) => {
                Cow::Owned(format!("DELETE /channels/{channel}/messages/:id"))
            }
            Route::Typing(channel) => Cow::Owned(format!("POST /channels/{channel}/typing")),
            Route::Ack(channel, _) => {
                Cow::Owned(format!("POST /channels/{channel}/messages/:id/ack"))
            }
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

    fn history(channel: u64) -> Route {
        Route::ChannelMessages {
            channel: ChannelId(channel),
            history: History::Latest,
            limit: PAGE,
        }
    }

    #[test]
    fn the_major_parameter_separates_two_channels() {
        assert_ne!(
            history(1).bucket(),
            history(2).bucket(),
            "two channels must not share one allowance"
        );
        assert_eq!(history(1).bucket(), history(1).bucket());
    }

    /// The other half of the same rule: the *message* is a minor parameter, so
    /// editing two messages in one channel is one allowance. A bucket key with
    /// the message id in it would be a fresh empty allowance every time.
    #[test]
    fn the_message_id_is_not_part_of_the_bucket() {
        let a = Route::EditMessage(ChannelId(1), MessageId(10));
        let b = Route::EditMessage(ChannelId(1), MessageId(11));
        assert_eq!(a.bucket(), b.bucket());
        assert_ne!(a.path(), b.path());

        assert_ne!(
            Route::EditMessage(ChannelId(1), MessageId(10)).bucket(),
            Route::DeleteMessage(ChannelId(1), MessageId(10)).bucket(),
            "the method is part of the bucket"
        );
    }

    /// Nor is the query string: a page of history and the page before it come
    /// out of the same allowance.
    #[test]
    fn paging_does_not_invent_a_new_allowance() {
        let first = history(1);
        let older = Route::ChannelMessages {
            channel: ChannelId(1),
            history: History::Before(MessageId(99)),
            limit: PAGE,
        };
        assert_eq!(first.bucket(), older.bucket());
        assert_ne!(first.path(), older.path());
    }

    #[test]
    fn a_history_request_carries_its_bound_and_its_limit() {
        for (history, expected) in [
            (History::Latest, "/channels/7/messages?limit=50"),
            (
                History::Before(MessageId(9)),
                "/channels/7/messages?limit=50&before=9",
            ),
            (
                History::After(MessageId(9)),
                "/channels/7/messages?limit=50&after=9",
            ),
            (
                History::Around(MessageId(9)),
                "/channels/7/messages?limit=50&around=9",
            ),
        ] {
            let route = Route::ChannelMessages {
                channel: ChannelId(7),
                history,
                limit: PAGE,
            };
            assert_eq!(route.path(), expected);
        }
    }

    #[test]
    fn the_methods_are_the_ones_discord_documents() {
        assert_eq!(Route::CreateMessage(ChannelId(1)).method(), "POST");
        assert_eq!(
            Route::EditMessage(ChannelId(1), MessageId(2)).method(),
            "PATCH"
        );
        assert_eq!(
            Route::DeleteMessage(ChannelId(1), MessageId(2)).method(),
            "DELETE"
        );
        assert_eq!(Route::Typing(ChannelId(1)).method(), "POST");
        assert_eq!(Route::Ack(ChannelId(1), MessageId(2)).method(), "POST");
    }

    #[test]
    fn the_bucket_carries_the_method() {
        assert!(Route::Me.bucket().starts_with("GET "));
        assert!(Route::RemoteAuthLogin.bucket().starts_with("POST "));
        assert!(Route::Typing(ChannelId(1)).bucket().starts_with("POST "));
    }

    #[test]
    fn a_path_has_no_version_prefix_of_its_own() {
        for route in [
            Route::Me,
            Route::RemoteAuthLogin,
            history(1),
            Route::CreateMessage(ChannelId(1)),
            Route::EditMessage(ChannelId(1), MessageId(2)),
            Route::DeleteMessage(ChannelId(1), MessageId(2)),
            Route::Typing(ChannelId(1)),
            Route::Ack(ChannelId(1), MessageId(2)),
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
