//! Shared types for Irori: the entity and registry model (`docs/specs/entities.md`), and later
//! rules, traces, and API messages. Compiles to native and `wasm32`, so the server, CLI, and
//! browser UI validate data with the same code.

mod context;
mod id;
mod int;
mod kind;
mod registry;
mod schema;
mod state;
mod time;

pub use context::{Context, Origin};
pub use id::{
    AreaId, AttributeKey, ContextId, DeviceId, EntityId, FloorId, IdError, IntegrationId, Name,
    RuleId, SLUG_MAX_LEN, TokenId, UniqueId, UserId,
};
pub use kind::EntityKind;
pub use registry::{
    Area, BinarySensorCapabilities, BinarySensorClass, Capabilities, ColorTempRange, Device,
    Entity, Floor, LightCapabilities, SensorCapabilities, SensorClass, SensorValueType, StateClass,
    SwitchCapabilities, SwitchClass,
};
pub use schema::{SchemaDoc, schemas};
pub use state::{
    Attributes, Availability, BinarySensorState, ColorMode, EntityState, LightState, SensorState,
    SensorValue, State, SwitchState,
};
pub use time::{Timestamp, TimestampError};

/// A problem with a value that is well-formed JSON of the right shape but breaks a rule that
/// spans several fields, e.g. an entity whose id says `light` but whose state says `switch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantError(String);

impl std::fmt::Display for InvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvariantError {}

/// Checks a number read from JSON against a field's allowed range, then narrows it. Reading into
/// `i64` first means `300` for a 1-255 field gets a message naming the field and range, instead
/// of serde's generic "expected u8".
fn ranged<T: TryFrom<i64>>(
    field: &str,
    value: i64,
    min: i64,
    max: i64,
) -> Result<T, InvariantError> {
    if (min..=max).contains(&value)
        && let Ok(narrowed) = T::try_from(value)
    {
        return Ok(narrowed);
    }
    Err(InvariantError(format!(
        "{field} {value} is out of range; it must be {min}-{max}"
    )))
}
