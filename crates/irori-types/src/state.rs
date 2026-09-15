//! Live state: the current value of each entity. Changes often.
//! See `docs/specs/entities.md` §5.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AttributeKey, Context, EntityId, EntityKind, InvariantError, Timestamp};

/// Free-form extra data from the integration, e.g. Zigbee link quality. Readable by rules, but
/// not type-checked: anything the core relies on is a typed field instead.
pub type Attributes = BTreeMap<AttributeKey, serde_json::Value>;

/// The current state of one entity.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::entity_state_kind_match)]
pub struct EntityState {
    /// Its kind must match `state.kind`.
    pub entity_id: EntityId,
    /// Whether the device is reachable. When `unavailable`, `state` keeps the last known value.
    pub availability: Availability,
    /// The typed value, or `null` if the entity has never reported one ("unknown").
    pub state: Option<State>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: Attributes,
    /// When `state` or `availability` last changed.
    pub last_changed: Timestamp,
    /// When `state`, `availability`, or `attributes` last changed.
    pub last_updated: Timestamp,
    /// When the integration last reported anything, even an identical value.
    pub last_reported: Timestamp,
    /// What caused the last change.
    pub context: Context,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntityState {
    entity_id: EntityId,
    availability: Availability,
    state: Option<State>,
    #[serde(default)]
    attributes: Attributes,
    last_changed: Timestamp,
    last_updated: Timestamp,
    last_reported: Timestamp,
    context: Context,
}

impl<'de> Deserialize<'de> for EntityState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawEntityState::deserialize(deserializer)?;
        EntityState::try_from(raw).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<RawEntityState> for EntityState {
    type Error = InvariantError;
    fn try_from(raw: RawEntityState) -> Result<Self, InvariantError> {
        let state = EntityState {
            entity_id: raw.entity_id,
            availability: raw.availability,
            state: raw.state,
            attributes: raw.attributes,
            last_changed: raw.last_changed,
            last_updated: raw.last_updated,
            last_reported: raw.last_reported,
            context: raw.context,
        };
        state.validate()?;
        Ok(state)
    }
}

impl EntityState {
    /// Checks the rules that span fields. Deserialization runs this; call it yourself when
    /// building an `EntityState` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if let Some(state) = &self.state
            && state.kind() != self.entity_id.kind()
        {
            return Err(InvariantError(format!(
                "entity `{}` is a {}, but its state is for a {}",
                self.entity_id,
                self.entity_id.kind(),
                state.kind()
            )));
        }
        if let Some(State::Light(LightState {
            brightness: Some(0),
            ..
        })) = &self.state
        {
            return Err(InvariantError(format!(
                "entity `{}` has brightness 0; brightness is 1-255 (use `on: false` for off)",
                self.entity_id
            )));
        }
        if !(self.last_changed <= self.last_updated && self.last_updated <= self.last_reported) {
            return Err(InvariantError(format!(
                "entity `{}` timestamps must satisfy last_changed <= last_updated <= last_reported",
                self.entity_id
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable,
}

/// A typed value, tagged with the entity kind it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum State {
    Light(LightState),
    Switch(SwitchState),
    Sensor(SensorState),
    BinarySensor(BinarySensorState),
}

impl State {
    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Light(_) => EntityKind::Light,
            Self::Switch(_) => EntityKind::Switch,
            Self::Sensor(_) => EntityKind::Sensor,
            Self::BinarySensor(_) => EntityKind::BinarySensor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightState {
    pub on: bool,
    /// 1-255, when the light supports dimming. Kept while off: the level it returns to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 255))]
    pub brightness: Option<u8>,
    /// Which color setting is active, when the light supports more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_mode: Option<ColorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1000, max = 20000))]
    pub color_temp_kelvin: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    ColorTemp,
    Rgb,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchState {
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SensorState {
    pub value: SensorValue,
}

/// A sensor reading: a finite number or text, matching the sensor's `value_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SensorValue {
    Number(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinarySensorState {
    /// `true` means detected/open/wet/etc., depending on the device class.
    pub on: bool,
}
