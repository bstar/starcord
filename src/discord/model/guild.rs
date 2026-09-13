//! Servers.
//!
//! A guild arrives in two shapes and this module's job is that nothing
//! downstream has to know which one it got.
//!
//! The flat shape is the documented one: `{id, name, icon, channels, …}`. The
//! nested one — `{id, properties: {name, icon, …}, channels, threads, members,
//! joined_at}` — is what the web client's own READY carries, and it exists
//! because Discord splits the rarely-changing properties from the lists that
//! churn. Both are in production; neither is going away; the normaliser below
//! is cheaper than picking one and being wrong six months later.

use serde::Deserialize;

use crate::discord::model::channel::Channel;
use crate::discord::snowflake::{EmojiId, GuildId, RoleId, UserId};

#[derive(Debug, Clone, Default)]
pub struct Guild {
    pub id: GuildId,
    pub name: String,
    pub icon: Option<String>,
    pub owner_id: Option<UserId>,
    pub channels: Vec<Channel>,
    /// Active threads, which arrive beside the channels rather than in them.
    pub threads: Vec<Channel>,
    pub roles: Vec<Role>,
    pub emojis: Vec<Emoji>,
    pub member_count: Option<u64>,
    /// An outage, not a departure. The guild keeps its place and its name until
    /// a later GUILD_CREATE fills it back in.
    pub unavailable: bool,
}

impl Guild {
    /// Whether the icon hash names an animated image.
    pub fn icon_is_animated(&self) -> bool {
        self.icon.as_deref().is_some_and(|h| h.starts_with("a_"))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Role {
    pub id: RoleId,
    #[serde(default)]
    pub name: String,
    /// Packed 0xRRGGBB. Zero means "no colour", not black: a member with only
    /// zero-coloured roles is drawn in the ordinary text colour.
    #[serde(default)]
    pub color: u32,
    #[serde(default)]
    pub position: i32,
    /// Whether members with this role get their own section in the member list.
    #[serde(default)]
    pub hoist: bool,
    #[serde(default)]
    pub managed: bool,
    /// A 64-bit bitfield that outgrew a JSON number and is sent as a string.
    #[serde(default)]
    pub permissions: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Emoji {
    #[serde(default)]
    pub id: Option<EmojiId>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub animated: bool,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub roles: Vec<RoleId>,
}

/// The rarely-changing half of the nested shape.
#[derive(Debug, Clone, Default, Deserialize)]
struct Properties {
    #[serde(default)]
    id: Option<GuildId>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    owner_id: Option<UserId>,
}

/// Everything either shape can carry, all of it optional.
#[derive(Debug, Clone, Default, Deserialize)]
struct Wire {
    #[serde(default)]
    id: Option<GuildId>,
    #[serde(default)]
    properties: Option<Properties>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    owner_id: Option<UserId>,
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    threads: Vec<Channel>,
    #[serde(default)]
    roles: Vec<Role>,
    #[serde(default)]
    emojis: Vec<Emoji>,
    #[serde(default)]
    member_count: Option<u64>,
    #[serde(default)]
    unavailable: bool,
}

impl<'de> Deserialize<'de> for Guild {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(d)?;
        let props = wire.properties.unwrap_or_default();

        // The id may be at the top level, inside `properties`, or in both. A
        // guild with neither is not a guild, and is the one thing here worth
        // failing on: everything downstream is keyed by it.
        let id = wire
            .id
            .or(props.id)
            .ok_or_else(|| serde::de::Error::missing_field("id"))?;

        let mut channels = wire.channels;
        // The flat shape puts the guild id on every channel; the nested one
        // does not, and a channel that does not know its guild cannot be
        // ordered or resolved later.
        for channel in channels.iter_mut() {
            channel.guild_id.get_or_insert(id);
        }
        let mut threads = wire.threads;
        for thread in threads.iter_mut() {
            thread.guild_id.get_or_insert(id);
        }

        Ok(Guild {
            id,
            name: wire.name.or(props.name).unwrap_or_default(),
            icon: wire.icon.or(props.icon),
            owner_id: wire.owner_id.or(props.owner_id),
            channels,
            threads,
            roles: wire.roles,
            emojis: wire.emojis,
            member_count: wire.member_count,
            unavailable: wire.unavailable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::ChannelId;

    const FLAT: &str = r#"{
        "id": "100",
        "name": "Flat",
        "icon": "abc",
        "owner_id": "7",
        "channels": [{"id":"200","type":0,"name":"general","guild_id":"100"}]
    }"#;

    const NESTED: &str = r#"{
        "id": "100",
        "properties": {"id":"100","name":"Flat","icon":"abc","owner_id":"7"},
        "channels": [{"id":"200","type":0,"name":"general"}],
        "threads": [],
        "joined_at": "2026-01-01T00:00:00+00:00"
    }"#;

    #[test]
    fn both_shapes_normalise_to_the_same_guild() {
        let flat: Guild = serde_json::from_str(FLAT).unwrap();
        let nested: Guild = serde_json::from_str(NESTED).unwrap();

        assert_eq!(flat.id, nested.id);
        assert_eq!(flat.name, nested.name);
        assert_eq!(flat.icon, nested.icon);
        assert_eq!(flat.owner_id, nested.owner_id);
        assert_eq!(flat.channels.len(), nested.channels.len());
    }

    /// The nested shape omits `guild_id` on its channels. Filling it in here is
    /// the difference between a channel that can be ordered and one that is
    /// stranded.
    #[test]
    fn a_nested_channel_learns_its_guild() {
        let nested: Guild = serde_json::from_str(NESTED).unwrap();
        assert_eq!(nested.channels[0].guild_id, Some(GuildId(100)));
        assert_eq!(nested.channels[0].id, ChannelId(200));
    }

    #[test]
    fn an_unavailable_guild_is_only_an_id() {
        let guild: Guild = serde_json::from_str(r#"{"id":"100","unavailable":true}"#).unwrap();
        assert!(guild.unavailable);
        assert!(guild.name.is_empty());
        assert!(guild.channels.is_empty());
    }

    #[test]
    fn a_guild_without_an_id_is_refused() {
        assert!(serde_json::from_str::<Guild>(r#"{"name":"nameless"}"#).is_err());
    }

    #[test]
    fn the_id_may_live_only_in_the_properties() {
        let guild: Guild =
            serde_json::from_str(r#"{"properties":{"id":"100","name":"x"}}"#).unwrap();
        assert_eq!(guild.id, GuildId(100));
        assert_eq!(guild.name, "x");
    }

    #[test]
    fn permissions_are_a_string_because_they_outgrew_a_number() {
        let role: Role =
            serde_json::from_str(r#"{"id":"1","name":"mod","permissions":"137438953471"}"#)
                .unwrap();
        assert_eq!(role.permissions, "137438953471");
    }
}
