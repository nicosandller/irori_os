//! A Matter node, in Irori's terms: its devices, their entities, and what each entity reads.
//!
//! The rules follow Home Assistant's Matter integration (`homeassistant/components/matter/`,
//! Apache-2.0): the endpoint's device type decides what its On/Off cluster is (a light, a plug,
//! an appliance's power), and each measurement cluster is a sensor in the units its spec gives.
//!
//! - **Devices.** The node is one device. A bridge's bridged endpoints (those with Bridged Device
//!   Basic Information, cluster 0x0039) are a device each, reached through the node. An
//!   endpoint in a bridged endpoint's parts list belongs to that bridged device.
//! - **Ids.** A node device is `<compressed fabric id>-<node id>` in hex, as Home Assistant's.
//!   A bridged device adds the bridged device's own UniqueID, or `ep<N>` when it gives none.
//!   An entity adds its endpoint and what it is: `…-1-light`, `…-1-temperature`.

mod climate;
mod cover;
mod fan;
mod light;
mod lock;
pub mod update;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::client::SoftwareUpdate;
use crate::model::{Command, Node, NodeId, Path, cluster, device_type};
use irori_types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities,
    ClimateCapabilities, CoverCapabilities, DeviceDescription, EntityCategory, EntityDescription,
    FanCapabilities, HvacMode, LightCapabilities, LockCapabilities, Name, ObjectId,
    SelectCapabilities, SelectState, SensorCapabilities, SensorClass, SensorState, SensorValue,
    SensorValueType, Service, State, StateClass, SwitchCapabilities, SwitchClass, SwitchState,
    UniqueId, Unmodeled,
};

/// Everything a node is, in Irori's terms.
#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    /// The node's own device first, then any it bridges, so each `via` is described first.
    pub devices: Vec<DeviceDescription>,
    /// Bridged devices the bridge says it can't reach right now (Bridged Device Basic
    /// Information's Reachable): a bulb switched off at the wall, a sensor out of range.
    pub unreachable: BTreeSet<UniqueId>,
    pub entities: Vec<Mapped>,
    pub unmodeled: Vec<Unmodeled>,
}

/// One entity, and where its state comes from.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub description: EntityDescription,
    pub endpoint: u16,
    pub reading: Reading,
}

/// What an entity's state is read from, and what it's commanded through.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    Light(LightCapabilities),
    Switch(SwitchClass),
    Sensor(Measure),
    Binary(Binary),
    Lock(LockCapabilities),
    Cover(CoverCapabilities),
    Climate(ClimateCapabilities),
    Fan(FanCapabilities),
    /// What its On/Off cluster does when power comes back (StartUpOnOff).
    PowerOn,
    /// The node's firmware.
    Update,
}

/// On/Off's StartUpOnOff: what a light or plug does when power comes back after a cut.
const START_UP_ON_OFF: u32 = 0x4003;

/// The power-on choices, as Home Assistant names them, and StartUpOnOff's value for each:
/// `null` is "as it was before the power went".
const POWER_ON: &[(&str, Option<u64>)] = &[
    ("On", Some(1)),
    ("Off", Some(0)),
    ("Toggle", Some(2)),
    ("Previous", None),
];

/// What an entity's state needs beside the node: what the Matter server found newer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Known<'a> {
    pub update: Option<&'a SoftwareUpdate>,
}

/// The node device's id.
pub fn node_device(fabric: u64, node: NodeId) -> UniqueId {
    UniqueId::try_from(format!("{fabric:016X}-{node:016X}")).expect("hex is a valid id")
}

/// Whether `unique_id` is a device of `node` on `fabric`: the node's own or one it bridges.
pub fn belongs_to(unique_id: &UniqueId, fabric: u64, node: NodeId) -> bool {
    let own = node_device(fabric, node);
    unique_id == &own
        || unique_id
            .as_str()
            .strip_prefix(own.as_str())
            .is_some_and(|rest| rest.starts_with('-'))
}

/// The light device types: Home Assistant's light schema, where an On/Off cluster is a light.
const LIGHTS: &[u32] = &[
    device_type::ON_OFF_LIGHT,
    device_type::DIMMABLE_LIGHT,
    device_type::COLOR_TEMPERATURE_LIGHT,
    device_type::EXTENDED_COLOR_LIGHT,
    device_type::DIMMABLE_PLUG_IN_UNIT,
    device_type::MOUNTED_DIMMABLE_LOAD_CONTROL,
    device_type::DIMMER_SWITCH,
    device_type::COLOR_DIMMER_SWITCH,
];

/// Appliances whose On/Off cluster is the appliance's power (Home Assistant's "Power" switch):
/// air purifier, video players, cooktops, dishwasher, hood, fan, laundry, oven, pump,
/// refrigerator, robot vacuum, air conditioner.
const APPLIANCES: &[u32] = &[
    0x002D, 0x0028, 0x0023, 0x0077, 0x0078, 0x0075, 0x007A, 0x002B, 0x007C, 0x0073, 0x007B, 0x0303,
    0x0304, 0x0070, 0x0074, 0x0072,
];

/// Devices whose own entity locks, moves or heats: an On/Off cluster beside it isn't a switch
/// anyone would use. Door lock, window covering, thermostat.
const NOT_SWITCHES: &[u32] = &[0x000A, 0x0202, 0x0301];

/// A speaker's On/Off cluster is its mute, which isn't a switch Irori can show the right way
/// round yet.
const SPEAKER: u32 = 0x0022;

/// Clusters for things Irori has a kind for but this extension doesn't read yet.
const NOT_YET: &[(u32, &str)] = &[
    (cluster::SWITCH, "event"),
    (cluster::VALVE, "valve"),
    (cluster::SMOKE_CO_ALARM, "smoke_co_alarm"),
];

/// A number a cluster measures, and how it becomes Irori's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Temperature,
    Humidity,
    Illuminance,
    Pressure,
    Flow,
    Battery,
    Power,
    Voltage,
    Current,
    Energy,
}

struct MeasureInfo {
    measure: Measure,
    cluster: u32,
    attribute: u32,
    /// Read instead when the device doesn't report `attribute`: a plug that measures RMS
    /// voltage and current reports those, not the instantaneous ones.
    fallback: Option<u32>,
    key: &'static str,
    name: &'static str,
    class: SensorClass,
    unit: &'static str,
    state_class: StateClass,
    diagnostic: bool,
}

