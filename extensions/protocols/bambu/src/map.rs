//! The printer's status, as an Irori device, its entities and their states.
//!
//! One device per printer, named after its serial number's last characters. Its main entity is
//! what it is doing. Each slot of a filament unit the printer reports is an entity too: its
//! state is the filament's type, with its colour and how much is left beside it.
//!
//! Ids are built from the serial number, so they survive a new address.

use std::collections::BTreeMap;

use irori_types::{
    AttributeKey, Attributes, Capabilities, DeviceDescription, EntityDescription, Name,
    SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, State, StateClass,
    UniqueId,
};

use crate::status::{Slot, Stage, Status};

/// What a slot with nothing in it reports.
pub const EMPTY: &str = "empty";

#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    pub device: DeviceDescription,
    pub entities: Vec<EntityDescription>,
}

/// One entity's state, with what goes beside it.
pub type Reading = (UniqueId, Option<State>, Attributes);

fn id(text: String) -> Result<UniqueId, String> {
    UniqueId::try_from(text).map_err(|error| error.to_string())
}

fn name(text: &str) -> Result<Name, String> {
    Name::try_from(text).map_err(|error| error.to_string())
}

struct Kind {
    suffix: &'static str,
    name: &'static str,
    class: Option<SensorClass>,
    unit: Option<&'static str>,
    read: fn(&Status) -> Option<f64>,
}

const NUMBERS: [Kind; 6] = [
    Kind {
        suffix: "progress",
        name: "Progress",
        class: None,
        unit: Some("%"),
        read: |status| status.progress.map(f64::from),
    },
    Kind {
        suffix: "remaining",
        name: "Remaining time",
        class: Some(SensorClass::Duration),
        unit: Some("min"),
        read: |status| status.remaining_minutes.map(f64::from),
    },
    Kind {
        suffix: "layer",
        name: "Layer",
        class: None,
        unit: None,
        read: |status| status.layer.map(f64::from),
    },
    Kind {
        suffix: "layers",
        name: "Total layers",
        class: None,
        unit: None,
        read: |status| status.total_layers.map(f64::from),
    },
    Kind {
        suffix: "nozzle",
        name: "Nozzle temperature",
        class: Some(SensorClass::Temperature),
        unit: Some("°C"),
        read: |status| status.nozzle,
    },
    Kind {
        suffix: "bed",
        name: "Bed temperature",
        class: Some(SensorClass::Temperature),
        unit: Some("°C"),
        read: |status| status.bed,
    },
];

fn sensor(
    device: &UniqueId,
    unique_id: String,
    label: Option<&str>,
    capabilities: SensorCapabilities,
) -> Result<EntityDescription, String> {
    Ok(EntityDescription {
        unique_id: id(unique_id)?,
        name: label.map(name).transpose()?,
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(capabilities),
        entity_category: None,
    })
}

fn text(options: Vec<String>) -> SensorCapabilities {
    SensorCapabilities {
        value_type: SensorValueType::Text,
        device_class: None,
        unit: None,
        state_class: None,
        options,
    }
}

/// Units count from 1 for people. The single-slot units' ids start at 128.
fn slot_name(unit: u8, slot: u8) -> String {
    match unit {
        128.. => format!("Filament unit HT {}", unit - 127),
        _ => format!("Filament unit {} slot {}", unit + 1, u16::from(slot) + 1),
    }
}

fn slot_id(serial: &str, unit: u8, slot: u8) -> String {
    format!("{serial}-unit{unit}-slot{slot}")
}

