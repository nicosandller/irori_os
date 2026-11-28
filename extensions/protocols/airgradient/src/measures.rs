//! A monitor's answer to `GET /measures/current`, as AirGradient documents it for firmware 3.0.10
//! onwards (`docs/local-server.md` in its firmware repository).
//!
//! Only fields that document gives a meaning and a unit for are read. A field the monitor leaves
//! out, sends as `null`, or sends as anything but a sensible number is absent: never zero.

use std::collections::BTreeMap;

use irori_types::{EntityCategory, SensorClass};
use serde_json::Value;

/// Where a monitor's current readings are.
pub const PATH: &str = "/measures/current";

/// A serial number is twelve characters on current monitors; this leaves room.
const MAX_SERIAL_LEN: usize = 32;
/// The most a model or firmware name is kept.
const MAX_TEXT_LEN: usize = 64;
/// No reading a monitor gives is anywhere near this, so a larger one is a fault.
const MAX_MAGNITUDE: f64 = 1_000_000.0;

/// One reading a monitor can give.
#[derive(Debug, PartialEq)]
pub struct Sensor {
    /// The field in the monitor's answer.
    pub field: &'static str,
    /// The end of the entity's unique id. Never changes.
    pub suffix: &'static str,
    pub name: &'static str,
    pub class: Option<SensorClass>,
    pub unit: Option<&'static str>,
    pub category: Option<EntityCategory>,
}

const fn sensor(
    field: &'static str,
    suffix: &'static str,
    name: &'static str,
    class: Option<SensorClass>,
    unit: Option<&'static str>,
) -> Sensor {
    Sensor {
        field,
        suffix,
        name,
        class,
        unit,
        category: None,
    }
}

const MASS: Option<&str> = Some("µg/m³");

/// Every reading taken from a monitor, in the order they're listed on its device.
///
/// The VOC and NOx values are Sensirion's indexes, not concentrations, so they carry no unit and
/// no class. The 0.3 µm value is a count, not a mass, so it isn't a particulate class either.
pub const SENSORS: [Sensor; 13] = [
    sensor(
        "rco2",
        "co2",
        "Carbon dioxide",
        Some(SensorClass::Co2),
        Some("ppm"),
    ),
    sensor("pm01", "pm1", "PM1", Some(SensorClass::Pm1), MASS),
    sensor("pm02", "pm25", "PM2.5", Some(SensorClass::Pm25), MASS),
    sensor(
        "pm02Compensated",
        "pm25_corrected",
        "PM2.5, corrected",
        Some(SensorClass::Pm25),
        MASS,
    ),
    sensor("pm10", "pm10", "PM10", Some(SensorClass::Pm10), MASS),
    sensor(
        "pm003Count",
        "pm003_count",
        "PM0.3 count",
        None,
        Some("particles/dL"),
    ),
    sensor(
        "atmp",
        "temperature",
        "Temperature",
        Some(SensorClass::Temperature),
        Some("°C"),
    ),
    sensor(
        "atmpCompensated",
        "temperature_corrected",
        "Temperature, corrected",
        Some(SensorClass::Temperature),
        Some("°C"),
    ),
    sensor(
        "rhum",
        "humidity",
        "Humidity",
        Some(SensorClass::Humidity),
        Some("%"),
    ),
    sensor(
        "rhumCompensated",
        "humidity_corrected",
        "Humidity, corrected",
        Some(SensorClass::Humidity),
        Some("%"),
    ),
    sensor("tvocIndex", "voc_index", "VOC index", None, None),
    sensor("noxIndex", "nox_index", "NOx index", None, None),
    Sensor {
        field: "wifi",
        suffix: "signal",
        name: "Signal strength",
        class: Some(SensorClass::SignalStrength),
        unit: Some("dBm"),
        category: Some(EntityCategory::Diagnostic),
    },
];

/// What one monitor said, once.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The serial number the monitor reports for itself, in lowercase.
    pub serial: String,
    pub model: Option<String>,
    pub firmware: Option<String>,
    /// By [`Sensor::suffix`]. A reading the monitor didn't give isn't here.
    pub values: BTreeMap<&'static str, f64>,
}

