//! The registry: what exists in the home and what it can do. Changes rarely.
//! See `docs/specs/entities.md` §4.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::num::{Num, whole};

use crate::{
    AreaId, Description, DeviceId, EntityId, EntityKind, FloorId, IntegrationId, InvariantError,
    Name, UniqueId,
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

/// A physical or virtual thing an integration talks to, e.g. a Zigbee motion sensor. A device
/// has one or more entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// The device's one id, everywhere: page addresses, config files, the API, rules. Made once
    /// from the integration and its permanent handle for the device, never from a name, so it
    /// doesn't change when the device is renamed (ROADMAP D36).
    pub id: DeviceId,
    /// The integration that provides this device.
    pub integration: IntegrationId,
    /// The integration's stable id for the device, e.g. the Zigbee IEEE address.
    pub unique_id: UniqueId,
    /// Its one name. Starts as whatever its integration reports, and once a person names it,
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
    /// The area a person put this device in. Absent means nobody has said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_id: Option<AreaId>,
    /// The area the device says it's in, e.g. ESPHome's `area:`. Only a hint: it names an area
    /// rather than pointing at one, and it is used only while `area_id` is absent
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
    pub integration: IntegrationId,
    /// The integration's stable id for this entity. Survives renames of `id`.
    pub unique_id: UniqueId,
    pub name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    /// Overrides the device's area when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_id: Option<AreaId>,
    pub capabilities: Capabilities,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntity {
    id: EntityId,
    integration: IntegrationId,
    unique_id: UniqueId,
    name: Name,
    #[serde(default)]
    device_id: Option<DeviceId>,
    #[serde(default)]
    area_id: Option<AreaId>,
    capabilities: Capabilities,
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
            integration: raw.integration,
            unique_id: raw.unique_id,
            name: raw.name,
            device_id: raw.device_id,
            area_id: raw.area_id,
            capabilities: raw.capabilities,
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
}

impl Capabilities {
    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Light(_) => EntityKind::Light,
            Self::Switch(_) => EntityKind::Switch,
            Self::Sensor(_) => EntityKind::Sensor,
            Self::BinarySensor(_) => EntityKind::BinarySensor,
        }
    }

    /// Deserialization runs this; call it yourself when building `Capabilities` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::Light(LightCapabilities {
                color_temp_kelvin: Some(range),
                ..
            }) => range.validate(),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightCapabilities {
    /// Supports dimming.
    #[serde(default)]
    pub brightness: bool,
    /// Supports color temperature, within this range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_temp_kelvin: Option<ColorTempRange>,
    /// Supports RGB color.
    #[serde(default)]
    pub rgb: bool,
}

/// Supported color temperatures in kelvin, `min` (warmest) to `max` (coolest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColorTempRange {
    #[schemars(range(min = 1000, max = 20000))]
    pub min: u16,
    #[schemars(range(min = 1000, max = 20000))]
    pub max: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawColorTempRange {
    // Wider than `u16` so out-of-range values get the range message, not "expected u16".
    min: Num,
    max: Num,
}

impl<'de> Deserialize<'de> for ColorTempRange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawColorTempRange::deserialize(deserializer)?;
        use serde::de::Error as _;
        let range = ColorTempRange {
            min: whole("color_temp_kelvin.min", raw.min, 1000, 20000).map_err(D::Error::custom)?,
            max: whole("color_temp_kelvin.max", raw.max, 1000, 20000).map_err(D::Error::custom)?,
        };
        range.validate().map_err(serde::de::Error::custom)?;
        Ok(range)
    }
}

impl ColorTempRange {
    /// Deserialization runs this; call it yourself when building a range in code.
    pub fn validate(self) -> Result<(), InvariantError> {
        let valid = |k: u16| (1000..=20000).contains(&k);
        if !valid(self.min) || !valid(self.max) {
            return Err(InvariantError(format!(
                "color temperature range {}-{} K must be within 1000-20000 K",
                self.min, self.max
            )));
        }
        if self.min > self.max {
            return Err(InvariantError(format!(
                "color temperature range has min {} K above max {} K",
                self.min, self.max
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<SwitchClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SwitchClass {
    Outlet,
    Switch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SensorCapabilities {
    /// Whether readings are numbers or text. Rules are type-checked against this.
    pub value_type: SensorValueType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<SensorClass>,
    /// Unit of numeric readings, e.g. `°C`, `lx`, `%`, `W`, `kWh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// How numeric readings accumulate, for history and statistics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_class: Option<StateClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SensorValueType {
    Number,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SensorClass {
    Temperature,
    Humidity,
    Illuminance,
    Pressure,
    Power,
    Energy,
    Voltage,
    Current,
    Battery,
    Co2,
    Pm25,
    SignalStrength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StateClass {
    /// A current value, e.g. temperature.
    Measurement,
    /// A running total that can go up or down, e.g. net energy.
    Total,
    /// A counter that only increases, resetting occasionally, e.g. a meter reading.
    TotalIncreasing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinarySensorCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<BinarySensorClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BinarySensorClass {
    Motion,
    Occupancy,
    Door,
    Window,
    Moisture,
    Smoke,
    Gas,
    Vibration,
    Plug,
    Connectivity,
    Problem,
    Battery,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_temp_range_is_checked_when_deserialized_on_its_own() {
        let inverted = serde_json::from_str::<ColorTempRange>(r#"{"min": 6500, "max": 2200}"#);
        assert!(inverted.is_err_and(|e| e.to_string().contains("min 6500 K above max 2200 K")));

        let via_capabilities = serde_json::from_str::<Capabilities>(
            r#"{"kind": "light", "color_temp_kelvin": {"min": 500, "max": 2200}}"#,
        );
        assert!(via_capabilities.is_err_and(|e| {
            e.to_string().contains(
                "color_temp_kelvin.min 500 is out of range; it must be from 1000 to 20000",
            )
        }));
    }
}
