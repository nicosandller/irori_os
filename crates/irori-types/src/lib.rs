//! Shared types for Irori: the entity and registry model (`docs/specs/entities.md`), extension
//! manifests (`docs/specs/extensions.md`), the integration contract (`docs/specs/integrations.md`),
//! and later rules, traces, and API messages. Compiles to native and `wasm32`, so the server, CLI, and
//! browser UI validate data with the same code.

// `id` first: its `string_newtype!` macro is used by later modules.
mod id;

mod context;
mod extension;
mod integration;
mod kind;
mod num;
mod registry;
mod schema;
mod settings;
mod state;
mod time;

pub use context::{Context, Origin};
pub use extension::{
    ApiScope, Contributions, CoreRequirement, ExtensionInfo, ExtensionManifest, HostPath,
    IntegrationContribution, IotClass, NetworkHost, PackagePath, Permissions, ReservedContribution,
    RunCommand, SerialPath, Version,
};
pub use id::{
    AreaId, AttributeKey, ContextId, Description, DeviceId, EntityId, ExtensionId, FloorId,
    IdError, IntegrationId, Name, ObjectId, RuleId, SLUG_MAX_LEN, TokenId, UniqueId, UserId,
};
pub use integration::{
    DeviceDescription, EntityDescription, LightTurnOn, Service, ServiceCall, ServiceName,
    StateReport,
};
pub use kind::EntityKind;
pub use registry::{
    Area, BinarySensorCapabilities, BinarySensorClass, Capabilities, ColorTempRange, Device,
    Entity, Floor, LightCapabilities, SensorCapabilities, SensorClass, SensorValueType, StateClass,
    SwitchCapabilities, SwitchClass,
};
pub use schema::{SchemaDoc, schemas};
pub use settings::{DeviceSettings, EntitySettings, Settings, SettingsKey};
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
