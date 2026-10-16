//! `lawn_mower`: a robot lawn mower. Home Assistant's model: what it's doing, and the three
//! things it can be told.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LawnMowerCapabilities {
    /// Can be told to start mowing.
    #[serde(default)]
    pub start_mowing: bool,
    /// Can be paused where it is.
    #[serde(default)]
    pub pause: bool,
    /// Can be sent back to its dock.
    #[serde(default)]
    pub dock: bool,
}

/// What a mower is doing. Home Assistant's `LawnMowerActivity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MowerActivity {
    Mowing,
    Docked,
    Paused,
    /// On its way back to the dock.
    Returning,
    /// Stuck, or something is wrong with it.
    Error,
}

impl MowerActivity {
    pub const ALL: [Self; 5] = [
        Self::Mowing,
        Self::Docked,
        Self::Paused,
        Self::Returning,
        Self::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mowing => "mowing",
            Self::Docked => "docked",
            Self::Paused => "paused",
            Self::Returning => "returning",
            Self::Error => "error",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// Every text a mower's primary value can be, for rules to check against.
pub(crate) static ACTIVITIES: LazyLock<Vec<String>> = LazyLock::new(|| {
    MowerActivity::ALL
        .iter()
        .map(|activity| activity.as_str().to_owned())
        .collect()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LawnMowerState {
    pub activity: MowerActivity,
}

pub(crate) fn primary(state: &LawnMowerState) -> Typed {
    Typed::Text(state.activity.as_str().to_owned())
}

pub(crate) fn with_primary(value: &Typed) -> Option<LawnMowerState> {
    match value {
        Typed::Text(text) => Some(LawnMowerState {
            activity: MowerActivity::parse(text)?,
        }),
        _ => None,
    }
}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::None
}

pub(crate) fn service(
    name: ServiceName,
    _: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::LawnMowerStartMowing => Service::LawnMowerStartMowing,
        ServiceName::LawnMowerPause => Service::LawnMowerPause,
        ServiceName::LawnMowerDock => Service::LawnMowerDock,
        _ => return Err(super::not_mine(name)),
    })
}

/// What it will be doing once it has done as asked. Told to dock, it's on its way.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    let activity = match service {
        Service::LawnMowerStartMowing => MowerActivity::Mowing,
        Service::LawnMowerPause => MowerActivity::Paused,
        Service::LawnMowerDock => MowerActivity::Returning,
        _ => return None,
    };
    Some(Typed::Text(activity.as_str().to_owned()))
}

/// Whether a mower can be asked this. `Err` follows the entity's name.
pub(crate) fn supports_service(
    caps: &LawnMowerCapabilities,
    service: &Service,
) -> Result<(), String> {
    let (can, what) = match service {
        Service::LawnMowerStartMowing => (caps.start_mowing, "be started from here"),
        Service::LawnMowerPause => (caps.pause, "be paused"),
        Service::LawnMowerDock => (caps.dock, "be sent back to its dock"),
        _ => return Ok(()),
    };
    if can {
        Ok(())
    } else {
        Err(format!("can't {what}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mower_is_asked_only_what_it_can_do() {
        let caps = LawnMowerCapabilities {
            start_mowing: true,
            pause: false,
            dock: true,
        };
        assert_eq!(
            supports_service(&caps, &Service::LawnMowerStartMowing),
            Ok(())
        );
        assert_eq!(
            supports_service(&caps, &Service::LawnMowerPause),
            Err("can't be paused".to_owned())
        );
        assert_eq!(
            asks_for(&Service::LawnMowerDock),
            Some(Typed::Text("returning".into()))
        );
        let mowing = LawnMowerState {
            activity: MowerActivity::Mowing,
        };
        assert_eq!(primary(&mowing), Typed::Text("mowing".into()));
        assert_eq!(with_primary(&Typed::Text("mowing".into())), Some(mowing));
        assert_eq!(with_primary(&Typed::Text("cleaning".into())), None);
    }
}
