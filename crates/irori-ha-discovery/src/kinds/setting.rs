//! The parts most Home Assistant MQTT components are made of: a [`Setting`] it's told and reads
//! back, a [`Reading`] it only reports, and its own on and off ([`Power`]). A component adds the
//! ones it has; [`crate::state::Message`] reads any of them out of a message.

use irori_types::units::TemperatureUnit;

use crate::discovery::{owned_str, str_field};
use crate::state::{Publish, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

/// A value it reports: on `topic`, picked out of the message by `template`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub topic: String,
    pub template: ValueTemplate,
}

/// Something it's told and, when it says so, reads back: a thermostat's mode, a fan's speed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub command_topic: String,
    pub command_template: CommandTemplate,
    /// Where it says what it's set to; `None` when it never does.
    pub state: Option<Reading>,
}

impl Setting {
    /// Sets it to `value`.
    pub fn publish(&self, value: &str) -> Publish {
        text_publish(&self.command_topic, &self.command_template.render(value))
    }
}

/// Where a setting reports, if it's there and does.
pub fn state_of(setting: Option<&Setting>) -> Option<&Reading> {
    setting.and_then(|setting| setting.state.as_ref())
}

/// Its own on and off, apart from its modes: `power_command_topic` and its payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Power {
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub payload_on: String,
    pub payload_off: String,
}

impl Power {
    /// Turns it on or off.
    pub fn publish(&self, on: bool) -> Publish {
        let payload = if on {
            &self.payload_on
        } else {
            &self.payload_off
        };
        text_publish(&self.command_topic, &self.command_template.render(payload))
    }
}

/// One setting from a discovery config: `<name>_command_topic` and `<name>_command_template`,
/// read back on `<name>_state_topic` through `<value_key>`. `None` without a command topic, or with
/// a command template Irori can't render: the rest of the entity still works.
pub(crate) fn setting(root: &serde_json::Value, name: &str, value_key: &str) -> Option<Setting> {
    let command_topic = str_field(root, &format!("{name}_command_topic"))?.to_owned();
    let command_template = CommandTemplate::parse(
        str_field(root, &format!("{name}_command_template")),
        "value",
    )
    .ok()?;
    Some(Setting {
        command_topic,
        command_template,
        state: str_field(root, &format!("{name}_state_topic")).map(|topic| Reading {
            topic: topic.to_owned(),
            template: ValueTemplate::parse(str_field(root, value_key)),
        }),
    })
}

/// A reading from a discovery config: `<name>_topic` through `<name>_template`.
pub(crate) fn reading(root: &serde_json::Value, name: &str) -> Option<Reading> {
    let topic = str_field(root, &format!("{name}_topic"))?.to_owned();
    Some(Reading {
        topic,
        template: looked_up(ValueTemplate::parse(str_field(
            root,
            &format!("{name}_template"),
        ))),
    })
}

/// A template that looks a value up in a Jinja table (Zigbee2MQTT's thermostat action, its fan's
/// preset) as the value it looks up, which the component then names the way the table would.
/// Any other template as it is.
pub(crate) fn looked_up(template: ValueTemplate) -> ValueTemplate {
    match template {
        ValueTemplate::Unsupported(text) => {
            ValueTemplate::first_path(&text).unwrap_or(ValueTemplate::Unsupported(text))
        }
        template => template,
    }
}

/// `power_command_topic` and its payloads, when it has its own on and off.
pub(crate) fn power(root: &serde_json::Value) -> Option<Power> {
    let topic = str_field(root, "power_command_topic")?;
    Some(Power {
        command_topic: topic.to_owned(),
        command_template: CommandTemplate::parse(
            str_field(root, "power_command_template"),
            "value",
        )
        .ok()?,
        payload_on: owned_str(root, "payload_on", "ON"),
        payload_off: owned_str(root, "payload_off", "OFF"),
    })
}

/// The unit a component's temperatures go out and come in in; Irori's model holds °C.
pub(crate) fn temperature_unit(root: &serde_json::Value) -> Result<TemperatureUnit, String> {
    match str_field(root, "temperature_unit") {
        Some(unit) => TemperatureUnit::parse(unit)
            .ok_or_else(|| format!("`temperature_unit` {unit:?} isn't C, F or K")),
        None => Ok(TemperatureUnit::Celsius),
    }
}

/// Turning on or off something with modes: by its own switch when it has one, otherwise by
/// `mode` (its `off` mode, or the one `turn_on` goes back to).
pub(crate) fn switch_or_mode(
    power: Option<&Power>,
    on: bool,
    mode: impl FnOnce() -> Result<Publish, String>,
) -> Result<Vec<Publish>, String> {
    Ok(vec![match power {
        Some(power) => power.publish(on),
        None => mode()?,
    }])
}
