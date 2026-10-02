//! What a protocol and the core say to each other. See `docs/specs/protocols.md`.
//!
//! Protocols refer to their devices and entities by `unique_id`, their own permanent handle.
//! The core assigns the user-facing ids (`DeviceId`, `EntityId`), which the user may rename.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::kinds::climate::{
    ClimateFanMode, ClimateHvacMode, ClimatePresetMode, ClimateSetTemperature, ClimateSwingMode,
    SetHumidity,
};
use crate::kinds::cover::{SetPosition, SetTilt};
use crate::kinds::fan::{FanOscillate, FanPercentage, FanPresetMode, FanSetDirection, FanTurnOn};
use crate::kinds::light::LightTurnOn;
use crate::kinds::lock::LockCode;
use crate::kinds::number::NumberSetValue;
use crate::kinds::select::SelectOption;
use crate::kinds::siren::SirenTurnOn;
use crate::kinds::text::TextSetValue;
use crate::{
    Attributes, Capabilities, Context, ContextId, EntityCategory, EntityKind, InvariantError, Name,
    ObjectId, State, UniqueId,
};

/// Something a protocol found that Irori has no entity kind for yet: a device's fan, its
/// infrared blaster. See `docs/specs/protocols.md` §6.7.
///
/// Not an entity: it has no id, no state, and nothing can be asked of it. It's listed on its
/// device (or its extension, without one) so a person can see what's there and isn't supported,
/// rather than a device that looks like it has less than it does. When Irori gains the kind, the
/// protocol describes it as an entity instead, and it drops off this list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unmodeled {
    /// The device it's on, when it's on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_unique_id: Option<UniqueId>,
    /// What the protocol calls this kind of thing, e.g. `fan`, `infrared`. Deliberately not an
    /// entity kind: it's one Irori doesn't have.
    pub platform: ObjectId,
    /// Its name, when the protocol knows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Name>,
    /// Why Irori can't use it, when it's a kind Irori has but this one couldn't be read: its
    /// command needs a template Irori doesn't run, say. Absent for a kind Irori doesn't have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Something a protocol found but can't use yet, because it needs a person first: a device
/// that wants an encryption key, one that has to be paired, an account that has to be signed in
/// to. See `docs/specs/protocols.md` §6.6.
///
/// Not a device in the registry. It has no entities and nothing is known about it beyond what
/// it announced, so putting it there would show a device that can't do anything. It's listed on
/// its extension instead, where the UI can offer to fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Waiting {
    /// The same handle the device will have once it's in the registry, so what a person
    /// provides now is attached to what it's for (ROADMAP D31).
    pub unique_id: UniqueId,
    /// What it calls itself, for recognising it.
    pub name: Name,
    /// What's needed, in a sentence, e.g. "it wants an encryption key".
    pub reason: String,
    /// Where a secret that would unlock it goes, if a secret is what it needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRequest>,
}

/// Where a secret goes: a path inside the extension's table in `secrets.toml`
/// (`docs/specs/config.md`). The extension decides the path, so the UI and the core can take a
/// secret for any extension without knowing what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretRequest {
    /// Table keys from the extension's own table down to the value, e.g.
    /// `["keys", "00:11:22:33:44:55"]`. At least one, and none empty — the same rules
    /// [`crate::ExtensionSettings::set`] enforces when the secret is written.
    pub path: Vec<String>,
    /// What to call the field, e.g. "Encryption key".
    pub label: String,
    /// Where to find it, e.g. "`api: encryption: key:` in the device's YAML".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecretRequest {
    path: Vec<String>,
    label: String,
    #[serde(default)]
    hint: Option<String>,
}

impl<'de> Deserialize<'de> for SecretRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawSecretRequest::deserialize(deserializer)?;
        let request = SecretRequest {
            path: raw.path,
            label: raw.label,
            hint: raw.hint,
        };
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

impl SecretRequest {
    /// Deserialization runs this; call it yourself when building a request in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.path.is_empty() {
            return Err(InvariantError(
                "a secret request needs a path of at least one key".into(),
            ));
        }
        if self.path.iter().any(|key| key.is_empty()) {
            return Err(InvariantError(
                "a secret request's path can't have an empty key in it".into(),
            ));
        }
        Ok(())
    }
}

