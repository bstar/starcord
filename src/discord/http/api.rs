//! Typed endpoints.
//!
//! One function per thing the client asks Discord to do, so that the route, the
//! request body and the response type sit together and a caller cannot pair a
//! path with the wrong shape.

use serde::{Deserialize, Serialize};

use crate::discord::model::gif::{GifPage, GifResult, Suggestion, Trending};
use crate::discord::model::{Message, User};
use crate::discord::snowflake::{ChannelId, MessageId};

use super::route::{GifProvider, GifRequest, History, Route, PAGE};
use super::{Http, HttpError};

/// The longest message this client will send.
///
/// Discord's own limit for an account without Nitro. It is checked here, before
/// a request is built, rather than by sending four thousand characters and
/// reading the 400 that comes back: a refused request is still a request, and
/// on a user account a rejected send is a line in somebody's ledger.
pub const MAX_CONTENT: usize = 2000;

/// The account the token belongs to.
///
/// This is how a token is validated: there is no "check this token" endpoint,
/// and a request that comes back 401 is the answer. It is also the first
/// request of every session, which makes it the one that discovers whether the
/// stored credential survived a password change.
pub async fn me(http: &Http) -> Result<User, HttpError> {
    http.request(Route::Me, None::<&()>).await
}

/// A page of a channel's history, newest first, as Discord returns it.
///
/// The order is not reversed here. `MessageStore` inserts by id and does not
/// care what order a page arrives in, and a client that reverses at the edge is
/// a client with two conventions.
pub async fn messages(
    http: &Http,
    channel: ChannelId,
    history: History,
) -> Result<Vec<Message>, HttpError> {
    http.request(
        Route::ChannelMessages {
            channel,
            history,
            limit: PAGE,
        },
        None::<&()>,
    )
    .await
}

/// Who a message is allowed to ping.
///
/// Sent explicitly on every message rather than left to the default, because
/// the default is "whatever the content parses to" and the reply case needs
/// `replied_user` set either way: a reply that pings when the sender chose not
/// to is the single most annoying thing a chat client can do.
#[derive(Debug, Clone, Serialize)]
pub struct AllowedMentions {
    /// The categories the content may ping. All three, because a person typing
    /// `@name` means to ping them; the reply toggle is the separate field.
    pub parse: &'static [&'static str],
    pub replied_user: bool,
}

impl AllowedMentions {
    pub fn new(replied_user: bool) -> Self {
        Self {
            parse: &["users", "roles", "everyone"],
            replied_user,
        }
    }
}

/// Where a reply points, on the way out.
#[derive(Debug, Clone, Serialize)]
pub struct ReplyTo {
    pub message_id: MessageId,
    pub channel_id: ChannelId,
    /// False, always: a reply to a message that has since been deleted should
    /// post as an ordinary message rather than fail.
    pub fail_if_not_exists: bool,
}

/// `POST /channels/{id}/messages`.
#[derive(Debug, Clone, Serialize)]
pub struct CreateMessage<'a> {
    pub content: &'a str,
    /// What ties the echo to the optimistic row already on screen. Discord
    /// returns it on the message, through the gateway as well as in the
    /// response body.
    pub nonce: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_reference: Option<ReplyTo>,
    pub allowed_mentions: AllowedMentions,
    pub tts: bool,
}

/// Post a message.
///
/// Refuses anything over the length limit before a request is built.
pub async fn create_message(
    http: &Http,
    channel: ChannelId,
    body: &CreateMessage<'_>,
) -> Result<Message, HttpError> {
    if body.content.chars().count() > MAX_CONTENT {
        return Err(HttpError::Status {
            status: 400,
            code: 50035,
            message: format!("a message may be at most {MAX_CONTENT} characters"),
        });
    }
    http.request(Route::CreateMessage(channel), Some(body))
        .await
}

#[derive(Debug, Clone, Serialize)]
struct EditMessage<'a> {
    content: &'a str,
}

