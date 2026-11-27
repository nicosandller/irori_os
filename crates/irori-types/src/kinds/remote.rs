//! `remote`: a remote control something can be driven by: a TV's, a universal remote's, an
//! infrared blaster. Home Assistant's `remote`.
//!
//! Its value is whether what it drives is on. A remote that knows activities ("Watch TV",
//! "Listen to music") says which one is going, as a field, and can be told to start one. Any
//! remote can be told to send commands, by the names its protocol knows them by.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Field, Typed};
use crate::{InvariantError, Service, ServiceName};

/// The most commands one `send_command` sends, and the most times it repeats them.
pub const COMMANDS_MOST: usize = 32;
pub const REPEATS_MOST: u16 = 255;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteCapabilities {
    /// The activities it can start, by name. Empty for one that has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteState {
    /// What it drives is on.
    pub on: bool,
    /// The activity going now, for one that has activities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
}

/// What `remote.turn_on` turns on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteTurnOn {
    /// One of its `activities`, to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
}

/// What `remote.send_command` sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteSendCommand {
    /// The commands, in order, by the names its protocol knows them by (`power`, `volume_up`).
    #[schemars(length(min = 1, max = 32))]
    pub command: Vec<String>,
    /// Which of the devices it drives they're for, where it drives several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// How many times to send them, once when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 255))]
    pub repeats: Option<u16>,
}

impl RemoteSendCommand {
    /// Deserialization of a call runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.command.is_empty() || self.command.len() > COMMANDS_MOST {
            return Err(InvariantError(format!(
                "a remote sends 1 to {COMMANDS_MOST} commands at a time"
            )));
        }
        if self.command.iter().any(|command| command.trim().is_empty()) {
            return Err(InvariantError("a command has a name".into()));
        }
        match self.repeats {
            Some(repeats) if !(1..=REPEATS_MOST).contains(&repeats) => Err(InvariantError(
                format!("repeats is 1 to {REPEATS_MOST}, not {repeats}"),
            )),
            _ => Ok(()),
        }
    }
}

pub(crate) fn primary(state: &RemoteState) -> Typed {
    Typed::Bool(state.on)
}

pub(crate) fn fields(caps: &RemoteCapabilities) -> Vec<Field> {
    if caps.activities.is_empty() {
        Vec::new()
    } else {
        vec![Field::text("activity")]
    }
}

pub(crate) fn field(state: &RemoteState, name: &str) -> Option<Typed> {
    match name {
        "activity" => state.activity.clone().map(Typed::Text),
        _ => None,
    }
}

/// Keeps the activity it said last, while it stays on.
pub(crate) fn with_primary(previous: Option<&RemoteState>, value: &Typed) -> Option<RemoteState> {
    let Typed::Bool(on) = value else {
        return None;
    };
    Some(RemoteState {
        on: *on,
        activity: previous
            .filter(|_| *on)
            .and_then(|previous| previous.activity.clone()),
    })
}

pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    super::on_off_toggle(
        current,
        ServiceName::RemoteTurnOn,
        ServiceName::RemoteTurnOff,
    )
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::RemoteTurnOn => Data::Optional,
        ServiceName::RemoteSendCommand => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::RemoteTurnOn => Service::RemoteTurnOn(super::parse(name, data)?),
        ServiceName::RemoteTurnOff => Service::RemoteTurnOff,
        ServiceName::RemoteSendCommand => Service::RemoteSendCommand(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::RemoteTurnOn(_) => Some(Typed::Bool(true)),
        Service::RemoteTurnOff => Some(Typed::Bool(false)),
        _ => None,
    }
}

pub(crate) fn supports_service(caps: &RemoteCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::RemoteTurnOn(RemoteTurnOn {
            activity: Some(activity),
        }) if !caps.activities.contains(activity) => {
            Err(format!("has no activity called {activity:?}"))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_starts_only_its_own_activities_and_sends_named_commands() {
        let hub = RemoteCapabilities {
            activities: vec!["Watch TV".into()],
        };
        let start = |activity: &str| {
            Service::RemoteTurnOn(RemoteTurnOn {
                activity: Some(activity.into()),
            })
        };
        assert!(supports_service(&hub, &start("Watch TV")).is_ok());
        assert!(supports_service(&hub, &start("Play games")).is_err());
        assert_eq!(fields(&hub), [Field::text("activity")]);
        assert!(fields(&RemoteCapabilities::default()).is_empty());

        let send = |command: Vec<&str>, repeats| RemoteSendCommand {
            command: command.into_iter().map(Into::into).collect(),
            device: None,
            repeats,
        };
        assert!(send(vec!["power"], None).validate().is_ok());
        assert!(send(vec![], None).validate().is_err());
        assert!(send(vec![" "], None).validate().is_err());
        assert!(send(vec!["up"], Some(0)).validate().is_err());
    }

    #[test]
    fn turning_it_off_forgets_the_activity() {
        let watching = RemoteState {
            on: true,
            activity: Some("Watch TV".into()),
        };
        assert_eq!(
            with_primary(Some(&watching), &Typed::Bool(false)),
            Some(RemoteState {
                on: false,
                activity: None
            })
        );
        assert_eq!(
            with_primary(Some(&watching), &Typed::Bool(true)),
            Some(watching)
        );
    }
}