pub fn describe(serial: &str, status: &Status) -> Result<Described, String> {
    let device = id(serial.to_owned())?;
    let tail = &serial[serial.len().saturating_sub(4)..];
    let mut entities = vec![sensor(
        &device,
        format!("{serial}-state"),
        // The device's main feature: it takes the device's name.
        None,
        text(
            Stage::ALL
                .iter()
                .map(|stage| stage.as_str().to_owned())
                .collect(),
        ),
    )?];
    for kind in &NUMBERS {
        let capabilities = SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: kind.class,
            unit: kind.unit.map(str::to_owned),
            state_class: Some(StateClass::Measurement),
            options: Vec::new(),
        };
        let unique_id = format!("{serial}-{}", kind.suffix);
        entities.push(sensor(&device, unique_id, Some(kind.name), capabilities)?);
    }
    for (unit, slots) in &status.units {
        for slot in slots.keys() {
            let label = slot_name(*unit, *slot);
            let unique_id = slot_id(serial, *unit, *slot);
            entities.push(sensor(&device, unique_id, Some(&label), text(Vec::new()))?);
        }
    }
    Ok(Described {
        device: DeviceDescription {
            unique_id: device,
            name: name(&format!("Bambu Lab printer {tail}"))?,
            manufacturer: Some("Bambu Lab".to_owned()),
            model: None,
            sw_version: None,
            hw_version: None,
            serial_number: Some(serial.to_owned()),
            suggested_area: None,
            via_device_unique_id: None,
            connections: Vec::new(),
        },
        entities,
    })
}

fn said(value: SensorValue) -> Option<State> {
    Some(State::Sensor(SensorState { value }))
}

fn slot_reading(slot: &Slot) -> Result<(Option<State>, Attributes), String> {
    let state = match (&slot.material, slot.empty) {
        (_, true) => said(SensorValue::Text(EMPTY.to_owned())),
        (Some(material), false) => said(SensorValue::Text(material.clone())),
        (None, false) => None,
    };
    let key = |key: &str| AttributeKey::try_from(key).map_err(|error| error.to_string());
    let mut attributes = BTreeMap::new();
    if let Some(color) = &slot.color {
        attributes.insert(key("color")?, color.clone().into());
    }
    if let Some(remaining) = slot.remaining {
        attributes.insert(key("remaining_percent")?, remaining.into());
    }
    Ok((state, attributes))
}

/// Every entity [`describe`] names, with its state now: unknown for what the printer hasn't
/// said.
pub fn readings(serial: &str, status: &Status) -> Result<Vec<Reading>, String> {
    let stage = status
        .stage
        .and_then(|stage| said(SensorValue::Text(stage.as_str().to_owned())));
    let mut readings = vec![(id(format!("{serial}-state"))?, stage, BTreeMap::new())];
    for kind in &NUMBERS {
        let state = (kind.read)(status).and_then(|number| said(SensorValue::Number(number)));
        readings.push((
            id(format!("{serial}-{}", kind.suffix))?,
            state,
            BTreeMap::new(),
        ));
    }
    for (unit, slots) in &status.units {
        for (number, slot) in slots {
            let (state, attributes) = slot_reading(slot)?;
            readings.push((id(slot_id(serial, *unit, *number))?, state, attributes));
        }
    }
    Ok(readings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::StateReport;

    /// The test host takes anything, so what is described and reported is checked here against
    /// the rules the core applies.
    #[test]
    fn what_is_described_and_reported_passes_the_cores_own_checks() {
        let mut status = Status::default();
        assert!(status.merge(
            br##"{"print":{"command":"push_status","gcode_state":"RUNNING","mc_percent":40,
            "nozzle_temper":219.5,"ams":{"ams":[{"id":"0","tray":[{"id":"0","tray_type":"PLA",
            "tray_color":"00AE42FF","remain":80},{"id":"1"}]},{"id":"128","tray":[{"id":"0",
            "tray_type":""}]}]}}}"##
        ));
        let described = describe("00TEST0000000001", &status).expect("described");
        described.device.validate().expect("a valid device");
        let readings = readings("00TEST0000000001", &status).expect("readings");
        assert_eq!(described.entities.len(), readings.len());
        let names: Vec<_> = described.entities.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names.iter().filter(|name| name.is_none()).count(), 1);
        assert_eq!(
            names[9].as_ref().map(Name::as_str),
            Some("Filament unit HT 1")
        );
        for (entity, (unique_id, state, attributes)) in described.entities.iter().zip(readings) {
            entity.validate().expect("a valid entity");
            assert_eq!(entity.unique_id, unique_id);
            let report = StateReport {
                unique_id,
                state,
                attributes,
                caused_by: None,
                replayed: false,
            };
            report.validate().expect("a valid report");
        }
    }
}
