//! Shared types for Irori: the entity and registry model (`docs/specs/entities.md`), and later
//! rules, traces, and API messages. Compiles to native and `wasm32`, so the server, CLI, and
//! browser UI validate data with the same code.

mod context;
mod id;
mod kind;
mod num;
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