pub async fn edit_message(
    http: &Http,
    channel: ChannelId,
    message: MessageId,
    content: &str,
) -> Result<Message, HttpError> {
    if content.chars().count() > MAX_CONTENT {
        return Err(HttpError::Status {
            status: 400,
            code: 50035,
            message: format!("a message may be at most {MAX_CONTENT} characters"),
        });
    }
    http.request(
        Route::EditMessage(channel, message),
        Some(&EditMessage { content }),
    )
    .await
}

pub async fn delete_message(
    http: &Http,
    channel: ChannelId,
    message: MessageId,
) -> Result<(), HttpError> {
    http.request(Route::DeleteMessage(channel, message), None::<&()>)
        .await
}

/// The typing indicator.
///
/// Rate-limited by the caller rather than here: see `ops::typing`, which sends
/// at most one every nine seconds per channel. This is the request, not the
/// policy.
pub async fn typing(http: &Http, channel: ChannelId) -> Result<(), HttpError> {
    http.request(Route::Typing(channel), None::<&()>).await
}

/// The ack body, which is `{"token": null}` and not an empty object.
///
/// The token here is not a session token — it is an opaque value Discord's own
/// client echoes back from a previous ack, and null is what a client that does
/// not track them sends. It is spelled out rather than omitted because an ack
/// with no body at all is a 400.
#[derive(Debug, Clone, Serialize)]
struct AckBody {
    token: Option<String>,
}

/// What an ack returns, which is a new token to echo next time. Parsed and
/// discarded: nothing here reads it, and a response shape that changes should
/// not fail the ack.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AckResponse {
    #[serde(default)]
    pub token: Option<String>,
}

/// Mark a message, and everything before it, read.
pub async fn ack(
    http: &Http,
    channel: ChannelId,
    message: MessageId,
) -> Result<AckResponse, HttpError> {
    http.request(Route::Ack(channel, message), Some(&AckBody { token: None }))
        .await
}

/// Exchange a scanned QR ticket for a token.
///
/// Defined and unused: the remote-auth state machine that produces a ticket
/// arrives at M5, and the route belongs with its siblings rather than appearing
/// beside the websocket that happens to need it.
#[derive(Debug, Serialize)]
pub struct RemoteAuthLogin<'a> {
    pub ticket: &'a str,
}

/// The reply, whose `encrypted_token` is RSA-OAEP sealed to the key this client
/// generated for the QR handshake.
#[derive(Debug, Deserialize)]
pub struct RemoteAuthToken {
    pub encrypted_token: String,
}

pub async fn remote_auth_login(http: &Http, ticket: &str) -> Result<RemoteAuthToken, HttpError> {
    http.request(Route::RemoteAuthLogin, Some(&RemoteAuthLogin { ticket }))
        .await
}

/// `POST /attachments/refresh-urls`.
#[derive(Debug, Serialize)]
struct RefreshUrls<'a> {
    attachment_urls: &'a [String],
}

