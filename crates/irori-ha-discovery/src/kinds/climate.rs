//! Home Assistant's MQTT climate: a thermostat or air conditioner, each setting a [`Setting`] of
//! its own, often all on one topic. Temperatures go out and come in in its `unit`; Irori's model
//! holds °C.

use irori_types::units::{TemperatureUnit, round_to};
use irori_types::{
    Capabilities, ClimateCapabilities, ClimateState, HumidityRange, HvacAction, HvacMode, Service,
    State, mode_to_turn_on,
};

use super::setting::{
    Power, Reading, Setting, power, reading, setting, state_of, switch_or_mode, temperature_unit,
};
use crate::discovery::{EntityTopics, number_field, string_list};
use crate::state::{Message, Publish, device_temperature, number_text, update};

#[derive(Debug, Clone, PartialEq)]
pub struct ClimateTopics {
    pub unit: TemperatureUnit,
    pub modes: Vec<HvacMode>,
    pub mode: Option<Setting>,
    pub temperature: Option<Setting>,
    pub temperature_low: Option<Setting>,
    pub temperature_high: Option<Setting>,
    pub target_humidity: Option<Setting>,
    pub fan_mode: Option<Setting>,
    pub swing_mode: Option<Setting>,
    pub preset_mode: Option<Setting>,
    pub current_temperature: Option<Reading>,
    pub current_humidity: Option<Reading>,
    pub action: Option<Reading>,
    /// Its own on and off, when it has them; otherwise turning it off sets the `off` mode.
    pub power: Option<Power>,
}

impl ClimateTopics {
    /// Everything it reports on.
    pub(crate) fn readings(&self) -> impl Iterator<Item = &Reading> {
        [
            &self.mode,
            &self.temperature,
            &self.temperature_low,
            &self.temperature_high,
            &self.target_humidity,
            &self.fan_mode,
            &self.swing_mode,
            &self.preset_mode,
        ]
        .into_iter()
        .filter_map(|setting| state_of(setting.as_ref()))
        .chain(
            [
                &self.current_temperature,
                &self.current_humidity,
                &self.action,
            ]
            .into_iter()
            .flatten(),
        )
    }

    /// Whether it ever says its mode: one that doesn't is in the first it has.
    fn says_its_mode(&self) -> bool {
        state_of(self.mode.as_ref()).is_some()
    }
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let unit = temperature_unit(root)?;
    let modes: Vec<HvacMode> = string_list(root, "modes")
        .unwrap_or_else(|| {
            ["auto", "off", "cool", "heat", "dry", "fan_only"]
                .map(str::to_owned)
                .to_vec()
        })
        .iter()
        .filter_map(|mode| HvacMode::parse(mode))
        .collect();
    let topics = ClimateTopics {
        unit,
        mode: setting(root, "mode", "mode_state_template"),
        temperature: setting(root, "temperature", "temperature_state_template"),
        temperature_low: setting(root, "temperature_low", "temperature_low_state_template"),
        temperature_high: setting(root, "temperature_high", "temperature_high_state_template"),
        target_humidity: setting(root, "target_humidity", "target_humidity_state_template"),
        fan_mode: setting(root, "fan_mode", "fan_mode_state_template"),
        swing_mode: setting(root, "swing_mode", "swing_mode_state_template"),
        preset_mode: setting(root, "preset_mode", "preset_mode_value_template"),
        current_temperature: reading(root, "current_temperature"),
        current_humidity: reading(root, "current_humidity"),
        action: reading(root, "action"),
        power: power(root),
        modes: modes.clone(),
    };
    // Home Assistant's defaults, which are in °C, or °F for a device that speaks °F.
    let (default_min, default_max) = match unit {
        TemperatureUnit::Fahrenheit => (44.6, 95.0),
        TemperatureUnit::Celsius | TemperatureUnit::Kelvin => (7.0, 35.0),
    };
    let to_celsius = |value: f64| round_to(unit.to_celsius(value), 2);
    let list = |key: &str, setting: &Option<Setting>| -> Vec<String> {
        if setting.is_some() {
            string_list(root, key).unwrap_or_default()
        } else {
            Vec::new()
        }
    };
    let capabilities = ClimateCapabilities {
        hvac_modes: if modes.is_empty() {
            vec![HvacMode::Off]
        } else {
            modes
        },
        min_temp: to_celsius(number_field(root, "min_temp").unwrap_or(default_min)),
        max_temp: to_celsius(number_field(root, "max_temp").unwrap_or(default_max)),
        temp_step: round_to(
            unit.step_to_celsius(number_field(root, "temp_step").unwrap_or(1.0)),
            2,
        ),
        target_temperature: topics.temperature.is_some(),
        target_temperature_range: topics.temperature_low.is_some()
            && topics.temperature_high.is_some(),
        target_humidity: topics.target_humidity.as_ref().map(|_| HumidityRange {
            min: number_field(root, "min_humidity").unwrap_or(30.0),
            max: number_field(root, "max_humidity").unwrap_or(99.0),
        }),
        fan_modes: list("fan_modes", &topics.fan_mode),
        swing_modes: list("swing_modes", &topics.swing_mode),
        preset_modes: list("preset_modes", &topics.preset_mode),
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Climate(capabilities),
        EntityTopics::Climate(Box::new(topics)),
    ))
}