const MEASURES: &[MeasureInfo] = &[
    MeasureInfo {
        measure: Measure::Temperature,
        cluster: cluster::TEMPERATURE,
        attribute: 0,
        fallback: None,
        key: "temperature",
        name: "Temperature",
        class: SensorClass::Temperature,
        unit: "°C",
        state_class: StateClass::Measurement,
        diagnostic: false,
    },
    MeasureInfo {
        measure: Measure::Humidity,
        cluster: cluster::HUMIDITY,
        attribute: 0,
        fallback: None,
        key: "humidity",
        name: "Humidity",
        class: SensorClass::Humidity,
        unit: "%",
        state_class: StateClass::Measurement,
        diagnostic: false,
    },
    MeasureInfo {
        measure: Measure::Illuminance,
        cluster: cluster::ILLUMINANCE,
        attribute: 0,
        fallback: None,
        key: "illuminance",
        name: "Illuminance",
        class: SensorClass::Illuminance,
        unit: "lx",
        state_class: StateClass::Measurement,
        diagnostic: false,
    },
    MeasureInfo {
        measure: Measure::Pressure,
        cluster: cluster::PRESSURE,
        attribute: 0,
        fallback: None,
        key: "pressure",
        name: "Pressure",
        class: SensorClass::Pressure,
        unit: "kPa",
        state_class: StateClass::Measurement,
        diagnostic: false,
    },
    MeasureInfo {
        measure: Measure::Flow,
        cluster: cluster::FLOW,
        attribute: 0,
        fallback: None,
        key: "flow",
        name: "Flow",
        class: SensorClass::VolumeFlowRate,
        unit: "m³/h",
        state_class: StateClass::Measurement,
        diagnostic: false,
    },
    MeasureInfo {
        measure: Measure::Battery,
        cluster: cluster::POWER_SOURCE,
        attribute: 12,
        fallback: None,
        key: "battery",
        name: "Battery",
        class: SensorClass::Battery,
        unit: "%",
        state_class: StateClass::Measurement,
        diagnostic: true,
    },
    MeasureInfo {
        measure: Measure::Power,
        cluster: cluster::ELECTRICAL_POWER,
        attribute: 8,
        fallback: None,
        key: "power",
        name: "Power",
        class: SensorClass::Power,
        unit: "W",
        state_class: StateClass::Measurement,
        diagnostic: false,
    },
    MeasureInfo {
        measure: Measure::Voltage,
        cluster: cluster::ELECTRICAL_POWER,
        attribute: 4,
        fallback: Some(0x000B),
        key: "voltage",
        name: "Voltage",
        class: SensorClass::Voltage,
        unit: "V",
        state_class: StateClass::Measurement,
        diagnostic: true,
    },
    MeasureInfo {
        measure: Measure::Current,
        cluster: cluster::ELECTRICAL_POWER,
        attribute: 5,
        fallback: Some(0x000C),
        key: "current",
        name: "Current",
        class: SensorClass::Current,
        unit: "A",
        state_class: StateClass::Measurement,
        diagnostic: true,
    },
    MeasureInfo {
        measure: Measure::Energy,
        cluster: cluster::ELECTRICAL_ENERGY,
        attribute: 1,
        fallback: None,
        key: "energy",
        name: "Energy",
        class: SensorClass::Energy,
        unit: "kWh",
        state_class: StateClass::TotalIncreasing,
        diagnostic: false,
    },
];

fn info(measure: Measure) -> &'static MeasureInfo {
    MEASURES
        .iter()
        .find(|info| info.measure == measure)
        .expect("every measure is listed")
}

/// A yes-or-no a cluster reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    Occupancy,
    /// Boolean State on a contact sensor: true is closed, so it's read the other way round.
    Contact,
    Leak,
    Freeze,
    Rain,
}

/// Everything the node is. `updates`: its firmware is offered as an `update` entity, when it
/// can take new firmware.
pub fn describe(fabric: u64, node: &Node, updates: bool) -> Described {
    let own = node_device(fabric, node.id);
    let bridged: Vec<u16> = node
        .endpoints()
        .into_iter()
        .filter(|&endpoint| {
            endpoint != 0
                && (node.is(endpoint, device_type::BRIDGED_NODE)
                    || node.has_cluster(endpoint, cluster::BRIDGED_DEVICE_BASIC_INFORMATION))
        })
        .collect();
    let bridged_ids: BTreeMap<u16, UniqueId> = bridged
        .iter()
        .map(|&endpoint| (endpoint, bridged_device(&own, node, endpoint)))
        .collect();

    let mut devices = vec![device(
        node,
        0,
        cluster::BASIC_INFORMATION,
        own.clone(),
        None,
    )];
    for (&endpoint, unique_id) in &bridged_ids {
        devices.push(device(
            node,
            endpoint,
            cluster::BRIDGED_DEVICE_BASIC_INFORMATION,
            unique_id.clone(),
            Some(own.clone()),
        ));
    }

    let unreachable = bridged_ids
        .iter()
        .filter(|&(&endpoint, _)| {
            node.value(endpoint, cluster::BRIDGED_DEVICE_BASIC_INFORMATION, 17)
                .and_then(serde_json::Value::as_bool)
                == Some(false)
        })
        .map(|(_, unique_id)| unique_id.clone())
        .collect();

    let owner = |endpoint: u16| -> &UniqueId {
        bridged_ids
            .get(&endpoint)
            .or_else(|| {
                bridged
                    .iter()
                    .find(|&&bridge| node.parts(bridge).contains(&endpoint))
                    .and_then(|bridge| bridged_ids.get(bridge))
            })
            .unwrap_or(&own)
    };

    let mut found: Vec<(UniqueId, u16, &'static str, Option<&'static str>, Reading)> = Vec::new();
    let mut unmodeled = Vec::new();
    for endpoint in node.endpoints() {
        let device = owner(endpoint).clone();
        let types = node.device_types(endpoint);
        let is_any = |list: &[u32]| types.iter().any(|t| list.contains(t));
        let switched = found.len();
        if node.has(endpoint, cluster::ON_OFF, 0) {
            if is_any(LIGHTS) {
                found.push((
                    device.clone(),
                    endpoint,
                    "light",
                    None,
                    light::reading(node, endpoint),
                ));
            } else if is_any(APPLIANCES) {
                found.push((
                    device.clone(),
                    endpoint,
                    "power",
                    Some("Power"),
                    Reading::Switch(SwitchClass::Switch),
                ));
            } else if !types.contains(&SPEAKER) && !is_any(NOT_SWITCHES) {
                // A plug is an outlet; an in-wall switch and the rest switch whatever's wired.
                let class = if types.contains(&device_type::ON_OFF_PLUG_IN_UNIT) {
                    SwitchClass::Outlet
                } else {
                    SwitchClass::Switch
                };
                found.push((
                    device.clone(),
                    endpoint,
                    "switch",
                    None,
                    Reading::Switch(class),
                ));
            }
        }
        // What a light or plug does when power comes back, beside the entity that switches it.
        if found.len() > switched && node.has(endpoint, cluster::ON_OFF, START_UP_ON_OFF) {
            found.push((
                device.clone(),
                endpoint,
                "power_on",
                Some("Power-on behaviour"),
                Reading::PowerOn,
            ));
        }
        for measure in MEASURES {
            let read = |attribute| node.has(endpoint, measure.cluster, attribute);
            if read(measure.attribute) || measure.fallback.is_some_and(read) {
                found.push((
                    device.clone(),
                    endpoint,
                    measure.key,
                    Some(measure.name),
                    Reading::Sensor(measure.measure),
                ));
            }
        }
        if node.has(endpoint, cluster::OCCUPANCY, 0) {
            found.push((
                device.clone(),
                endpoint,
                "occupancy",
                Some("Occupancy"),
                Reading::Binary(Binary::Occupancy),
            ));
        }
        if node.has(endpoint, cluster::BOOLEAN_STATE, 0) {
            let binary = [
                (
                    device_type::CONTACT_SENSOR,
                    Binary::Contact,
                    "contact",
                    "Door",
                ),
                (
                    device_type::WATER_LEAK_DETECTOR,
                    Binary::Leak,
                    "leak",
                    "Water leak",
                ),
                (
                    device_type::WATER_FREEZE_DETECTOR,
                    Binary::Freeze,
                    "freeze",
                    "Freeze",
                ),
                (device_type::RAIN_SENSOR, Binary::Rain, "rain", "Rain"),
            ]
            .into_iter()
            .find(|(of, ..)| types.contains(of));
            if let Some((_, binary, key, name)) = binary {
                found.push((
                    device.clone(),
                    endpoint,
                    key,
                    Some(name),
                    Reading::Binary(binary),
                ));
            }
        }
        if node.has(endpoint, cluster::DOOR_LOCK, 0) {
            found.push((
                device.clone(),
                endpoint,
                "lock",
                None,
                Reading::Lock(lock::capabilities(node, endpoint)),
            ));
        }
        if node.has_cluster(endpoint, cluster::WINDOW_COVERING) {
            found.push((
                device.clone(),
                endpoint,
                "cover",
                None,
                Reading::Cover(cover::capabilities(node, endpoint)),
            ));
        }
        if node.has(endpoint, cluster::THERMOSTAT, 0x001C) {
            found.push((
                device.clone(),
                endpoint,
                "climate",
                None,
                Reading::Climate(climate::capabilities(node, endpoint)),
            ));
        }
        if node.has(endpoint, cluster::FAN_CONTROL, 0) {
            found.push((
                device.clone(),
                endpoint,
                "fan",
                None,
                Reading::Fan(fan::capabilities(node, endpoint)),
            ));
        }
        if endpoint == 0 && updates && update::has_requestor(node) {
            found.push((
                own.clone(),
                0,
                "firmware",
                Some("Firmware"),
                Reading::Update,
            ));
        }
        for &(of, platform) in NOT_YET {
            if node.has_cluster(endpoint, of) {
                unmodeled.push(Unmodeled {
                    device_unique_id: Some(device.clone()),
                    platform: ObjectId::try_from(platform).expect("a slug"),
                    name: None,
                    reason: Some("not read from Matter devices yet".to_owned()),
                });
            }
        }
    }

    // A device with one light or one switch shows it under the device's own name. With more,
    // and for any name two entities would share, the endpoint tells them apart.
    let count = |device: &UniqueId, key: &str| {
        found
            .iter()
            .filter(|(d, _, k, ..)| d == device && *k == key)
            .count()
    };
    let entities = found
        .iter()
        .map(|(device, endpoint, key, name, reading)| {
            let shared = count(device, key) > 1;
            let name = match (name, shared) {
                (None, false) => None,
                (None, true) => Some(format!("{} {endpoint}", title(key))),
                (Some(name), false) => Some((*name).to_owned()),
                (Some(name), true) => Some(format!("{name} {endpoint}")),
            };
            Mapped {
                description: EntityDescription {
                    unique_id: UniqueId::try_from(format!("{device}-{endpoint}-{key}"))
                        .expect("built from valid parts"),
                    name: name.map(|name| Name::try_from(name).expect("a short name")),
                    device_unique_id: Some(device.clone()),
                    suggested_object_id: None,
                    capabilities: capabilities(reading),
                    entity_category: match reading {
                        Reading::Sensor(measure) if info(*measure).diagnostic => {
                            Some(EntityCategory::Diagnostic)
                        }
                        Reading::Update | Reading::PowerOn => Some(EntityCategory::Config),
                        _ => None,
                    },
                },
                endpoint: *endpoint,
                reading: reading.clone(),
            }
        })
        .collect();

    Described {
        devices,
        unreachable,
        entities,
        unmodeled,
    }
}

