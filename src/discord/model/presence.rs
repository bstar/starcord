//! Who is online.
//!
//! Activities are deliberately dropped. This client shows a coloured dot and
//! nothing else: rendering somebody's Rich Presence means fetching application
//! assets, and the point of the panel is telling at a glance whether a DM will
//! be answered.

use serde::{Deserialize, Serialize};

use crate::discord::snowflake::{GuildId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Online,
    Idle,
    Dnd,
    #[default]
    Offline,
    /// Only ever sent, never received: Discord reports an invisible user as
    /// offline to everybody including themselves.
    Invisible,
}

impl PresenceStatus {
    /// What Discord calls it on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            PresenceStatus::Online => "online",
            PresenceStatus::Idle => "idle",
            PresenceStatus::Dnd => "dnd",
            PresenceStatus::Offline => "offline",
            PresenceStatus::Invisible => "invisible",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "online" => PresenceStatus::Online,
            "idle" => PresenceStatus::Idle,
            "dnd" => PresenceStatus::Dnd,
            "invisible" => PresenceStatus::Invisible,
            // Anything else, including the `unknown` Discord occasionally
            // sends, reads as offline. A dot that is wrong in the quiet
            // direction is better than one that promises somebody is there.
            _ => PresenceStatus::Offline,
        }
    }
}

/// Which of somebody's clients is in which state.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientStatus {
    #[serde(default)]
    pub desktop: Option<String>,
    #[serde(default)]
    pub mobile: Option<String>,
    #[serde(default)]
    pub web: Option<String>,
}

impl ClientStatus {
    /// Whether the only client online is a phone, which is worth showing
    /// differently: it usually means a reply is coming slowly.
    pub fn mobile_only(&self) -> bool {
        self.mobile.is_some() && self.desktop.is_none() && self.web.is_none()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Presence {
    /// A partial user; only the id is guaranteed.
    #[serde(default)]
    pub user: PresenceUser,
    /// Absent for friends, present for guild members.
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    #[serde(default, deserialize_with = "lossy_status")]
    pub status: PresenceStatus,
    #[serde(default)]
    pub client_status: ClientStatus,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PresenceUser {
    pub id: UserId,
}

/// A status string this client has not heard of reads as offline rather than
/// failing the whole PRESENCE_UPDATE — and PRESENCE_UPDATE arrives in bulk, so
/// one bad entry would otherwise cost a guild's worth of dots.
fn lossy_status<'de, D: serde::Deserializer<'de>>(d: D) -> Result<PresenceStatus, D::Error> {
    let s = String::deserialize(d)?;
    Ok(PresenceStatus::from_str_lossy(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_round_trips_through_its_wire_name() {
        for status in [
            PresenceStatus::Online,
            PresenceStatus::Idle,
            PresenceStatus::Dnd,
            PresenceStatus::Offline,
            PresenceStatus::Invisible,
        ] {
            assert_eq!(PresenceStatus::from_str_lossy(status.as_str()), status);
        }
    }

    #[test]
    fn an_unknown_status_reads_as_offline() {
        let presence: Presence =
            serde_json::from_str(r#"{"user":{"id":"1"},"status":"streaming-maybe"}"#).unwrap();
        assert_eq!(presence.status, PresenceStatus::Offline);
        assert_eq!(presence.user.id, UserId(1));
    }

    #[test]
    fn activities_are_ignored_rather_than_refused() {
        let presence: Presence = serde_json::from_str(
            r#"{"user":{"id":"1"},"status":"online","activities":[{"name":"a","type":0}],"client_status":{"mobile":"online"}}"#,
        )
        .unwrap();
        assert_eq!(presence.status, PresenceStatus::Online);
        assert!(presence.client_status.mobile_only());
    }
}
