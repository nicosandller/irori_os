//! `update`: newer software for something, e.g. a plug's firmware or one of Irori's own
//! extensions. Whether one is available is its value; the versions and how far an install has
//! got ride along.
//!
//! Installing rewrites what a device runs and takes it away for a while, so pages ask before
//! sending it (`docs/specs/entities.md` §4.4). Skipping a version is Irori's to remember, not
//! the device's: the core answers `update.skip` itself and never passes it on.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

/// What the update is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateClass {
    /// A device's firmware.
    Firmware,
}

impl UpdateClass {
    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<UpdateClass>,
    /// Can be told to install. Without it, it only says an update exists.
    #[serde(default)]
    pub install: bool,
    /// Can be told to look for a newer version now.
    #[serde(default)]
    pub check: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateState {
    /// A newer version than the installed one is there to install, and it isn't the skipped one.
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// An install is running.
    #[serde(default)]
    pub in_progress: bool,
    /// How far the install has got, in %, from a device that says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 100.0))]
    pub progress: Option<f64>,
    /// What the software is called, when that isn't the device's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A few words on what the latest version changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_summary: Option<String>,
    /// Where the full release notes are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_url: Option<String>,
    /// The version someone chose to pass over. The core sets it; a protocol's is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_version: Option<String>,
    /// Why the last install didn't finish: the protocol's reason when it has one, otherwise the
    /// core's. The next install clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl UpdateState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if let Some(progress) = self.progress {
            if !(0.0..=100.0).contains(&progress) {
                return Err(InvariantError(format!(
                    "progress is 0-100%, not {progress}"
                )));
            }
            if !self.in_progress {
                return Err(InvariantError(
                    "progress is only there while an install is in progress".into(),
                ));
            }
        }
        if self.available && self.skipped_version.is_some() && self.is_skipped() {
            return Err(InvariantError(
                "a skipped version isn't available; clear the skip first".into(),
            ));
        }
        Ok(())
    }

    /// Whether the latest version is the one that was skipped.
    pub fn is_skipped(&self) -> bool {
        self.skipped_version.is_some() && self.skipped_version == self.latest_version
    }

    /// Whether there is a newer version, skipped or not: what `available` would be without the
    /// skip. A device that doesn't say is taken to have one when both versions are known and
    /// differ, since versions come in too many spellings to order.
    pub fn differs(installed: Option<&str>, latest: Option<&str>) -> bool {
        matches!((installed, latest), (Some(installed), Some(latest)) if installed != latest)
    }
}

pub(crate) fn primary(state: &UpdateState) -> Typed {
    Typed::Bool(state.available)
}

/// Keeps its versions and the rest as it said them last.
pub(crate) fn with_primary(previous: Option<&UpdateState>, value: &Typed) -> Option<UpdateState> {
    let Typed::Bool(available) = value else {
        return None;
    };
    Some(UpdateState {
        available: *available,
        ..previous.cloned().unwrap_or_default()
    })
}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::None
}

pub(crate) fn service(
    name: ServiceName,
    _: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::UpdateInstall => Service::UpdateInstall,
        ServiceName::UpdateCheck => Service::UpdateCheck,
        ServiceName::UpdateSkip => Service::UpdateSkip,
        ServiceName::UpdateClearSkipped => Service::UpdateClearSkipped,
        _ => return Err(super::not_mine(name)),
    })
}

/// Nothing an install asks for is known ahead: it's available until the device says otherwise.
pub(crate) fn asks_for(_: &Service) -> Option<Typed> {
    None
}

pub(crate) fn supports_service(caps: &UpdateCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::UpdateInstall if !caps.install => {
            Err("says when there's an update, but can't be told to install it".into())
        }
        Service::UpdateCheck if !caps.check => Err("can't be told to look for updates".into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_installs_only_where_it_can() {
        let reports_only = UpdateCapabilities::default();
        assert!(supports_service(&reports_only, &Service::UpdateInstall).is_err());
        assert!(supports_service(&reports_only, &Service::UpdateCheck).is_err());
        assert!(
            supports_service(&reports_only, &Service::UpdateSkip).is_ok(),
            "skipping is Irori's, whatever the device can do"
        );
        let plug = UpdateCapabilities {
            device_class: Some(UpdateClass::Firmware),
            install: true,
            check: false,
        };
        assert!(supports_service(&plug, &Service::UpdateInstall).is_ok());
    }

    #[test]
    fn progress_and_skips_follow_their_rules() {
        let installing = UpdateState {
            available: true,
            installed_version: Some("1.4.2".into()),
            latest_version: Some("1.5.0".into()),
            in_progress: true,
            progress: Some(40.0),
            ..UpdateState::default()
        };
        assert!(installing.validate().is_ok());
        let idle_with_progress = UpdateState {
            in_progress: false,
            ..installing.clone()
        };
        assert!(idle_with_progress.validate().is_err());
        let skipped_but_available = UpdateState {
            skipped_version: Some("1.5.0".into()),
            in_progress: false,
            progress: None,
            ..installing.clone()
        };
        assert!(skipped_but_available.validate().is_err());
        assert!(
            UpdateState {
                available: false,
                ..skipped_but_available
            }
            .validate()
            .is_ok()
        );
        assert!(UpdateState::differs(Some("16777241"), Some("16777250")));
        assert!(!UpdateState::differs(Some("1.0"), None));
    }
}