fn title(key: &str) -> &'static str {
    match key {
        "light" => "Light",
        "lock" => "Lock",
        "cover" => "Cover",
        "climate" => "Thermostat",
        "fan" => "Fan",
        _ => "Switch",
    }
}

/// A bridged device's id: the bridge node's, then the bridged device's own UniqueID, which
/// stays the same when the bridge renumbers its endpoints. `ep<N>` when it gives none.
fn bridged_device(own: &UniqueId, node: &Node, endpoint: u16) -> UniqueId {
    let tag = node
        .text(endpoint, cluster::BRIDGED_DEVICE_BASIC_INFORMATION, 18)
        .map(|unique| {
            unique
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
                .take(64)
                .collect::<String>()
        })
        .filter(|tag| !tag.is_empty())
        .unwrap_or_else(|| format!("ep{endpoint}"));
    UniqueId::try_from(format!("{own}-{tag}")).expect("built from valid parts")
}

/// A device's description, from Basic Information (the node) or Bridged Device Basic
/// Information (a bridged device): the same attribute ids in both.
fn device(
    node: &Node,
    endpoint: u16,
    info: u32,
    unique_id: UniqueId,
    via: Option<UniqueId>,
) -> DeviceDescription {
    let text = |attribute| node.text(endpoint, info, attribute).map(str::to_owned);
    // Its label, else its product's name, else what it is.
    let name = [5, 14, 3]
        .into_iter()
        .find_map(|attribute| text(attribute).and_then(|name| short_name(&name)))
        .unwrap_or_else(|| {
            let fallback = if via.is_some() {
                format!("Bridged Matter device {endpoint}")
            } else {
                format!("Matter device {}", node.id)
            };
            Name::try_from(fallback).expect("a short name")
        });
    DeviceDescription {
        unique_id,
        name,
        manufacturer: text(1),
        model: text(3).or_else(|| text(14)),
        sw_version: text(10),
        hw_version: text(8),
        serial_number: text(15),
        suggested_area: None,
        via_device_unique_id: via,
    }
}

/// A name of at most 100 characters, cut at a character boundary.
fn short_name(text: &str) -> Option<Name> {
    let cut: String = text.chars().take(100).collect();
    Name::try_from(cut.trim()).ok()
}

fn capabilities(reading: &Reading) -> Capabilities {
    match reading {
        Reading::Light(light) => Capabilities::Light(light.clone()),
        Reading::Switch(class) => Capabilities::Switch(SwitchCapabilities {
            device_class: Some(*class),
        }),
        Reading::Sensor(measure) => {
            let info = info(*measure);
            Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Number,
                device_class: Some(info.class),
                unit: Some(info.unit.to_owned()),
                state_class: Some(info.state_class),
                options: Vec::new(),
            })
        }
        Reading::Lock(lock) => Capabilities::Lock(lock.clone()),
        Reading::Cover(cover) => Capabilities::Cover(cover.clone()),
        Reading::Climate(climate) => Capabilities::Climate(climate.clone()),
        Reading::Fan(fan) => Capabilities::Fan(fan.clone()),
        Reading::Update => Capabilities::Update(update::capabilities()),
        Reading::PowerOn => Capabilities::Select(SelectCapabilities {
            options: POWER_ON
                .iter()
                .map(|(option, _)| (*option).to_owned())
                .collect(),
        }),
        Reading::Binary(binary) => Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: Some(match binary {
                Binary::Occupancy => BinarySensorClass::Occupancy,
                Binary::Contact => BinarySensorClass::Door,
                Binary::Leak | Binary::Rain => BinarySensorClass::Moisture,
                Binary::Freeze => BinarySensorClass::Cold,
            }),
        }),
    }
}