/// A message changes the settings it carries and keeps the rest of `previous`; until its mode is
/// known there's nothing to report, since a climate state has to say its mode.
pub(crate) fn decode(
    climate: &ClimateTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    if !message.on_any(climate.readings()) {
        return None;
    }
    let unit = climate.unit;
    let temperature =
        |setting: &Option<Setting>| message.temperature(state_of(setting.as_ref()), unit);
    let said = |setting: &Option<Setting>| message.said(state_of(setting.as_ref()));
    Some((|| {
        let (mut state, mut mode_known) = match previous {
            Some(State::Climate(old)) => (old.clone(), true),
            _ if !climate.says_its_mode() => (
                ClimateState::in_mode(*climate.modes.first().unwrap_or(&HvacMode::Off)),
                true,
            ),
            _ => (ClimateState::in_mode(HvacMode::Off), false),
        };
        if let Some(Some(text)) = said(&climate.mode) {
            state.hvac_mode = HvacMode::parse(&text)
                .ok_or_else(|| format!("{text:?} isn't a mode this thermostat has"))?;
            mode_known = true;
        }
        update(
            &mut state.target_temperature,
            temperature(&climate.temperature),
        );
        update(
            &mut state.target_temp_low,
            temperature(&climate.temperature_low),
        );
        update(
            &mut state.target_temp_high,
            temperature(&climate.temperature_high),
        );
        update(
            &mut state.target_humidity,
            message.number(state_of(climate.target_humidity.as_ref())),
        );
        update(&mut state.fan_mode, said(&climate.fan_mode));
        update(&mut state.swing_mode, said(&climate.swing_mode));
        update(
            &mut state.preset_mode,
            said(&climate.preset_mode).map(|mode| mode.filter(|mode| mode != "none")),
        );
        update(
            &mut state.current_temperature,
            message.temperature(climate.current_temperature.as_ref(), unit),
        );
        update(
            &mut state.current_humidity,
            message.number(climate.current_humidity.as_ref()),
        );
        update(
            &mut state.hvac_action,
            message
                .said(climate.action.as_ref())
                .map(|text| text.as_deref().and_then(hvac_action)),
        );
        if !mode_known {
            return Err("it hasn't said its mode yet".to_owned());
        }
        Ok(State::Climate(state))
    })())
}

/// What a thermostat says it's doing, as Home Assistant's action, or as Zigbee2MQTT's
/// `running_state` that its action template would have turned into one.
fn hvac_action(said: &str) -> Option<HvacAction> {
    HvacAction::parse(said).or(match said {
        "heat" => Some(HvacAction::Heating),
        "cool" => Some(HvacAction::Cooling),
        "fan_only" => Some(HvacAction::Fan),
        _ => None,
    })
}

