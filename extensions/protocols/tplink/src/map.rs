//! A reading, as Irori devices, entities and states.
//!
//! A plug is one device: its relay (named after the device), its meter's sensors, its LED and
//! its Wi-Fi signal. A strip is a device with no relay of its own, and each outlet is a device
//! reached through it, with its own switch and, when it meters, its own sensors.
//!
//! Ids are built from the MAC, and an outlet's from its position on the strip, so they survive
//! renaming in the Tapo app, a new address, and resetting the plug.

use std::collections::BTreeMap;

use irori_types::{
    Capabilities, DeviceDescription, EntityCategory, EntityDescription, Name, SensorCapabilities,
    SensorClass, SensorState, SensorValue, SensorValueType, State, StateClass, SwitchCapabilities,
    SwitchClass, SwitchState, UniqueId,
};

use crate::device::{Child, Metered, Snapshot};
use crate::smart::{Info, Meter};

#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    pub devices: Vec<DeviceDescription>,
    pub entities: Vec<EntityDescription>,
}

impl Described {
    pub fn device_ids(&self) -> Vec<UniqueId> {
        self.devices
            .iter()
            .map(|device| device.unique_id.clone())
            .collect()
    }
}

struct SensorKind {
    suffix: &'static str,
    name: &'static str,
    class: SensorClass,
    unit: &'static str,
    state_class: StateClass,
    category: Option<EntityCategory>,
}

const POWER: SensorKind = SensorKind {
    suffix: "power",
    name: "Power",
    class: SensorClass::Power,
    unit: "W",
    state_class: StateClass::Measurement,
    category: None,
};
const VOLTAGE: SensorKind = SensorKind {
    suffix: "voltage",
    name: "Voltage",
    class: SensorClass::Voltage,
    unit: "V",
    state_class: StateClass::Measurement,
    category: None,
};
const CURRENT: SensorKind = SensorKind {
    suffix: "current",
    name: "Current",
    class: SensorClass::Current,
    unit: "A",
    state_class: StateClass::Measurement,
    category: None,
};
const TODAY: SensorKind = SensorKind {
    suffix: "energy_today",
    name: "Energy today",
    class: SensorClass::Energy,
    unit: "kWh",
    state_class: StateClass::TotalIncreasing,
    category: None,
};
const TOTAL: SensorKind = SensorKind {
    suffix: "energy_total",
    name: "Total energy",
    class: SensorClass::Energy,
    unit: "kWh",
    state_class: StateClass::TotalIncreasing,
    category: None,
};
const SIGNAL: SensorKind = SensorKind {
    suffix: "signal",
    name: "Signal strength",
    class: SensorClass::SignalStrength,
    unit: "dBm",
    state_class: StateClass::Measurement,
    category: Some(EntityCategory::Diagnostic),
};

fn id(text: String) -> Result<UniqueId, String> {
    UniqueId::try_from(text).map_err(|error| error.to_string())
}

/// A name from any text: cut to a name's 100 characters, then trimmed, so a long strip name
/// with " outlet 3" on the end still fits.
fn name(text: &str) -> Result<Name, String> {
    let cut: String = text.chars().take(100).collect();
    Name::try_from(cut.trim()).map_err(|error| error.to_string())
}

/// The model without its region, `PS4(EU)` → `PS4`.
fn model(info: &Info) -> Option<String> {
    info.model
        .as_deref()
        .map(|model| model.split('(').next().unwrap_or(model).trim().to_owned())
        .filter(|model| !model.is_empty())
}

/// A device's name: its own, or its model and the end of its MAC, so six unnamed strips can
/// still be told apart.
fn device_name(info: &Info, mac: &str) -> String {
    info.name.clone().unwrap_or_else(|| {
        let model = model(info).unwrap_or_else(|| "TP-Link device".to_owned());
        format!("{model} {}", &mac[mac.len().saturating_sub(5)..])
    })
}

fn outlet_key(mac: &str, child: &Child, index: usize) -> String {
    let position = child
        .info
        .position
        .map_or_else(|| (index + 1).to_string(), |position| position.to_string());
    format!("{mac}-outlet{position}")
}

/// The sensors a device meters, with this reading's value for each: unknown when it missed.
fn readings(
    meter: Option<&Meter>,
    metered: Metered,
) -> impl Iterator<Item = (&'static SensorKind, Option<f64>)> {
    let value = |read: fn(&Meter) -> Option<f64>| meter.and_then(read);
    [
        (&POWER, metered.power, value(|m| m.power)),
        (&VOLTAGE, metered.voltage, value(|m| m.voltage)),
        (&CURRENT, metered.current, value(|m| m.current)),
        (&TODAY, metered.today, value(|m| m.today)),
        (&TOTAL, metered.total, value(|m| m.total)),
    ]
    .into_iter()
    .filter(|(_, has, _)| *has)
    .map(|(kind, _, value)| (kind, value))
}