/// The entity's state now, or `None` when the node doesn't know it (a `null` reading).
pub fn state(node: &Node, mapped: &Mapped, known: &Known<'_>) -> Option<State> {
    let endpoint = mapped.endpoint;
    match &mapped.reading {
        Reading::Lock(_) => lock::state(node, endpoint),
        Reading::Cover(caps) => cover::state(node, endpoint, caps),
        Reading::Climate(caps) => climate::state(node, endpoint, caps),
        Reading::Fan(caps) => fan::state(node, endpoint, caps),
        Reading::Update => update::state(node, known.update),
        Reading::PowerOn => {
            // Read with `get`, not `value`: its `null` is a choice, not "not known".
            let value = node.get(endpoint, cluster::ON_OFF, START_UP_ON_OFF)?;
            let (option, _) = POWER_ON
                .iter()
                .find(|(_, raw)| *raw == value.as_u64() && (raw.is_some() || value.is_null()))?;
            Some(State::Select(SelectState {
                option: (*option).to_owned(),
            }))
        }
        Reading::Light(known) => light::state(node, endpoint, known),
        Reading::Switch(_) => Some(State::Switch(SwitchState {
            on: node.value(endpoint, cluster::ON_OFF, 0)?.as_bool()?,
        })),
        Reading::Sensor(measure) => {
            let value = reading(node, endpoint, *measure)?;
            value.is_finite().then_some(State::Sensor(SensorState {
                value: SensorValue::Number(value),
            }))
        }
        Reading::Binary(binary) => {
            let on = match binary {
                Binary::Occupancy => {
                    node.value(endpoint, cluster::OCCUPANCY, 0)?.as_u64()? & 1 == 1
                }
                Binary::Contact => !node.value(endpoint, cluster::BOOLEAN_STATE, 0)?.as_bool()?,
                Binary::Leak | Binary::Freeze | Binary::Rain => {
                    node.value(endpoint, cluster::BOOLEAN_STATE, 0)?.as_bool()?
                }
            };
            Some(State::BinarySensor(BinarySensorState { on }))
        }
    }
}

/// What a service call sends the node, in order. `last_on` is the mode a thermostat was last in
/// that wasn't off, for `climate.turn_on`. Firmware is installed and checked through the
/// Matter server itself, not with commands (`lib`).
pub fn command(
    node: &Node,
    mapped: &Mapped,
    service: &Service,
    last_on: Option<HvacMode>,
) -> Result<Vec<Command>, String> {
    let endpoint = mapped.endpoint;
    match &mapped.reading {
        Reading::Light(caps) => light::command(node, endpoint, caps, service),
        Reading::Switch(_) => match service {
            Service::SwitchTurnOn => Ok(vec![Command::invoke(
                endpoint,
                cluster::ON_OFF,
                "on",
                json!({}),
            )]),
            Service::SwitchTurnOff => Ok(vec![Command::invoke(
                endpoint,
                cluster::ON_OFF,
                "off",
                json!({}),
            )]),
            other => Err(format!("a switch doesn't take {}", other.name())),
        },
        Reading::Lock(_) => lock::command(endpoint, service),
        Reading::Cover(_) => cover::command(endpoint, service),
        Reading::Climate(caps) => {
            let current = match climate::state(node, endpoint, caps) {
                Some(State::Climate(state)) => Some(state.hvac_mode),
                _ => None,
            };
            climate::command(endpoint, caps, current, last_on, service)
        }
        Reading::Fan(_) => fan::command(node, endpoint, service),
        Reading::PowerOn => match service {
            Service::SelectSelectOption(choice) => {
                let (_, raw) = POWER_ON
                    .iter()
                    .find(|(option, _)| *option == choice.option)
                    .ok_or_else(|| format!("power-on behaviour can't be {:?}", choice.option))?;
                Ok(vec![Command::Write {
                    path: Path::new(endpoint, cluster::ON_OFF, START_UP_ON_OFF),
                    value: raw.map_or(serde_json::Value::Null, |raw| json!(raw)),
                }])
            }
            other => Err(format!("power-on behaviour doesn't take {}", other.name())),
        },
        Reading::Sensor(_) | Reading::Binary(_) | Reading::Update => {
            Err(format!("it doesn't take {}", service.name()))
        }
    }
}

/// A measurement in Irori's units, from the spec's own.
fn reading(node: &Node, endpoint: u16, measure: Measure) -> Option<f64> {
    let info = info(measure);
    if measure == Measure::Energy {
        // A struct whose field 0 is the energy in mWh.
        let energy = node.value(endpoint, info.cluster, info.attribute)?;
        return Some(round(energy.get("0")?.as_f64()? / 1_000_000.0, 3));
    }
    let raw = node
        .number(endpoint, info.cluster, info.attribute)
        .or_else(|| {
            info.fallback
                .and_then(|attribute| node.number(endpoint, info.cluster, attribute))
        })?;
    Some(match measure {
        Measure::Temperature | Measure::Humidity => round(raw / 100.0, 2),
        Measure::Illuminance => round(10_f64.powf((raw - 1.0) / 10_000.0), 1),
        Measure::Pressure | Measure::Flow => round(raw / 10.0, 1),
        Measure::Battery => round(raw / 2.0, 1),
        Measure::Power | Measure::Voltage | Measure::Current => round(raw / 1000.0, 3),
        Measure::Energy => unreachable!("read above"),
    })
}

