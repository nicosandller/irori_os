//! Live state: the current value of each entity. Changes often.
//! See `docs/specs/entities.md` §5.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::num::{Num, whole};
use crate::{AttributeKey, Context, EntityId, EntityKind, InvariantError, Timestamp};

/// Free-form extra data from the integration, e.g. Zigbee link quality. Readable by rules, but
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
    /// When the integration last reported anything, even an identical value.
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
        // hearing from it (marking it unavailable after its integration crashed).
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
}

impl State {
    /// Checks the value's own rules. Deserialization runs this; call it yourself when building
    /// a `State` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::Light(light) => light.validate(),
            Self::Sensor(sensor) => sensor.validate(),
            Self::Switch(_) | Self::BinarySensor(_) => Ok(()),
        }
    }

    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Light(_) => EntityKind::Light,
            Self::Switch(_) => EntityKind::Switch,
            Self::Sensor(_) => EntityKind::Sensor,
            Self::BinarySensor(_) => EntityKind::BinarySensor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightState {
    pub on: bool,
    /// 1-255, when the light supports dimming. Kept while off: the level it returns to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 255))]
    pub brightness: Option<u8>,
    /// Which color setting is active, when the light supports more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_mode: Option<ColorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1000, max = 20000))]
    pub color_temp_kelvin: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLightState {
    on: bool,
    // Wider than the real types so out-of-range values get a message naming the field.
    // Numbers as written, so the checks below can name the field (see `crate::num`).
    #[serde(default)]
    brightness: Option<Num>,
    #[serde(default)]
    color_mode: Option<ColorMode>,
    #[serde(default)]
    color_temp_kelvin: Option<Num>,
    #[serde(default)]
    rgb: Option<[Num; 3]>,
}

impl<'de> Deserialize<'de> for LightState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = RawLightState::deserialize(deserializer)?;
        if matches!(raw.brightness, Some(Num::Int(0) | Num::UInt(0)))
            || matches!(raw.brightness, Some(Num::Float(b)) if b == 0.0)
        {
            return Err(D::Error::custom(BRIGHTNESS_ZERO));
        }
        let rgb = match raw.rgb {
            Some([r, g, b]) => Some([
                whole("rgb[0]", r, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[1]", g, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[2]", b, 0, 255).map_err(D::Error::custom)?,
            ]),
            None => None,
        };
        let light = LightState {
            on: raw.on,
            brightness: raw
                .brightness
                .map(|n| whole("brightness", n, 1, 255))
                .transpose()
                .map_err(D::Error::custom)?,
            color_mode: raw.color_mode,
            color_temp_kelvin: raw
                .color_temp_kelvin
                .map(|n| whole("color_temp_kelvin", n, 1000, 20000))
                .transpose()
                .map_err(D::Error::custom)?,
            rgb,
        };
        light.validate().map_err(D::Error::custom)?;
        Ok(light)
    }
}

const BRIGHTNESS_ZERO: &str =
    "brightness 0 is invalid; brightness is 1-255 (use `on: false` for off)";

impl LightState {
    /// Deserialization runs this; call it yourself when building a `LightState` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.brightness == Some(0) {
            return Err(InvariantError(BRIGHTNESS_ZERO.into()));
        }
        if let Some(kelvin) = self.color_temp_kelvin
            && !(1000..=20000).contains(&kelvin)
        {
            return Err(InvariantError(format!(
                "color_temp_kelvin {kelvin} is out of range; it must be 1000-20000"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    ColorTemp,
    Rgb,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchState {
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SensorState {
    pub value: SensorValue,
}

impl SensorState {
    /// JSON can't carry NaN or infinity, but code can build them; they'd fail to serialize.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self.value {
            SensorValue::Number(n) if !n.is_finite() => Err(InvariantError(format!(
                "sensor value {n} is not a finite number"
            ))),
            _ => Ok(()),
        }
    }
}

/// A sensor reading: a finite number or text, matching the sensor's `value_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SensorValue {
    Number(f64),
    Text(String),
}

impl JsonSchema for SensorValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SensorValue".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Bounded to f64's range: a literal like `1e400` is valid JSON, but Rust can't read it.
        schemars::json_schema!({
            "description": "A sensor reading: a finite number or text, matching the sensor's `value_type`.",
            "anyOf": [
                { "type": "number", "minimum": f64::MIN, "maximum": f64::MAX },
                { "type": "string" },
            ],
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinarySensorState {
    /// `true` means detected/open/wet/etc., depending on the device class.
    pub on: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_rules_hold_when_deserialized_on_their_own() {
        let zero = serde_json::from_str::<LightState>(r#"{"on": true, "brightness": 0}"#);
        assert!(zero.is_err_and(|e| e.to_string().contains("brightness 0 is invalid")));

        let hot = serde_json::from_str::<State>(
            r#"{"kind": "light", "on": true, "color_temp_kelvin": 20001}"#,
        );
        assert!(hot.is_err_and(|e| {
            e.to_string()
                .contains("color_temp_kelvin 20001 is out of range")
        }));

        let bright = serde_json::from_str::<LightState>(r#"{"on": true, "brightness": 300}"#);
        assert!(bright.is_err_and(|e| {
            e.to_string()
                .contains("brightness 300 is out of range; it must be from 1 to 255")
        }));

        assert!(
            serde_json::from_str::<LightState>(
                r#"{"on": false, "brightness": 1, "color_temp_kelvin": 1000}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn sensor_value_schema_rejects_numbers_rust_cant_read() {
        let schema = SensorValue::json_schema(&mut schemars::SchemaGenerator::default());
        let validator = jsonschema::validator_for(schema.as_value()).expect("valid schema");
        assert!(validator.is_valid(&serde_json::json!(f64::MAX)));
        assert!(validator.is_valid(&serde_json::json!("rinse")));
        // `1e400` can't even be held in a serde_json::Value, so check the bounds directly.
        let number = &schema.as_value()["anyOf"][0];
        assert_eq!(number["minimum"], serde_json::json!(f64::MIN));
        assert_eq!(number["maximum"], serde_json::json!(f64::MAX));
        assert!(serde_json::from_str::<SensorValue>("1e400").is_err());
    }

    #[test]
    fn non_finite_sensor_values_are_rejected_when_built_in_code() {
        for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let state = State::Sensor(SensorState {
                value: SensorValue::Number(n),
            });
            assert!(state.validate().is_err(), "{n}");
        }
        let ok = State::Sensor(SensorState {
            value: SensorValue::Number(21.5),
        });
        assert!(ok.validate().is_ok());
    }
}
