//! What opens and closes: curtains, roller shades, tilting blinds and garage doors. Positions
//! are Irori's (0 closed, 100 open); SwitchBot's curtains and shades count the other way, which
//! `model` and the commands here turn around.

use irori_protocol::ServiceError;
use irori_types::{
    Capabilities, CoverCapabilities, CoverClass, CoverState, OpenState, Service, State,
};

use super::{Command, Entity, sensors, unsupported};
use crate::model::{Family, Reading};

pub fn entities(family: Family) -> Vec<Entity> {
    let cover = |class, position, tilt, stop| {
        Entity::main(
            "main",
            Capabilities::Cover(CoverCapabilities {
                device_class: Some(class),
                position,
                tilt,
                stop,
            }),
        )
    };
    match family {
        Family::Curtain => vec![
            cover(CoverClass::Curtain, true, false, true),
            sensors::calibration(),
            sensors::battery(),
        ],
        Family::RollerShade => vec![
            cover(CoverClass::Shade, true, false, false),
            sensors::calibration(),
            sensors::battery(),
        ],
        Family::BlindTilt => vec![
            cover(CoverClass::Blind, false, true, false),
            sensors::calibration(),
        ],
        Family::Garage => vec![cover(CoverClass::Garage, false, false, false)],
        _ => vec![],
    }
}

pub fn state(family: Family, key: &str, reading: &Reading) -> Option<State> {
    if key != "main" {
        return None;
    }
    let shut = |closed: bool| {
        if closed {
            OpenState::Closed
        } else {
            OpenState::Open
        }
    };
    let state = match family {
        Family::Garage => CoverState {
            state: shut(!reading.door_open?),
            position: None,
            tilt: None,
        },
        Family::BlindTilt => {
            let tilt = percent(reading.position?);
            CoverState {
                state: shut(tilt == 0),
                position: None,
                tilt: Some(tilt),
            }
        }
        _ => {
            let position = percent(reading.position?);
            CoverState {
                state: shut(position == 0),
                position: Some(position),
                tilt: None,
            }
        }
    };
    Some(State::Cover(state))
}

fn percent(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

pub fn command(family: Family, key: &str, service: &Service) -> Result<Vec<Command>, ServiceError> {
    if key != "main" {
        return Err(unsupported());
    }
    let command = match (family, service) {
        (Family::Curtain, Service::CoverOpen) => Command::plain("turnOn"),
        (Family::Curtain, Service::CoverClose) => Command::plain("turnOff"),
        (Family::Curtain, Service::CoverStop) => Command::plain("pause"),
        // Index 0, the default speed, and SwitchBot's position counted from open.
        (Family::Curtain, Service::CoverSetPosition(to)) => Command::with(
            "setPosition",
            format!("0,ff,{}", 100 - to.position.min(100)),
        ),
        (Family::RollerShade, Service::CoverOpen) => Command::with("setPosition", 0),
        (Family::RollerShade, Service::CoverClose) => Command::with("setPosition", 100),
        (Family::RollerShade, Service::CoverSetPosition(to)) => {
            Command::with("setPosition", 100 - u64::from(to.position.min(100)))
        }
        (Family::BlindTilt, Service::CoverOpen) => Command::plain("fullyOpen"),
        (Family::BlindTilt, Service::CoverClose) => Command::plain("closeDown"),
        // SwitchBot takes only even tilts.
        (Family::BlindTilt, Service::CoverSetTilt(to)) => {
            Command::with("setPosition", format!("up;{}", to.tilt.min(100) / 2 * 2))
        }
        (Family::Garage, Service::CoverOpen) => Command::plain("turnOn"),
        (Family::Garage, Service::CoverClose) => Command::plain("turnOff"),
        _ => return Err(unsupported()),
    };
    Ok(vec![command])
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{SetPosition, SetTilt};

    #[test]
    fn a_curtain_is_sent_where_switchbot_counts_from_open() {
        assert_eq!(
            command(
                Family::Curtain,
                "main",
                &Service::CoverSetPosition(SetPosition { position: 30 })
            )
            .expect("supported"),
            [Command::with("setPosition", "0,ff,70")]
        );
        let reading = Reading {
            position: Some(30.0),
            ..Reading::default()
        };
        assert_eq!(
            state(Family::Curtain, "main", &reading),
            Some(State::Cover(CoverState {
                state: OpenState::Open,
                position: Some(30),
                tilt: None
            }))
        );
    }

    #[test]
    fn a_blind_tilt_takes_even_tilts() {
        assert_eq!(
            command(
                Family::BlindTilt,
                "main",
                &Service::CoverSetTilt(SetTilt { tilt: 37 })
            )
            .expect("supported"),
            [Command::with("setPosition", "up;36")]
        );
    }

    #[test]
    fn a_garage_door_reads_door_status() {
        let open = Reading {
            door_open: Some(true),
            ..Reading::default()
        };
        assert!(matches!(
            state(Family::Garage, "main", &open),
            Some(State::Cover(CoverState {
                state: OpenState::Open,
                ..
            }))
        ));
    }
}
