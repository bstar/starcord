//! Typed endpoints.
//!
//! One function per thing the client asks Discord to do, so that the route, the
//! request body and the response type sit together and a caller cannot pair a
//! path with the wrong shape. M1 needs exactly one of them; the rest arrive
//! with the milestones that use them.

use crate::discord::model::User;

use super::route::Route;
use super::{Http, HttpError};

/// The account the token belongs to.
///
/// This is how a token is validated: there is no "check this token" endpoint,
/// and a request that comes back 401 is the answer. It is also the first
/// request of every session, which makes it the one that discovers whether the
/// stored credential survived a password change.
pub async fn me(http: &Http) -> Result<User, HttpError> {
    http.request(Route::Me, None::<&()>).await
}

/// Exchange a scanned QR ticket for a token.
///
/// Defined and unused: the remote-auth state machine that produces a ticket
/// arrives at M5, and the route belongs with its siblings rather than appearing
/// beside the websocket that happens to need it.
#[derive(Debug, serde::Serialize)]
pub struct RemoteAuthLogin<'a> {
    pub ticket: &'a str,
}

/// The reply, whose `encrypted_token` is RSA-OAEP sealed to the key this client
/// generated for the QR handshake.
#[derive(Debug, serde::Deserialize)]
pub struct RemoteAuthToken {
    pub encrypted_token: String,
}

pub async fn remote_auth_login(http: &Http, ticket: &str) -> Result<RemoteAuthToken, HttpError> {
    http.request(Route::RemoteAuthLogin, Some(&RemoteAuthLogin { ticket }))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_body_is_just_a_ticket() {
        let body = serde_json::to_value(RemoteAuthLogin { ticket: "abc" }).unwrap();
        assert_eq!(body, serde_json::json!({"ticket": "abc"}));
    }
}
