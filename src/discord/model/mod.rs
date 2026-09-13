//! The wire types.
//!
//! Three rules run through all of them, and they are the difference between a
//! client that survives a Discord deploy and one that stops connecting on a
//! Tuesday afternoon:
//!
//! 1. **Never `deny_unknown_fields`.** Discord adds fields constantly and
//!    announces none of them. An unknown field is not an error; it is Tuesday.
//! 2. **`#[serde(default)]` on everything that is not load-bearing.** Fields
//!    documented as always present are omitted in practice — in partial
//!    objects, in older gateway shapes, and in whatever the current experiment
//!    is doing. A missing `name` should cost a channel its title, not cost the
//!    session its READY.
//! 3. **Both shapes, where there are two.** `read_state` and
//!    `user_guild_settings` arrive either as a bare array or as
//!    `{version, partial, entries}` depending on the capabilities the client
//!    identified with, and a guild arrives either flat or as
//!    `{id, properties, channels, …}`. Both are supported permanently rather
//!    than picked once from a recording, because both are in production today.
//!
//! The property tests mutate real fixture JSON — deleting keys, swapping types
//! — and assert only that the result is an `Ok` or an `Err` and never a panic.

pub mod channel;
pub mod guild;
pub mod presence;
pub mod read_state;
pub mod ready;
pub mod user;

// Re-exported so that callers write `model::Channel` rather than
// `model::channel::Channel`. Several of these have no consumer until a later
// milestone; they are named here anyway, because the alternative is a re-export
// list that grows a line at a time and reads as an accident.
#[allow(unused_imports)]
pub use channel::{Channel, ChannelKind};
#[allow(unused_imports)]
pub use guild::{Guild, Role};
#[allow(unused_imports)]
pub use presence::{ClientStatus, Presence, PresenceStatus};
#[allow(unused_imports)]
pub use read_state::{ChannelOverride, MuteConfig, ReadState, UserGuildSettings};
#[allow(unused_imports)]
pub use ready::{Member, Ready, ReadySupplemental, Relationship, RelationshipKind};
#[allow(unused_imports)]
pub use user::User;

use serde::{Deserialize, Deserializer};

/// A list that may or may not be wrapped in a version envelope.
///
/// `VERSIONED_READ_STATES` in the IDENTIFY capabilities is what decides which
/// one arrives, and it is set here — but a resumed session, an older gateway
/// version or a future capability change can all produce the other, and the
/// cost of accepting both is this enum.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Collection<T> {
    Bare(Vec<T>),
    Versioned {
        #[serde(default)]
        version: u64,
        #[serde(default)]
        partial: bool,
        #[serde(default)]
        entries: Vec<T>,
    },
}

impl<T> Collection<T> {
    pub fn entries(self) -> Vec<T> {
        match self {
            Collection::Bare(v) => v,
            Collection::Versioned { entries, .. } => entries,
        }
    }

    /// Whether this is a delta rather than the whole truth. A partial list
    /// updates what is known; a complete one replaces it.
    pub fn partial(&self) -> bool {
        match self {
            Collection::Bare(_) => false,
            Collection::Versioned { partial, .. } => *partial,
        }
    }

    pub fn version(&self) -> u64 {
        match self {
            Collection::Bare(_) => 0,
            Collection::Versioned { version, .. } => *version,
        }
    }
}

impl<T> Default for Collection<T> {
    fn default() -> Self {
        Collection::Bare(Vec::new())
    }
}

/// An optional id where Discord writes `0` or `"0"` for "none".
///
/// `last_message_id` in a read state is the common case: a channel nobody has
/// posted in carries a zero rather than a null, and treating that as a real
/// snowflake makes every message in the channel look already-read.
pub fn optional_id<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: From<u64> + Deserialize<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    let Some(raw) = raw else { return Ok(None) };
    match raw {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) if s.is_empty() || s == "0" => Ok(None),
        serde_json::Value::Number(n) if n.as_u64() == Some(0) => Ok(None),
        other => T::deserialize(other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::MessageId;

    #[derive(Debug, Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "optional_id")]
        last_message_id: Option<MessageId>,
    }

    #[test]
    fn a_zero_id_means_none() {
        for text in [
            r#"{"last_message_id":0}"#,
            r#"{"last_message_id":"0"}"#,
            r#"{"last_message_id":""}"#,
            r#"{"last_message_id":null}"#,
            r#"{}"#,
        ] {
            let h: Holder = serde_json::from_str(text).unwrap();
            assert_eq!(h.last_message_id, None, "{text}");
        }
        let h: Holder = serde_json::from_str(r#"{"last_message_id":"12"}"#).unwrap();
        assert_eq!(h.last_message_id, Some(MessageId(12)));
    }

    #[test]
    fn a_collection_arrives_bare_or_versioned() {
        let bare: Collection<u64> = serde_json::from_str("[1,2,3]").unwrap();
        assert!(!bare.partial());
        assert_eq!(bare.entries(), vec![1, 2, 3]);

        let wrapped: Collection<u64> =
            serde_json::from_str(r#"{"version":9,"partial":true,"entries":[4]}"#).unwrap();
        assert_eq!(wrapped.version(), 9);
        assert!(wrapped.partial());
        assert_eq!(wrapped.entries(), vec![4]);

        let empty: Collection<u64> =
            serde_json::from_str(r#"{"version":1,"partial":false}"#).unwrap();
        assert!(
            empty.entries().is_empty(),
            "a versioned list may carry no entries"
        );
    }
}
