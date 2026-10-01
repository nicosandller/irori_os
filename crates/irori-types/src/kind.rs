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
    /// A value set within a range, e.g. a timeout or a calibration offset.
    Number,
    /// One choice out of a fixed list, e.g. a sensor's sensitivity or a heater's mode.
    Select,
    /// A piece of text set by a person, e.g. a message for a display.
    Text,
    /// Something to press, e.g. a restart button. It has no value.
    Button,
    /// Something that happens, e.g. a remote's button pressed twice. Every report counts.
    Event,
}

impl EntityKind {
    pub const ALL: &'static [EntityKind] = &[
        Self::Light,
        Self::Switch,
        Self::Sensor,
        Self::BinarySensor,
        Self::Number,
        Self::Select,
        Self::Text,
        Self::Button,
        Self::Event,
    ];

    /// The domain string used in entity ids and service names.
    pub fn domain(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Switch => "switch",
            Self::Sensor => "sensor",
            Self::BinarySensor => "binary_sensor",
            Self::Number => "number",
            Self::Select => "select",
            Self::Text => "text",
            Self::Button => "button",
            Self::Event => "event",
        }
    }

    pub fn from_domain(domain: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.domain() == domain)
    }
}

impl std::fmt::Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.domain())
    }
}
