//! A reading, as an Irori device, its sensors and their states.
//!
//! A monitor is one device, named and identified by the serial number it reports, so it survives
//! a new address. A reading becomes a sensor the first time the monitor gives it, and stays one:
//! when a later answer leaves it out, its state is unknown instead of holding the last number.

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{
    Capabilities, DeviceDescription, EntityDescription, Name, SensorCapabilities, SensorState,
    SensorValue, SensorValueType, State, StateClass, UniqueId,
};

use crate::measures::{Reading, SENSORS, Sensor};

#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    pub device: DeviceDescription,
    pub entities: Vec<EntityDescription>,
}

fn id(text: String) -> Result<UniqueId, String> {
    UniqueId::try_from(text).map_err(|error| error.to_string())
}

fn name(text: &str) -> Result<Name, String> {
    Name::try_from(text).map_err(|error| error.to_string())
}

fn entity_id(serial: &str, sensor: &Sensor) -> Result<UniqueId, String> {
    id(format!("{serial}-{}", sensor.suffix))
}

/// The sensors this monitor has given a value for, in listing order.
fn given<'a>(seen: &'a BTreeSet<&'static str>) -> impl Iterator<Item = &'static Sensor> + 'a {
    SENSORS
        .iter()
        .filter(move |sensor| seen.contains(sensor.suffix))
}