/// One re-signed URL. `original` comes back so a batch can be matched up; this
/// client sends one at a time and still reads it, because a response that
/// answers a different question is worth noticing.
#[derive(Debug, Clone, Deserialize)]
pub struct RefreshedUrl {
    #[serde(default)]
    pub original: String,
    pub refreshed: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RefreshedUrls {
    #[serde(default)]
    pub refreshed_urls: Vec<RefreshedUrl>,
}

/// Re-sign attachment URLs that have expired.
///
/// Discord's attachment links carry an expiry and a signature and stop working
/// after a few hours. The bytes have not moved; only the signature has lapsed.
/// This is the one endpoint that says so, and it is why a 403 or a 404 on an
/// attachment is worth exactly one retry rather than being cached as a failure.
pub async fn refresh_attachment_urls(
    http: &Http,
    urls: &[String],
) -> Result<RefreshedUrls, HttpError> {
    http.request(
        Route::RefreshAttachmentUrls,
        Some(&RefreshUrls {
            attachment_urls: urls,
        }),
    )
    .await
}

/// What everybody is posting today.
pub async fn gifs_trending(http: &Http, provider: &GifProvider) -> Result<GifPage, HttpError> {
    let route = Route::Gifs(GifRequest::Trending);
    let path = route.path_with(provider).into_owned();
    let trending: Trending = http.request_at(route, &path, None::<&()>).await?;
    Ok(GifPage {
        categories: trending.categories().to_vec(),
        results: trending.into_results(),
        suggestions: Vec::new(),
    })
}

/// GIFs matching some text.
pub async fn gifs_search(
    http: &Http,
    provider: &GifProvider,
    query: &str,
) -> Result<GifPage, HttpError> {
    let route = Route::Gifs(GifRequest::Search(query.to_string()));
    let path = route.path_with(provider).into_owned();
    let results: Vec<GifResult> = http.request_at(route, &path, None::<&()>).await?;
    Ok(GifPage {
        results,
        ..Default::default()
    })
}

/// Search *terms* matching some text, for completing what is being typed.
pub async fn gifs_suggest(
    http: &Http,
    provider: &GifProvider,
    prefix: &str,
) -> Result<GifPage, HttpError> {
    let route = Route::Gifs(GifRequest::Suggest(prefix.to_string()));
    let path = route.path_with(provider).into_owned();
    let suggestions: Vec<Suggestion> = http.request_at(route, &path, None::<&()>).await?;
    Ok(GifPage {
        suggestions: suggestions.into_iter().map(Suggestion::into_text).collect(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refresh_asks_by_url_and_answers_by_url() {
        let body = serde_json::to_value(RefreshUrls {
            attachment_urls: &["https://cdn.discordapp.com/attachments/1/2/a.png".to_string()],
        })
        .unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "attachment_urls": ["https://cdn.discordapp.com/attachments/1/2/a.png"]
            })
        );

        // `original` is optional, because a response that omits it is still an
        // answer to a batch of one.
        let answer: RefreshedUrls = serde_json::from_str(
            r#"{"refreshed_urls":[{"refreshed":"https://cdn.discordapp.com/a?ex=1"}]}"#,
        )
        .unwrap();
        assert_eq!(answer.refreshed_urls.len(), 1);
        assert_eq!(answer.refreshed_urls[0].original, "");
        assert!(answer.refreshed_urls[0].refreshed.contains("ex=1"));
    }

    #[test]
    fn the_login_body_is_just_a_ticket() {
        let body = serde_json::to_value(RemoteAuthLogin { ticket: "abc" }).unwrap();
        assert_eq!(body, serde_json::json!({"ticket": "abc"}));
    }

    #[test]
    fn an_ack_sends_a_null_token_rather_than_an_empty_object() {
        let body = serde_json::to_value(AckBody { token: None }).unwrap();
        assert_eq!(body, serde_json::json!({"token": null}));
    }

    #[test]
    fn a_plain_message_carries_its_nonce_and_no_reply() {
        let body = CreateMessage {
            content: "hello",
            nonce: "81237712343".into(),
            message_reference: None,
            allowed_mentions: AllowedMentions::new(false),
            tts: false,
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["content"], "hello");
        assert_eq!(
            value["nonce"], "81237712343",
            "without the nonce the echo cannot find the row it belongs to"
        );
        assert!(
            value.get("message_reference").is_none(),
            "an ordinary message must not carry an empty reference"
        );
        assert_eq!(
            value["allowed_mentions"]["parse"],
            serde_json::json!(["users", "roles", "everyone"])
        );
        assert_eq!(value["allowed_mentions"]["replied_user"], false);
    }

    #[test]
    fn a_reply_says_whether_it_pings() {
        for ping in [true, false] {
            let body = CreateMessage {
                content: "answer",
                nonce: "1".into(),
                message_reference: Some(ReplyTo {
                    message_id: MessageId(9),
                    channel_id: ChannelId(8),
                    fail_if_not_exists: false,
                }),
                allowed_mentions: AllowedMentions::new(ping),
                tts: false,
            };
            let value = serde_json::to_value(&body).unwrap();
            assert_eq!(value["message_reference"]["message_id"], "9");
            assert_eq!(value["message_reference"]["channel_id"], "8");
            assert_eq!(
                value["message_reference"]["fail_if_not_exists"], false,
                "a reply to a deleted message should post, not fail"
            );
            assert_eq!(value["allowed_mentions"]["replied_user"], ping);
        }
    }
}
