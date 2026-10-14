//! Home Assistant's MQTT vacuum (its `state` schema): one JSON message saying what it's doing
//! and how fast its fan runs, a payload for each thing it can be told, and a topic of its own
//! for the fan speed.

use irori_types::{Capabilities, Service, State, VacuumActivity, VacuumCapabilities, VacuumState};

use crate::discovery::{EntityTopics, owned_str, str_field, string_list};
use crate::state::{Message, Publish, text_publish};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VacuumTopics {
    pub state_topic: Option<String>,
    /// `None` for a vacuum that is only watched.
    pub command_topic: Option<String>,
    pub set_fan_speed_topic: Option<String>,
    pub payload_start: String,
    pub payload_pause: String,
    pub payload_stop: String,
    pub payload_return_to_base: String,
    pub payload_locate: String,
    pub payload_clean_spot: String,
    /// The fan speeds it listed, so a speed it reports that it never listed is left out rather
    /// than refused with the rest of the message.
    pub fan_speeds: Vec<String>,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let command_topic = str_field(root, "command_topic").map(str::to_owned);
    let set_fan_speed_topic = str_field(root, "set_fan_speed_topic").map(str::to_owned);
    // Home Assistant's defaults, for a config that lists none.
    let features = string_list(root, "supported_features").unwrap_or_else(|| {
        [
            "start",
            "stop",
            "return_home",
            "status",
            "battery",
            "clean_spot",
        ]
        .map(str::to_owned)
        .to_vec()
    });
    let has = |feature: &str| command_topic.is_some() && features.iter().any(|f| f == feature);
    let fan_speeds = if set_fan_speed_topic.is_some() && features.iter().any(|f| f == "fan_speed") {
        string_list(root, "fan_speed_list").unwrap_or_default()
    } else {
        Vec::new()
    };
    let word = |key: &str, default: &str| owned_str(root, key, default);
    let capabilities = VacuumCapabilities {
        start: has("start"),
        pause: has("pause"),
        stop: has("stop"),
        return_home: has("return_home"),
        locate: has("locate"),
        clean_spot: has("clean_spot"),
        fan_speeds: fan_speeds.clone(),
    };
    Ok((
        Capabilities::Vacuum(capabilities),
        EntityTopics::Vacuum(Box::new(VacuumTopics {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            set_fan_speed_topic,
            payload_start: word("payload_start", "start"),
            payload_pause: word("payload_pause", "pause"),
            payload_stop: word("payload_stop", "stop"),
            payload_return_to_base: word("payload_return_to_base", "return_to_base"),
            payload_locate: word("payload_locate", "locate"),
            payload_clean_spot: word("payload_clean_spot", "clean_spot"),
            fan_speeds,
        })),
    ))
}

/// Its state message is JSON: `{"state": "docked", "fan_speed": "quiet", "battery_level": 61}`.
pub(crate) fn decode(vacuum: &VacuumTopics, message: Message) -> Option<Result<State, String>> {
    message.on(vacuum.state_topic.as_deref()).then(|| {
        let said: serde_json::Value = serde_json::from_slice(message.payload)
            .map_err(|e| format!("its state isn't JSON: {e}"))?;
        let activity = said
            .get("state")
            .and_then(serde_json::Value::as_str)
            .ok_or("its state message has no `state`")?;
        let activity = VacuumActivity::parse(activity)
            .ok_or_else(|| format!("{activity:?} isn't something a vacuum does"))?;
        let fan_speed = said
            .get("fan_speed")
            .and_then(serde_json::Value::as_str)
            .filter(|speed| vacuum.fan_speeds.iter().any(|listed| listed == speed))
            .map(str::to_owned);
        Ok(State::Vacuum(VacuumState {
            activity,
            fan_speed,
        }))
    })
}