/// A device as a protocol describes it. The core adds it to the registry, or updates the
/// entry with the same `unique_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceDescription {
    /// The protocol's permanent handle for the device, e.g. its MAC address.
    pub unique_id: UniqueId,
    pub name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hw_version: Option<String>,
    /// An area name the device reports for itself, e.g. ESPHome's `area`. The core uses it only
    /// when the device is new and the user hasn't assigned an area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_area: Option<Name>,
    /// The `unique_id` of the bridge or hub this device is reached through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_device_unique_id: Option<UniqueId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeviceDescription {
    unique_id: UniqueId,
    name: Name,
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    sw_version: Option<String>,
    #[serde(default)]
    hw_version: Option<String>,
    #[serde(default)]
    suggested_area: Option<Name>,
    #[serde(default)]
    via_device_unique_id: Option<UniqueId>,
}

impl<'de> Deserialize<'de> for DeviceDescription {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawDeviceDescription::deserialize(deserializer)?;
        let device = DeviceDescription {
            unique_id: raw.unique_id,
            name: raw.name,
            manufacturer: raw.manufacturer,
            model: raw.model,
            sw_version: raw.sw_version,
            hw_version: raw.hw_version,
            suggested_area: raw.suggested_area,
            via_device_unique_id: raw.via_device_unique_id,
        };
        device.validate().map_err(serde::de::Error::custom)?;
        Ok(device)
    }
}

impl DeviceDescription {
    /// Deserialization runs this; call it yourself when building a description in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.via_device_unique_id.as_ref() == Some(&self.unique_id) {
            return Err(InvariantError(format!(
                "device `{}` can't be reached via itself (via_device_unique_id is its own unique_id)",
                self.unique_id
            )));
        }
        Ok(())
    }
}

/// An entity as a protocol describes it. The core adds it to the registry, or updates the
/// entry with the same `unique_id`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::entity_description_name_or_device)]
pub struct EntityDescription {
    /// The protocol's permanent handle for the entity.
    pub unique_id: UniqueId,
    /// Leave out for a device's main feature (e.g. the relay of a smart plug): the entity then
    /// uses its device's name, and must have `device_unique_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Name>,
    /// The `unique_id` of the device it belongs to. The device must be described first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_unique_id: Option<UniqueId>,
    /// What to call it after the `.` in its entity id, when it's new. Otherwise the core derives
    /// one from the names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_object_id: Option<ObjectId>,
    /// What it can do. `capabilities.kind` is the entity's kind.
    pub capabilities: Capabilities,
    /// Whether it's one of the device's settings or diagnostics rather than something it's for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_category: Option<EntityCategory>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntityDescription {
    unique_id: UniqueId,
    #[serde(default)]
    name: Option<Name>,
    #[serde(default)]
    device_unique_id: Option<UniqueId>,
    #[serde(default)]
    suggested_object_id: Option<ObjectId>,
    capabilities: Capabilities,
    #[serde(default)]
    entity_category: Option<EntityCategory>,
}

impl<'de> Deserialize<'de> for EntityDescription {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawEntityDescription::deserialize(deserializer)?;
        let entity = EntityDescription {
            unique_id: raw.unique_id,
            name: raw.name,
            device_unique_id: raw.device_unique_id,
            suggested_object_id: raw.suggested_object_id,
            capabilities: raw.capabilities,
            entity_category: raw.entity_category,
        };
        entity.validate().map_err(serde::de::Error::custom)?;
        Ok(entity)
    }
}

impl EntityDescription {
    /// Deserialization runs this; call it yourself when building a description in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.name.is_none() && self.device_unique_id.is_none() {
            return Err(InvariantError(format!(
                "entity `{}` has no name, so it must belong to a device (set `device_unique_id`) whose name it uses",
                self.unique_id
            )));
        }
        self.capabilities
            .validate()
            .map_err(|e| InvariantError(format!("entity `{}`: {e}", self.unique_id)))
    }

    pub fn kind(&self) -> EntityKind {
        self.capabilities.kind()
    }
}