fn sensor(device: &UniqueId, key: &str, kind: &SensorKind) -> Result<EntityDescription, String> {
    Ok(EntityDescription {
        unique_id: id(format!("{key}-{}", kind.suffix))?,
        name: Some(name(kind.name)?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: Some(kind.class),
            unit: Some(kind.unit.to_owned()),
            state_class: Some(kind.state_class),
            options: Vec::new(),
        }),
        entity_category: kind.category,
    })
}

fn relay(device: &UniqueId, key: &str) -> Result<EntityDescription, String> {
    Ok(EntityDescription {
        unique_id: id(format!("{key}-switch"))?,
        // The device's main feature: it takes the device's name.
        name: None,
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Switch(SwitchCapabilities {
            device_class: Some(SwitchClass::Outlet),
        }),
        entity_category: None,
    })
}

pub fn describe(mac: &str, snapshot: &Snapshot) -> Result<Described, String> {
    let info = &snapshot.info;
    let parent = id(mac.to_owned())?;
    let parent_name = device_name(info, mac);
    let mut devices = vec![DeviceDescription {
        unique_id: parent.clone(),
        name: name(&parent_name)?,
        manufacturer: Some("TP-Link".to_owned()),
        model: model(info),
        sw_version: info.fw_ver.clone(),
        hw_version: info.hw_ver.clone(),
        suggested_area: None,
        via_device_unique_id: None,
    }];
    let mut entities = Vec::new();
    if info.on.is_some() {
        entities.push(relay(&parent, mac)?);
    }
    for (kind, _) in readings(snapshot.meter.as_ref(), snapshot.metered) {
        entities.push(sensor(&parent, mac, kind)?);
    }
    if snapshot.led.is_some() {
        entities.push(EntityDescription {
            unique_id: id(format!("{mac}-led"))?,
            name: Some(name("LED")?),
            device_unique_id: Some(parent.clone()),
            suggested_object_id: None,
            capabilities: Capabilities::Switch(SwitchCapabilities { device_class: None }),
            entity_category: Some(EntityCategory::Config),
        });
    }
    if info.rssi.is_some() {
        entities.push(sensor(&parent, mac, &SIGNAL)?);
    }

    for (index, child) in snapshot.children.iter().enumerate() {
        let key = outlet_key(mac, child, index);
        let device = id(key.clone())?;
        let outlet_name = child.info.name.clone().unwrap_or_else(|| {
            let position = child.info.position.unwrap_or(index as u64 + 1);
            format!("{parent_name} outlet {position}")
        });
        devices.push(DeviceDescription {
            unique_id: device.clone(),
            name: name(&outlet_name)?,
            manufacturer: Some("TP-Link".to_owned()),
            model: model(&child.info).or_else(|| model(info)),
            sw_version: child.info.fw_ver.clone(),
            hw_version: child.info.hw_ver.clone(),
            suggested_area: None,
            via_device_unique_id: Some(parent.clone()),
        });
        if child.info.on.is_some() {
            entities.push(relay(&device, &key)?);
        }
        for (kind, _) in readings(child.meter.as_ref(), child.metered) {
            entities.push(sensor(&device, &key, kind)?);
        }
    }
    Ok(Described { devices, entities })
}

fn number(value: f64) -> State {
    State::Sensor(SensorState {
        value: SensorValue::Number(value),
    })
}

fn switch(on: bool) -> State {
    State::Switch(SwitchState { on })
}

