//! Shared types for Irori: the entity and registry model (`docs/specs/entities.md`), extension
//! manifests (`docs/specs/extensions.md`), the protocol contract (`docs/specs/protocols.md`),
//! and later traces and API messages. Compiles to native and `wasm32`, so the server, CLI, and
//! browser UI validate data with the same code.

/// The version this build reports: the release tag, else an exact tag on `HEAD`, else `0.0.0`
/// (`build.rs`). Defined here so the binary and the core's compatibility check share one source.
pub const VERSION: &str = env!("IRORI_VERSION");

// `id` first: its `string_newtype!` macro is used by later modules.
mod id;

mod api;
mod audit;
mod context;
mod extension;
mod floorplan;
pub mod format;
mod home;
mod kind;
mod kinds;
mod network;
mod num;
mod protocol;
mod registry;
mod release_version;
mod schema;
mod service_fields;
mod settings;
mod state;
mod time;
pub mod units;
mod users;

pub use api::LiveMessage;
pub use audit::{AuditActor, AuditEntry, AuditPage, MOST_ROWS as MOST_AUDIT_ROWS, Named, Outcome};
pub use context::{Context, Origin};
pub use extension::{
    ActionField, ActionPlace, ActionWay, ApiScope, AppContribution, AppPlacement,
    AutomationContribution, Contributions, CoreRequirement, ExtensionInfo, ExtensionManifest,
    HostPath, IotClass, MAX_ACTION_ANSWER_SECONDS, MAX_ACTION_INPUT, MAX_ACTION_WAYS,
    MAX_WAY_FIELDS, MAX_WAY_SETTINGS, NetworkHost, PackagePath, Permissions, ProtocolAction,
    ProtocolContribution, ReservedContribution, RunCommand, SerialPath, Version,
};
pub use floorplan::{
    Floorplan, Glyph, Hinge, Level, Marked, Opening, OpeningKind, PlacedArea, PlacedDevice,
    PlanError, PlanOp, Point, Side, Tint, Wall,
};
pub use home::{HomeSettings, Location, TimeZoneName};
pub use id::{
    AreaId, AttributeKey, ContextId, Description, DeviceId, EntityId, ExtensionId, FloorId,
    IdError, Name, ObjectId, ProtocolId, RuleId, SLUG_MAX_LEN, TokenId, UniqueId, UserId,
};
pub use kind::EntityKind;
pub use kinds::alarm_control_panel::{
    AlarmCapabilities, AlarmState, AlarmStatus, ArmMode, CodeFormat,
};
pub use kinds::binary_sensor::{BinarySensorCapabilities, BinarySensorClass, BinarySensorState};
pub use kinds::button::{ButtonCapabilities, ButtonClass};
pub use kinds::camera::{CameraCapabilities, CameraState, CameraStatus};
pub use kinds::climate::{
    ClimateCapabilities, ClimateFanMode, ClimateHvacMode, ClimatePresetMode, ClimateSetTemperature,
    ClimateState, ClimateSwingMode, HumidityRange, HvacAction, HvacMode, SetHumidity,
};
pub use kinds::cover::{CoverCapabilities, CoverClass, CoverState, SetTilt};
pub use kinds::device_tracker::{TrackerCapabilities, TrackerSource, TrackerState};
pub use kinds::event::{EventCapabilities, EventClass, EventState};
pub use kinds::fan::{
    FanCapabilities, FanDirection, FanOscillate, FanPercentage, FanPresetMode, FanSetDirection,
    FanState, FanTurnOn, percentage_to_speed, speed_to_percentage,
};
pub use kinds::humidifier::{
    HumidifierAction, HumidifierCapabilities, HumidifierClass, HumidifierMode, HumidifierState,
};
pub use kinds::lawn_mower::{LawnMowerCapabilities, LawnMowerState, MowerActivity};
pub use kinds::light::{ColorMode, ColorTempRange, LightCapabilities, LightState, LightTurnOn};
pub use kinds::lock::{LockCapabilities, LockCode, LockState, LockStatus};
pub use kinds::media_player::{
    MediaPlayerCapabilities, MediaPlayerClass, MediaPlayerState, MediaSeek, PlayMedia, Playback,
    VolumeMute, VolumeSet,
};
pub use kinds::moment::{
    CivilDate, ClockTime, DateSetValue, DateState, DatetimeSetValue, DatetimeState,
    MomentCapabilities, TimeSetValue, TimeState,
};
pub use kinds::number::{
    NumberCapabilities, NumberMode, NumberSetValue, NumberState, NumberStep, stepped,
};
pub use kinds::opening::{OpenState, OpeningAbilities, OpeningCommand, OpeningState, SetPosition};
pub use kinds::scene::SceneCapabilities;
pub use kinds::schedule::{ScheduleCapabilities, ScheduleState};
pub use kinds::select::{SelectCapabilities, SelectOption, SelectState};
pub use kinds::sensor::{
    SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, StateClass,
};
pub use kinds::siren::{SirenCapabilities, SirenState, SirenTurnOn};
pub use kinds::switch::{SwitchCapabilities, SwitchClass, SwitchState};
pub use kinds::text::{TextCapabilities, TextMode, TextSetValue, TextState};
pub use kinds::timer;
pub use kinds::timer::{TimerCapabilities, TimerChange, TimerStart, TimerState, TimerStatus};
pub use kinds::update::{UpdateCapabilities, UpdateClass, UpdateState};
pub use kinds::vacuum::{VacuumActivity, VacuumCapabilities, VacuumFanSpeed, VacuumState};
pub use kinds::valve::{ValveCapabilities, ValveClass, ValveState};
pub use kinds::water_heater::{
    WaterHeaterCapabilities, WaterHeaterMode, WaterHeaterOperationMode, WaterHeaterSetTemperature,
    WaterHeaterState,
};
pub use kinds::{Field, Typed, ValueShape, mode_to_turn_on};
pub use network::{
    LinkQuality, MAX_NETWORK_LINKS, MAX_NETWORK_NODES, MAX_NETWORK_READING, MAX_NETWORKS, Network,
    NetworkLink, NetworkMedium, NetworkNode, NetworkRole, access_point_id, host_name,
};
pub use protocol::{
    DeviceDescription, EntityDescription, SecretRequest, Service, ServiceCall, ServiceName,
    StateReport, Unmodeled, Waiting,
};
pub use registry::{Area, Capabilities, Device, Entity, EntityCategory, Floor};
pub use schema::{SchemaDoc, schemas};
pub use service_fields::{FieldShape, ServiceField};
pub use settings::{
    DeviceSettings, EntitySettings, ExtensionSettings, Placement, SecretError, Settings,
    SettingsKey,
};
pub use state::{Attributes, Availability, EntityState, State};
pub use time::{Timestamp, TimestampError};
pub use users::{
    PASSWORD_MAX_LEN, PASSWORD_MIN_LEN, Role, User, check_password, check_users, token_id_from,
    user_id_from,
};

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