/// A new value for one entity, from its protocol. The core adds the timestamps and context,
/// and checks the value against the entity's kind and capabilities.
///
/// Reporting a value doesn't change the entity's availability; that's reported separately.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::state_report_requires_state)]
pub struct StateReport {
    pub unique_id: UniqueId,
    /// The typed value, or `null` if the device doesn't know it (e.g. it just restarted). Must be
    /// present even when `null`.
    pub state: Option<State>,
    /// Replaces all of the entity's attributes. Leave out to clear them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(schema_with = "crate::schema::attributes_schema")]
    pub attributes: Attributes,
    /// The context of the service call that caused this change, when the protocol knows it
    /// (e.g. the device confirmed a command). Otherwise the change is attributed to the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<ContextId>,
    /// The protocol is repeating what it last heard rather than hearing something new, e.g. a
    /// retained MQTT message delivered on (re)subscribing. For an `event`, whose every report is
    /// an occurrence, a replayed one sets the value without counting as something happening.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replayed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStateReport {
    unique_id: UniqueId,
    // `deserialize_with` turns off serde's "missing Option means None", making the key required.
    #[serde(deserialize_with = "Option::deserialize")]
    state: Option<State>,
    #[serde(default)]
    attributes: Attributes,
    #[serde(default)]
    caused_by: Option<ContextId>,
    #[serde(default)]
    replayed: bool,
}

impl<'de> Deserialize<'de> for StateReport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawStateReport::deserialize(deserializer)?;
        let report = StateReport {
            unique_id: raw.unique_id,
            state: raw.state,
            attributes: raw.attributes,
            caused_by: raw.caused_by,
            replayed: raw.replayed,
        };
        report.validate().map_err(serde::de::Error::custom)?;
        Ok(report)
    }
}

impl StateReport {
    /// Whether it reports something happening (an event's press) rather than a value: each one
    /// counts, so none may stand in for another (`EntityKind::counts_every_report`).
    pub fn is_occurrence(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.kind().counts_every_report())
    }

    /// Deserialization runs this; call it yourself when building a report in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match &self.state {
            Some(state) => state
                .validate()
                .map_err(|e| InvariantError(format!("entity `{}`: {e}", self.unique_id))),
            None => Ok(()),
        }
    }
}

/// The core asking a protocol to act on one of its entities.
///
/// By the time a protocol receives a call, the core has checked that the entity exists,
/// belongs to it, is of the service's kind, and supports what's asked (e.g. `brightness` only on
/// a dimmable light).
///
/// There's no `entity_id`: that's the user's name for the entity and may change, while the
/// protocol only ever uses its own `unique_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceCall {
    /// Which entity, in the protocol's own terms.
    pub unique_id: UniqueId,
    pub service: Service,
    /// Why it's being called. Pass its id back as `caused_by` when reporting the result.
    pub context: Context,
}

/// A service and its data. The standard services of every entity kind; each protocol handles
/// the ones for the kinds it provides.
#[derive(Debug, Clone, PartialEq)]
pub enum Service {
    LightTurnOn(LightTurnOn),
    LightTurnOff,
    SwitchTurnOn,
    SwitchTurnOff,
    NumberSetValue(NumberSetValue),
    SelectSelectOption(SelectOption),
    TextSetValue(TextSetValue),
    ButtonPress,
    CoverOpen,
    CoverClose,
    CoverStop,
    CoverSetPosition(SetPosition),
    CoverSetTilt(SetTilt),
    LockLock(LockCode),
    LockUnlock(LockCode),
    LockOpen(LockCode),
    FanTurnOn(FanTurnOn),
    FanTurnOff,
    FanSetPercentage(FanPercentage),
    FanOscillate(FanOscillate),
    FanSetDirection(FanSetDirection),
    FanSetPresetMode(FanPresetMode),
    ValveOpen,
    ValveClose,
    ValveStop,
    ValveSetPosition(SetPosition),
    SirenTurnOn(SirenTurnOn),
    SirenTurnOff,
    ClimateSetHvacMode(ClimateHvacMode),
    ClimateSetTemperature(ClimateSetTemperature),
    ClimateSetHumidity(SetHumidity),
    ClimateSetFanMode(ClimateFanMode),
    ClimateSetSwingMode(ClimateSwingMode),
    ClimateSetPresetMode(ClimatePresetMode),
    ClimateTurnOn,
    ClimateTurnOff,
}

