//! ESPHome's `update`: firmware the device can fetch and flash itself (its `update:` component,
//! usually pointed at a manifest over HTTP).

use esphome_client::types::{
    ListEntitiesUpdateResponse, UpdateCommandRequest, UpdateStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, Service, State, UniqueId,
    UpdateCapabilities, UpdateClass, UpdateState,
};

use super::{category, decimal, entity_id, optional};

/// ESPHome's `update`. Every one can be told to install and to look again; whether it reports
/// progress only shows in what it reports.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesUpdateResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Update, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Update(UpdateCapabilities {
            device_class: UpdateClass::parse(&entity.device_class),
            install: true,
            check: true,
        }),
        entity_category: category(entity.entity_category),
    })
}

/// What the device knows of its versions. `None` until it has looked (`missing_state`).
pub fn state(state: &UpdateStateResponse) -> Option<State> {
    if state.missing_state {
        return None;
    }
    let installed_version = optional(&state.current_version);
    let latest_version = optional(&state.latest_version);
    Some(State::Update(UpdateState {
        available: UpdateState::differs(installed_version.as_deref(), latest_version.as_deref()),
        installed_version,
        latest_version,
        in_progress: state.in_progress,
        progress: (state.in_progress && state.has_progress)
            .then(|| decimal(state.progress).clamp(0.0, 100.0)),
        title: optional(&state.title),
        release_summary: optional(&state.release_summary),
        release_url: optional(&state.release_url),
        skipped_version: None,
        last_error: None,
    }))
}

/// An update command (`UpdateCommand`: 1 update, 2 check).
pub fn command(key: u32, service: &Service) -> UpdateCommandRequest {
    let command = match service {
        Service::UpdateInstall => 1,
        Service::UpdateCheck => 2,
        _ => 0,
    };
    UpdateCommandRequest {
        key,
        command,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_says_what_is_there_and_how_far_it_has_got() {
        let waiting = UpdateStateResponse {
            key: 9,
            current_version: "2026.8.0".into(),
            latest_version: "2026.9.1".into(),
            title: "Hall plug".into(),
            ..Default::default()
        };
        let Some(State::Update(there)) = state(&waiting) else {
            panic!("it reported");
        };
        assert!(there.available);
        assert_eq!(there.title.as_deref(), Some("Hall plug"));
        assert_eq!(there.progress, None);

        let flashing = UpdateStateResponse {
            in_progress: true,
            has_progress: true,
            progress: 42.5,
            ..waiting.clone()
        };
        let Some(State::Update(busy)) = state(&flashing) else {
            panic!("it reported");
        };
        assert_eq!((busy.in_progress, busy.progress), (true, Some(42.5)));

        let current = UpdateStateResponse {
            latest_version: "2026.8.0".into(),
            ..waiting.clone()
        };
        assert!(matches!(state(&current), Some(State::Update(s)) if !s.available));
        let unknown = UpdateStateResponse {
            missing_state: true,
            ..waiting
        };
        assert_eq!(state(&unknown), None);
        assert_eq!(command(9, &Service::UpdateInstall).command, 1);
        assert_eq!(command(9, &Service::UpdateCheck).command, 2);
    }
}
