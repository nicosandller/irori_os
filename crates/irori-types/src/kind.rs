use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What an entity is. The kind is also the domain part of its [`crate::EntityId`] and the
/// prefix of its services (`light.turn_on`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    /// On/off, optionally dimmable and colored.
    Light,
    /// On/off, e.g. a smart plug or relay.
    Switch,
    /// A numeric or text reading, e.g. temperature or illuminance.
    Sensor,
    /// A two-state reading, e.g. motion or a door contact.
    BinarySensor,
}

impl EntityKind {
    pub const ALL: [EntityKind; 4] = [Self::Light, Self::Switch, Self::Sensor, Self::BinarySensor];

    /// The domain string used in entity ids and service names.
    pub fn domain(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Switch => "switch",
            Self::Sensor => "sensor",
            Self::BinarySensor => "binary_sensor",
        }
    }

    pub fn from_domain(domain: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.domain() == domain)
    }
}

impl std::fmt::Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.domain())
    }
}
