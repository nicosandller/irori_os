//! Home Assistant's MQTT fan: on and off, and optionally a speed, presets, oscillation and a
//! direction, each a [`Setting`] of its own. Zigbee2MQTT sends them all in one body; others give
//! each setting a topic of its own.

use irori_types::{
    Capabilities, FanCapabilities, FanDirection, FanPercentage, FanState, Service, State,
    percentage_to_speed, speed_to_percentage,
};

use super::setting::{Reading, Setting, looked_up, setting, state_of};
use crate::discovery::{EntityTopics, owned_str, plain_command, str_field, string_list};
use crate::state::{Message, Publish};
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanTopics {
    /// On and off.
    pub power: Setting,
    pub payload_on: String,
    pub payload_off: String,
    /// Its speed, as the device numbers it from `speed_min` to `speed_max`.
    pub speed: Option<Setting>,
    pub speed_min: i64,
    pub speed_max: i64,
    pub preset: Option<Setting>,
    pub oscillation: Option<Setting>,
    pub payload_oscillation_on: String,
    pub payload_oscillation_off: String,
    pub direction: Option<Setting>,
}

impl FanTopics {
    /// Everything it reports on.
    pub(crate) fn readings(&self) -> impl Iterator<Item = &Reading> {
        [
            Some(&self.power),
            self.speed.as_ref(),
            self.preset.as_ref(),
            self.oscillation.as_ref(),
            self.direction.as_ref(),
        ]
        .into_iter()
        .filter_map(state_of)
    }
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a fan")?;
    let speed_min = root
        .get("speed_range_min")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1);
    let speed_max = root
        .get("speed_range_max")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(100);
    // A command template Irori can't render leaves that setting out, rather than the whole fan:
    // on and off still work.
    let speed = setting(root, "percentage", "percentage_value_template");
    // Only presets are read through a lookup's value: a value outside the list is dropped anyway.
    let preset = setting(root, "preset_mode", "preset_mode_value_template").map(|mut preset| {
        if let Some(reading) = &mut preset.state {
            reading.template = looked_up(reading.template.clone());
        }
        preset
    });
    let preset_modes = if preset.is_some() {
        string_list(root, "preset_modes").unwrap_or_default()
    } else {
        Vec::new()
    };
    let topics = FanTopics {
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
        speed_min,
        speed_max,
        oscillation: setting(root, "oscillation", "oscillation_value_template"),
        payload_oscillation_on: owned_str(root, "payload_oscillation_on", "oscillate_on"),
        payload_oscillation_off: owned_str(root, "payload_oscillation_off", "oscillate_off"),
        direction: setting(root, "direction", "direction_value_template"),
        speed,
        preset,
    };
    let capabilities = FanCapabilities {
        speed_count: if topics.speed.is_some() && speed_max >= speed_min {
            u16::try_from(speed_max - speed_min + 1).unwrap_or(u16::MAX)
        } else {
            0
        },
        oscillate: topics.oscillation.is_some(),
        direction: topics.direction.is_some(),
        preset_modes,
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Fan(capabilities),
        EntityTopics::Fan(Box::new(topics)),
    ))
}

/// A fan's state from a message on any of its topics, merged with what it last said.
pub(crate) fn decode(
    fan: &FanTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    if !message.on_any(fan.readings()) {
        return None;
    }
    Some((|| {
        let mut state = match previous {
            Some(State::Fan(old)) => old.clone(),
            _ => FanState {
                on: false,
                percentage: None,
                oscillating: None,
                direction: None,
                preset_mode: None,
            },
        };
        match message.said(fan.power.state.as_ref()).flatten() {
            Some(said) if said == fan.payload_on => state.on = true,
            Some(said) if said == fan.payload_off => state.on = false,
            Some(said) => return Err(format!("{said:?} isn't on or off for this fan")),
            None => {}
        }
        if let Some(raw) = message.number(state_of(fan.speed.as_ref())).flatten() {
            let count = fan.speed_max - fan.speed_min + 1;
            #[allow(clippy::cast_possible_truncation)]
            let level = raw.round() as i64 - fan.speed_min + 1;
            state.percentage = Some(if level <= 0 || count <= 0 {
                0
            } else {
                speed_to_percentage(
                    u16::try_from(level).unwrap_or(u16::MAX),
                    u16::try_from(count).unwrap_or(u16::MAX),
                )
            });
        }
        // Anything not a preset (a speed, `None`) means it isn't in one.
        if let Some(said) = message.said(state_of(fan.preset.as_ref())) {
            state.preset_mode = said;
        }
        match message.said(state_of(fan.oscillation.as_ref())).flatten() {
            Some(said) if said == fan.payload_oscillation_on => state.oscillating = Some(true),
            Some(said) if said == fan.payload_oscillation_off => state.oscillating = Some(false),
            _ => {}
        }
        match message
            .said(state_of(fan.direction.as_ref()))
            .flatten()
            .as_deref()
        {
            Some("forward") => state.direction = Some(FanDirection::Forward),
            Some("reverse") => state.direction = Some(FanDirection::Reverse),
            _ => {}
        }
        Ok(State::Fan(state))
    })())
}