/// Reads a monitor's answer, or says why it isn't one.
pub fn parse(body: &[u8]) -> Result<Reading, &'static str> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "the monitor's answer isn't JSON")?;
    let Value::Object(fields) = value else {
        return Err("the monitor's answer isn't a JSON object");
    };
    let serial = fields
        .get("serialno")
        .and_then(Value::as_str)
        .and_then(serial)
        .ok_or("the monitor's answer has no serial number")?;
    let text = |key: &str| {
        fields
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| {
                !text.is_empty()
                    && text.chars().count() <= MAX_TEXT_LEN
                    && !text.chars().any(char::is_control)
            })
            .map(str::to_owned)
    };
    let values = SENSORS
        .iter()
        .filter_map(|sensor| Some((sensor.suffix, number(fields.get(sensor.field)?)?)))
        .collect();
    Ok(Reading {
        serial,
        model: text("model"),
        firmware: text("firmware"),
        values,
    })
}

/// A serial number as ids are built from it: letters and digits only, in lowercase.
fn serial(text: &str) -> Option<String> {
    let text = text.trim();
    let ok = !text.is_empty()
        && text.len() <= MAX_SERIAL_LEN
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric());
    ok.then(|| text.to_ascii_lowercase())
}

/// A JSON number that could be a reading. `null`, text and booleans aren't.
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|number| number.is_finite() && number.abs() <= MAX_MAGNITUDE)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn reads_every_documented_reading() {
        let body = serde_json::json!({
            "wifi": -52, "serialno": "0A1B2C3D4E5F", "rco2": 612,
            "pm01": 3, "pm02": 7, "pm10": 8, "pm02Compensated": 6.4, "pm003Count": 442,
            "atmp": 21.87, "atmpCompensated": 21.47, "rhum": 43, "rhumCompensated": 49,
            "tvocIndex": 100, "tvocRaw": 33051, "noxIndex": 1, "noxRaw": 16307,
            "boot": 6, "bootCount": 6, "ledMode": "pm", "firmware": "3.1.3", "model": "I-9PSL"
        });
        let reading = parse(body.to_string().as_bytes()).expect("a reading");
        assert_eq!(reading.serial, "0a1b2c3d4e5f");
        assert_eq!(reading.model.as_deref(), Some("I-9PSL"));
        assert_eq!(reading.firmware.as_deref(), Some("3.1.3"));
        assert_eq!(reading.values.len(), SENSORS.len());
        assert_eq!(reading.values["co2"], 612.0);
        assert_eq!(reading.values["temperature"], 21.87);
        assert_eq!(reading.values["signal"], -52.0);
    }

    #[test]
    fn a_missing_or_null_reading_is_absent_and_zero_is_a_reading() {
        let body = br#"{"serialno":"0a1b2c3d4e5f","rco2":null,"pm02":0,"atmp":"21.8","rhum":true}"#;
        let reading = parse(body).expect("a reading");
        assert_eq!(
            reading.values,
            BTreeMap::from([("pm25", 0.0)]),
            "null, text and a boolean are no reading; 0 is one"
        );
    }

    #[test]
    fn a_reading_out_of_all_proportion_is_absent() {
        let body = br#"{"serialno":"0a1b2c3d4e5f","rco2":1e300,"pm02":-5000000,"pm10":12}"#;
        let reading = parse(body).expect("a reading");
        assert_eq!(reading.values, BTreeMap::from([("pm10", 12.0)]));
    }

    #[test]
    fn anything_but_a_monitors_json_is_refused() {
        for body in [
            &b"<html>Welcome</html>"[..],
            b"",
            b"[]",
            b"\"0a1b2c3d4e5f\"",
            b"{}",
            br#"{"rco2":600}"#,
            br#"{"serialno":12}"#,
            br#"{"serialno":""}"#,
            br#"{"serialno":"../../etc"}"#,
            br#"{"serialno":"0a1b 2c3d"}"#,
            br#"{"serialno":"0123456789012345678901234567890123"}"#,
            br#"{"serialno":"0a1b2c3d4e5f","rco2":600"#,
        ] {
            assert!(parse(body).is_err(), "{}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn a_model_that_isnt_plain_text_is_left_out() {
        let long = "x".repeat(65);
        let body =
            serde_json::json!({ "serialno": "0a1b2c3d4e5f", "model": long, "firmware": "3\u{7}1" });
        let reading = parse(body.to_string().as_bytes()).expect("a reading");
        assert_eq!(reading.model, None);
        assert_eq!(reading.firmware, None);
    }

    #[test]
    fn every_sensor_has_its_own_field_and_id() {
        let fields: BTreeSet<_> = SENSORS.iter().map(|sensor| sensor.field).collect();
        let suffixes: BTreeSet<_> = SENSORS.iter().map(|sensor| sensor.suffix).collect();
        assert_eq!(fields.len(), SENSORS.len());
        assert_eq!(suffixes.len(), SENSORS.len());
    }
}