impl Service {
    pub fn name(&self) -> ServiceName {
        match self {
            Self::LightTurnOn(_) => ServiceName::LightTurnOn,
            Self::LightTurnOff => ServiceName::LightTurnOff,
            Self::SwitchTurnOn => ServiceName::SwitchTurnOn,
            Self::SwitchTurnOff => ServiceName::SwitchTurnOff,
            Self::NumberSetValue(_) => ServiceName::NumberSetValue,
            Self::SelectSelectOption(_) => ServiceName::SelectSelectOption,
            Self::TextSetValue(_) => ServiceName::TextSetValue,
            Self::ButtonPress => ServiceName::ButtonPress,
            Self::CoverOpen => ServiceName::CoverOpen,
            Self::CoverClose => ServiceName::CoverClose,
            Self::CoverStop => ServiceName::CoverStop,
            Self::CoverSetPosition(_) => ServiceName::CoverSetPosition,
            Self::CoverSetTilt(_) => ServiceName::CoverSetTilt,
            Self::LockLock(_) => ServiceName::LockLock,
            Self::LockUnlock(_) => ServiceName::LockUnlock,
            Self::LockOpen(_) => ServiceName::LockOpen,
            Self::FanTurnOn(_) => ServiceName::FanTurnOn,
            Self::FanTurnOff => ServiceName::FanTurnOff,
            Self::FanSetPercentage(_) => ServiceName::FanSetPercentage,
            Self::FanOscillate(_) => ServiceName::FanOscillate,
            Self::FanSetDirection(_) => ServiceName::FanSetDirection,
            Self::FanSetPresetMode(_) => ServiceName::FanSetPresetMode,
            Self::ValveOpen => ServiceName::ValveOpen,
            Self::ValveClose => ServiceName::ValveClose,
            Self::ValveStop => ServiceName::ValveStop,
            Self::ValveSetPosition(_) => ServiceName::ValveSetPosition,
            Self::SirenTurnOn(_) => ServiceName::SirenTurnOn,
            Self::SirenTurnOff => ServiceName::SirenTurnOff,
            Self::ClimateSetHvacMode(_) => ServiceName::ClimateSetHvacMode,
            Self::ClimateSetTemperature(_) => ServiceName::ClimateSetTemperature,
            Self::ClimateSetHumidity(_) => ServiceName::ClimateSetHumidity,
            Self::ClimateSetFanMode(_) => ServiceName::ClimateSetFanMode,
            Self::ClimateSetSwingMode(_) => ServiceName::ClimateSetSwingMode,
            Self::ClimateSetPresetMode(_) => ServiceName::ClimateSetPresetMode,
            Self::ClimateTurnOn => ServiceName::ClimateTurnOn,
            Self::ClimateTurnOff => ServiceName::ClimateTurnOff,
        }
    }
}

/// The name of a standard service: `<kind>.<action>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ServiceName {
    #[serde(rename = "light.turn_on")]
    LightTurnOn,
    #[serde(rename = "light.turn_off")]
    LightTurnOff,
    #[serde(rename = "switch.turn_on")]
    SwitchTurnOn,
    #[serde(rename = "switch.turn_off")]
    SwitchTurnOff,
    #[serde(rename = "number.set_value")]
    NumberSetValue,
    #[serde(rename = "select.select_option")]
    SelectSelectOption,
    #[serde(rename = "text.set_value")]
    TextSetValue,
    #[serde(rename = "button.press")]
    ButtonPress,
    #[serde(rename = "cover.open")]
    CoverOpen,
    #[serde(rename = "cover.close")]
    CoverClose,
    #[serde(rename = "cover.stop")]
    CoverStop,
    #[serde(rename = "cover.set_position")]
    CoverSetPosition,
    #[serde(rename = "cover.set_tilt")]
    CoverSetTilt,
    #[serde(rename = "lock.lock")]
    LockLock,
    #[serde(rename = "lock.unlock")]
    LockUnlock,
    #[serde(rename = "lock.open")]
    LockOpen,
    #[serde(rename = "fan.turn_on")]
    FanTurnOn,
    #[serde(rename = "fan.turn_off")]
    FanTurnOff,
    #[serde(rename = "fan.set_percentage")]
    FanSetPercentage,
    #[serde(rename = "fan.oscillate")]
    FanOscillate,
    #[serde(rename = "fan.set_direction")]
    FanSetDirection,
    #[serde(rename = "fan.set_preset_mode")]
    FanSetPresetMode,
    #[serde(rename = "valve.open")]
    ValveOpen,
    #[serde(rename = "valve.close")]
    ValveClose,
    #[serde(rename = "valve.stop")]
    ValveStop,
    #[serde(rename = "valve.set_position")]
    ValveSetPosition,
    #[serde(rename = "siren.turn_on")]
    SirenTurnOn,
    #[serde(rename = "siren.turn_off")]
    SirenTurnOff,
    #[serde(rename = "climate.set_hvac_mode")]
    ClimateSetHvacMode,
    #[serde(rename = "climate.set_temperature")]
    ClimateSetTemperature,
    #[serde(rename = "climate.set_humidity")]
    ClimateSetHumidity,
    #[serde(rename = "climate.set_fan_mode")]
    ClimateSetFanMode,
    #[serde(rename = "climate.set_swing_mode")]
    ClimateSetSwingMode,
    #[serde(rename = "climate.set_preset_mode")]
    ClimateSetPresetMode,
    #[serde(rename = "climate.turn_on")]
    ClimateTurnOn,
    #[serde(rename = "climate.turn_off")]
    ClimateTurnOff,
}