pub(crate) fn encode(vacuum: &VacuumTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let watched = "this vacuum is only watched: it has nowhere to send a command";
    let payload = match service {
        Service::VacuumStart => &vacuum.payload_start,
        Service::VacuumPause => &vacuum.payload_pause,
        Service::VacuumStop => &vacuum.payload_stop,
        Service::VacuumReturnToBase => &vacuum.payload_return_to_base,
        Service::VacuumLocate => &vacuum.payload_locate,
        Service::VacuumCleanSpot => &vacuum.payload_clean_spot,
        Service::VacuumSetFanSpeed(data) => {
            let topic = vacuum
                .set_fan_speed_topic
                .as_ref()
                .ok_or("this vacuum has no fan speed to set")?;
            return Ok(vec![text_publish(topic, &data.fan_speed)]);
        }
        service => return Err(super::no_service("a vacuum", service)),
    };
    let topic = vacuum.command_topic.as_ref().ok_or(watched)?;
    Ok(vec![text_publish(topic, payload)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;
    use irori_types::VacuumFanSpeed;

    /// What Valetudo publishes for a robot, near enough.
    const ROBOT: &[u8] = br#"{"unique_id": "robot_vacuum", "name": "Robot",
        "state_topic": "valetudo/robot/state", "command_topic": "valetudo/robot/command",
        "set_fan_speed_topic": "valetudo/robot/fan/set",
        "fan_speed_list": ["min", "medium", "max"],
        "supported_features": ["start", "pause", "stop", "return_home", "locate", "fan_speed"]}"#;

    #[test]
    fn a_robot_says_what_it_does_and_is_told_by_payload() {
        let parsed = parse(Component::Vacuum, ROBOT).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Vacuum(VacuumCapabilities {
                start: true,
                pause: true,
                stop: true,
                return_home: true,
                locate: true,
                clean_spot: false,
                fan_speeds: vec!["min".into(), "medium".into(), "max".into()],
            })
        );
        assert_eq!(
            crate::state::decode(
                &parsed.topics,
                "valetudo/robot/state",
                br#"{"state": "cleaning", "fan_speed": "max", "battery_level": 61}"#,
                None
            ),
            Some(Ok(State::Vacuum(VacuumState {
                activity: VacuumActivity::Cleaning,
                fan_speed: Some("max".into()),
            })))
        );
        // A speed it never listed is left out; the rest of the message still counts.
        assert_eq!(
            crate::state::decode(
                &parsed.topics,
                "valetudo/robot/state",
                br#"{"state": "docked", "fan_speed": "custom"}"#,
                None
            ),
            Some(Ok(State::Vacuum(VacuumState {
                activity: VacuumActivity::Docked,
                fan_speed: None,
            })))
        );
        assert!(
            crate::state::decode(&parsed.topics, "valetudo/robot/state", b"cleaning", None)
                .expect("its topic")
                .is_err()
        );
        let sent =
            crate::state::encode(&parsed.topics, &Service::VacuumReturnToBase).expect("goes home");
        assert_eq!(
            (sent[0].topic.as_str(), sent[0].payload.as_slice()),
            ("valetudo/robot/command", &b"return_to_base"[..])
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &Service::VacuumSetFanSpeed(VacuumFanSpeed {
                fan_speed: "min".into(),
            }),
        )
        .expect("sets");
        assert_eq!(
            (sent[0].topic.as_str(), sent[0].payload.as_slice()),
            ("valetudo/robot/fan/set", &b"min"[..])
        );
    }

    #[test]
    fn home_assistants_defaults_fill_what_a_config_leaves_out() {
        let bare = br#"{"unique_id": "v", "name": "V", "command_topic": "v/set"}"#;
        let parsed = parse(Component::Vacuum, bare).expect("valid");
        let Capabilities::Vacuum(caps) = &parsed.capabilities else {
            panic!("a vacuum");
        };
        assert!(caps.start && caps.stop && caps.return_home && caps.clean_spot);
        assert!(!caps.pause && !caps.locate && caps.fan_speeds.is_empty());
        let sent = crate::state::encode(&parsed.topics, &Service::VacuumStart).expect("starts");
        assert_eq!(sent[0].payload, b"start");
    }
}
