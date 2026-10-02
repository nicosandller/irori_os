//! Live state: the current value of each entity. Changes often.
//! See `docs/specs/entities.md` §5.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kinds::binary_sensor::BinarySensorState;
use crate::kinds::cover::CoverState;
use crate::kinds::event::EventState;
use crate::kinds::fan::FanState;
use crate::kinds::light::LightState;
use crate::kinds::lock::LockState;
use crate::kinds::number::NumberState;
use crate::kinds::select::SelectState;
use crate::kinds::sensor::SensorState;
use crate::kinds::siren::SirenState;
use crate::kinds::switch::SwitchState;
use crate::kinds::text::TextState;
use crate::kinds::valve::ValveState;
use crate::{AttributeKey, Context, EntityId, EntityKind, InvariantError, Timestamp};

/// Free-form extra data from the protocol, e.g. Zigbee link quality. Readable by rules, but
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
    /// The typed value, or `null` if the entity has never reported one ("unknown"). Must be
    /// present even when `null`, so a producer can't mark an entity unknown by forgetting it.
    pub state: Option<State>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(schema_with = "crate::schema::attributes_schema")]
    pub attributes: Attributes,
    /// When `state` or `availability` last changed.
    pub last_changed: Timestamp,
    /// When `state`, `availability`, or `attributes` last changed.
    pub last_updated: Timestamp,
    /// When the protocol last reported anything, even an identical value.
    pub last_reported: Timestamp,
    /// What caused the last change.
    pub context: Context,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntityState {
    entity_id: EntityId,
    availability: Availability,
    // `deserialize_with` turns off serde's "missing Option means None", making the key required.
    #[serde(deserialize_with = "Option::deserialize")]
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
        if let Some(state) = &self.state {
            state
                .validate()
                .map_err(|e| InvariantError(format!("entity `{}`: {e}", self.entity_id)))?;
        }
        // `last_reported` has no order with the others: Irori can change an entity without
        // hearing from it (marking it unavailable after its protocol crashed).
        if self.last_changed > self.last_updated {
            return Err(InvariantError(format!(
                "entity `{}` timestamps must satisfy last_changed <= last_updated",
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
    Number(NumberState),
    Select(SelectState),
    Text(TextState),
    Event(EventState),
    Cover(CoverState),
    Lock(LockState),
    Fan(FanState),
    Valve(ValveState),
    Siren(SirenState),
}

impl State {
    /// Checks the value's own rules. Deserialization runs this; call it yourself when building
    /// a `State` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::Light(light) => light.validate(),
            Self::Sensor(sensor) => sensor.validate(),
            Self::Number(number) => number.validate(),
            Self::Cover(cover) => cover.validate(),
            Self::Fan(fan) => fan.validate(),
            Self::Valve(valve) => valve.validate(),
            Self::Switch(_)
            | Self::BinarySensor(_)
            | Self::Select(_)
            | Self::Text(_)
            | Self::Event(_)
            | Self::Lock(_)
            | Self::Siren(_) => Ok(()),
        }
    }

    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Light(_) => EntityKind::Light,
            Self::Switch(_) => EntityKind::Switch,
            Self::Sensor(_) => EntityKind::Sensor,
            Self::BinarySensor(_) => EntityKind::BinarySensor,
            Self::Number(_) => EntityKind::Number,
            Self::Select(_) => EntityKind::Select,
            Self::Text(_) => EntityKind::Text,
            Self::Event(_) => EntityKind::Event,
            Self::Cover(_) => EntityKind::Cover,
            Self::Lock(_) => EntityKind::Lock,
            Self::Fan(_) => EntityKind::Fan,
            Self::Valve(_) => EntityKind::Valve,
            Self::Siren(_) => EntityKind::Siren,
        }
    }
}
