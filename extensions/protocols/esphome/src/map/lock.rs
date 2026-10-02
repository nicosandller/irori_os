//! ESPHome's `lock`.

use esphome_client::types::{ListEntitiesLockResponse, LockCommandRequest, LockStateResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, LockCapabilities, LockCode, LockState, LockStatus,
    Name, Service, State, UniqueId,
};

use super::{category, entity_id, optional};

/// ESPHome's `lock`.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesLockResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Lock, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Lock(LockCapabilities {
            open: entity.supports_open,
            requires_code: entity.requires_code,
            code_format: optional(&entity.code_format),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// ESPHome's `LockState` (api.proto); 0 is "none", which says nothing.
pub fn state(state: &LockStateResponse) -> Option<State> {
    let state = match state.state {
        1 => LockStatus::Locked,
        2 => LockStatus::Unlocked,
        3 => LockStatus::Jammed,
        4 => LockStatus::Locking,
        5 => LockStatus::Unlocking,
        6 => LockStatus::Opening,
        7 => LockStatus::Open,
        _ => return None,
    };
    Some(State::Lock(LockState { state }))
}

/// A lock command (`LockCommand`: 0 unlock, 1 lock, 2 open), with its code when one was given.
pub fn command(key: u32, service: &Service) -> LockCommandRequest {
    let (command, code) = match service {
        Service::LockUnlock(code) => (0, code),
        Service::LockOpen(code) => (2, code),
        Service::LockLock(code) => (1, code),
        _ => (1, &LockCode::default()),
    };
    LockCommandRequest {
        key,
        command,
        has_code: code.code.is_some(),
        code: code.code.clone().unwrap_or_default(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_reports_its_state_and_takes_its_code() {
        let jammed = LockStateResponse {
            key: 4,
            state: 3,
            ..Default::default()
        };
        assert_eq!(
            state(&jammed),
            Some(State::Lock(LockState {
                state: LockStatus::Jammed
            }))
        );
        let nothing = LockStateResponse {
            key: 4,
            state: 0,
            ..Default::default()
        };
        assert_eq!(state(&nothing), None);
        let unlock = command(
            4,
            &Service::LockUnlock(LockCode {
                code: Some("1234".into()),
            }),
        );
        assert_eq!((unlock.command, unlock.has_code), (0, true));
        assert_eq!(unlock.code, "1234");
    }
}
