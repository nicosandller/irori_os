//! Home Assistant's MQTT cover: something that opens and closes ([`super::opening`]), and
//! sometimes tilts.

use irori_types::{
    Capabilities, CoverCapabilities, CoverClass, CoverState, EntityKind, OpeningCommand, Service,
    State,
};

use super::opening::{OpeningTopics, number, percent, scale};
use crate::discovery::{EntityTopics, str_field};
use crate::state::{Message, Publish, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq)]
pub struct CoverTopics {
    pub opening: OpeningTopics,
    pub tilt_command_topic: Option<String>,
    pub tilt_command_template: CommandTemplate,
    pub tilt_status_topic: Option<String>,
    pub tilt_status_template: ValueTemplate,
    pub tilt_min: f64,
    pub tilt_max: f64,
}

impl CoverTopics {
    pub(crate) fn listens(&self) -> Vec<&str> {
        let mut topics = self.opening.listens();
        topics.extend(self.tilt_status_topic.as_deref());
        topics
    }
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let owned = |key: &str| str_field(root, key).map(str::to_owned);
    let number = |key: &str, default: f64| {
        root.get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default)
    };
    let template = |key: &str, variable: &str| {
        CommandTemplate::parse(str_field(root, key), variable)
            .map_err(|why| format!("`{key}`: {why}"))
    };
    let state_topic = owned("state_topic");
    // A state template that needs Jinja (Zigbee2MQTT's for covers that report their motor) isn't
    // run: the state is worked out from the position instead, if it has one.
    let value_template = match ValueTemplate::parse(str_field(root, "value_template")) {
        ValueTemplate::Unsupported(_) => None,
        template => Some(template),
    };
    let position_topic = owned("position_topic");
    if state_topic.is_some() && value_template.is_none() && position_topic.is_none() {
        return Err("its state template needs Jinja, and it has no position to go by".to_owned());
    }
    // `payload_stop: null` says it can't be stopped.
    let payload_stop = match root.get("payload_stop") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(stop)) => Some(stop.clone()),
        _ => Some("STOP".to_owned()),
    };
    let command_topic = owned("command_topic");
    let opening = OpeningTopics {
        payload_open: owned("payload_open").unwrap_or_else(|| "OPEN".to_owned()),
        payload_close: owned("payload_close").unwrap_or_else(|| "CLOSE".to_owned()),
        payload_stop: payload_stop.filter(|_| command_topic.is_some()),
        command_topic,
        state_topic,
        value_template,
        state_open: owned("state_open").unwrap_or_else(|| "open".to_owned()),
        state_opening: owned("state_opening").unwrap_or_else(|| "opening".to_owned()),
        state_closed: owned("state_closed").unwrap_or_else(|| "closed".to_owned()),
        state_closing: owned("state_closing").unwrap_or_else(|| "closing".to_owned()),
        state_stopped: owned("state_stopped").unwrap_or_else(|| "stopped".to_owned()),
        position_template: ValueTemplate::parse(str_field(root, "position_template")),
        position_topic,
        position_open: number("position_open", 100.0),
        position_closed: number("position_closed", 0.0),
        set_position_topic: owned("set_position_topic"),
        set_position_template: template("set_position_template", "position")?,
    };
    let topics = CoverTopics {
        opening,
        tilt_command_topic: owned("tilt_command_topic"),
        tilt_command_template: template("tilt_command_template", "tilt_position")?,
        tilt_status_topic: owned("tilt_status_topic"),
        tilt_status_template: ValueTemplate::parse(str_field(root, "tilt_status_template")),
        tilt_min: number("tilt_min", 0.0),
        tilt_max: number("tilt_max", 100.0),
    };
    let capabilities = CoverCapabilities {
        device_class: str_field(root, "device_class").and_then(CoverClass::from_ha),
        position: topics.opening.set_position_topic.is_some(),
        tilt: topics.tilt_command_topic.is_some(),
        stop: topics.opening.payload_stop.is_some(),
    };
    Ok((
        Capabilities::Cover(capabilities),
        EntityTopics::Cover(Box::new(topics)),
    ))
}

/// Where it is and how its slats are tilted, from a message on any of its topics.
pub(crate) fn decode(
    cover: &CoverTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let for_tilt = message.on(cover.tilt_status_topic.as_deref());
    if !(for_tilt || cover.opening.listens().contains(&message.topic)) {
        return None;
    }
    let previous = match previous {
        Some(State::Cover(old)) => Some(old),
        _ => None,
    };
    Some((|| {
        let mut tilt = previous.and_then(|old| old.tilt);
        if for_tilt && let Some(raw) = number(message, &cover.tilt_status_template)? {
            tilt = percent(raw, cover.tilt_min, cover.tilt_max);
        }
        let opening = cover.opening.read(
            EntityKind::Cover,
            message,
            previous.map(CoverState::opening),
        )?;
        Ok(State::Cover(CoverState::at(opening, tilt)))
    })())
}