pub(crate) fn encode(fan: &FanTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let speed = |percentage: u8| -> Result<Publish, String> {
        let setting = fan.speed.as_ref().ok_or("this fan has no speeds to set")?;
        let count = u16::try_from(fan.speed_max - fan.speed_min + 1).unwrap_or(u16::MAX);
        let level = i64::from(percentage_to_speed(percentage, count)) + fan.speed_min - 1;
        Ok(setting.publish(&level.to_string()))
    };
    let preset = |mode: &str| -> Result<Publish, String> {
        let setting = fan.preset.as_ref().ok_or("this fan has no preset modes")?;
        Ok(setting.publish(mode))
    };
    match service {
        Service::FanTurnOff | Service::FanSetPercentage(FanPercentage { percentage: 0 }) => {
            Ok(vec![fan.power.publish(&fan.payload_off)])
        }
        Service::FanTurnOn(data) => {
            let mut messages = vec![fan.power.publish(&fan.payload_on)];
            if let Some(percentage) = data.percentage {
                messages.push(speed(percentage)?);
            }
            if let Some(mode) = &data.preset_mode {
                messages.push(preset(mode)?);
            }
            Ok(messages)
        }
        Service::FanSetPercentage(data) => Ok(vec![speed(data.percentage)?]),
        Service::FanSetPresetMode(data) => Ok(vec![preset(&data.preset_mode)?]),
        Service::FanOscillate(data) => {
            let setting = fan.oscillation.as_ref().ok_or("this fan doesn't swing")?;
            let payload = if data.oscillating {
                &fan.payload_oscillation_on
            } else {
                &fan.payload_oscillation_off
            };
            Ok(vec![setting.publish(payload)])
        }
        Service::FanSetDirection(data) => {
            let setting = fan
                .direction
                .as_ref()
                .ok_or("this fan only turns one way")?;
            let direction = match data.direction {
                FanDirection::Forward => "forward",
                FanDirection::Reverse => "reverse",
            };
            Ok(vec![setting.publish(direction)])
        }
        service => Err(super::no_service("a fan", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    /// Zigbee2MQTT's mode-controlled fan (`case "fan"`, a ZCL hvacFanCtrl): speeds are the
    /// modes low, medium and high, translated through lookup tables, plus preset modes.
    #[test]
    fn a_z2m_mode_fan_turns_speeds_into_modes_and_back() {
        let payload = br#"{
            "unique_id": "0x03_fan", "name": null,
            "device": {"identifiers": ["zigbee2mqtt_0x03"], "name": "Ceiling fan"},
            "command_topic": "zigbee2mqtt/Ceiling fan/set/fan_state",
            "state_topic": "zigbee2mqtt/Ceiling fan",
            "state_value_template": "{{ value_json.fan_state }}",
            "percentage_state_topic": "zigbee2mqtt/Ceiling fan",
            "percentage_command_topic": "zigbee2mqtt/Ceiling fan/set/fan_mode",
            "percentage_value_template": "{{ {'off':0, 'low':1, 'medium':2, 'high':3}[value_json[\"fan_mode\"]] | default('None') }}",
            "percentage_command_template": "{{ {0:'off', 1:'low', 2:'medium', 3:'high'}[value] | default('') }}",
            "speed_range_min": 1, "speed_range_max": 3,
            "preset_mode_state_topic": "zigbee2mqtt/Ceiling fan",
            "preset_mode_command_topic": "zigbee2mqtt/Ceiling fan/set/fan_mode",
            "preset_mode_value_template": "{{ value_json[\"fan_mode\"] if value_json[\"fan_mode\"] in ['on', 'auto'] else 'None' | default('None') }}",
            "preset_modes": ["on", "auto"]
        }"#;
        let parsed = parse(Component::Fan, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Fan(FanCapabilities {
                speed_count: 3,
                oscillate: false,
                direction: false,
                preset_modes: vec!["on".into(), "auto".into()],
            })
        );
        let decoded = |body: &[u8]| {
            crate::state::decode(&parsed.topics, "zigbee2mqtt/Ceiling fan", body, None)
        };
        let Some(Ok(irori_types::State::Fan(medium))) =
            decoded(br#"{"fan_state": "ON", "fan_mode": "medium"}"#)
        else {
            panic!("a fan state");
        };
        assert!(medium.on);
        assert_eq!(medium.percentage, Some(66));
        let Some(Ok(irori_types::State::Fan(auto))) =
            decoded(br#"{"fan_state": "ON", "fan_mode": "auto"}"#)
        else {
            panic!("a fan state");
        };
        assert_eq!(auto.preset_mode.as_deref(), Some("auto"));
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::FanSetPercentage(irori_types::FanPercentage { percentage: 100 }),
        )
        .expect("a speed");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Ceiling fan/set/fan_mode");
        assert_eq!(sent[0].payload, b"high");
        let off = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::FanSetPercentage(irori_types::FanPercentage { percentage: 0 }),
        )
        .expect("off");
        assert_eq!(off[0].payload, b"OFF");
    }

    /// Zigbee2MQTT's speed-controlled fan: a number with `| default`, sent as it is.
    #[test]
    fn a_z2m_speed_fan_sends_its_speed() {
        let payload = br#"{"unique_id": "f", "name": "Fan", "command_topic": "z/Fan/set/state",
            "state_topic": "z/Fan", "state_value_template": "{{ value_json.state }}",
            "percentage_state_topic": "z/Fan", "percentage_command_topic": "z/Fan/set/speed",
            "percentage_value_template": "{{ value_json[\"speed\"] | default('None') }}",
            "percentage_command_template": "{{ value | default('') }}",
            "speed_range_min": 1, "speed_range_max": 10}"#;
        let parsed = parse(Component::Fan, payload).expect("valid");
        let Some(Ok(irori_types::State::Fan(fan))) = crate::state::decode(
            &parsed.topics,
            "z/Fan",
            br#"{"state": "ON", "speed": 5}"#,
            None,
        ) else {
            panic!("a fan state");
        };
        assert_eq!((fan.on, fan.percentage), (true, Some(50)));
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::FanSetPercentage(irori_types::FanPercentage { percentage: 75 }),
        )
        .expect("a speed");
        assert_eq!(sent[0].payload, b"8");
    }
}