/// Every entity's state in this reading. A value the device stopped giving becomes unknown.
pub fn states(mac: &str, snapshot: &Snapshot) -> Result<BTreeMap<UniqueId, Option<State>>, String> {
    let mut out = BTreeMap::new();
    let info = &snapshot.info;
    if let Some(on) = info.on {
        out.insert(id(format!("{mac}-switch"))?, Some(switch(on)));
    }
    for (kind, value) in readings(snapshot.meter.as_ref(), snapshot.metered) {
        out.insert(id(format!("{mac}-{}", kind.suffix))?, value.map(number));
    }
    if let Some(led) = snapshot.led {
        out.insert(id(format!("{mac}-led"))?, Some(switch(led)));
    }
    if let Some(rssi) = info.rssi {
        out.insert(id(format!("{mac}-{}", SIGNAL.suffix))?, Some(number(rssi)));
    }
    for (index, child) in snapshot.children.iter().enumerate() {
        let key = outlet_key(mac, child, index);
        if let Some(on) = child.info.on {
            out.insert(id(format!("{key}-switch"))?, Some(switch(on)));
        }
        for (kind, value) in readings(child.meter.as_ref(), child.metered) {
            out.insert(id(format!("{key}-{}", kind.suffix))?, value.map(number));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: Option<&str>, on: Option<bool>, position: Option<u64>) -> Info {
        Info {
            device_id: "ID".into(),
            name: name.map(str::to_owned),
            model: Some("PS4(EU)".into()),
            fw_ver: Some("1.0.0".into()),
            hw_ver: Some("1.0".into()),
            mac: None,
            on,
            rssi: None,
            position,
        }
    }

    fn strip() -> Snapshot {
        Snapshot {
            info: Info {
                rssi: Some(-52.0),
                ..info(None, None, None)
            },
            meter: None,
            metered: Metered::default(),
            led: Some(true),
            children: vec![
                Child {
                    info: info(Some("Desk Lamp"), Some(true), Some(2)),
                    meter: Some(Meter {
                        power: Some(2900.0),
                        today: Some(0.4),
                        ..Meter::default()
                    }),
                    metered: Metered {
                        power: true,
                        today: true,
                        ..Metered::default()
                    },
                },
                Child {
                    info: info(None, Some(false), Some(1)),
                    meter: None,
                    metered: Metered::default(),
                },
            ],
        }
    }

    #[test]
    fn a_strip_is_a_device_with_an_outlet_device_per_socket() {
        let described = describe("02:00:00:00:00:03", &strip()).expect("valid");
        let names: Vec<&str> = described.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["PS4 00:03", "Desk Lamp", "PS4 00:03 outlet 1"]);
        assert_eq!(
            described.devices[1].unique_id.as_str(),
            "02:00:00:00:00:03-outlet2"
        );
        assert_eq!(
            described.devices[1]
                .via_device_unique_id
                .as_ref()
                .map(UniqueId::as_str),
            Some("02:00:00:00:00:03")
        );
        assert_eq!(described.devices[1].model.as_deref(), Some("PS4"));
        let ids: Vec<&str> = described
            .entities
            .iter()
            .map(|e| e.unique_id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "02:00:00:00:00:03-led",
                "02:00:00:00:00:03-signal",
                "02:00:00:00:00:03-outlet2-switch",
                "02:00:00:00:00:03-outlet2-power",
                "02:00:00:00:00:03-outlet2-energy_today",
                "02:00:00:00:00:03-outlet1-switch",
            ]
        );
    }

    #[test]
    fn a_long_strip_name_still_names_its_outlets() {
        let mut snapshot = strip();
        snapshot.info.name = Some(format!("{} x", "a".repeat(98)));
        let described = describe("02:00:00:00:00:03", &snapshot).expect("valid");
        let unnamed = described.devices[2].name.as_str();
        assert_eq!(unnamed.chars().count(), 100);
        assert_eq!(
            unnamed,
            format!("{} x outlet", "a".repeat(98))
                .chars()
                .take(100)
                .collect::<String>()
                .trim()
        );
    }

    #[test]
    fn states_cover_every_entity_described() {
        let mac = "02:00:00:00:00:03";
        let described = describe(mac, &strip()).expect("valid");
        let states = states(mac, &strip()).expect("valid");
        for entity in &described.entities {
            assert!(
                states.contains_key(&entity.unique_id),
                "{}",
                entity.unique_id.as_str()
            );
        }
        assert_eq!(
            states[&UniqueId::try_from("02:00:00:00:00:03-outlet2-power").expect("id")],
            Some(number(2900.0))
        );
        // A meter value the outlet has never given isn't a sensor at all.
        assert!(
            !states.contains_key(
                &UniqueId::try_from("02:00:00:00:00:03-outlet2-voltage").expect("id")
            )
        );
    }

    #[test]
    fn a_missed_reading_keeps_the_sensors_and_makes_them_unknown() {
        let mac = "02:00:00:00:00:03";
        let read = strip();
        let mut missed = strip();
        missed.children[0].meter = None;
        assert_eq!(
            describe(mac, &missed).expect("valid"),
            describe(mac, &read).expect("valid")
        );
        let power = UniqueId::try_from("02:00:00:00:00:03-outlet2-power").expect("id");
        assert_eq!(states(mac, &missed).expect("valid")[&power], None);
    }

    #[test]
    fn a_plug_has_its_relay_under_its_own_name() {
        let plug = Snapshot {
            info: info(Some("Lamp"), Some(true), None),
            meter: Some(Meter {
                power: Some(80.5),
                voltage: Some(241.2),
                current: Some(0.4),
                today: Some(1.2),
                total: Some(300.0),
            }),
            metered: Metered {
                power: true,
                voltage: true,
                current: true,
                today: true,
                total: true,
            },
            led: None,
            children: Vec::new(),
        };
        let described = describe("02:00:00:00:00:01", &plug).expect("valid");
        assert_eq!(described.devices.len(), 1);
        assert_eq!(described.devices[0].name.as_str(), "Lamp");
        assert_eq!(described.entities[0].name, None);
        assert_eq!(described.entities.len(), 6);
    }
}