pub(crate) fn encode(cover: &CoverTopics, service: &Service) -> Result<Vec<Publish>, String> {
    if let Some(command) = OpeningCommand::of(EntityKind::Cover, service) {
        return cover.opening.encode(EntityKind::Cover, command);
    }
    match service {
        Service::CoverSetTilt(data) => match &cover.tilt_command_topic {
            Some(topic) => Ok(vec![text_publish(
                topic,
                &cover.tilt_command_template.render(&scale(
                    data.tilt,
                    cover.tilt_min,
                    cover.tilt_max,
                )),
            )]),
            None => Err("this cover has nothing to tilt".to_owned()),
        },
        service => Err(super::no_service("a cover", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;
    use irori_types::*;

    /// What Zigbee2MQTT publishes for a blind with a position (`case "cover"`, no motor state).
    const Z2M_COVER: &[u8] = br#"{
        "unique_id": "0x0211000000000010_cover_zigbee2mqtt", "name": null,
        "device": {"identifiers": ["zigbee2mqtt_0x0211000000000010"], "name": "Office blind"},
        "command_topic": "zigbee2mqtt/Office blind/set",
        "state_topic": "zigbee2mqtt/Office blind",
        "value_template": "{{ value_json[\"state\"] }}",
        "state_open": "OPEN", "state_closed": "CLOSE", "state_stopped": "STOP",
        "position_template": "{{ value_json[\"position\"] }}",
        "set_position_template": "{ \"position\": {{ position }} }",
        "set_position_topic": "zigbee2mqtt/Office blind/set",
        "position_topic": "zigbee2mqtt/Office blind"
    }"#;

    #[test]
    fn a_z2m_cover_is_read_and_sent_to_a_position() {
        let parsed = parse(Component::Cover, Z2M_COVER).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Cover(CoverCapabilities {
                device_class: None,
                position: true,
                tilt: false,
                stop: true,
            })
        );
        let unique_id = UniqueId::try_from("0x0211000000000010_cover_zigbee2mqtt").expect("valid");
        assert_eq!(
            crate::state::topics_of(&unique_id, &parsed.topics).len(),
            1,
            "state and position share one topic, listened to once"
        );
        let decoded = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/Office blind",
            br#"{"state": "OPEN", "position": 40}"#,
            None,
        );
        assert_eq!(
            decoded,
            Some(Ok(irori_types::State::Cover(irori_types::CoverState {
                state: irori_types::OpenState::Open,
                position: Some(40),
                tilt: None,
            })))
        );
        // Stopped at the bottom is closed.
        let stopped = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/Office blind",
            br#"{"state": "STOP", "position": 0}"#,
            None,
        );
        assert!(matches!(
            stopped,
            Some(Ok(irori_types::State::Cover(irori_types::CoverState {
                state: irori_types::OpenState::Closed,
                ..
            })))
        ));
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::CoverSetPosition(irori_types::SetPosition { position: 75 }),
        )
        .expect("a position");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Office blind/set");
        assert_eq!(sent[0].payload, br#"{ "position": 75 }"#);
        let sent =
            crate::state::encode(&parsed.topics, &irori_types::Service::CoverClose).expect("close");
        assert_eq!(sent[0].payload, b"CLOSE");
    }

    #[test]
    fn a_cover_whose_state_needs_jinja_goes_by_its_position() {
        let payload = br#"{"unique_id": "c", "name": "Curtain", "command_topic": "c/set",
            "state_topic": "c", "position_topic": "c",
            "value_template": "{% if value_json.motor_state == 'opening' %}opening{% endif %}",
            "position_template": "{{ value_json.position }}", "set_position_topic": "c/set"}"#;
        let parsed = parse(Component::Cover, payload).expect("valid");
        let decoded = crate::state::decode(&parsed.topics, "c", br#"{"position": 0}"#, None);
        assert!(matches!(
            decoded,
            Some(Ok(irori_types::State::Cover(irori_types::CoverState {
                state: irori_types::OpenState::Closed,
                position: Some(0),
                ..
            })))
        ));
        // With nothing else to go by, it's refused, and listed with why.
        let blind = br#"{"unique_id": "d", "name": "Blind", "command_topic": "d/set",
            "state_topic": "d", "value_template": "{% if x %}open{% endif %}"}"#;
        assert!(parse(Component::Cover, blind).is_err_and(|e| e.contains("Jinja")));
    }
}