impl ServiceName {
    pub const ALL: &'static [ServiceName] = &[
        Self::LightTurnOn,
        Self::LightTurnOff,
        Self::SwitchTurnOn,
        Self::SwitchTurnOff,
        Self::NumberSetValue,
        Self::SelectSelectOption,
        Self::TextSetValue,
        Self::ButtonPress,
        Self::CoverOpen,
        Self::CoverClose,
        Self::CoverStop,
        Self::CoverSetPosition,
        Self::CoverSetTilt,
        Self::LockLock,
        Self::LockUnlock,
        Self::LockOpen,
        Self::FanTurnOn,
        Self::FanTurnOff,
        Self::FanSetPercentage,
        Self::FanOscillate,
        Self::FanSetDirection,
        Self::FanSetPresetMode,
        Self::ValveOpen,
        Self::ValveClose,
        Self::ValveStop,
        Self::ValveSetPosition,
        Self::SirenTurnOn,
        Self::SirenTurnOff,
        Self::ClimateSetHvacMode,
        Self::ClimateSetTemperature,
        Self::ClimateSetHumidity,
        Self::ClimateSetFanMode,
        Self::ClimateSetSwingMode,
        Self::ClimateSetPresetMode,
        Self::ClimateTurnOn,
        Self::ClimateTurnOff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LightTurnOn => "light.turn_on",
            Self::LightTurnOff => "light.turn_off",
            Self::SwitchTurnOn => "switch.turn_on",
            Self::SwitchTurnOff => "switch.turn_off",
            Self::NumberSetValue => "number.set_value",
            Self::SelectSelectOption => "select.select_option",
            Self::TextSetValue => "text.set_value",
            Self::ButtonPress => "button.press",
            Self::CoverOpen => "cover.open",
            Self::CoverClose => "cover.close",
            Self::CoverStop => "cover.stop",
            Self::CoverSetPosition => "cover.set_position",
            Self::CoverSetTilt => "cover.set_tilt",
            Self::LockLock => "lock.lock",
            Self::LockUnlock => "lock.unlock",
            Self::LockOpen => "lock.open",
            Self::FanTurnOn => "fan.turn_on",
            Self::FanTurnOff => "fan.turn_off",
            Self::FanSetPercentage => "fan.set_percentage",
            Self::FanOscillate => "fan.oscillate",
            Self::FanSetDirection => "fan.set_direction",
            Self::FanSetPresetMode => "fan.set_preset_mode",
            Self::ValveOpen => "valve.open",
            Self::ValveClose => "valve.close",
            Self::ValveStop => "valve.stop",
            Self::ValveSetPosition => "valve.set_position",
            Self::SirenTurnOn => "siren.turn_on",
            Self::SirenTurnOff => "siren.turn_off",
            Self::ClimateSetHvacMode => "climate.set_hvac_mode",
            Self::ClimateSetTemperature => "climate.set_temperature",
            Self::ClimateSetHumidity => "climate.set_humidity",
            Self::ClimateSetFanMode => "climate.set_fan_mode",
            Self::ClimateSetSwingMode => "climate.set_swing_mode",
            Self::ClimateSetPresetMode => "climate.set_preset_mode",
            Self::ClimateTurnOn => "climate.turn_on",
            Self::ClimateTurnOff => "climate.turn_off",
        }
    }

    /// The kind of entity it acts on.
    pub fn kind(self) -> EntityKind {
        match self {
            Self::LightTurnOn | Self::LightTurnOff => EntityKind::Light,
            Self::SwitchTurnOn | Self::SwitchTurnOff => EntityKind::Switch,
            Self::NumberSetValue => EntityKind::Number,
            Self::SelectSelectOption => EntityKind::Select,
            Self::TextSetValue => EntityKind::Text,
            Self::ButtonPress => EntityKind::Button,
            Self::CoverOpen
            | Self::CoverClose
            | Self::CoverStop
            | Self::CoverSetPosition
            | Self::CoverSetTilt => EntityKind::Cover,
            Self::LockLock | Self::LockUnlock | Self::LockOpen => EntityKind::Lock,
            Self::FanTurnOn
            | Self::FanTurnOff
            | Self::FanSetPercentage
            | Self::FanOscillate
            | Self::FanSetDirection
            | Self::FanSetPresetMode => EntityKind::Fan,
            Self::ValveOpen | Self::ValveClose | Self::ValveStop | Self::ValveSetPosition => {
                EntityKind::Valve
            }
            Self::SirenTurnOn | Self::SirenTurnOff => EntityKind::Siren,
            Self::ClimateSetHvacMode
            | Self::ClimateSetTemperature
            | Self::ClimateSetHumidity
            | Self::ClimateSetFanMode
            | Self::ClimateSetSwingMode
            | Self::ClimateSetPresetMode
            | Self::ClimateTurnOn
            | Self::ClimateTurnOff => EntityKind::Climate,
        }
    }
}

