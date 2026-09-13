//! Discord ids.
//!
//! A snowflake is a 64-bit integer that JavaScript cannot hold, so Discord
//! sends it as a string everywhere except in a handful of older payloads where
//! it is still a number. Both are accepted on the way in; a string always goes
//! out, because that is what every documented request body asks for.
//!
//! The top 42 bits are a millisecond timestamp, which is why nothing here needs
//! to store a creation time: ids sort by age, and `timestamp()` recovers the
//! moment an object was made. Message ordering in this client is snowflake
//! ordering for exactly that reason.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 2015-01-01T00:00:00Z in milliseconds, the epoch Discord counts from.
const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

macro_rules! snowflake {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(pub u64);

        impl $name {
            pub const fn get(self) -> u64 {
                self.0
            }

            /// When the object was created, from the id itself.
            pub fn timestamp(self) -> jiff::Timestamp {
                timestamp_of(self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        /// Ids are printed bare. They are not secrets and a `{:?}` full of
        /// `UserId(UserId(123))` helps nobody.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{}", stringify!($name), self.0)
            }
        }

        impl From<u64> for $name {
            fn from(v: u64) -> Self {
                Self(v)
            }
        }

        impl std::str::FromStr for $name {
            type Err = std::num::ParseIntError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<u64>().map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                deserialize_u64(d).map(Self)
            }
        }
    };
}

snowflake!(
    /// An account, whether a person, a bot or a webhook.
    UserId
);
snowflake!(
    /// A server. Discord's API still calls them guilds.
    GuildId
);
snowflake!(
    /// A text channel, a category, a thread, a DM or a group DM.
    ChannelId
);
snowflake!(MessageId);
snowflake!(RoleId);
snowflake!(EmojiId);
snowflake!(AttachmentId);
snowflake!(StickerId);
snowflake!(ApplicationId);

/// The moment a snowflake was minted.
pub fn timestamp_of(id: u64) -> jiff::Timestamp {
    let millis = ((id >> 22) as i64).saturating_add(DISCORD_EPOCH_MS);
    jiff::Timestamp::from_millisecond(millis).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

/// Accept a string or a number, and refuse anything else.
///
/// Untagged enums would do this in fewer lines and would lose the error
/// message, which is the only thing that makes a changed payload diagnosable.
fn deserialize_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    struct V;

    impl serde::de::Visitor<'_> for V {
        type Value = u64;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a snowflake, as a decimal string or an integer")
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::custom(format!("negative snowflake {v}")))
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u64, E> {
            v.parse()
                .map_err(|_| E::custom(format!("snowflake {v:?} is not a u64")))
        }
    }

    d.deserialize_any(V)
}

/// A snowflake for a moment in time, for `before`/`after` history bounds.
///
/// Discord's own clients build these rather than searching for a real id.
pub fn snowflake_for(time: jiff::Timestamp) -> u64 {
    let millis = time
        .as_millisecond()
        .saturating_sub(DISCORD_EPOCH_MS)
        .max(0);
    (millis as u64) << 22
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_arrives_as_a_string_or_a_number_and_leaves_as_a_string() {
        let from_string: UserId = serde_json::from_str("\"80351110224678912\"").unwrap();
        let from_number: UserId = serde_json::from_str("80351110224678912").unwrap();
        assert_eq!(from_string, from_number);
        assert_eq!(
            serde_json::to_string(&from_string).unwrap(),
            "\"80351110224678912\"",
            "an id must go back out as a string or Discord rejects the body"
        );
    }

    /// The documented example from Discord's own reference.
    #[test]
    fn the_timestamp_comes_out_of_the_id() {
        let t = UserId(175_928_847_299_117_063).timestamp();
        assert_eq!(t.as_millisecond(), 1_462_015_105_796);
    }

    #[test]
    fn rubbish_is_an_error_rather_than_a_panic() {
        for text in ["\"\"", "\"not a number\"", "null", "-1", "{}", "[]", "true"] {
            assert!(
                serde_json::from_str::<UserId>(text).is_err(),
                "{text} was accepted as an id"
            );
        }
    }

    #[test]
    fn ids_sort_by_age() {
        let old = MessageId(snowflake_for(
            "2020-01-01T00:00:00Z".parse::<jiff::Timestamp>().unwrap(),
        ));
        let new = MessageId(snowflake_for(
            "2026-01-01T00:00:00Z".parse::<jiff::Timestamp>().unwrap(),
        ));
        assert!(old < new);
    }

    proptest::proptest! {
        /// Round-tripping is what makes ordering by id safe: if a parse ever
        /// dropped the low bits, two messages a millisecond apart would sort
        /// arbitrarily and history would interleave.
        #[test]
        fn every_u64_round_trips(v in proptest::prelude::any::<u64>()) {
            let id = MessageId(v);
            let json = serde_json::to_string(&id).unwrap();
            let back: MessageId = serde_json::from_str(&json).unwrap();
            proptest::prop_assert_eq!(id, back);
        }

        #[test]
        fn a_timestamp_is_never_a_panic(v in proptest::prelude::any::<u64>()) {
            let _ = MessageId(v).timestamp();
        }
    }
}
