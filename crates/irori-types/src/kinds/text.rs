//! `text`: a piece of text a person (or a rule) sets, e.g. a message for a display, a device's
//! Wi-Fi password, a name it announces itself by.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{Service, ServiceName};

use crate::InvariantError;

/// The longest text anything is asked to hold, as in Home Assistant.
const MAX_TEXT: u32 = 255;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextCapabilities {
    /// The fewest characters it takes.
    #[serde(default)]
    #[schemars(range(max = 255))]
    pub min_length: u32,
    /// The most characters it takes, at most 255.
    #[serde(default = "max_text")]
    #[schemars(range(min = 0, max = 255))]
    pub max_length: u32,
    /// A regular expression the text has to match, as the device says. The device checks it;
    /// Irori shows it to whoever is typing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Whether it's a secret, which pages don't show.
    #[serde(default, skip_serializing_if = "TextMode::is_text")]
    pub mode: TextMode,
}

fn max_text() -> u32 {
    MAX_TEXT
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextMode {
    #[default]
    Text,
    Password,
}

impl TextMode {
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
    fn is_text(&self) -> bool {
        *self == Self::Text
    }
}

impl TextCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.max_length > MAX_TEXT {
            return Err(InvariantError(format!(
                "a text's max_length is at most {MAX_TEXT}, not {}",
                self.max_length
            )));
        }
        if self.min_length > self.max_length {
            return Err(InvariantError(format!(
                "a text's min_length {} is above its max_length {}",
                self.min_length, self.max_length
            )));
        }
        Ok(())
    }

    /// Whether it can hold `value`. `Err` follows the entity's name.
    fn takes(&self, value: &str) -> Result<(), String> {
        let length = value.chars().count();
        let (min, max) = (self.min_length as usize, self.max_length as usize);
        if (min..=max).contains(&length) {
            Ok(())
        } else {
            Err(format!("takes {min} to {max} characters, not {length}"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextState {
    pub value: String,
}

/// Data for `text.set_value`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextSetValue {
    /// Within the text's `min_length` and `max_length`.
    pub value: String,
}

/// Whether a text can be asked to hold this.
pub(crate) fn supports(caps: &TextCapabilities, data: &TextSetValue) -> Result<(), String> {
    caps.takes(&data.value)
}

/// Whether a reported text is one it can hold.
pub(crate) fn fits(caps: &TextCapabilities, state: &TextState) -> Result<(), String> {
    caps.takes(&state.value)
        .map_err(|what| format!("it {what}"))
}

pub(crate) fn primary(state: &TextState) -> Typed {
    Typed::Text(state.value.clone())
}

pub(crate) fn with_primary(value: &Typed) -> Option<TextState> {
    match value {
        Typed::Text(value) => Some(TextState {
            value: value.clone(),
        }),
        _ => None,
    }
}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::Required
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    match name {
        ServiceName::TextSetValue => Ok(Service::TextSetValue(super::parse(name, data)?)),
        _ => Err(super::not_mine(name)),
    }
}

pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::TextSetValue(data) => Some(Typed::Text(data.value.clone())),
        _ => None,
    }
}

pub(crate) fn supports_service(caps: &TextCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::TextSetValue(data) => supports(caps, data),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_takes_only_what_fits() {
        let message = TextCapabilities {
            min_length: 1,
            max_length: 8,
            pattern: None,
            mode: TextMode::Text,
        };
        assert!(
            supports(
                &message,
                &TextSetValue {
                    value: "Hallo".into()
                }
            )
            .is_ok()
        );
        assert_eq!(
            supports(
                &message,
                &TextSetValue {
                    value: "Good morning".into()
                }
            ),
            Err("takes 1 to 8 characters, not 12".to_owned())
        );
        // Characters, not bytes: an ü is one.
        assert!(
            supports(
                &message,
                &TextSetValue {
                    value: "Grüße".into()
                }
            )
            .is_ok()
        );
        let backwards = TextCapabilities {
            min_length: 9,
            ..message.clone()
        };
        assert!(backwards.validate().is_err());
        let too_long = TextCapabilities {
            max_length: 1000,
            ..message
        };
        assert!(too_long.validate().is_err());
    }

    #[test]
    fn a_text_left_unsaid_takes_up_to_255_characters() {
        let caps: TextCapabilities = serde_json::from_str("{}").expect("all defaults");
        assert_eq!((caps.min_length, caps.max_length), (0, 255));
        assert_eq!(caps.mode, TextMode::Text);
    }
}