/// The device and a sensor for each reading in `seen`: every reading the monitor has given since
/// the extension started.
pub fn describe(reading: &Reading, seen: &BTreeSet<&'static str>) -> Result<Described, String> {
    let serial = reading.serial.as_str();
    let device_id = id(serial.to_owned())?;
    // Monitors carry no name of their own; the end of the serial number tells two apart.
    let tail = &serial[serial.len().saturating_sub(4)..];
    let device = DeviceDescription {
        unique_id: device_id.clone(),
        name: name(&format!("AirGradient {tail}"))?,
        manufacturer: Some("AirGradient".to_owned()),
        model: reading.model.clone(),
        sw_version: reading.firmware.clone(),
        hw_version: None,
        serial_number: Some(serial.to_owned()),
        suggested_area: None,
        via_device_unique_id: None,
        connections: Vec::new(),
    };
    let entities = given(seen)
        .map(|sensor| {
            Ok(EntityDescription {
                unique_id: entity_id(serial, sensor)?,
                name: Some(name(sensor.name)?),
                device_unique_id: Some(device_id.clone()),
                suggested_object_id: None,
                capabilities: Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: sensor.class,
                    unit: sensor.unit.map(str::to_owned),
                    state_class: Some(StateClass::Measurement),
                    options: Vec::new(),
                }),
                entity_category: sensor.category,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Described { device, entities })
}

/// Every described sensor's state in this reading: unknown for one the monitor left out.
pub fn states(
    reading: &Reading,
    seen: &BTreeSet<&'static str>,
) -> Result<BTreeMap<UniqueId, Option<State>>, String> {
    given(seen)
        .map(|sensor| {
            let state = reading.values.get(sensor.suffix).map(|value| {
                State::Sensor(SensorState {
                    value: SensorValue::Number(*value),
                })
            });
            Ok((entity_id(&reading.serial, sensor)?, state))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use irori_types::SensorClass;

    use super::*;

    fn reading(values: &[(&'static str, f64)]) -> Reading {
        Reading {
            serial: "0a1b2c3d4e5f".to_owned(),
            model: Some("I-9PSL".to_owned()),
            firmware: Some("3.1.3".to_owned()),
            values: values.iter().copied().collect(),
        }
    }

    fn seen(reading: &Reading) -> BTreeSet<&'static str> {
        reading.values.keys().copied().collect()
    }

    fn capabilities(entity: &EntityDescription) -> &SensorCapabilities {
        match &entity.capabilities {
            Capabilities::Sensor(sensor) => sensor,
            other => panic!("not a sensor: {other:?}"),
        }
    }

    #[test]
    fn a_monitor_is_one_device_named_by_its_serial_number() {
        let reading = reading(&[("co2", 612.0)]);
        let described = describe(&reading, &seen(&reading)).expect("valid");
        assert_eq!(described.device.unique_id.as_str(), "0a1b2c3d4e5f");
        assert_eq!(described.device.name.as_str(), "AirGradient 4e5f");
        assert_eq!(
            described.device.manufacturer.as_deref(),
            Some("AirGradient")
        );
        assert_eq!(described.device.model.as_deref(), Some("I-9PSL"));
        assert_eq!(described.device.sw_version.as_deref(), Some("3.1.3"));
        assert_eq!(
            described.device.serial_number.as_deref(),
            Some("0a1b2c3d4e5f")
        );
    }

    #[test]
    fn each_reading_has_its_unit_and_class() {
        let all: Vec<(&'static str, f64)> =
            SENSORS.iter().map(|sensor| (sensor.suffix, 1.0)).collect();
        let reading = reading(&all);
        let described = describe(&reading, &seen(&reading)).expect("valid");
        let shown: Vec<(&str, Option<SensorClass>, Option<&str>)> = described
            .entities
            .iter()
            .map(|entity| {
                let sensor = capabilities(entity);
                (
                    entity.unique_id.as_str(),
                    sensor.device_class,
                    sensor.unit.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("0a1b2c3d4e5f-co2", Some(SensorClass::Co2), Some("ppm")),
                ("0a1b2c3d4e5f-pm1", Some(SensorClass::Pm1), Some("µg/m³")),
                ("0a1b2c3d4e5f-pm25", Some(SensorClass::Pm25), Some("µg/m³")),
                (
                    "0a1b2c3d4e5f-pm25_corrected",
                    Some(SensorClass::Pm25),
                    Some("µg/m³")
                ),
                ("0a1b2c3d4e5f-pm10", Some(SensorClass::Pm10), Some("µg/m³")),
                ("0a1b2c3d4e5f-pm003_count", None, Some("particles/dL")),
                (
                    "0a1b2c3d4e5f-temperature",
                    Some(SensorClass::Temperature),
                    Some("°C")
                ),
                (
                    "0a1b2c3d4e5f-temperature_corrected",
                    Some(SensorClass::Temperature),
                    Some("°C")
                ),
                (
                    "0a1b2c3d4e5f-humidity",
                    Some(SensorClass::Humidity),
                    Some("%")
                ),
                (
                    "0a1b2c3d4e5f-humidity_corrected",
                    Some(SensorClass::Humidity),
                    Some("%")
                ),
                ("0a1b2c3d4e5f-voc_index", None, None),
                ("0a1b2c3d4e5f-nox_index", None, None),
                (
                    "0a1b2c3d4e5f-signal",
                    Some(SensorClass::SignalStrength),
                    Some("dBm")
                ),
            ]
        );
        for entity in &described.entities {
            let sensor = capabilities(entity);
            assert_eq!(sensor.value_type, SensorValueType::Number);
            assert_eq!(sensor.state_class, Some(StateClass::Measurement));
            sensor.validate().expect("valid capabilities");
        }
        let signal = described.entities.last().expect("signal");
        assert_eq!(
            signal.entity_category,
            Some(irori_types::EntityCategory::Diagnostic)
        );
    }

    #[test]
    fn a_reading_never_given_is_no_sensor() {
        let reading = reading(&[("pm25", 0.0)]);
        let seen = seen(&reading);
        let described = describe(&reading, &seen).expect("valid");
        assert_eq!(described.entities.len(), 1);
        let states = states(&reading, &seen).expect("valid");
        assert_eq!(states.len(), 1);
        let pm25 = UniqueId::try_from("0a1b2c3d4e5f-pm25").expect("id");
        assert_eq!(
            states[&pm25],
            Some(State::Sensor(SensorState {
                value: SensorValue::Number(0.0)
            })),
            "zero is a reading"
        );
    }

    #[test]
    fn a_reading_left_out_later_keeps_its_sensor_and_is_unknown() {
        let first = reading(&[("co2", 612.0), ("pm25", 7.0)]);
        let seen = seen(&first);
        let later = reading(&[("pm25", 7.0)]);
        assert_eq!(
            describe(&later, &seen).expect("valid"),
            describe(&first, &seen).expect("valid")
        );
        let co2 = UniqueId::try_from("0a1b2c3d4e5f-co2").expect("id");
        assert_eq!(states(&later, &seen).expect("valid")[&co2], None);
    }
}
