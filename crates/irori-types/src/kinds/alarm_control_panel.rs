//! `alarm_control_panel`: a burglar alarm's panel, armed and disarmed.
//!
//! Disarming is the action that lets someone in, so pages ask before sending it
//! (`docs/specs/entities.md` §4.4). A panel may need a code, which travels with the call and is
//! never kept: the same [`LockCode`] a lock takes.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::lock::LockCode;
use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AlarmCapabilities {
    /// The ways it can be armed. A panel with none can only be disarmed and watched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("uniqueItems" = true))]
    pub arm_modes: Vec<ArmMode>,
    /// Can be set off on purpose.
    #[serde(default)]
    pub trigger: bool,
    /// What its code looks like, when it takes one: digits, or any text. Pages show a keypad
    /// for the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_format: Option<CodeFormat>,
    /// Needs its code to arm. Only means something with a `code_format`.
    #[serde(default)]
    pub code_arm_required: bool,
    /// Needs its code to disarm. Only means something with a `code_format`.
    #[serde(default)]
    pub code_disarm_required: bool,
}

/// A way a panel is armed. Home Assistant's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArmMode {
    /// People are in: the doors and windows, not the rooms.
    Home,
    /// Nobody is in.
    Away,
    /// People are in and asleep.
    Night,
    /// Nobody is in for a while.
    Vacation,
    /// With some of its sensors left out.
    CustomBypass,
}

impl ArmMode {
    pub const ALL: [Self; 5] = [
        Self::Home,
        Self::Away,
        Self::Night,
        Self::Vacation,
        Self::CustomBypass,
    ];

    /// The state a panel armed this way is in.
    pub fn armed(self) -> AlarmStatus {
        match self {
            Self::Home => AlarmStatus::ArmedHome,
            Self::Away => AlarmStatus::ArmedAway,
            Self::Night => AlarmStatus::ArmedNight,
            Self::Vacation => AlarmStatus::ArmedVacation,
            Self::CustomBypass => AlarmStatus::ArmedCustomBypass,
        }
    }

    /// The service that arms a panel this way.
    pub fn service(self) -> ServiceName {
        match self {
            Self::Home => ServiceName::AlarmArmHome,
            Self::Away => ServiceName::AlarmArmAway,
            Self::Night => ServiceName::AlarmArmNight,
            Self::Vacation => ServiceName::AlarmArmVacation,
            Self::CustomBypass => ServiceName::AlarmArmCustomBypass,
        }
    }

    /// As people say it: "custom bypass".
    pub fn words(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Away => "away",
            Self::Night => "night",
            Self::Vacation => "vacation",
            Self::CustomBypass => "custom bypass",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeFormat {
    Number,
    Text,
}

/// What a panel is doing. Home Assistant's `AlarmControlPanelState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlarmStatus {
    Disarmed,
    ArmedHome,
    ArmedAway,
    ArmedNight,
    ArmedVacation,
    ArmedCustomBypass,
    /// Something tripped it, and it's counting down before it sounds.
    Pending,
    /// Told to arm, and counting down so people can leave.
    Arming,
    Disarming,
    /// Going off.
    Triggered,
}

impl AlarmStatus {
    pub const ALL: [Self; 10] = [
        Self::Disarmed,
        Self::ArmedHome,
        Self::ArmedAway,
        Self::ArmedNight,
        Self::ArmedVacation,
        Self::ArmedCustomBypass,
        Self::Pending,
        Self::Arming,
        Self::Disarming,
        Self::Triggered,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disarmed => "disarmed",
            Self::ArmedHome => "armed_home",
            Self::ArmedAway => "armed_away",
            Self::ArmedNight => "armed_night",
            Self::ArmedVacation => "armed_vacation",
            Self::ArmedCustomBypass => "armed_custom_bypass",
            Self::Pending => "pending",
            Self::Arming => "arming",
            Self::Disarming => "disarming",
            Self::Triggered => "triggered",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }

    /// Armed, in whichever way.
    pub fn is_armed(self) -> bool {
        ArmMode::ALL.iter().any(|mode| mode.armed() == self)
    }
}

/// Every text a panel's primary value can be, for rules to check against.
pub(crate) static ALARM_STATES: LazyLock<Vec<String>> = LazyLock::new(|| {
    AlarmStatus::ALL
        .iter()
        .map(|state| state.as_str().to_owned())
        .collect()
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AlarmState {
    pub state: AlarmStatus,
}

/// What a service asks of a panel: to arm it one way, disarm it, or set it off.
enum Asked {
    Arm(ArmMode),
    Disarm,
    Trigger,
}

fn asked(service: &Service) -> Option<(Asked, &LockCode)> {
    Some(match service {
        Service::AlarmArmHome(code) => (Asked::Arm(ArmMode::Home), code),
        Service::AlarmArmAway(code) => (Asked::Arm(ArmMode::Away), code),
        Service::AlarmArmNight(code) => (Asked::Arm(ArmMode::Night), code),
        Service::AlarmArmVacation(code) => (Asked::Arm(ArmMode::Vacation), code),
        Service::AlarmArmCustomBypass(code) => (Asked::Arm(ArmMode::CustomBypass), code),
        Service::AlarmDisarm(code) => (Asked::Disarm, code),
        Service::AlarmTrigger(code) => (Asked::Trigger, code),
        _ => return None,
    })
}

pub(crate) fn primary(state: &AlarmState) -> Typed {
    Typed::Text(state.state.as_str().to_owned())
}

pub(crate) fn with_primary(value: &Typed) -> Option<AlarmState> {
    match value {
        Typed::Text(text) => Some(AlarmState {
            state: AlarmStatus::parse(text)?,
        }),
        _ => None,
    }
}

/// Each takes an optional code.
pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::Optional
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    let code = super::parse(name, data)?;
    Ok(match name {
        ServiceName::AlarmArmHome => Service::AlarmArmHome(code),
        ServiceName::AlarmArmAway => Service::AlarmArmAway(code),
        ServiceName::AlarmArmNight => Service::AlarmArmNight(code),
        ServiceName::AlarmArmVacation => Service::AlarmArmVacation(code),
        ServiceName::AlarmArmCustomBypass => Service::AlarmArmCustomBypass(code),
        ServiceName::AlarmDisarm => Service::AlarmDisarm(code),
        ServiceName::AlarmTrigger => Service::AlarmTrigger(code),
        _ => return Err(super::not_mine(name)),
    })
}

/// What it will be once it has done as asked. A panel that counts down first says `arming` on
/// the way; this is where it ends up.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    let status = match asked(service)?.0 {
        Asked::Arm(mode) => mode.armed(),
        Asked::Disarm => AlarmStatus::Disarmed,
        Asked::Trigger => AlarmStatus::Triggered,
    };
    Some(Typed::Text(status.as_str().to_owned()))
}

/// Whether a panel can be asked this, with the code it was given. `Err` follows the entity's
/// name.
pub(crate) fn supports_service(caps: &AlarmCapabilities, service: &Service) -> Result<(), String> {
    let Some((asked, code)) = asked(service) else {
        return Ok(());
    };
    let needs_code = caps.code_format.is_some()
        && match asked {
            Asked::Arm(_) => caps.code_arm_required,
            Asked::Disarm => caps.code_disarm_required,
            Asked::Trigger => false,
        };
    match asked {
        Asked::Arm(mode) if !caps.arm_modes.contains(&mode) => {
            let can: Vec<&str> = caps.arm_modes.iter().map(|mode| mode.words()).collect();
            return Err(if can.is_empty() {
                "can't be armed from here".to_owned()
            } else {
                format!(
                    "can't be armed {}; it arms {}",
                    mode.words(),
                    can.join(", ")
                )
            });
        }
        Asked::Trigger if !caps.trigger => return Err("can't be set off from here".into()),
        _ => {}
    }
    let given = code.code.as_deref().filter(|code| !code.is_empty());
    match (given, caps.code_format) {
        (None, _) if needs_code => Err("needs its code".into()),
        (Some(code), Some(CodeFormat::Number)) if !code.chars().all(|c| c.is_ascii_digit()) => {
            Err("takes a code of digits".into())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> AlarmCapabilities {
        AlarmCapabilities {
            arm_modes: vec![ArmMode::Home, ArmMode::Away],
            trigger: false,
            code_format: Some(CodeFormat::Number),
            code_arm_required: false,
            code_disarm_required: true,
        }
    }

    fn code(text: &str) -> LockCode {
        LockCode {
            code: Some(text.to_owned()),
        }
    }

    #[test]
    fn a_panel_is_asked_only_what_it_can_do_and_with_the_code_it_needs() {
        let caps = panel();
        let ask = |service: Service| supports_service(&caps, &service);
        assert_eq!(ask(Service::AlarmArmAway(LockCode::default())), Ok(()));
        assert_eq!(
            ask(Service::AlarmArmNight(LockCode::default())),
            Err("can't be armed night; it arms home, away".to_owned())
        );
        assert_eq!(
            ask(Service::AlarmDisarm(LockCode::default())),
            Err("needs its code".to_owned())
        );
        assert_eq!(
            ask(Service::AlarmDisarm(code("open sesame"))),
            Err("takes a code of digits".to_owned())
        );
        assert_eq!(ask(Service::AlarmDisarm(code("1234"))), Ok(()));
        assert_eq!(
            ask(Service::AlarmTrigger(LockCode::default())),
            Err("can't be set off from here".to_owned())
        );
        // One that takes no code never asks for one, whatever else it says.
        let open = AlarmCapabilities {
            code_format: None,
            ..panel()
        };
        assert_eq!(
            supports_service(&open, &Service::AlarmDisarm(LockCode::default())),
            Ok(())
        );
    }

    #[test]
    fn what_a_panel_was_asked_is_what_it_will_be() {
        assert_eq!(
            asks_for(&Service::AlarmArmVacation(LockCode::default())),
            Some(Typed::Text("armed_vacation".into()))
        );
        assert_eq!(
            asks_for(&Service::AlarmDisarm(code("1234"))),
            Some(Typed::Text("disarmed".into()))
        );
        assert!(AlarmStatus::ArmedCustomBypass.is_armed());
        assert!(!AlarmStatus::Arming.is_armed());
        assert_eq!(
            AlarmStatus::parse("triggered"),
            Some(AlarmStatus::Triggered)
        );
        assert_eq!(ArmMode::parse("custom_bypass"), Some(ArmMode::CustomBypass));
        assert_eq!(ALARM_STATES.len(), 10);
    }
}
