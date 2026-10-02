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
pub mod format;
mod kind;
mod kinds;
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
    ApiScope, AppContribution, AppPlacement, AutomationContribution, Contributions,
    CoreRequirement, ExtensionInfo, ExtensionManifest, HostPath, IotClass, NetworkHost,
    PackagePath, Permissions, ProtocolAction, ProtocolContribution, ReservedContribution,
    RunCommand, SerialPath, Version,
};
pub use floorplan::{
    Floorplan, Level, Opening, OpeningKind, PlacedArea, PlacedDevice, PlanError, Point, Wall,
};
pub use id::{
    AreaId, AttributeKey, ContextId, Description, DeviceId, EntityId, ExtensionId, FloorId,
    IdError, Name, ObjectId, ProtocolId, RuleId, SLUG_MAX_LEN, TokenId, UniqueId, UserId,
};
pub use kind::EntityKind;
pub use kinds::binary_sensor::{BinarySensorCapabilities, BinarySensorClass, BinarySensorState};
pub use kinds::button::{ButtonCapabilities, ButtonClass};
pub use kinds::cover::{
    CoverCapabilities, CoverClass, CoverState, OpenState, SetPosition, SetTilt,
};
pub use kinds::event::{EventCapabilities, EventClass, EventState};
pub use kinds::fan::{
    FanCapabilities, FanDirection, FanOscillate, FanPercentage, FanPresetMode, FanSetDirection,
    FanState, FanTurnOn, percentage_to_speed, speed_to_percentage,
};
pub use kinds::light::{ColorMode, ColorTempRange, LightCapabilities, LightState, LightTurnOn};
pub use kinds::lock::{LockCapabilities, LockCode, LockState, LockStatus};
pub use kinds::number::{NumberCapabilities, NumberMode, NumberSetValue, NumberState};
pub use kinds::select::{SelectCapabilities, SelectOption, SelectState};
pub use kinds::sensor::{
    SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, StateClass,
};
pub use kinds::siren::{SirenCapabilities, SirenState, SirenTurnOn};
pub use kinds::switch::{SwitchCapabilities, SwitchClass, SwitchState};
pub use kinds::text::{TextCapabilities, TextMode, TextSetValue, TextState};
pub use kinds::valve::{ValveCapabilities, ValveClass, ValveState};
pub use kinds::{Typed, ValueShape};
pub use protocol::{
    DeviceDescription, EntityDescription, SecretRequest, Service, ServiceCall, ServiceName,
    StateReport, Unmodeled, Waiting,
};
pub use registry::{Area, Capabilities, Device, Entity, EntityCategory, Floor};
pub use schema::{SchemaDoc, schemas};
pub use settings::{
    DeviceSettings, EntitySettings, ExtensionSettings, Placement, SecretError, Settings,
    SettingsKey,
};
pub use state::{Attributes, Availability, EntityState, State};
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