impl fmt::Display for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The JSON shape of a [`ServiceCall`]: the service's data sits next to its name.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServiceCall {
    service: ServiceName,
    unique_id: UniqueId,
    // Present-but-null is an error, not "no data": the schema says `data` is an object.
    #[serde(
        default,
        deserialize_with = "some_object",
        skip_serializing_if = "Option::is_none"
    )]
    data: Option<serde_json::Map<String, serde_json::Value>>,
    context: Context,
}

fn some_object<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, D::Error> {
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Object(map) => Ok(Some(map)),
        other => Err(serde::de::Error::custom(format!(
            "`data` must be an object, not {}; leave `data` out when there's nothing to send",
            json_type(&other)
        ))),
    }
}

fn json_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

impl Serialize for ServiceCall {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RawServiceCall {
            service: self.service.name(),
            unique_id: self.unique_id.clone(),
            data: self.service.data(),
            context: self.context.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ServiceCall {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = RawServiceCall::deserialize(deserializer)?;
        let service = Service::from_data(raw.service, raw.data.unwrap_or_default())
            .map_err(D::Error::custom)?;
        let call = ServiceCall {
            unique_id: raw.unique_id,
            service,
            context: raw.context,
        };
        call.validate().map_err(D::Error::custom)?;
        Ok(call)
    }
}

impl ServiceCall {
    /// Deserialization runs this; call it yourself when building a call in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        self.service.validate()
    }
}

