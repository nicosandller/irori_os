//! The ultrasonic Humidifier and the Evaporative Humidifier.
//!
//! Neither says the humidity it's aiming for, so the target is unknown; each says the room's.

use irori_protocol::ServiceError;
use irori_types::{
    BinarySensorClass, Capabilities, HumidifierCapabilities, HumidifierClass, HumidifierState,
    HumidityRange, Service, State,
};
use serde_json::json;

use super::{Command, Entity, sensors, unsupported};
use crate::model::{Family, Reading};

/// The ultrasonic Humidifier's modes: Auto, or one of three mist levels.
const MIST: [&str; 4] = ["auto", "low", "medium", "high"];

/// The Evaporative Humidifier's modes, by SwitchBot's number less one.
const EVAPORATIVE: [&str; 8] = [
    "level 4", "level 3", "level 2", "level 1", "humidity", "sleep", "auto", "drying",
];

/// The mode an Evaporative Humidifier aims at a humidity in.
const HUMIDITY_MODE: u8 = 5;

pub fn entities(family: Family) -> Vec<Entity> {
    let modes: &[&str] = match family {
        Family::Humidifier => &MIST,
        _ => &EVAPORATIVE,
    };
    let mut list = vec![
        Entity::main(
            "main",
            Capabilities::Humidifier(HumidifierCapabilities {
                device_class: Some(HumidifierClass::Humidifier),
                humidity: HumidityRange {
                    min: 0.0,
                    max: 100.0,
                },
                modes: modes.iter().map(|mode| (*mode).to_owned()).collect(),
            }),
        ),
        sensors::humidity(),
    ];
    if family == Family::Humidifier {
        list.push(sensors::temperature());
        list.push(Entity {
            category: Some(irori_types::EntityCategory::Diagnostic),
            ..sensors::binary("water_tank", "Water tank", BinarySensorClass::Problem)
        });
    }
    list
}

pub fn state(family: Family, key: &str, reading: &Reading) -> Option<State> {
    if key != "main" {
        return None;
    }
    let mode = match family {
        Family::Humidifier => reading
            .humidifier_auto
            .and_then(|auto| auto.then_some("auto")),
        _ => reading
            .humidifier_mode
            .and_then(|mode| EVAPORATIVE.get(usize::from(mode).checked_sub(1)?).copied()),
    };
    Some(State::Humidifier(HumidifierState {
        on: reading.on?,
        target_humidity: None,
        current_humidity: reading.humidity,
        mode: mode.map(str::to_owned),
        action: None,
    }))
}

pub fn command(
    family: Family,
    key: &str,
    service: &Service,
    reading: &Reading,
) -> Result<Vec<Command>, ServiceError> {
    if key != "main" {
        return Err(unsupported());
    }
    let command = match (family, service) {
        (_, Service::HumidifierTurnOn) => Command::plain("turnOn"),
        (_, Service::HumidifierTurnOff) => Command::plain("turnOff"),
        (Family::Humidifier, Service::HumidifierSetHumidity(to)) => {
            Command::with("setMode", humidity(to.humidity).to_string())
        }
        (Family::Humidifier, Service::HumidifierSetMode(to)) => {
            let parameter = match to.mode.as_str() {
                "auto" => "auto",
                "low" => "101",
                "medium" => "102",
                "high" => "103",
                _ => return Err(ServiceError::failed("the humidifier has no such mode")),
            };
            Command::with("setMode", parameter)
        }
        (Family::Evaporative, Service::HumidifierSetHumidity(to)) => Command::with(
            "setMode",
            json!({ "mode": HUMIDITY_MODE, "targetHumidify": humidity(to.humidity) }),
        ),
        (Family::Evaporative, Service::HumidifierSetMode(to)) => {
            let Some(index) = EVAPORATIVE.iter().position(|mode| *mode == to.mode) else {
                return Err(ServiceError::failed("the humidifier has no such mode"));
            };
            // SwitchBot wants a target with every mode. The room's humidity is as good a guess
            // as any at what the person wants to keep.
            let target = reading.humidity.map_or(50, humidity);
            Command::with(
                "setMode",
                json!({ "mode": index + 1, "targetHumidify": target }),
            )
        }
        _ => return Err(unsupported()),
    };
    Ok(vec![command])
}

fn humidity(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{HumidifierMode, SetHumidity};

    #[test]
    fn an_evaporative_humidifier_aims_at_a_humidity_in_its_humidity_mode() {
        assert_eq!(
            command(
                Family::Evaporative,
                "main",
                &Service::HumidifierSetHumidity(SetHumidity { humidity: 55.0 }),
                &Reading::default()
            )
            .expect("supported"),
            [Command::with(
                "setMode",
                json!({ "mode": 5, "targetHumidify": 55 })
            )]
        );
        let reading = Reading {
            on: Some(true),
            humidifier_mode: Some(7),
            humidity: Some(41.0),
            ..Reading::default()
        };
        let Some(State::Humidifier(state)) = super::state(Family::Evaporative, "main", &reading)
        else {
            panic!("a humidifier state");
        };
        assert_eq!(state.mode.as_deref(), Some("auto"));
        assert_eq!(state.current_humidity, Some(41.0));
    }

    #[test]
    fn the_ultrasonic_humidifier_has_mist_levels() {
        assert_eq!(
            command(
                Family::Humidifier,
                "main",
                &Service::HumidifierSetMode(HumidifierMode {
                    mode: "high".into()
                }),
                &Reading::default()
            )
            .expect("supported"),
            [Command::with("setMode", "103")]
        );
    }
}
