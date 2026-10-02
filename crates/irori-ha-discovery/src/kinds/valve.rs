//! Home Assistant's MQTT valve: something that opens and closes ([`super::opening`]). One that
//! `reports_position` says a number on its state topic and takes one on its command topic.

use irori_types::{
    Capabilities, EntityKind, OpeningCommand, Service, State, ValveCapabilities, ValveClass,
    ValveState,
};

use super::opening::OpeningTopics;
use crate::discovery::{EntityTopics, bool_field, owned_str, plain_command, str_field};
use crate::state::{Message, Publish};
use crate::template::ValueTemplate;

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a valve")?;
    let owned = |key: &str| str_field(root, key).map(str::to_owned);
    let number = |key: &str, default: f64| {
        root.get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default)
    };
    let reports_position = bool_field(root, "reports_position");
    let state_topic = owned("state_topic");
    let value_template = ValueTemplate::parse(str_field(root, "value_template"));
    let topics = OpeningTopics {
        command_topic: Some(command_topic.clone()),
        payload_open: owned_str(root, "payload_open", "OPEN"),
        payload_close: owned_str(root, "payload_close", "CLOSE"),
        // A valve stops only when it says how.
        payload_stop: owned("payload_stop"),
        state_topic: state_topic.clone(),
        value_template: (!reports_position).then(|| value_template.clone()),
        state_open: owned_str(root, "state_open", "open"),
        state_opening: owned_str(root, "state_opening", "opening"),
        state_closed: owned_str(root, "state_closed", "closed"),
        state_closing: owned_str(root, "state_closing", "closing"),
        state_stopped: "stopped".to_owned(),
        position_topic: reports_position.then(|| state_topic.clone()).flatten(),
        position_template: value_template,
        position_open: number("position_open", 100.0),
        position_closed: number("position_closed", 0.0),
        set_position_topic: reports_position.then_some(command_topic),
        set_position_template: command_template,
    };
    let capabilities = ValveCapabilities {
        device_class: str_field(root, "device_class").and_then(ValveClass::from_ha),
        position: reports_position,
        stop: topics.payload_stop.is_some(),
    };
    Ok((
        Capabilities::Valve(capabilities),
        EntityTopics::Valve(Box::new(topics)),
    ))
}

pub(crate) fn decode(
    valve: &OpeningTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    if !valve.listens().contains(&message.topic) {
        return None;
    }
    let previous = match previous {
        Some(State::Valve(old)) => Some(old.opening()),
        _ => None,
    };
    Some(
        valve
            .read(EntityKind::Valve, message, previous)
            .map(|opening| State::Valve(ValveState::from(opening))),
    )
}

pub(crate) fn encode(valve: &OpeningTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match OpeningCommand::of(EntityKind::Valve, service) {
        Some(command) => valve.encode(EntityKind::Valve, command),
        None => Err(super::no_service("a valve", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn a_valve_opens_and_closes_or_says_how_far() {
        let plain = parse(
            Component::Valve,
            br#"{"unique_id": "v", "name": "Main water", "command_topic": "v/set",
                "state_topic": "v/state", "device_class": "water"}"#,
        )
        .expect("valid");
        assert_eq!(
            plain.capabilities,
            Capabilities::Valve(ValveCapabilities {
                device_class: Some(ValveClass::Water),
                position: false,
                stop: false,
            })
        );
        assert_eq!(
            crate::state::decode(&plain.topics, "v/state", b"closed", None),
            Some(Ok(irori_types::State::Valve(irori_types::ValveState {
                state: irori_types::OpenState::Closed,
                position: None,
            })))
        );
        let sent =
            crate::state::encode(&plain.topics, &irori_types::Service::ValveOpen).expect("open");
        assert_eq!(sent[0].payload, b"OPEN");
        // Its errors are a valve's own, and a cover's service isn't one it sends.
        assert_eq!(
            crate::state::encode(&plain.topics, &irori_types::Service::ValveStop),
            Err("this valve can't be stopped".to_owned())
        );
        assert!(crate::state::encode(&plain.topics, &irori_types::Service::CoverOpen).is_err());

        let zone = parse(
            Component::Valve,
            br#"{"unique_id": "z", "name": "Garden zone", "command_topic": "z/set",
                "state_topic": "z/state", "reports_position": true, "payload_stop": "STOP"}"#,
        )
        .expect("valid");
        assert_eq!(
            crate::state::decode(&zone.topics, "z/state", b"40", None),
            Some(Ok(irori_types::State::Valve(irori_types::ValveState {
                state: irori_types::OpenState::Open,
                position: Some(40),
            })))
        );
        let sent = crate::state::encode(
            &zone.topics,
            &irori_types::Service::ValveSetPosition(irori_types::SetPosition { position: 25 }),
        )
        .expect("a position");
        assert_eq!(
            (sent[0].topic.as_str(), sent[0].payload.as_slice()),
            ("z/set", &b"25"[..])
        );
    }
}