impl JsonSchema for ServiceCall {
    fn schema_name() -> Cow<'static, str> {
        "ServiceCall".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let no_data = json_schema!({ "type": "object", "maxProperties": 0 });
        let light_turn_on = generator.subschema_for::<LightTurnOn>();
        let number_set_value = generator.subschema_for::<NumberSetValue>();
        let select_option = generator.subschema_for::<SelectOption>();
        let text_set_value = generator.subschema_for::<TextSetValue>();
        let set_position = generator.subschema_for::<SetPosition>();
        let set_tilt = generator.subschema_for::<SetTilt>();
        let lock_code = generator.subschema_for::<LockCode>();
        let fan_turn_on = generator.subschema_for::<FanTurnOn>();
        let fan_percentage = generator.subschema_for::<FanPercentage>();
        let fan_oscillate = generator.subschema_for::<FanOscillate>();
        let fan_direction = generator.subschema_for::<FanSetDirection>();
        let fan_preset = generator.subschema_for::<FanPresetMode>();
        let siren_turn_on = generator.subschema_for::<SirenTurnOn>();
        let climate_mode = generator.subschema_for::<ClimateHvacMode>();
        let climate_temperature = generator.subschema_for::<ClimateSetTemperature>();
        let set_humidity = generator.subschema_for::<SetHumidity>();
        let climate_fan = generator.subschema_for::<ClimateFanMode>();
        let climate_swing = generator.subschema_for::<ClimateSwingMode>();
        let climate_preset = generator.subschema_for::<ClimatePresetMode>();
        // Per service: the shape of `data`.
        let rules: Vec<_> = ServiceName::ALL
            .iter()
            .map(|name| {
                let data = match name {
                    ServiceName::LightTurnOn => light_turn_on.clone(),
                    ServiceName::NumberSetValue => number_set_value.clone(),
                    ServiceName::SelectSelectOption => select_option.clone(),
                    ServiceName::TextSetValue => text_set_value.clone(),
                    ServiceName::CoverSetPosition | ServiceName::ValveSetPosition => {
                        set_position.clone()
                    }
                    ServiceName::CoverSetTilt => set_tilt.clone(),
                    ServiceName::LockLock | ServiceName::LockUnlock | ServiceName::LockOpen => {
                        lock_code.clone()
                    }
                    ServiceName::FanTurnOn => fan_turn_on.clone(),
                    ServiceName::FanSetPercentage => fan_percentage.clone(),
                    ServiceName::FanOscillate => fan_oscillate.clone(),
                    ServiceName::FanSetDirection => fan_direction.clone(),
                    ServiceName::FanSetPresetMode => fan_preset.clone(),
                    ServiceName::SirenTurnOn => siren_turn_on.clone(),
                    ServiceName::ClimateSetHvacMode => climate_mode.clone(),
                    ServiceName::ClimateSetTemperature => climate_temperature.clone(),
                    ServiceName::ClimateSetHumidity => set_humidity.clone(),
                    ServiceName::ClimateSetFanMode => climate_fan.clone(),
                    ServiceName::ClimateSetSwingMode => climate_swing.clone(),
                    ServiceName::ClimateSetPresetMode => climate_preset.clone(),
                    _ => no_data.clone(),
                };
                json_schema!({
                    "if": {
                        "properties": { "service": { "const": name.as_str() } },
                        "required": ["service"],
                    },
                    "then": if name.requires_data() {
                        json_schema!({ "properties": { "data": data }, "required": ["data"] })
                    } else {
                        json_schema!({ "properties": { "data": data } })
                    },
                })
            })
            .collect();
        json_schema!({
            "type": "object",
            "description": "The core asking a protocol to act on one of its entities.",
            "properties": {
                "service": generator.subschema_for::<ServiceName>(),
                "unique_id": generator.subschema_for::<UniqueId>(),
                "data": { "type": "object", "description": "The service's parameters. Leave out when there are none." },
                "context": generator.subschema_for::<Context>(),
            },
            "required": ["service", "unique_id", "context"],
            "additionalProperties": false,
            "allOf": rules,
        })
    }
}

#[cfg(test)]
mod secret_request_tests {
    use super::*;

    #[test]
    fn an_empty_secret_path_is_refused() {
        let empty = SecretRequest {
            path: vec![],
            label: "Key".into(),
            hint: None,
        };
        assert!(empty.validate().is_err());
        let blank_key = SecretRequest {
            path: vec!["keys".into(), "".into()],
            label: "Key".into(),
            hint: None,
        };
        assert!(blank_key.validate().is_err());
        let ok = SecretRequest {
            path: vec!["keys".into(), "aa:bb".into()],
            label: "Key".into(),
            hint: None,
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn deserializing_an_empty_secret_path_fails() {
        let err = serde_json::from_value::<SecretRequest>(serde_json::json!({
            "path": [],
            "label": "Key"
        }));
        assert!(err.is_err(), "{err:?}");
    }
}
