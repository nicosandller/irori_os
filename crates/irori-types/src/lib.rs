//! Shared types for Irori: the entity and registry model (`docs/specs/entities.md`), extension
//! manifests (`docs/specs/extensions.md`), the protocol contract (`docs/specs/protocols.md`),
//! and later traces and API messages. Compiles to native and `wasm32`, so the server, CLI, and
//! browser UI validate data with the same code.

/// The version this build reports: the release tag, else an exact tag on `HEAD`, else `0.0.0`
/// (`build.rs`). Defined here so the binary and the core's compatibility check share one source.
pub const VERSION: &str = env!("IRORI_VERSION");

// `id` first: its `string_newtype!` macro is used by later modules.
mod id;

mod context;
mod extension;
mod floorplan;
mod kind;
mod num;
mod protocol;
mod registry;
mod release_version;
mod schema;
mod settings;
mod state;
mod time;

pub use context::{Context, Origin};
pub use extension::{
    ApiScope, Contributions, CoreRequirement, ExtensionInfo, ExtensionManifest, HostPath, IotClass,
    NetworkHost, PackagePath, Permissions, ProtocolContribution, ReservedContribution, RunCommand,
    SerialPath, Version,
};
pub use floorplan::{
    Floorplan, Level, Opening, OpeningKind, PlacedArea, PlacedDevice, PlanError, Point, Wall,
};
pub use id::{
    AreaId, AttributeKey, ContextId, Description, DeviceId, EntityId, ExtensionId, FloorId,
    IdError, Name, ObjectId, ProtocolId, RuleId, SLUG_MAX_LEN, TokenId, UniqueId, UserId,
};
pub use kind::EntityKind;
pub use protocol::{
    DeviceDescription, EntityDescription, LightTurnOn, SecretRequest, Service, ServiceCall,
    ServiceName, StateReport, Waiting,
};
pub use registry::{
    Area, BinarySensorCapabilities, BinarySensorClass, Capabilities, ColorTempRange, Device,
    Entity, Floor, LightCapabilities, SensorCapabilities, SensorClass, SensorValueType, StateClass,
    SwitchCapabilities, SwitchClass,
};
pub use schema::{SchemaDoc, schemas};
pub use settings::{
    DeviceSettings, EntitySettings, ExtensionSettings, Placement, SecretError, Settings,
    SettingsKey,
};
pub use state::{
    Attributes, Availability, BinarySensorState, ColorMode, EntityState, LightState, SensorState,
    SensorValue, State, SwitchState,
};
pub use time::{Timestamp, TimestampError};

/// A problem with a value that is well-formed JSON of the right shape but breaks a rule that
/// spans several fields, e.g. an entity whose id says `light` but whose state says `switch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantError(String);

impl InvariantError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for InvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvariantError {}
