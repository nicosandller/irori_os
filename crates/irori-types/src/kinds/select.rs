//! `select`: one choice out of a fixed list, set by a person (or a rule), e.g. a presence
//! sensor's sensitivity, a heater's mode, a bulb's power-on behaviour.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{Service, ServiceName};

use super::sensor::validate_options;
use crate::InvariantError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectCapabilities {
    /// Every choice it has, at least one.
    #[schemars(length(min = 1, max = 256))]
    pub options: Vec<String>,
}

impl SelectCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.options.is_empty() {
            return Err(InvariantError("a select needs at least one option".into()));
        }
        validate_options(&self.options)
    }

    /// Whether `option` is one of its choices. `Err` follows the entity's name.
    fn has(&self, option: &str) -> Result<(), String> {
        if self.options.iter().any(|o| o == option) {
            Ok(())
        } else {
            Err(format!(
                "is one of {}, not {option:?}",
                self.options.join(", ")
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectState {
    /// The choice it's on: one of its options.
    pub option: String,
}

/// Data for `select.select_option`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    /// One of the select's options.
    pub option: String,
}

/// Whether a select can be asked for this option.
pub(crate) fn supports(caps: &SelectCapabilities, data: &SelectOption) -> Result<(), String> {
    caps.has(&data.option)
}

/// Whether a reported option is one of its choices.
pub(crate) fn fits(caps: &SelectCapabilities, state: &SelectState) -> Result<(), String> {
    caps.has(&state.option).map_err(|what| format!("it {what}"))
}

pub(crate) fn primary(state: &SelectState) -> Typed {
    Typed::Text(state.option.clone())
}

pub(crate) fn with_primary(value: &Typed) -> Option<SelectState> {
    match value {
        Typed::Text(option) => Some(SelectState {
            option: option.clone(),
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
        ServiceName::SelectSelectOption => {
            Ok(Service::SelectSelectOption(super::parse(name, data)?))
        }
        _ => Err(super::not_mine(name)),
    }
}

pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::SelectSelectOption(data) => Some(Typed::Text(data.option.clone())),
        _ => None,
    }
}

pub(crate) fn supports_service(caps: &SelectCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::SelectSelectOption(data) => supports(caps, data),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_select_takes_only_its_options() {
        let caps = SelectCapabilities {
            options: vec!["low".into(), "medium".into(), "high".into()],
        };
        assert!(caps.validate().is_ok());
        assert!(
            supports(
                &caps,
                &SelectOption {
                    option: "high".into()
                }
            )
            .is_ok()
        );
        assert_eq!(
            supports(
                &caps,
                &SelectOption {
                    option: "max".into()
                }
            ),
            Err(r#"is one of low, medium, high, not "max""#.to_owned())
        );
        assert!(
            SelectCapabilities {
                options: Vec::new()
            }
            .validate()
            .is_err()
        );
    }
}
