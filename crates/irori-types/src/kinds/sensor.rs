//! `sensor`: a numeric or text reading, e.g. temperature or illuminance.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Typed;

use crate::InvariantError;

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
    /// Every text it can report, when that's a fixed list (a washing machine's program, Home
    /// Assistant's `enum` sensors). Only for text; readings outside it are refused, and rules
    /// comparing it with text it can never have are refused when saved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub options: Vec<String>,
}

/// At most this many options, and this long each: a fixed list a person picks from, not a
/// catalogue.
const MAX_OPTIONS: usize = 256;
const MAX_OPTION_LEN: usize = 255;

impl SensorCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        validate_options(&self.options)?;
        if !self.options.is_empty() && self.value_type != SensorValueType::Text {
            return Err(InvariantError(
                "options are for a sensor that reports text; this one reports numbers".into(),
            ));
        }
        Ok(())
    }
}

/// The rules for a list of text options, shared by every kind that has one.
pub(crate) fn validate_options(options: &[String]) -> Result<(), InvariantError> {
    if options.len() > MAX_OPTIONS {
        return Err(InvariantError(format!(
            "{} options is too many; at most {MAX_OPTIONS}",
            options.len()
        )));
    }
    for (i, option) in options.iter().enumerate() {
        if option.trim().is_empty() || option.chars().count() > MAX_OPTION_LEN {
            return Err(InvariantError(format!(
                "option {option:?} must be 1-{MAX_OPTION_LEN} characters and not blank"
            )));
        }
        if options[..i].contains(option) {
            return Err(InvariantError(format!("option {option:?} is listed twice")));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SensorValueType {
    Number,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
/// What a sensor measures: Home Assistant's sensor device classes, by the same names. A
/// sensor whose readings come from a fixed list says so with `options` instead of a class.
pub enum SensorClass {
    AbsoluteHumidity,
    ApparentPower,
    Aqi,
    Area,
    AtmosphericPressure,
    Battery,
    BloodGlucoseConcentration,
    /// Carbon dioxide; Home Assistant's `carbon_dioxide`.
    Co2,
    CarbonMonoxide,
    Conductivity,
    Current,
    DataRate,
    DataSize,
    Date,
    Distance,
    Duration,
    Energy,
    EnergyDistance,
    EnergyStorage,
    Frequency,
    Gas,
    Humidity,
    Illuminance,
    Irradiance,
    Moisture,
    Monetary,
    NitrogenDioxide,
    NitrogenMonoxide,
    NitrousOxide,
    Ozone,
    Ph,
    Pm1,
    Pm10,
    Pm25,
    Pm4,
    Power,
    PowerFactor,
    Precipitation,
    PrecipitationIntensity,
    Pressure,
    Radon,
    ReactiveEnergy,
    ReactivePower,
    SignalStrength,
    SoundPressure,
    Speed,
    SulphurDioxide,
    Temperature,
    TemperatureDelta,
    Timestamp,
    Uptime,
    VolatileOrganicCompounds,
    VolatileOrganicCompoundsParts,
    Voltage,
    Volume,
    VolumeFlowRate,
    VolumeStorage,
    Water,
    Weight,
    WindDirection,
    WindSpeed,
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

/// Whether a reading matches what the sensor says it reports.
pub(crate) fn fits(caps: &SensorCapabilities, sensor: &SensorState) -> Result<(), String> {
    match (caps.value_type, &sensor.value) {
        (SensorValueType::Number, SensorValue::Text(text)) => Err(format!(
            "it reports numbers, but the value is text {text:?}"
        )),
        (SensorValueType::Text, SensorValue::Number(n)) => {
            Err(format!("it reports text, but the value is the number {n}"))
        }
        (SensorValueType::Text, SensorValue::Text(text))
            if !caps.options.is_empty() && !caps.options.contains(text) =>
        {
            Err(format!(
                "{text:?} isn't one of its options ({})",
                caps.options.join(", ")
            ))
        }
        _ => Ok(()),
    }
}

impl SensorClass {
    /// Whether a sensor of this class reports text rather than a number: a date, a timestamp.
    pub fn reports_text(self) -> bool {
        matches!(self, Self::Date | Self::Timestamp)
    }

    /// The class Home Assistant calls `name` (`temperature`), as protocols that speak its vocabulary
    /// (ESPHome, MQTT discovery) report it.
    pub fn from_ha(name: &str) -> Option<Self> {
        match name {
            // Named for the gas here; Home Assistant spells it out, and never sends `co2`.
            "carbon_dioxide" => Some(Self::Co2),
            "co2" => None,
            _ => super::from_ha(name),
        }
    }
}

pub(crate) fn primary(state: &SensorState) -> Typed {
    match &state.value {
        SensorValue::Number(n) => Typed::Number(*n),
        SensorValue::Text(text) => Typed::Text(text.clone()),
    }
}

pub(crate) fn with_primary(value: &Typed) -> Option<SensorState> {
    Some(SensorState {
        value: match value {
            Typed::Number(n) => SensorValue::Number(*n),
            Typed::Text(text) => SensorValue::Text(text.clone()),
            Typed::Bool(_) => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::State;

    #[test]
    fn a_sensor_with_options_reports_only_those() {
        let caps = SensorCapabilities {
            value_type: SensorValueType::Text,
            device_class: None,
            unit: None,
            state_class: None,
            options: vec!["wash".into(), "rinse".into()],
        };
        let text = |t: &str| SensorState {
            value: SensorValue::Text(t.into()),
        };
        assert!(fits(&caps, &text("rinse")).is_ok());
        assert_eq!(
            fits(&caps, &text("spin")),
            Err(r#""spin" isn't one of its options (wash, rinse)"#.to_owned())
        );
        let blank = SensorCapabilities {
            options: vec![" ".into()],
            ..caps.clone()
        };
        assert!(blank.validate().is_err());
        assert_eq!(
            State::Sensor(text("rinse")).primary(),
            crate::Typed::Text("rinse".into())
        );
    }

    #[test]
    fn classes_are_read_by_their_home_assistant_names() {
        assert_eq!(
            SensorClass::from_ha("temperature"),
            Some(SensorClass::Temperature)
        );
        assert_eq!(
            SensorClass::from_ha("carbon_dioxide"),
            Some(SensorClass::Co2)
        );
        assert_eq!(
            SensorClass::from_ha("volatile_organic_compounds_parts"),
            Some(SensorClass::VolatileOrganicCompoundsParts)
        );
        assert_eq!(SensorClass::from_ha("co2"), None);
        assert_eq!(SensorClass::from_ha("enum"), None);
        assert_eq!(SensorClass::from_ha("Temperature"), None);
        assert_eq!(
            crate::BinarySensorClass::from_ha("garage_door"),
            Some(crate::BinarySensorClass::GarageDoor)
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
