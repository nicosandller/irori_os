//! The registry: what exists in the home and what it can do. Changes rarely.
//! See `docs/specs/entities.md` §4.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kinds::binary_sensor::BinarySensorCapabilities;
use crate::kinds::button::ButtonCapabilities;
use crate::kinds::light::LightCapabilities;
use crate::kinds::number::NumberCapabilities;
use crate::kinds::select::SelectCapabilities;
use crate::kinds::sensor::SensorCapabilities;
use crate::kinds::switch::SwitchCapabilities;
use crate::kinds::text::TextCapabilities;
use crate::{
    AreaId, Description, DeviceId, EntityId, EntityKind, FloorId, InvariantError, Name, ProtocolId,
    UniqueId,
};

/// A level of the home, e.g. the ground floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub id: FloorId,
    pub name: Name,
    /// Ordering from lowest to highest; 0 is the entrance level, negative is below ground.
    #[serde(deserialize_with = "crate::num::level")]
    pub level: i8,
}

/// A room or zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Area {
    pub id: AreaId,
    pub name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor_id: Option<FloorId>,
}

/// A physical or virtual thing a protocol talks to, e.g. a Zigbee motion sensor. A device
/// has one or more entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// The device's one id, everywhere: page addresses, config files, the API, rules. Made once
    /// from the protocol and its permanent handle for the device, never from a name, so it
    /// doesn't change when the device is renamed (ROADMAP D36).
    pub id: DeviceId,
    /// The protocol that provides this device.
    pub protocol: ProtocolId,
    /// The protocol's stable id for the device, e.g. the Zigbee IEEE address.
    pub unique_id: UniqueId,
    /// Its one name. Starts as whatever its protocol reports, and once a person names it,
    /// that name is the only one — there is no second name kept alongside (D36).
    pub name: Name,
    /// What it's for, in a person's words. Only ever set by a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Description>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hw_version: Option<String>,
    /// The resolved area for this device: a person's placement when they chose a room (or
    /// deliberately none), otherwise the room matching `suggested_area` while placement is still
    /// unsaid. Absent means it isn't in a room — either on purpose, or because no matching room
    /// exists yet (`docs/specs/config.md` §5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_id: Option<AreaId>,
    /// The area the device says it's in, e.g. ESPHome's `area:`. Only a hint: it names an area
    /// rather than pointing at one, and it is used only while placement is still unsaid
    /// (`docs/specs/config.md` §5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_area: Option<Name>,
    /// The device this one is reached through, e.g. a Zigbee coordinator or a bridge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_device_id: Option<DeviceId>,
}

/// The registry entry for an entity: its identity and what it can do. Its current value lives
/// in [`crate::EntityState`].
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::entity_kind_match)]
pub struct Entity {
    /// `<kind>.<object_id>`. Its kind must match `capabilities.kind`.
    pub id: EntityId,
    pub protocol: ProtocolId,
    /// The protocol's stable id for this entity. Survives renames of `id`.
    pub unique_id: UniqueId,
    pub name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    /// Overrides the device's area when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_id: Option<AreaId>,
    pub capabilities: Capabilities,
    /// Whether it's one of the device's settings or diagnostics rather than something it's for.
    /// Pages list these after the device's main entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_category: Option<EntityCategory>,
}

/// What an entity is to its device, when it isn't what the device is for: a light's power-on
/// behaviour is a setting, its signal strength a diagnostic. Home Assistant's names.
/// Ordered as pages list them: settings before diagnostics, and both after an entity with none.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntityCategory {
    /// Changes how the device behaves, e.g. a motion sensor's timeout.
    Config,
    /// Tells how the device is doing, e.g. its signal strength or firmware version.
    Diagnostic,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntity {
    id: EntityId,
    protocol: ProtocolId,
    unique_id: UniqueId,
    name: Name,
    #[serde(default)]
    device_id: Option<DeviceId>,
    #[serde(default)]
    area_id: Option<AreaId>,
    capabilities: Capabilities,
    #[serde(default)]
    entity_category: Option<EntityCategory>,
}

impl<'de> Deserialize<'de> for Entity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawEntity::deserialize(deserializer)?;
        Entity::try_from(raw).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<RawEntity> for Entity {
    type Error = InvariantError;
    fn try_from(raw: RawEntity) -> Result<Self, InvariantError> {
        let entity = Entity {
            id: raw.id,
            protocol: raw.protocol,
            unique_id: raw.unique_id,
            name: raw.name,
            device_id: raw.device_id,
            area_id: raw.area_id,
            capabilities: raw.capabilities,
            entity_category: raw.entity_category,
        };
        entity.validate()?;
        Ok(entity)
    }
}

impl Entity {
    /// Checks the rules that span fields. Deserialization runs this; call it yourself when
    /// building an `Entity` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        let (id_kind, caps_kind) = (self.id.kind(), self.capabilities.kind());
        if id_kind != caps_kind {
            return Err(InvariantError(format!(
                "entity `{}` is a {id_kind}, but its capabilities are for a {caps_kind}",
                self.id
            )));
        }
        self.capabilities.validate()
    }
}

/// What an entity can do, per kind. Static facts that don't change with its state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Capabilities {
    Light(LightCapabilities),
    Switch(SwitchCapabilities),
    Sensor(SensorCapabilities),
    BinarySensor(BinarySensorCapabilities),
    Number(NumberCapabilities),
    Select(SelectCapabilities),
    Text(TextCapabilities),
    Button(ButtonCapabilities),
}

impl Capabilities {
    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Light(_) => EntityKind::Light,
            Self::Switch(_) => EntityKind::Switch,
            Self::Sensor(_) => EntityKind::Sensor,
            Self::BinarySensor(_) => EntityKind::BinarySensor,
            Self::Number(_) => EntityKind::Number,
            Self::Select(_) => EntityKind::Select,
            Self::Text(_) => EntityKind::Text,
            Self::Button(_) => EntityKind::Button,
        }
    }

    /// Deserialization runs this; call it yourself when building `Capabilities` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::Light(LightCapabilities {
                color_temp_kelvin: Some(range),
                ..
            }) => range.validate(),
            Self::Sensor(sensor) => sensor.validate(),
            Self::Number(number) => number.validate(),
            Self::Select(select) => select.validate(),
            Self::Text(text) => text.validate(),
            _ => Ok(()),
        }
    }
}
