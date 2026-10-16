//! ESPHome's `alarm_control_panel`.

use esphome_client::types::{
    AlarmControlPanelCommandRequest, AlarmControlPanelStateResponse,
    ListEntitiesAlarmControlPanelResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    AlarmCapabilities, AlarmState, AlarmStatus, ArmMode, Capabilities, CodeFormat,
    EntityDescription, EntityKind, LockCode, Name, Service, State, UniqueId,
};

use super::{category, entity_id};

/// ESPHome's `supported_features` are Home Assistant's `AlarmControlPanelEntityFeature` bits.
const ARM_HOME: u32 = 1;
const ARM_AWAY: u32 = 2;
const ARM_NIGHT: u32 = 4;
const TRIGGER: u32 = 8;
const ARM_CUSTOM_BYPASS: u32 = 16;
const ARM_VACATION: u32 = 32;

/// ESPHome's `alarm_control_panel`.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesAlarmControlPanelResponse,
) -> Result<EntityDescription, ProtocolError> {
    let has = |bit: u32| entity.supported_features & bit != 0;
    let arm_modes = [
        (ARM_HOME, ArmMode::Home),
        (ARM_AWAY, ArmMode::Away),
        (ARM_NIGHT, ArmMode::Night),
        (ARM_VACATION, ArmMode::Vacation),
        (ARM_CUSTOM_BYPASS, ArmMode::CustomBypass),
    ]
    .into_iter()
    .filter_map(|(bit, mode)| has(bit).then_some(mode))
    .collect();
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::AlarmControlPanel, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::AlarmControlPanel(AlarmCapabilities {
            arm_modes,
            trigger: has(TRIGGER),
            // An ESPHome panel's codes are digits, as Home Assistant reads them.
            code_format: entity.requires_code.then_some(CodeFormat::Number),
            code_arm_required: entity.requires_code_to_arm,
            code_disarm_required: entity.requires_code,
        }),
        entity_category: category(entity.entity_category),
    })
}

/// ESPHome's `AlarmControlPanelState` (api.proto).
pub fn state(state: &AlarmControlPanelStateResponse) -> Option<State> {
    let state = match state.state {
        0 => AlarmStatus::Disarmed,
        1 => AlarmStatus::ArmedHome,
        2 => AlarmStatus::ArmedAway,
        3 => AlarmStatus::ArmedNight,
        4 => AlarmStatus::ArmedVacation,
        5 => AlarmStatus::ArmedCustomBypass,
        6 => AlarmStatus::Pending,
        7 => AlarmStatus::Arming,
        8 => AlarmStatus::Disarming,
        9 => AlarmStatus::Triggered,
        _ => return None,
    };
    Some(State::AlarmControlPanel(AlarmState { state }))
}

/// A panel command (`AlarmControlPanelStateCommand`), with its code when one was given. `None`
/// for a service that isn't a panel's.
pub fn command(key: u32, service: &Service) -> Option<AlarmControlPanelCommandRequest> {
    let (command, code): (i32, &LockCode) = match service {
        Service::AlarmDisarm(code) => (0, code),
        Service::AlarmArmAway(code) => (1, code),
        Service::AlarmArmHome(code) => (2, code),
        Service::AlarmArmNight(code) => (3, code),
        Service::AlarmArmVacation(code) => (4, code),
        Service::AlarmArmCustomBypass(code) => (5, code),
        Service::AlarmTrigger(code) => (6, code),
        _ => return None,
    };
    Some(AlarmControlPanelCommandRequest {
        key,
        command,
        code: code.code.clone().unwrap_or_default(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_says_how_it_arms_what_it_is_doing_and_takes_its_code() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesAlarmControlPanelResponse {
            key: 6,
            name: "House alarm".into(),
            supported_features: ARM_HOME | ARM_AWAY | TRIGGER,
            requires_code: true,
            requires_code_to_arm: false,
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        assert_eq!(
            described.unique_id.as_str(),
            "00:11:22:33:44:55-alarm_control_panel-6"
        );
        assert_eq!(
            described.capabilities,
            Capabilities::AlarmControlPanel(AlarmCapabilities {
                arm_modes: vec![ArmMode::Home, ArmMode::Away],
                trigger: true,
                code_format: Some(CodeFormat::Number),
                code_arm_required: false,
                code_disarm_required: true,
            })
        );

        let going_off = AlarmControlPanelStateResponse {
            key: 6,
            state: 9,
            ..Default::default()
        };
        assert_eq!(
            state(&going_off),
            Some(State::AlarmControlPanel(AlarmState {
                state: AlarmStatus::Triggered
            }))
        );
        let disarm = command(
            6,
            &Service::AlarmDisarm(LockCode {
                code: Some("1234".into()),
            }),
        )
        .expect("a panel's");
        assert_eq!((disarm.command, disarm.code.as_str()), (0, "1234"));
        let away = command(6, &Service::AlarmArmAway(LockCode::default())).expect("a panel's");
        assert_eq!((away.command, away.code.as_str()), (1, ""));
        assert!(command(6, &Service::SwitchTurnOn).is_none());
    }
}