fn round(value: f64, places: i32) -> f64 {
    let scale = 10_f64.powi(places);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{ColorMode, ColorTempRange, LightState};

    const FABRIC: u64 = 0x8E57_6D93_9A1C_2B52;

    fn fixture(name: &str) -> Node {
        let text = match name {
            "atios_knx_bridge" => include_str!("../../tests/fixtures/atios_knx_bridge.json"),
            "color_temperature_light" => {
                include_str!("../../tests/fixtures/color_temperature_light.json")
            }
            "eve_contact_sensor" => include_str!("../../tests/fixtures/eve_contact_sensor.json"),
            "eve_energy_plug" => include_str!("../../tests/fixtures/eve_energy_plug.json"),
            "extended_color_light" => {
                include_str!("../../tests/fixtures/extended_color_light.json")
            }
            "fritz_bridge" => include_str!("../../tests/fixtures/fritz_bridge.json"),
            "mock_composed_bridge" => {
                include_str!("../../tests/fixtures/mock_composed_bridge.json")
            }
            "mock_dimmable_light" => include_str!("../../tests/fixtures/mock_dimmable_light.json"),
            "mock_generic_switch" => include_str!("../../tests/fixtures/mock_generic_switch.json"),
            "mock_humidity_sensor" => {
                include_str!("../../tests/fixtures/mock_humidity_sensor.json")
            }
            "mock_leak_sensor" => include_str!("../../tests/fixtures/mock_leak_sensor.json"),
            "mock_light_sensor" => include_str!("../../tests/fixtures/mock_light_sensor.json"),
            "mock_occupancy_sensor" => {
                include_str!("../../tests/fixtures/mock_occupancy_sensor.json")
            }
            "mock_on_off_plugin_unit" => {
                include_str!("../../tests/fixtures/mock_on_off_plugin_unit.json")
            }
            "mock_onoff_light" => include_str!("../../tests/fixtures/mock_onoff_light.json"),
            "mock_pressure_sensor" => {
                include_str!("../../tests/fixtures/mock_pressure_sensor.json")
            }
            "mock_temperature_sensor" => {
                include_str!("../../tests/fixtures/mock_temperature_sensor.json")
            }
            "silabs_light_switch" => include_str!("../../tests/fixtures/silabs_light_switch.json"),
            "lab_plug" => include_str!("../../tests/fixtures/lab_plug.json"),
            "tapo_p110m" => include_str!("../../tests/fixtures/tapo_p110m.json"),
            "lab_occupancy" => include_str!("../../tests/fixtures/lab_occupancy.json"),
            "lab_switch" => include_str!("../../tests/fixtures/lab_switch.json"),
            "mock_door_lock" => include_str!("../../tests/fixtures/mock_door_lock.json"),
            "mock_door_lock_with_unbolt" => {
                include_str!("../../tests/fixtures/mock_door_lock_with_unbolt.json")
            }
            "mock_window_covering_full" => {
                include_str!("../../tests/fixtures/mock_window_covering_full.json")
            }
            "mock_window_covering_lift" => {
                include_str!("../../tests/fixtures/mock_window_covering_lift.json")
            }
            "mock_thermostat" => include_str!("../../tests/fixtures/mock_thermostat.json"),
            "mock_fan" => include_str!("../../tests/fixtures/mock_fan.json"),
            "mock_air_purifier" => include_str!("../../tests/fixtures/mock_air_purifier.json"),
            "tado_smart_radiator_thermostat_x" => {
                include_str!("../../tests/fixtures/tado_smart_radiator_thermostat_x.json")
            }
            "lab_light" => include_str!("../../tests/fixtures/lab_light.json"),
            "lab_lock" => include_str!("../../tests/fixtures/lab_lock.json"),
            "lab_blind" => include_str!("../../tests/fixtures/lab_blind.json"),
            "lab_thermostat" => include_str!("../../tests/fixtures/lab_thermostat.json"),
            "lab_fan" => include_str!("../../tests/fixtures/lab_fan.json"),
            other => panic!("no fixture {other}"),
        };
        Node::from_json(&serde_json::from_str(text).expect("a fixture is JSON")).expect("a node")
    }

    /// Each entity as `(id after the device's, name, kind, state)`.
    fn summary(node: &Node) -> Vec<(String, Option<String>, String, Option<State>)> {
        let described = describe(FABRIC, node, false);
        described
            .entities
            .iter()
            .map(|mapped| {
                let device = mapped
                    .description
                    .device_unique_id
                    .as_ref()
                    .expect("on a device");
                (
                    mapped.description.unique_id.as_str()[device.as_str().len()..].to_owned(),
                    mapped
                        .description
                        .name
                        .as_ref()
                        .map(|n| n.as_str().to_owned()),
                    mapped.description.capabilities.kind().to_string(),
                    state(node, mapped, &Known::default()),
                )
            })
            .collect()
    }

    fn light_of(on: bool) -> LightState {
        LightState {
            on,
            brightness: None,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        }
    }

    fn number(value: f64) -> Option<State> {
        Some(State::Sensor(SensorState {
            value: SensorValue::Number(value),
        }))
    }

    fn selected(option: &str) -> Option<State> {
        Some(State::Select(SelectState {
            option: option.to_owned(),
        }))
    }

    fn binary(on: bool) -> Option<State> {
        Some(State::BinarySensor(BinarySensorState { on }))
    }

    #[test]
    fn a_node_is_a_device_named_by_its_label_with_its_maker_and_firmware() {
        let node = fixture("lab_plug");
        let described = describe(FABRIC, &node, false);
        assert_eq!(described.devices.len(), 1);
        let device = &described.devices[0];
        assert_eq!(
            device.unique_id.as_str(),
            "8E576D939A1C2B52-0000000000000003"
        );
        assert_eq!(device.name.as_str(), "Lab plug");
        assert_eq!(device.manufacturer.as_deref(), Some("Irori Lab"));
        assert_eq!(device.model.as_deref(), Some("plug"));
        assert_eq!(device.via_device_unique_id, None);
        // No label: the product's name.
        let eve = describe(FABRIC, &fixture("eve_contact_sensor"), false);
        assert_eq!(eve.devices[0].name.as_str(), "Eve Door");
        assert_eq!(eve.devices[0].sw_version.as_deref(), Some("3.2.1"));
    }

    #[test]
    fn a_plug_is_a_switch_under_the_devices_own_name() {
        let on_off = |on| Some(State::Switch(SwitchState { on }));
        let switch_and_power_on = vec![
            (
                "-1-switch".to_owned(),
                None,
                "switch".to_owned(),
                on_off(false),
            ),
            (
                "-1-power_on".to_owned(),
                Some("Power-on behaviour".to_owned()),
                "select".to_owned(),
                selected("Previous"),
            ),
        ];
        assert_eq!(summary(&fixture("lab_plug")), switch_and_power_on);
        // A plug with Level Control is still a plug, as Home Assistant has it.
        assert_eq!(
            summary(&fixture("mock_on_off_plugin_unit")),
            switch_and_power_on
        );
        assert_eq!(
            summary(&fixture("eve_energy_plug"))[0],
            (
                "-1-switch".to_owned(),
                None,
                "switch".to_owned(),
                on_off(false)
            )
        );
    }

    #[test]
    fn a_plug_is_an_outlet_and_an_in_wall_switch_a_switch() {
        let class = |name: &str| match describe(FABRIC, &fixture(name), false).entities[0]
            .description
            .capabilities
        {
            Capabilities::Switch(SwitchCapabilities { device_class }) => device_class,
            ref other => panic!("{name}: {other:?}"),
        };
        assert_eq!(class("lab_plug"), Some(SwitchClass::Outlet));
        assert_eq!(class("lab_switch"), Some(SwitchClass::Switch));
        assert_eq!(
            describe(FABRIC, &fixture("lab_switch"), false).devices[0]
                .name
                .as_str(),
            "Outside wall switch"
        );
    }

    #[test]
    fn lights_dim_and_colour_as_their_clusters_say() {
        let entities = summary(&fixture("mock_onoff_light"));
        assert_eq!(
            entities,
            vec![
                (
                    "-1-light".to_owned(),
                    None,
                    "light".to_owned(),
                    Some(State::Light(light_of(true)))
                ),
                (
                    "-1-power_on".to_owned(),
                    Some("Power-on behaviour".to_owned()),
                    "select".to_owned(),
                    selected("Previous")
                )
            ]
        );

        let dimmable = summary(&fixture("mock_dimmable_light"));
        assert_eq!(
            dimmable[0].3,
            Some(State::Light(LightState {
                // Level 52 of 254.
                brightness: Some(52),
                ..light_of(true)
            }))
        );

        let node = fixture("color_temperature_light");
        let described = describe(FABRIC, &node, false);
        assert_eq!(
            described.entities[0].reading,
            Reading::Light(LightCapabilities {
                brightness: true,
                color_temp_kelvin: Some(ColorTempRange {
                    min: 2000,
                    max: 6536
                }),
                rgb: false,
            })
        );
        assert_eq!(
            state(&node, &described.entities[0], &Known::default()),
            Some(State::Light(LightState {
                brightness: Some(129),
                // 284 mireds; only colour temperature, so no mode to say.
                color_temp_kelvin: Some(3521),
                ..light_of(true)
            }))
        );

        let node = fixture("extended_color_light");
        let described = describe(FABRIC, &node, false);
        let Reading::Light(caps) = &described.entities[0].reading else {
            panic!("a light");
        };
        assert!(caps.rgb && caps.color_temp_kelvin.is_some());
        assert_eq!(
            state(&node, &described.entities[0], &Known::default()),
            Some(State::Light(LightState {
                brightness: Some(129),
                // Showing hue and saturation (mode 0): hue 36 of 254, saturation 51 of 254.
                color_mode: Some(ColorMode::Rgb),
                rgb: Some([255, 247, 204]),
                ..light_of(true)
            }))
        );
    }

    #[test]
    fn sensors_read_in_irori_units() {
        let one = |name: &str| summary(&fixture(name)).remove(0);
        assert_eq!(
            one("mock_temperature_sensor"),
            (
                "-1-temperature".to_owned(),
                Some("Temperature".to_owned()),
                "sensor".to_owned(),
                number(21.0)
            )
        );
        assert_eq!(one("mock_humidity_sensor").3, number(0.0));
        // 10^((1000 - 1) / 10000) lux.
        assert_eq!(one("mock_light_sensor").3, number(1.3));
        assert_eq!(one("mock_pressure_sensor").3, number(0.0));
        assert_eq!(
            one("mock_occupancy_sensor"),
            (
                "-1-occupancy".to_owned(),
                Some("Occupancy".to_owned()),
                "binary_sensor".to_owned(),
                binary(true)
            )
        );
        assert_eq!(one("lab_occupancy").3, binary(false));
        // A contact sensor reports true for closed: the door isn't open.
        let eve = summary(&fixture("eve_contact_sensor"));
        assert_eq!(
            eve,
            vec![
                (
                    "-1-battery".to_owned(),
                    Some("Battery".to_owned()),
                    "sensor".to_owned(),
                    number(100.0)
                ),
                (
                    "-1-contact".to_owned(),
                    Some("Door".to_owned()),
                    "binary_sensor".to_owned(),
                    binary(true)
                ),
            ]
        );
        assert_eq!(one("mock_leak_sensor").3, binary(true));
    }

    #[test]
    fn a_battery_is_a_diagnostic() {
        let described = describe(FABRIC, &fixture("eve_contact_sensor"), false);
        assert_eq!(
            described.entities[0].description.entity_category,
            Some(EntityCategory::Diagnostic)
        );
        assert_eq!(described.entities[1].description.entity_category, None);
    }

    #[test]
    fn a_bridged_device_is_its_own_device_through_the_bridge() {
        let described = describe(FABRIC, &fixture("mock_composed_bridge"), false);
        let bridge = "8E576D939A1C2B52-0000000000000063";
        assert_eq!(described.devices.len(), 2);
        assert_eq!(described.devices[0].unique_id.as_str(), bridge);
        assert_eq!(described.devices[0].name.as_str(), "Mock Bridge");
        let plug = &described.devices[1];
        // Named by its own UniqueID, which stays when the bridge renumbers.
        assert_eq!(
            plug.unique_id.as_str(),
            format!("{bridge}-0011223344556677")
        );
        assert_eq!(plug.name.as_str(), "Kitchen Plug");
        assert_eq!(plug.model.as_deref(), Some("Mock Bridged Plug"));
        assert_eq!(
            plug.via_device_unique_id.as_ref().map(UniqueId::as_str),
            Some(bridge)
        );
        // Its parts are its entities, not the bridge's.
        let ids: Vec<&str> = described
            .entities
            .iter()
            .map(|mapped| mapped.description.unique_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                format!("{bridge}-0011223344556677-3-switch"),
                format!("{bridge}-0011223344556677-4-temperature"),
            ]
        );

        // No UniqueID: its endpoint, then.
        let fritz = describe(FABRIC, &fixture("fritz_bridge"), false);
        assert_eq!(
            fritz.devices[1].unique_id.as_str(),
            "8E576D939A1C2B52-0000000000000001-ep42"
        );
        assert_eq!(fritz.devices[1].name.as_str(), "my_device");
        // Its bridge says it can't reach it; the composed bridge's plug is reachable.
        assert_eq!(
            fritz.unreachable,
            BTreeSet::from([fritz.devices[1].unique_id.clone()])
        );
        assert!(described.unreachable.is_empty());
        assert!(
            fritz
                .entities
                .iter()
                .all(|mapped| mapped.description.device_unique_id.as_ref()
                    == Some(&fritz.devices[1].unique_id))
        );
        let atios = summary(&fixture("atios_knx_bridge"));
        assert_eq!(
            atios,
            vec![
                (
                    "-29-power".to_owned(),
                    Some("Power".to_owned()),
                    "sensor".to_owned(),
                    number(0.0)
                ),
                (
                    "-29-energy".to_owned(),
                    Some("Energy".to_owned()),
                    "sensor".to_owned(),
                    None
                ),
            ]
        );
    }

    #[test]
    fn buttons_are_listed_as_not_read_yet() {
        for name in ["mock_generic_switch", "silabs_light_switch"] {
            let described = describe(FABRIC, &fixture(name), false);
            assert!(described.entities.is_empty(), "{name}");
            assert_eq!(described.unmodeled.len(), 1, "{name}");
            assert_eq!(described.unmodeled[0].platform.as_str(), "event");
        }
    }

    #[test]
    fn a_device_with_two_lights_names_them_by_endpoint() {
        let mut node = fixture("mock_onoff_light");
        let copied: Vec<_> = node
            .attributes
            .iter()
            .filter(|(path, _)| path.endpoint == 1)
            .map(|(path, value)| {
                (
                    crate::model::Path {
                        endpoint: 2,
                        ..*path
                    },
                    value.clone(),
                )
            })
            .collect();
        node.attributes.extend(copied);
        let names: Vec<Option<String>> = summary(&node).into_iter().map(|e| e.1).collect();
        assert_eq!(
            names,
            [
                "Light 1",
                "Power-on behaviour 1",
                "Light 2",
                "Power-on behaviour 2"
            ]
            .map(|name| Some(name.to_owned()))
        );
    }

    #[test]
    fn a_plug_that_measures_rms_says_its_voltage_and_current() {
        // A Tapo P110M: Electrical Power Measurement with RMS voltage and current only.
        let node = fixture("tapo_p110m");
        assert_eq!(
            summary(&node),
            vec![
                (
                    "-1-switch".to_owned(),
                    None,
                    "switch".to_owned(),
                    Some(State::Switch(SwitchState { on: false }))
                ),
                (
                    "-1-power_on".to_owned(),
                    Some("Power-on behaviour".to_owned()),
                    "select".to_owned(),
                    selected("Off")
                ),
                (
                    "-1-power".to_owned(),
                    Some("Power".to_owned()),
                    "sensor".to_owned(),
                    number(0.0)
                ),
                (
                    "-1-voltage".to_owned(),
                    Some("Voltage".to_owned()),
                    "sensor".to_owned(),
                    number(101.442)
                ),
                (
                    "-1-current".to_owned(),
                    Some("Current".to_owned()),
                    "sensor".to_owned(),
                    number(0.015)
                ),
                (
                    "-1-energy".to_owned(),
                    Some("Energy".to_owned()),
                    "sensor".to_owned(),
                    number(123.203)
                ),
            ]
        );
        let described = describe(FABRIC, &node, false);
        let category = |key: &str| {
            described
                .entities
                .iter()
                .find(|mapped| mapped.description.unique_id.as_str().ends_with(key))
                .and_then(|mapped| mapped.description.entity_category)
        };
        // As Home Assistant has them: the readings an electrician wants are diagnostics.
        assert_eq!(category("-voltage"), Some(EntityCategory::Diagnostic));
        assert_eq!(category("-current"), Some(EntityCategory::Diagnostic));
        assert_eq!(category("-power"), None);
        assert_eq!(category("-power_on"), Some(EntityCategory::Config));
        assert_eq!(
            described.devices[0].serial_number.as_deref(),
            Some("P110M00000001")
        );
    }

    #[test]
    fn power_on_behaviour_is_written_to_start_up_on_off() {
        let mut node = fixture("tapo_p110m");
        let (mapped, _) = only(&node, "select");
        let choose = |node: &Node, option: &str| {
            command(
                node,
                &mapped,
                &Service::SelectSelectOption(irori_types::SelectOption {
                    option: option.to_owned(),
                }),
                None,
            )
        };
        let path = Path::new(1, cluster::ON_OFF, START_UP_ON_OFF);
        for (option, value) in [
            ("On", json!(1)),
            ("Off", json!(0)),
            ("Toggle", json!(2)),
            ("Previous", serde_json::Value::Null),
        ] {
            assert_eq!(
                choose(&node, option),
                Ok(vec![Command::Write {
                    path,
                    value: value.clone()
                }])
            );
            // The node reporting it back is the state.
            node.set(path, value);
            assert_eq!(state(&node, &mapped, &Known::default()), selected(option));
        }
        assert!(choose(&node, "Sometimes").is_err());
        // A value the spec doesn't define isn't a choice to show.
        node.set(path, json!(7));
        assert_eq!(state(&node, &mapped, &Known::default()), None);
    }

    #[test]
    fn every_description_is_one_the_core_accepts() {
        for name in [
            "atios_knx_bridge",
            "color_temperature_light",
            "eve_contact_sensor",
            "eve_energy_plug",
            "extended_color_light",
            "fritz_bridge",
            "mock_composed_bridge",
            "mock_dimmable_light",
            "mock_generic_switch",
            "mock_humidity_sensor",
            "mock_leak_sensor",
            "mock_light_sensor",
            "mock_occupancy_sensor",
            "mock_on_off_plugin_unit",
            "mock_onoff_light",
            "mock_pressure_sensor",
            "mock_temperature_sensor",
            "silabs_light_switch",
            "lab_plug",
            "lab_occupancy",
            "lab_switch",
            "tapo_p110m",
        ] {
            let node = fixture(name);
            let described = describe(FABRIC, &node, false);
            for mapped in &described.entities {
                mapped
                    .description
                    .validate()
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                if let Some(state) = state(&node, mapped, &Known::default()) {
                    state.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
                    mapped
                        .description
                        .capabilities
                        .fits(&state)
                        .unwrap_or_else(|e| panic!("{name}: {e}"));
                }
            }
        }
    }

    #[test]
    fn a_device_belongs_to_its_node_and_no_other() {
        let own = node_device(FABRIC, 3);
        assert!(belongs_to(&own, FABRIC, 3));
        let bridged = UniqueId::try_from(format!("{own}-ep2")).expect("valid");
        assert!(belongs_to(&bridged, FABRIC, 3));
        assert!(!belongs_to(&own, FABRIC, 4));
        assert!(!belongs_to(&own, FABRIC + 1, 3));
    }

    /// The one entity of `kind` on the node, and what it reads.
    fn only(node: &Node, kind: &str) -> (Mapped, Option<State>) {
        let described = describe(FABRIC, node, false);
        let mut of_kind = described
            .entities
            .into_iter()
            .filter(|mapped| mapped.description.capabilities.kind().to_string() == kind);
        let mapped = of_kind.next().unwrap_or_else(|| panic!("no {kind}"));
        assert!(of_kind.next().is_none(), "one {kind}");
        let state = state(node, &mapped, &Known::default());
        (mapped, state)
    }

    #[test]
    fn a_lock_says_whether_it_unlatches_and_a_switch_beside_it_isnt_one() {
        let (mapped, state) = only(&fixture("mock_door_lock"), "lock");
        assert_eq!(
            mapped.description.capabilities,
            Capabilities::Lock(irori_types::LockCapabilities {
                open: false,
                requires_code: false,
                code_format: None,
            })
        );
        assert_eq!(
            state,
            Some(State::Lock(irori_types::LockState {
                state: irori_types::LockStatus::Unlocked
            }))
        );
        // Its On/Off cluster isn't offered as a switch, as Home Assistant doesn't.
        assert_eq!(
            summary(&fixture("mock_door_lock"))
                .into_iter()
                .map(|(id, ..)| id)
                .collect::<Vec<_>>(),
            vec!["-1-lock"]
        );
        let (unbolts, _) = only(&fixture("mock_door_lock_with_unbolt"), "lock");
        assert!(matches!(
            unbolts.description.capabilities,
            Capabilities::Lock(irori_types::LockCapabilities { open: true, .. })
        ));
        let (lab, state) = only(&fixture("lab_lock"), "lock");
        assert!(matches!(
            lab.description.capabilities,
            Capabilities::Lock(irori_types::LockCapabilities { open: true, .. })
        ));
        assert_eq!(
            state,
            Some(State::Lock(irori_types::LockState {
                state: irori_types::LockStatus::Locked
            }))
        );
    }

    #[test]
    fn a_covering_is_percent_open_and_only_says_where_it_is_if_it_knows() {
        let (full, state) = only(&fixture("mock_window_covering_full"), "cover");
        assert_eq!(
            full.description.capabilities,
            Capabilities::Cover(irori_types::CoverCapabilities {
                device_class: Some(irori_types::CoverClass::Shutter),
                position: true,
                tilt: true,
                stop: true,
            })
        );
        assert_eq!(
            state,
            Some(State::Cover(irori_types::CoverState {
                state: irori_types::OpenState::Open,
                position: Some(100),
                tilt: Some(100),
            }))
        );
        // Lift without Position Aware Lift: it moves, but can't say where it is.
        let (lift, state) = only(&fixture("mock_window_covering_lift"), "cover");
        assert!(matches!(
            lift.description.capabilities,
            Capabilities::Cover(irori_types::CoverCapabilities {
                device_class: Some(irori_types::CoverClass::Awning),
                position: false,
                tilt: false,
                ..
            })
        ));
        assert!(matches!(
            state,
            Some(State::Cover(irori_types::CoverState { position: None, .. }))
        ));
        let (_, state) = only(&fixture("lab_blind"), "cover");
        assert!(matches!(
            state,
            Some(State::Cover(irori_types::CoverState {
                position: Some(100),
                ..
            }))
        ));
    }

    #[test]
    fn a_thermostat_offers_the_modes_its_features_and_sequence_allow() {
        let (both, state) = only(&fixture("mock_thermostat"), "climate");
        let Capabilities::Climate(caps) = &both.description.capabilities else {
            panic!("a climate");
        };
        assert_eq!(
            caps.hvac_modes,
            vec![
                HvacMode::Off,
                HvacMode::Heat,
                HvacMode::Cool,
                HvacMode::HeatCool
            ]
        );
        assert!(caps.target_temperature_range);
        let Some(State::Climate(climate)) = state else {
            panic!("a climate state");
        };
        assert_eq!(climate.hvac_mode, HvacMode::HeatCool);
        assert_eq!(climate.hvac_action, Some(irori_types::HvacAction::Heating));
        assert_eq!(climate.current_temperature, Some(18.0));
        assert_eq!(
            (climate.target_temp_low, climate.target_temp_high),
            (Some(20.0), Some(26.0))
        );
        assert_eq!(climate.target_temperature, None);

        // A radiator valve: heat only, its own limits.
        let (valve, state) = only(&fixture("tado_smart_radiator_thermostat_x"), "climate");
        let Capabilities::Climate(caps) = &valve.description.capabilities else {
            panic!("a climate");
        };
        assert_eq!(caps.hvac_modes, vec![HvacMode::Off, HvacMode::Heat]);
        assert_eq!((caps.min_temp, caps.max_temp), (5.0, 30.0));
        assert!(!caps.target_temperature_range);
        let Some(State::Climate(climate)) = state else {
            panic!("a climate state");
        };
        assert_eq!(climate.target_temperature, Some(18.0));
        assert_eq!(climate.current_temperature, Some(20.9));

        let (_, state) = only(&fixture("lab_thermostat"), "climate");
        let Some(State::Climate(climate)) = state else {
            panic!("a climate state");
        };
        assert_eq!(climate.hvac_mode, HvacMode::Heat);
        assert_eq!(climate.target_temperature, Some(21.0));
    }

    #[test]
    fn a_fan_reads_its_speed_and_presets_and_standing_still_is_off() {
        let (fan, state) = only(&fixture("mock_fan"), "fan");
        assert_eq!(
            fan.description.capabilities,
            Capabilities::Fan(irori_types::FanCapabilities {
                speed_count: 3,
                oscillate: false,
                direction: false,
                preset_modes: vec!["sleep_wind".to_owned(), "natural_wind".to_owned()],
            })
        );
        assert!(matches!(
            state,
            Some(State::Fan(irori_types::FanState {
                on: false,
                percentage: Some(0),
                ..
            }))
        ));
        // An air purifier's fan in auto: on, at a speed it doesn't say.
        let (purifier, state) = only(&fixture("mock_air_purifier"), "fan");
        assert!(matches!(
            &purifier.description.capabilities,
            Capabilities::Fan(irori_types::FanCapabilities {
                speed_count: 10,
                oscillate: true,
                direction: true,
                preset_modes,
            }) if preset_modes == &["auto", "sleep_wind", "natural_wind"]
        ));
        assert_eq!(
            state,
            Some(State::Fan(irori_types::FanState {
                on: true,
                percentage: None,
                oscillating: Some(false),
                direction: Some(irori_types::FanDirection::Forward),
                preset_mode: Some("auto".to_owned()),
            }))
        );
    }

    #[test]
    fn each_kind_is_commanded_with_the_nodes_own_commands() {
        let send = |fixture_name: &str, kind: &str, service: Service| {
            let node = fixture(fixture_name);
            let (mapped, _) = only(&node, kind);
            command(&node, &mapped, &service, None)
        };
        assert_eq!(
            send("lab_plug", "switch", Service::SwitchTurnOn),
            Ok(vec![Command::invoke(1, cluster::ON_OFF, "on", json!({}))])
        );
        assert_eq!(
            send(
                "lab_lock",
                "lock",
                Service::LockOpen(irori_types::LockCode::default())
            ),
            Ok(vec![Command::Invoke {
                endpoint: 1,
                cluster: cluster::DOOR_LOCK,
                name: "unboltDoor",
                payload: json!({}),
                timed: true,
            }])
        );
        assert_eq!(
            send(
                "lab_blind",
                "cover",
                Service::CoverSetPosition(irori_types::SetPosition { position: 30 })
            ),
            Ok(vec![Command::invoke(
                1,
                cluster::WINDOW_COVERING,
                "goToLiftPercentage",
                json!({"liftPercent100thsValue": 7000})
            )])
        );
        assert_eq!(
            send(
                "lab_thermostat",
                "climate",
                Service::ClimateSetHvacMode(irori_types::ClimateHvacMode {
                    hvac_mode: HvacMode::Cool
                })
            ),
            Ok(vec![Command::write(1, cluster::THERMOSTAT, 0x1C, json!(3))])
        );
        assert_eq!(
            send(
                "lab_fan",
                "fan",
                Service::FanSetPercentage(irori_types::FanPercentage { percentage: 40 })
            ),
            Ok(vec![Command::write(1, cluster::FAN_CONTROL, 2, json!(40))])
        );
        assert_eq!(
            send(
                "lab_fan",
                "fan",
                Service::FanSetPresetMode(irori_types::FanPresetMode {
                    preset_mode: "auto".to_owned()
                })
            ),
            Ok(vec![Command::write(1, cluster::FAN_CONTROL, 0, json!(5))])
        );
        assert_eq!(
            send(
                "lab_fan",
                "fan",
                Service::FanOscillate(irori_types::FanOscillate { oscillating: true })
            ),
            Ok(vec![Command::write(1, cluster::FAN_CONTROL, 8, json!(1))])
        );
        // A sensor takes nothing.
        assert!(send("lab_occupancy", "binary_sensor", Service::SwitchTurnOn).is_err());
    }

    #[test]
    fn firmware_is_an_update_entity_and_says_what_the_server_found() {
        let node = fixture("mock_door_lock");
        assert!(
            !describe(FABRIC, &node, false)
                .entities
                .iter()
                .any(|mapped| mapped.reading == Reading::Update)
        );
        let described = describe(FABRIC, &node, true);
        let firmware = described
            .entities
            .iter()
            .find(|mapped| mapped.reading == Reading::Update)
            .expect("a firmware entity");
        assert_eq!(
            firmware.description.unique_id.as_str(),
            format!("{}-0-firmware", node_device(FABRIC, node.id))
        );
        assert_eq!(
            firmware.description.entity_category,
            Some(EntityCategory::Config)
        );
        let Some(State::Update(now)) = state(&node, firmware, &Known::default()) else {
            panic!("an update state");
        };
        assert!(!now.available);
        assert_eq!(now.installed_version.as_deref(), Some("1.0"));
        assert_eq!(now.latest_version.as_deref(), Some("1.0"));

        let newer = SoftwareUpdate {
            version: 2,
            version_string: "2.0".to_owned(),
            release_url: Some("https://example.com/notes".to_owned()),
        };
        let known = Known {
            update: Some(&newer),
        };
        let Some(State::Update(offered)) = state(&node, firmware, &known) else {
            panic!("an update state");
        };
        assert!(offered.available);
        assert_eq!(offered.latest_version.as_deref(), Some("2.0"));
        assert_eq!(
            offered.release_url.as_deref(),
            Some("https://example.com/notes")
        );

        // Once the node runs it, it isn't newer, even before the server forgets it.
        let mut updated = node.clone();
        updated.set(
            Path {
                endpoint: 0,
                cluster: cluster::BASIC_INFORMATION,
                attribute: 9,
            },
            json!(2),
        );
        let Some(State::Update(done)) = state(&updated, firmware, &known) else {
            panic!("an update state");
        };
        assert!(!done.available);

        // Downloading, halfway.
        let mut installing = node;
        for (attribute, value) in [(2, json!(4)), (3, json!(50))] {
            installing.set(
                Path {
                    endpoint: 0,
                    cluster: cluster::OTA_SOFTWARE_UPDATE_REQUESTOR,
                    attribute,
                },
                value,
            );
        }
        let Some(State::Update(going)) = state(&installing, firmware, &known) else {
            panic!("an update state");
        };
        assert!(going.in_progress);
        assert_eq!(going.progress, Some(50.0));
    }
}
