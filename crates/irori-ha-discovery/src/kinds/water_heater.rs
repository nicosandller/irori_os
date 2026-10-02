//! Home Assistant's MQTT water heater: a mode, a target and the water's temperature, each on its
//! own topics, read and told the way a thermostat's are ([`super::climate`]).

use irori_types::units::{TemperatureUnit, round_to};
use irori_types::{
    Capabilities, Service, State, WaterHeaterCapabilities, WaterHeaterMode, WaterHeaterState,
    mode_to_turn_on,
};

use super::setting::{
    Power, Reading, Setting, power, reading, setting, state_of, switch_or_mode, temperature_unit,
};
use crate::discovery::{EntityTopics, number_field, string_list};
use crate::state::{Message, Publish, device_temperature, update};

/// How a water heater is told what to do and says what it's doing, temperatures in `unit`.
#[derive(Debug, Clone, PartialEq)]
pub struct WaterHeaterTopics {
    pub unit: TemperatureUnit,
    pub modes: Vec<WaterHeaterMode>,
    pub mode: Option<Setting>,
    pub temperature: Option<Setting>,
    pub current_temperature: Option<Reading>,
    pub power: Option<Power>,
}

impl WaterHeaterTopics {
    /// Everything it reports on.
    pub(crate) fn readings(&self) -> impl Iterator<Item = &Reading> {
        [
            state_of(self.mode.as_ref()),
            state_of(self.temperature.as_ref()),
            self.current_temperature.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let unit = temperature_unit(root)?;
    let modes: Vec<WaterHeaterMode> = string_list(root, "modes")
        .unwrap_or_else(|| {
            [
                "off",
                "eco",
                "electric",
                "gas",
                "heat_pump",
                "high_demand",
                "performance",
            ]
            .map(str::to_owned)
            .to_vec()
        })
        .iter()
        .filter_map(|mode| WaterHeaterMode::parse(mode))
        .collect();
    let topics = WaterHeaterTopics {
        unit,
        modes: modes.clone(),
        mode: setting(root, "mode", "mode_state_template"),
        temperature: setting(root, "temperature", "temperature_state_template"),
        current_temperature: reading(root, "current_temperature"),
        power: power(root),
    };
    // Home Assistant's defaults: 110-140 °F, which is 43.3-60 °C.
    let (default_min, default_max) = match unit {
        TemperatureUnit::Fahrenheit => (110.0, 140.0),
        TemperatureUnit::Celsius | TemperatureUnit::Kelvin => (43.3, 60.0),
    };
    let to_celsius = |value: f64| round_to(unit.to_celsius(value), 2);
    let capabilities = WaterHeaterCapabilities {
        operation_modes: if modes.is_empty() {
            vec![WaterHeaterMode::Off]
        } else {
            modes
        },
        min_temp: to_celsius(number_field(root, "min_temp").unwrap_or(default_min)),
        max_temp: to_celsius(number_field(root, "max_temp").unwrap_or(default_max)),
        temp_step: round_to(
            unit.step_to_celsius(number_field(root, "precision").unwrap_or(1.0)),
            2,
        ),
        target_temperature: topics.temperature.is_some(),
        on_off: topics.power.is_some(),
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::WaterHeater(capabilities),
        EntityTopics::WaterHeater(Box::new(topics)),
    ))
}

pub(crate) fn decode(
    heater: &WaterHeaterTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    if !message.on_any(heater.readings()) {
        return None;
    }
    let previous = match previous {
        Some(State::WaterHeater(old)) => Some(old),
        _ => None,
    };
    Some((|| {
        let said_mode = message.said(state_of(heater.mode.as_ref())).flatten();
        let operation_mode = match (&said_mode, previous) {
            (Some(text), _) => WaterHeaterMode::parse(text)
                .ok_or_else(|| format!("{text:?} isn't a mode this water heater has"))?,
            (None, Some(old)) => old.operation_mode,
            // One with no mode topic never says its mode: it's in the first it has.
            (None, None) if state_of(heater.mode.as_ref()).is_none() => {
                *heater.modes.first().unwrap_or(&WaterHeaterMode::Off)
            }
            (None, None) => return Err("it hasn't said its mode yet".to_owned()),
        };
        let mut state = previous.cloned().unwrap_or(WaterHeaterState {
            operation_mode,
            current_temperature: None,
            target_temperature: None,
        });
        state.operation_mode = operation_mode;
        update(
            &mut state.target_temperature,
            message.temperature(state_of(heater.temperature.as_ref()), heater.unit),
        );
        update(
            &mut state.current_temperature,
            message.temperature(heater.current_temperature.as_ref(), heater.unit),
        );
        Ok(State::WaterHeater(state))
    })())
}

/// `last_on` is the last state it reported in a mode other than `off`: what `turn_on` goes back
/// to when it has no switch of its own.
pub(crate) fn encode(
    heater: &WaterHeaterTopics,
    service: &Service,
    last_on: Option<&State>,
) -> Result<Vec<Publish>, String> {
    let mode = |mode: WaterHeaterMode| -> Result<Publish, String> {
        let setting = heater
            .mode
            .as_ref()
            .ok_or("this water heater's mode can't be set")?;
        Ok(setting.publish(mode.as_str()))
    };
    match service {
        Service::WaterHeaterSetOperationMode(data) => Ok(vec![mode(data.operation_mode)?]),
        Service::WaterHeaterSetTemperature(data) => {
            let mut messages = Vec::new();
            if let Some(operation_mode) = data.operation_mode {
                messages.push(mode(operation_mode)?);
            }
            let setting = heater
                .temperature
                .as_ref()
                .ok_or("this water heater doesn't take a target")?;
            messages.push(setting.publish(&device_temperature(heater.unit, data.temperature)));
            Ok(messages)
        }
        Service::WaterHeaterTurnOff => {
            switch_or_mode(heater.power.as_ref(), false, || mode(WaterHeaterMode::Off))
        }
        Service::WaterHeaterTurnOn => switch_or_mode(heater.power.as_ref(), true, || {
            let last = match last_on {
                Some(State::WaterHeater(state)) => Some(state.operation_mode),
                _ => None,
            };
            mode(
                mode_to_turn_on(&heater.modes, WaterHeaterMode::Off, last)
                    .ok_or("this water heater has no mode to turn on to")?,
            )
        }),
        service => Err(super::no_service("a water heater", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn an_mqtt_water_heater_in_fahrenheit_is_read_and_told_in_celsius() {
        let parsed = parse(
            Component::WaterHeater,
            br#"{"unique_id": "w", "name": "Tank", "temperature_unit": "F",
                "modes": ["off", "eco", "performance"],
                "mode_command_topic": "w/mode/set", "mode_state_topic": "w/state",
                "mode_state_template": "{{ value_json.mode }}",
                "temperature_command_topic": "w/temp/set", "temperature_state_topic": "w/state",
                "temperature_state_template": "{{ value_json.target }}",
                "current_temperature_topic": "w/state",
                "current_temperature_template": "{{ value_json.water }}"}"#,
        )
        .expect("valid");
        let Capabilities::WaterHeater(caps) = &parsed.capabilities else {
            panic!("a water heater");
        };
        assert_eq!((caps.min_temp, caps.max_temp), (43.33, 60.0));
        assert!(!caps.on_off && caps.target_temperature);
        let Some(Ok(irori_types::State::WaterHeater(state))) = crate::state::decode(
            &parsed.topics,
            "w/state",
            br#"{"mode": "eco", "target": 131, "water": 122}"#,
            None,
        ) else {
            panic!("a water heater state");
        };
        assert_eq!(state.operation_mode, WaterHeaterMode::Eco);
        assert_eq!(
            (state.target_temperature, state.current_temperature),
            (Some(55.0), Some(50.0))
        );
        let was = irori_types::State::WaterHeater(state);
        let on = crate::state::encode_with(
            &parsed.topics,
            &irori_types::Service::WaterHeaterTurnOn,
            Some(&was),
        )
        .expect("sent");
        assert_eq!(
            (on[0].topic.as_str(), on[0].payload.as_slice()),
            ("w/mode/set", b"eco".as_slice())
        );
        let hotter = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::WaterHeaterSetTemperature(
                irori_types::WaterHeaterSetTemperature {
                    temperature: 60.0,
                    operation_mode: None,
                },
            ),
        )
        .expect("sent");
        assert_eq!(hotter[0].payload, b"140");
    }
}