/// `last_on` is the last state it reported in a mode other than `off`: what `turn_on` goes back
/// to when it has no switch of its own.
pub(crate) fn encode(
    climate: &ClimateTopics,
    service: &Service,
    last_on: Option<&State>,
) -> Result<Vec<Publish>, String> {
    let mode = |mode: HvacMode| -> Result<Publish, String> {
        let setting = climate
            .mode
            .as_ref()
            .ok_or("this thermostat's mode can't be set")?;
        Ok(setting.publish(mode.as_str()))
    };
    let temperature = |setting: &Option<Setting>, celsius: f64| -> Result<Publish, String> {
        let setting = setting
            .as_ref()
            .ok_or("this thermostat doesn't take that target")?;
        Ok(setting.publish(&device_temperature(climate.unit, celsius)))
    };
    let word =
        |setting: &Option<Setting>, value: &str, what: &str| -> Result<Vec<Publish>, String> {
            let setting = setting
                .as_ref()
                .ok_or_else(|| format!("this thermostat has no {what}"))?;
            Ok(vec![setting.publish(value)])
        };
    match service {
        Service::ClimateSetHvacMode(data) => Ok(vec![mode(data.hvac_mode)?]),
        Service::ClimateTurnOff => {
            switch_or_mode(climate.power.as_ref(), false, || mode(HvacMode::Off))
        }
        Service::ClimateTurnOn => switch_or_mode(climate.power.as_ref(), true, || {
            let last = match last_on {
                Some(State::Climate(state)) => Some(state.hvac_mode),
                _ => None,
            };
            mode(
                mode_to_turn_on(&climate.modes, HvacMode::Off, last)
                    .ok_or("this thermostat has no mode to turn on to")?,
            )
        }),
        Service::ClimateSetTemperature(data) => {
            let mut messages = Vec::new();
            if let Some(hvac_mode) = data.hvac_mode {
                messages.push(mode(hvac_mode)?);
            }
            if let Some(value) = data.temperature {
                messages.push(temperature(&climate.temperature, value)?);
            }
            if let Some(low) = data.target_temp_low {
                messages.push(temperature(&climate.temperature_low, low)?);
            }
            if let Some(high) = data.target_temp_high {
                messages.push(temperature(&climate.temperature_high, high)?);
            }
            Ok(messages)
        }
        Service::ClimateSetHumidity(data) => word(
            &climate.target_humidity,
            &number_text(data.humidity),
            "target humidity",
        ),
        Service::ClimateSetFanMode(data) => word(&climate.fan_mode, &data.fan_mode, "fan modes"),
        Service::ClimateSetSwingMode(data) => {
            word(&climate.swing_mode, &data.swing_mode, "swing modes")
        }
        Service::ClimateSetPresetMode(data) => {
            word(&climate.preset_mode, &data.preset_mode, "presets")
        }
        service => Err(super::no_service("a thermostat", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    /// A Zigbee2MQTT radiator valve, as 2.x publishes it.
    #[test]
    fn a_zigbee2mqtt_thermostat_is_read_and_told() {
        let parsed = parse(
            Component::Climate,
            br#"{"unique_id": "0x01_climate_zigbee2mqtt", "name": null,
                "device": {"identifiers": ["zigbee2mqtt_0x01"], "name": "Bedroom TRV"},
                "action_template": "{% set values = {None:None,'idle':'idle','heat':'heating','cool':'cooling','fan_only':'fan'} %}{{ values[value_json.running_state] }}",
                "action_topic": "zigbee2mqtt/TRV",
                "current_temperature_template": "{{ value_json.local_temperature }}",
                "current_temperature_topic": "zigbee2mqtt/TRV",
                "max_temp": "30", "min_temp": "5",
                "mode_command_topic": "zigbee2mqtt/TRV/set/system_mode",
                "mode_state_template": "{{ value_json.system_mode }}",
                "mode_state_topic": "zigbee2mqtt/TRV",
                "modes": ["off", "heat", "auto"],
                "preset_mode_command_topic": "zigbee2mqtt/TRV/set/preset",
                "preset_mode_state_topic": "zigbee2mqtt/TRV",
                "preset_mode_value_template": "{{ value_json.preset }}",
                "preset_modes": ["manual", "boost"],
                "temp_step": 0.5,
                "temperature_command_topic": "zigbee2mqtt/TRV/set/current_heating_setpoint",
                "temperature_state_template": "{{ value_json.current_heating_setpoint }}",
                "temperature_state_topic": "zigbee2mqtt/TRV",
                "temperature_unit": "C"}"#,
        )
        .expect("valid");
        let Capabilities::Climate(caps) = &parsed.capabilities else {
            panic!("a climate entity");
        };
        assert_eq!(
            caps.hvac_modes,
            vec![HvacMode::Off, HvacMode::Heat, HvacMode::Auto]
        );
        assert_eq!(
            (caps.min_temp, caps.max_temp, caps.temp_step),
            (5.0, 30.0, 0.5)
        );
        assert!(caps.target_temperature);
        assert_eq!(
            caps.preset_modes,
            vec!["manual".to_owned(), "boost".to_owned()]
        );

        let Some(Ok(irori_types::State::Climate(state))) = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/TRV",
            br#"{"system_mode": "heat", "local_temperature": 19.5,
                 "current_heating_setpoint": 21, "running_state": "heat", "preset": "manual"}"#,
            None,
        ) else {
            panic!(
                "a climate state: {:?}",
                crate::state::decode(
                    &parsed.topics,
                    "zigbee2mqtt/TRV",
                    br#"{"system_mode": "heat"}"#,
                    None
                )
            );
        };
        assert_eq!(state.hvac_mode, HvacMode::Heat);
        assert_eq!(state.hvac_action, Some(irori_types::HvacAction::Heating));
        assert_eq!(state.current_temperature, Some(19.5));
        assert_eq!(state.target_temperature, Some(21.0));
        assert_eq!(state.preset_mode.as_deref(), Some("manual"));

        let warmer = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::ClimateSetTemperature(irori_types::ClimateSetTemperature {
                temperature: Some(22.5),
                ..Default::default()
            }),
        )
        .expect("sent");
        assert_eq!(
            warmer[0].topic,
            "zigbee2mqtt/TRV/set/current_heating_setpoint"
        );
        assert_eq!(warmer[0].payload, b"22.5");

        // Off, then on again: back to the mode it was in, not the first in its list.
        let was = irori_types::State::Climate(irori_types::ClimateState::in_mode(HvacMode::Auto));
        let on = crate::state::encode_with(
            &parsed.topics,
            &irori_types::Service::ClimateTurnOn,
            Some(&was),
        )
        .expect("sent");
        assert_eq!(
            (on[0].topic.as_str(), on[0].payload.as_slice()),
            ("zigbee2mqtt/TRV/set/system_mode", b"auto".as_slice())
        );
        let off = crate::state::encode(&parsed.topics, &irori_types::Service::ClimateTurnOff)
            .expect("sent");
        assert_eq!(off[0].payload, b"off");
    }

    #[test]
    fn a_thermostat_in_fahrenheit_is_held_in_celsius() {
        let parsed = parse(
            Component::Climate,
            br#"{"unique_id": "t", "name": "Hall", "temperature_unit": "F",
                "mode_command_topic": "t/mode/set", "mode_state_topic": "t/mode",
                "modes": ["off", "cool"],
                "temperature_command_topic": "t/target/set", "temperature_state_topic": "t/target"}"#,
        )
        .expect("valid");
        let Capabilities::Climate(caps) = &parsed.capabilities else {
            panic!("a climate entity");
        };
        assert_eq!((caps.min_temp, caps.max_temp), (7.0, 35.0));
        // The mode first, then the target in °F comes in as °C.
        let mode = crate::state::decode(&parsed.topics, "t/mode", b"cool", None)
            .expect("its topic")
            .expect("read");
        let Some(Ok(irori_types::State::Climate(state))) =
            crate::state::decode(&parsed.topics, "t/target", b"77", Some(&mode))
        else {
            panic!("a climate state");
        };
        assert_eq!(state.target_temperature, Some(25.0));
        assert!(
            crate::state::decode(&parsed.topics, "t/target", b"77", None)
                .expect("its topic")
                .is_err(),
            "no state until it says its mode"
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::ClimateSetTemperature(irori_types::ClimateSetTemperature {
                temperature: Some(21.0),
                ..Default::default()
            }),
        )
        .expect("sent");
        assert_eq!(sent[0].payload, b"69.8");
    }
}
