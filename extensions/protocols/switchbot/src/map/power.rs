//! What switches something on: the Bot, plugs, and relays.
//!
//! A Bot is a switch when SwitchBot's app has it in switch mode, and a button otherwise. Until
//! its mode is known it's a button: pressing works in every mode. The two have different keys
//! (`switch`, `press`): a kind is part of an entity's id, so a Bot whose mode changes loses one
//! entity and gains the other rather than one entity changing kind.

use irori_protocol::ServiceError;
use irori_types::{
    ButtonCapabilities, Capabilities, Service, State, SwitchCapabilities, SwitchClass, SwitchState,
};

use super::sensors::{self, Channel};
use super::{Command, Entity, unsupported};
use crate::model::{Family, Meter, Reading};

pub fn entities(family: Family, reading: &Reading) -> Vec<Entity> {
    match family {
        Family::Bot => {
            let main = if switch_mode(reading) {
                Entity::main(
                    "switch",
                    Capabilities::Switch(SwitchCapabilities { device_class: None }),
                )
            } else {
                Entity::main("press", Capabilities::Button(ButtonCapabilities::default()))
            };
            vec![main, sensors::battery()]
        }
        Family::Plug { meter, outlet } => {
            let mut list = vec![switch(outlet.then_some(SwitchClass::Outlet))];
            match meter {
                Meter::None => {}
                // The Plug Mini US and JP don't count the day's energy.
                Meter::Mini => list.extend(
                    sensors::metered(Channel::Only)
                        .into_iter()
                        .filter(|entity| entity.key != "energy_today"),
                ),
                Meter::Full => list.extend(sensors::metered(Channel::Only)),
            }
            list
        }
        Family::Relay2 => {
            let channel = |key, name| Entity {
                key,
                name: Some(name),
                capabilities: Capabilities::Switch(SwitchCapabilities {
                    device_class: Some(SwitchClass::Switch),
                }),
                category: None,
            };
            let mut list = vec![
                channel("switch_1", "Channel 1"),
                channel("switch_2", "Channel 2"),
            ];
            list.extend(sensors::metered(Channel::First));
            list.extend(sensors::metered(Channel::Second));
            list
        }
        _ => vec![],
    }
}

fn switch_mode(reading: &Reading) -> bool {
    reading.bot_mode.as_deref() == Some("switchMode")
}

fn switch(class: Option<SwitchClass>) -> Entity {
    Entity::main(
        "main",
        Capabilities::Switch(SwitchCapabilities {
            device_class: class,
        }),
    )
}

pub fn state(key: &str, reading: &Reading) -> Option<State> {
    let on = |on: Option<bool>| on.map(|on| State::Switch(SwitchState { on }));
    match key {
        "main" | "switch" | "switch_1" => on(reading.on),
        "switch_2" => on(reading.on_2),
        _ => None,
    }
}

pub fn command(family: Family, key: &str, service: &Service) -> Result<Vec<Command>, ServiceError> {
    if family == Family::Bot {
        return Ok(vec![match (key, service) {
            ("switch", Service::SwitchTurnOn) => Command::plain("turnOn"),
            ("switch", Service::SwitchTurnOff) => Command::plain("turnOff"),
            ("press", Service::ButtonPress) => Command::plain("press"),
            _ => return Err(unsupported()),
        }]);
    }
    let channel = match (family, key) {
        (Family::Relay2, "switch_1") => Some("1"),
        (Family::Relay2, "switch_2") => Some("2"),
        (Family::Relay2, _) => return Err(unsupported()),
        (_, "main") => None,
        _ => return Err(unsupported()),
    };
    let send = |command: &str| match channel {
        Some(channel) => Command::with(command, channel),
        None => Command::plain(command),
    };
    Ok(vec![match service {
        Service::SwitchTurnOn => send("turnOn"),
        Service::SwitchTurnOff => send("turnOff"),
        _ => return Err(unsupported()),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_is_a_switch_only_in_switch_mode() {
        let pressing = Reading {
            bot_mode: Some("pressMode".into()),
            ..Reading::default()
        };
        assert!(matches!(
            entities(Family::Bot, &pressing)[0].capabilities,
            Capabilities::Button(_)
        ));
        assert_eq!(entities(Family::Bot, &pressing)[0].key, "press");
        let switching = Reading {
            bot_mode: Some("switchMode".into()),
            on: Some(true),
            ..Reading::default()
        };
        assert!(matches!(
            entities(Family::Bot, &switching)[0].capabilities,
            Capabilities::Switch(_)
        ));
        assert_eq!(
            state("switch", &switching),
            Some(State::Switch(SwitchState { on: true }))
        );
        assert_eq!(
            command(Family::Bot, "press", &Service::ButtonPress).expect("a press"),
            [Command::plain("press")]
        );
        assert!(command(Family::Bot, "switch", &Service::ButtonPress).is_err());
    }

    #[test]
    fn a_relay_2pm_switches_each_channel_by_number() {
        assert_eq!(
            command(Family::Relay2, "switch_2", &Service::SwitchTurnOn).expect("supported"),
            [Command::with("turnOn", "2")]
        );
        assert!(command(Family::Relay2, "main", &Service::SwitchTurnOn).is_err());
    }
}
