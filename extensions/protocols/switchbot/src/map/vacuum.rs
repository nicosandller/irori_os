//! SwitchBot's robot vacuums. Two command sets: the older ones start and stop, the newer ones
//! start a cleaning and pause it.

use irori_protocol::ServiceError;
use irori_types::{Capabilities, Service, State, VacuumActivity, VacuumCapabilities, VacuumState};
use serde_json::{Value, json};

use super::{Command, Entity, sensors, unsupported};
use crate::model::{Family, Reading, VacuumApi};

/// Suction, from quietest: SwitchBot's `PowLevel` 0-3, or `fanLevel` 1-4.
const FAN_SPEEDS: [&str; 4] = ["quiet", "standard", "strong", "max"];

pub fn entities(family: Family) -> Vec<Entity> {
    let Family::Vacuum(api) = family else {
        return vec![];
    };
    let simple = api == VacuumApi::Simple;
    vec![
        Entity::main(
            "main",
            Capabilities::Vacuum(VacuumCapabilities {
                start: true,
                pause: !simple,
                stop: simple,
                return_home: true,
                locate: false,
                clean_spot: false,
                fan_speeds: FAN_SPEEDS.iter().map(|speed| (*speed).to_owned()).collect(),
            }),
        ),
        sensors::battery(),
    ]
}

pub fn state(_family: Family, key: &str, reading: &Reading) -> Option<State> {
    if key != "main" {
        return None;
    }
    let activity = match reading.working.as_deref()? {
        "Clearing" | "InRemoteControl" => VacuumActivity::Cleaning,
        "Paused" => VacuumActivity::Paused,
        "GotoChargeBase" => VacuumActivity::Returning,
        "Charging" | "ChargeDone" | "InDustCollecting" => VacuumActivity::Docked,
        "InTrouble" => VacuumActivity::Error,
        _ => VacuumActivity::Idle,
    };
    Some(State::Vacuum(VacuumState {
        activity,
        fan_speed: None,
    }))
}

pub fn command(api: VacuumApi, key: &str, service: &Service) -> Result<Vec<Command>, ServiceError> {
    if key != "main" {
        return Err(unsupported());
    }
    // A newer vacuum takes its suction (and, for one that mops, how wet) with each cleaning.
    let params = |fan: usize| {
        let mut params = json!({ "fanLevel": fan, "times": 1 });
        if api == VacuumApi::Mop {
            params["waterLevel"] = Value::from(1);
        }
        params
    };
    let command = match (api, service) {
        (VacuumApi::Simple, Service::VacuumStart) => Command::plain("start"),
        (VacuumApi::Simple, Service::VacuumStop) => Command::plain("stop"),
        (_, Service::VacuumReturnToBase) => Command::plain("dock"),
        (VacuumApi::Simple, Service::VacuumSetFanSpeed(to)) => {
            Command::with("PowLevel", speed(&to.fan_speed)?.to_string())
        }
        (VacuumApi::Simple, _) => return Err(unsupported()),
        (_, Service::VacuumStart) => Command::with(
            "startClean",
            json!({ "action": "sweep", "param": params(2) }),
        ),
        (_, Service::VacuumPause) => Command::plain("pause"),
        (_, Service::VacuumSetFanSpeed(to)) => {
            Command::with("changeParam", params(speed(&to.fan_speed)? + 1))
        }
        _ => return Err(unsupported()),
    };
    Ok(vec![command])
}

fn speed(name: &str) -> Result<usize, ServiceError> {
    FAN_SPEEDS
        .iter()
        .position(|speed| *speed == name)
        .ok_or_else(|| ServiceError::failed("the vacuum has no such suction level"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::VacuumFanSpeed;

    #[test]
    fn a_mopping_vacuum_starts_with_a_water_level() {
        assert_eq!(
            command(VacuumApi::Mop, "main", &Service::VacuumStart).expect("supported"),
            [Command::with(
                "startClean",
                json!({ "action": "sweep", "param": { "fanLevel": 2, "times": 1, "waterLevel": 1 } })
            )]
        );
        assert_eq!(
            command(
                VacuumApi::Simple,
                "main",
                &Service::VacuumSetFanSpeed(VacuumFanSpeed {
                    fan_speed: "max".into()
                })
            )
            .expect("supported"),
            [Command::with("PowLevel", "3")]
        );
        assert!(command(VacuumApi::Simple, "main", &Service::VacuumPause).is_err());
    }

    #[test]
    fn charging_is_docked() {
        let reading = Reading {
            working: Some("ChargeDone".into()),
            ..Reading::default()
        };
        assert_eq!(
            state(Family::Vacuum(VacuumApi::Clean), "main", &reading),
            Some(State::Vacuum(VacuumState {
                activity: VacuumActivity::Docked,
                fan_speed: None
            }))
        );
    }
}
