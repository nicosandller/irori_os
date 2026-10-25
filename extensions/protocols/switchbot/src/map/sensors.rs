//! Readings many SwitchBot devices share: the room's temperature, humidity and CO2, light, a
//! battery, motion, a door or window, a leak, power and energy.

use irori_types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, EntityCategory,
    SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, State, StateClass,
};

use super::Entity;
use crate::model::{Family, HubSensors, Reading};

pub fn entities(family: Family) -> Vec<Entity> {
    match family {
        Family::Hub(HubSensors::None) => vec![],
        Family::Hub(HubSensors::Hub2) => vec![temperature(), humidity(), light_level()],
        Family::Hub(HubSensors::Hub3) => vec![temperature(), humidity(), light_level(), motion()],
        Family::Meter { co2 } => {
            let mut list = vec![temperature(), humidity()];
            if co2 {
                list.push(co2_sensor());
            }
            list.push(battery());
            list
        }
        Family::Contact => vec![
            binary("contact", "Door", BinarySensorClass::Door),
            motion(),
            binary("bright", "Light", BinarySensorClass::Light),
            battery(),
        ],
        Family::Motion => vec![
            motion(),
            binary("bright", "Light", BinarySensorClass::Light),
            battery(),
        ],
        Family::Presence => vec![
            binary("occupied", "Presence", BinarySensorClass::Occupancy),
            light_level(),
            battery(),
        ],
        Family::Leak => vec![
            binary("leak", "Leak", BinarySensorClass::Moisture),
            battery(),
        ],
        _ => vec![],
    }
}

/// The state of a shared reading, by its key.
pub fn state(key: &str, reading: &Reading) -> Option<State> {
    let number = |value: Option<f64>| {
        value.map(|value| {
            State::Sensor(SensorState {
                value: SensorValue::Number(value),
            })
        })
    };
    let on = |value: Option<bool>| value.map(|on| State::BinarySensor(BinarySensorState { on }));
    match key {
        "temperature" => number(reading.temperature),
        "humidity" => number(reading.humidity),
        "co2" => number(reading.co2),
        "light_level" => number(reading.light_level),
        "battery" => number(reading.battery),
        "power" => number(reading.power),
        "power_2" => number(reading.power_2),
        "voltage" => number(reading.voltage),
        "voltage_2" => number(reading.voltage_2),
        "current" => number(reading.current),
        "current_2" => number(reading.current_2),
        "energy_today" => number(reading.energy_today),
        "energy_today_2" => number(reading.energy_today_2),
        "motion" => on(reading.motion),
        "contact" => on(reading.contact_open),
        "bright" => on(reading.bright),
        "occupied" => on(reading.occupied),
        "leak" => on(reading.leak),
        "door" => on(reading.door_open),
        // A problem when it isn't calibrated.
        "calibration" => on(reading.calibrated.map(|calibrated| !calibrated)),
        "water_tank" => on(reading.lack_water),
        _ => None,
    }
}

fn measured(
    key: &'static str,
    name: &'static str,
    class: Option<SensorClass>,
    unit: Option<&str>,
    state_class: StateClass,
) -> Entity {
    Entity {
        key,
        name: Some(name),
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: class,
            unit: unit.map(str::to_owned),
            state_class: Some(state_class),
            options: Vec::new(),
        }),
        category: None,
    }
}

pub fn binary(key: &'static str, name: &'static str, class: BinarySensorClass) -> Entity {
    Entity {
        key,
        name: Some(name),
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: Some(class),
        }),
        category: None,
    }
}

pub fn temperature() -> Entity {
    measured(
        "temperature",
        "Temperature",
        Some(SensorClass::Temperature),
        Some("°C"),
        StateClass::Measurement,
    )
}

pub fn humidity() -> Entity {
    measured(
        "humidity",
        "Humidity",
        Some(SensorClass::Humidity),
        Some("%"),
        StateClass::Measurement,
    )
}

fn co2_sensor() -> Entity {
    measured(
        "co2",
        "Carbon dioxide",
        Some(SensorClass::Co2),
        Some("ppm"),
        StateClass::Measurement,
    )
}

/// SwitchBot's own 1-20 (or 1-10) scale, not lux: no class, no unit.
fn light_level() -> Entity {
    measured(
        "light_level",
        "Light level",
        None,
        None,
        StateClass::Measurement,
    )
}

pub fn battery() -> Entity {
    Entity {
        category: Some(EntityCategory::Diagnostic),
        ..measured(
            "battery",
            "Battery",
            Some(SensorClass::Battery),
            Some("%"),
            StateClass::Measurement,
        )
    }
}

fn motion() -> Entity {
    binary("motion", "Motion", BinarySensorClass::Motion)
}

/// Which of a device's meters: a plug's only one, or a Relay Switch 2PM's first or second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Only,
    First,
    Second,
}

/// Power, voltage, current and today's energy for one channel.
pub fn metered(channel: Channel) -> Vec<Entity> {
    let [power, voltage, current, energy] = match channel {
        Channel::Only | Channel::First => ["power", "voltage", "current", "energy_today"],
        Channel::Second => ["power_2", "voltage_2", "current_2", "energy_today_2"],
    };
    let names = match channel {
        Channel::Only => ["Power", "Voltage", "Current", "Energy today"],
        Channel::First => [
            "Channel 1 power",
            "Channel 1 voltage",
            "Channel 1 current",
            "Channel 1 energy today",
        ],
        Channel::Second => [
            "Channel 2 power",
            "Channel 2 voltage",
            "Channel 2 current",
            "Channel 2 energy today",
        ],
    };
    vec![
        measured(
            power,
            names[0],
            Some(SensorClass::Power),
            Some("W"),
            StateClass::Measurement,
        ),
        Entity {
            category: Some(EntityCategory::Diagnostic),
            ..measured(
                voltage,
                names[1],
                Some(SensorClass::Voltage),
                Some("V"),
                StateClass::Measurement,
            )
        },
        Entity {
            category: Some(EntityCategory::Diagnostic),
            ..measured(
                current,
                names[2],
                Some(SensorClass::Current),
                Some("A"),
                StateClass::Measurement,
            )
        },
        // SwitchBot starts it again each day: an increasing total that resets.
        measured(
            energy,
            names[3],
            Some(SensorClass::Energy),
            Some("Wh"),
            StateClass::TotalIncreasing,
        ),
    ]
}

/// Whether a device's open and closed positions are set. A problem when they aren't.
pub fn calibration() -> Entity {
    Entity {
        category: Some(EntityCategory::Diagnostic),
        ..binary("calibration", "Calibration", BinarySensorClass::Problem)
    }
}
