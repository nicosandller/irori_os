//! Home Assistant's MQTT humidifier: on and off, a target humidity, and optionally modes, the
//! room's humidity and what it's doing, each setting on its own topic or sharing one.

use irori_types::{
    Capabilities, HumidifierAction, HumidifierCapabilities, HumidifierClass, HumidifierState,
    HumidityRange, Service, State,
};

use super::setting::{Reading, Setting, reading, setting, state_of};
use crate::discovery::{
    EntityTopics, number_field, owned_str, plain_command, str_field, string_list,
};
use crate::state::{Message, Publish, number_text, update};
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq)]
pub struct HumidifierTopics {
    /// On and off: `command_topic`, read back on `state_topic`.
    pub power: Setting,
    pub payload_on: String,
    pub payload_off: String,
    pub target: Setting,
    pub mode: Option<Setting>,
    pub current_humidity: Option<Reading>,
    pub action: Option<Reading>,
}

impl HumidifierTopics {
    /// Everything it reports on.
    pub(crate) fn readings(&self) -> impl Iterator<Item = &Reading> {
        [
            state_of(Some(&self.power)),
            state_of(Some(&self.target)),
            state_of(self.mode.as_ref()),
            self.current_humidity.as_ref(),
            self.action.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a humidifier")?;
    let target = setting(root, "target_humidity", "target_humidity_state_template").ok_or(
        "a humidifier needs a `target_humidity_command_topic` with a template Irori can render",
    )?;
    let mode = setting(root, "mode", "mode_state_template");
    let modes = if mode.is_some() {
        string_list(root, "modes").unwrap_or_default()
    } else {
        Vec::new()
    };
    let topics = HumidifierTopics {
        power: Setting {
            command_topic,
            command_template,
            state: str_field(root, "state_topic").map(|topic| Reading {
                topic: topic.to_owned(),
                template: ValueTemplate::parse(str_field(root, "state_value_template")),
            }),
        },
        payload_on: owned_str(root, "payload_on", "ON"),
        payload_off: owned_str(root, "payload_off", "OFF"),
        target,
        mode,
        current_humidity: reading(root, "current_humidity"),
        action: reading(root, "action"),
    };
    let capabilities = HumidifierCapabilities {
        device_class: str_field(root, "device_class").and_then(HumidifierClass::from_ha),
        humidity: HumidityRange {
            min: number_field(root, "min_humidity").unwrap_or(0.0),
            max: number_field(root, "max_humidity").unwrap_or(100.0),
        },
        modes,
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Humidifier(capabilities),
        EntityTopics::Humidifier(Box::new(topics)),
    ))
}

pub(crate) fn decode(
    humidifier: &HumidifierTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    if !message.on_any(humidifier.readings()) {
        return None;
    }
    Some((|| {
        let mut state = match previous {
            Some(State::Humidifier(old)) => old.clone(),
            _ => HumidifierState {
                on: false,
                target_humidity: None,
                current_humidity: None,
                mode: None,
                action: None,
            },
        };
        if let Some(text) = message.said(humidifier.power.state.as_ref()).flatten() {
            state.on = match text {
                text if text == humidifier.payload_on => true,
                text if text == humidifier.payload_off => false,
                text => return Err(format!("{text:?} isn't on or off for this humidifier")),
            };
        }
        update(
            &mut state.target_humidity,
            message.number(humidifier.target.state.as_ref()),
        );
        update(
            &mut state.mode,
            message.said(state_of(humidifier.mode.as_ref())),
        );
        update(
            &mut state.current_humidity,
            message.number(humidifier.current_humidity.as_ref()),
        );
        update(
            &mut state.action,
            message
                .said(humidifier.action.as_ref())
                .map(|text| text.as_deref().and_then(HumidifierAction::parse)),
        );
        Ok(State::Humidifier(state))
    })())
}

pub(crate) fn encode(
    humidifier: &HumidifierTopics,
    service: &Service,
) -> Result<Vec<Publish>, String> {
    let message = match service {
        Service::HumidifierTurnOn => humidifier.power.publish(&humidifier.payload_on),
        Service::HumidifierTurnOff => humidifier.power.publish(&humidifier.payload_off),
        Service::HumidifierSetHumidity(data) => {
            humidifier.target.publish(&number_text(data.humidity))
        }
        Service::HumidifierSetMode(data) => humidifier
            .mode
            .as_ref()
            .ok_or("this humidifier has no modes")?
            .publish(&data.mode),
        service => return Err(super::no_service("a humidifier", service)),
    };
    Ok(vec![message])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn an_mqtt_dehumidifier_is_read_and_told() {
        let parsed = parse(
            Component::Humidifier,
            br#"{"unique_id": "d", "name": "Basement", "device_class": "dehumidifier",
                "command_topic": "d/set", "state_topic": "d/state",
                "state_value_template": "{{ value_json.state }}",
                "target_humidity_command_topic": "d/target/set",
                "target_humidity_state_topic": "d/state",
                "target_humidity_state_template": "{{ value_json.target }}",
                "current_humidity_topic": "d/state",
                "current_humidity_template": "{{ value_json.humidity }}",
                "mode_command_topic": "d/mode/set", "modes": ["normal", "sleep"],
                "min_humidity": 35, "max_humidity": 80}"#,
        )
        .expect("valid");
        let Capabilities::Humidifier(caps) = &parsed.capabilities else {
            panic!("a humidifier");
        };
        assert_eq!(
            caps.device_class,
            Some(irori_types::HumidifierClass::Dehumidifier)
        );
        assert_eq!((caps.humidity.min, caps.humidity.max), (35.0, 80.0));
        assert_eq!(caps.modes, vec!["normal".to_owned(), "sleep".to_owned()]);
        let Some(Ok(irori_types::State::Humidifier(state))) = crate::state::decode(
            &parsed.topics,
            "d/state",
            br#"{"state": "ON", "target": 50, "humidity": 64}"#,
            None,
        ) else {
            panic!("a humidifier state");
        };
        assert!(state.on);
        assert_eq!(
            (state.target_humidity, state.current_humidity),
            (Some(50.0), Some(64.0))
        );
        let drier = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::HumidifierSetHumidity(irori_types::SetHumidity {
                humidity: 45.0,
            }),
        )
        .expect("sent");
        assert_eq!(
            (drier[0].topic.as_str(), drier[0].payload.as_slice()),
            ("d/target/set", b"45".as_slice())
        );
        let off = crate::state::encode(&parsed.topics, &irori_types::Service::HumidifierTurnOff)
            .expect("sent");
        assert_eq!(off[0].payload, b"OFF");
    }
}
