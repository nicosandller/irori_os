//! `notify`: somewhere a message can be sent: a phone's notifications, a TV's screen, a chat.
//! Home Assistant's `notify` entities.
//!
//! It has no value, so its `EntityState.state` is always `null`; one thing can be asked of it,
//! `notify.send_message`. A person being told something by an automation is milestone 12's
//! notifications (#145); this is the place a device offers for it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

/// The longest message, and the longest title.
pub const MESSAGE_MOST: usize = 4096;
pub const TITLE_MOST: usize = 255;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotifyCapabilities {
    /// A message can have a title.
    #[serde(default)]
    pub title: bool,
}

/// What `notify.send_message` sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotifySendMessage {
    /// What it says.
    #[schemars(length(min = 1, max = 4096))]
    pub message: String,
    /// A title above it, where it can have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub title: Option<String>,
}

impl NotifySendMessage {
    /// Deserialization of a call runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.message.trim().is_empty() {
            return Err(InvariantError("a message has to say something".into()));
        }
        if self.message.chars().count() > MESSAGE_MOST {
            return Err(InvariantError(format!(
                "a message is at most {MESSAGE_MOST} characters"
            )));
        }
        match &self.title {
            Some(title) if title.trim().is_empty() => Err(InvariantError(
                "a title, when there is one, says something".into(),
            )),
            Some(title) if title.chars().count() > TITLE_MOST => Err(InvariantError(format!(
                "a title is at most {TITLE_MOST} characters"
            ))),
            _ => Ok(()),
        }
    }
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::NotifySendMessage => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    match name {
        ServiceName::NotifySendMessage => Ok(Service::NotifySendMessage(super::parse(name, data)?)),
        _ => Err(super::not_mine(name)),
    }
}

/// Sending leaves nothing of its own to remember.
pub(crate) fn asks_for(_: &Service) -> Option<Typed> {
    None
}

pub(crate) fn supports_service(caps: &NotifyCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::NotifySendMessage(message) if message.title.is_some() && !caps.title => {
            Err("can't show a title".into())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_says_something_and_a_title_only_where_it_shows() {
        let message = |text: &str, title: Option<&str>| NotifySendMessage {
            message: text.into(),
            title: title.map(Into::into),
        };
        assert!(message("Dishwasher done", None).validate().is_ok());
        assert!(message("  ", None).validate().is_err());
        assert!(
            message(&"a".repeat(MESSAGE_MOST + 1), None)
                .validate()
                .is_err()
        );
        assert!(message("hi", Some("")).validate().is_err());

        let titled = Service::NotifySendMessage(message("Leak", Some("Kitchen")));
        assert!(supports_service(&NotifyCapabilities { title: true }, &titled).is_ok());
        assert_eq!(
            supports_service(&NotifyCapabilities { title: false }, &titled),
            Err("can't show a title".into())
        );
    }
}
