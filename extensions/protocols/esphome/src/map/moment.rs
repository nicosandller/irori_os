//! ESPHome's `date`, `time` and `datetime`: a day, a time of day, or a moment set from outside.

use esphome_client::types::{
    DateCommandRequest, DateStateResponse, DateTimeCommandRequest, DateTimeStateResponse,
    ListEntitiesDateResponse, ListEntitiesDateTimeResponse, ListEntitiesTimeResponse,
    TimeCommandRequest, TimeStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, CivilDate, ClockTime, DateState, DatetimeState, EntityCategory,
    EntityDescription, EntityKind, MomentCapabilities, Name, Service, State, TimeState, Timestamp,
    UniqueId,
};

use super::{category, entity_id};

fn described(
    device: &UniqueId,
    kind: EntityKind,
    (key, name, entity_category): (u32, &str, Option<EntityCategory>),
    capabilities: Capabilities,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, kind, key)?,
        name: Some(Name::try_from(name)?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities,
        entity_category,
    })
}

pub fn describe_date(
    device: &UniqueId,
    entity: &ListEntitiesDateResponse,
) -> Result<EntityDescription, ProtocolError> {
    described(
        device,
        EntityKind::Date,
        (entity.key, &entity.name, category(entity.entity_category)),
        Capabilities::Date(MomentCapabilities {}),
    )
}

pub fn describe_time(
    device: &UniqueId,
    entity: &ListEntitiesTimeResponse,
) -> Result<EntityDescription, ProtocolError> {
    described(
        device,
        EntityKind::Time,
        (entity.key, &entity.name, category(entity.entity_category)),
        Capabilities::Time(MomentCapabilities {}),
    )
}

pub fn describe_datetime(
    device: &UniqueId,
    entity: &ListEntitiesDateTimeResponse,
) -> Result<EntityDescription, ProtocolError> {
    described(
        device,
        EntityKind::Datetime,
        (entity.key, &entity.name, category(entity.entity_category)),
        Capabilities::Datetime(MomentCapabilities {}),
    )
}

/// `None` when the device has no date right now, or says a day that doesn't exist.
pub fn date_state(state: &DateStateResponse) -> Option<State> {
    if state.missing_state {
        return None;
    }
    let value = CivilDate::new(
        i16::try_from(state.year).ok()?,
        i8::try_from(state.month).ok()?,
        i8::try_from(state.day).ok()?,
    )?;
    Some(State::Date(DateState { value }))
}

/// `None` when the device has no time right now, or says one that doesn't exist.
pub fn time_state(state: &TimeStateResponse) -> Option<State> {
    if state.missing_state {
        return None;
    }
    let value = ClockTime::new(
        i8::try_from(state.hour).ok()?,
        i8::try_from(state.minute).ok()?,
        i8::try_from(state.second).ok()?,
    )?;
    Some(State::Time(TimeState { value }))
}

/// `None` when the device has no moment right now. ESPHome counts seconds since 1970.
pub fn datetime_state(state: &DateTimeStateResponse) -> Option<State> {
    if state.missing_state {
        return None;
    }
    let value = Timestamp::from_second(i64::from(state.epoch_seconds))?;
    Some(State::Datetime(DatetimeState { value }))
}

pub fn date_command(key: u32, service: &Service) -> Option<DateCommandRequest> {
    let Service::DateSetValue(data) = service else {
        return None;
    };
    let date = data.date.as_jiff();
    Some(DateCommandRequest {
        key,
        year: u32::try_from(date.year()).ok()?,
        month: u32::try_from(date.month()).ok()?,
        day: u32::try_from(date.day()).ok()?,
        ..Default::default()
    })
}

pub fn time_command(key: u32, service: &Service) -> Option<TimeCommandRequest> {
    let Service::TimeSetValue(data) = service else {
        return None;
    };
    let time = data.time.as_jiff();
    Some(TimeCommandRequest {
        key,
        hour: u32::try_from(time.hour()).ok()?,
        minute: u32::try_from(time.minute()).ok()?,
        second: u32::try_from(time.second()).ok()?,
        ..Default::default()
    })
}

/// `None` for a moment before 1970 or after 2106, which the device's count can't hold.
pub fn datetime_command(key: u32, service: &Service) -> Option<DateTimeCommandRequest> {
    let Service::DatetimeSetValue(data) = service else {
        return None;
    };
    Some(DateTimeCommandRequest {
        key,
        epoch_seconds: u32::try_from(data.datetime.as_second()).ok()?,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{DateSetValue, DatetimeSetValue, TimeSetValue};

    use super::*;

    #[test]
    fn a_day_a_time_and_a_moment_cross_both_ways() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesTimeResponse {
            key: 3,
            name: "Wake up".into(),
            ..Default::default()
        };
        let described = describe_time(&device, &listed).expect("valid");
        assert_eq!(described.unique_id.as_str(), "00:11:22:33:44:55-time-3");

        let day = DateStateResponse {
            key: 1,
            year: 2026,
            month: 10,
            day: 14,
            ..Default::default()
        };
        let value: CivilDate = "2026-10-14".parse().expect("a date");
        assert_eq!(date_state(&day), Some(State::Date(DateState { value })));
        let sent = date_command(1, &Service::DateSetValue(DateSetValue { date: value }))
            .expect("a date's");
        assert_eq!((sent.year, sent.month, sent.day), (2026, 10, 14));
        // A day that doesn't exist, and no day at all, are both nothing known.
        assert_eq!(
            date_state(&DateStateResponse {
                day: 31,
                month: 2,
                ..day
            }),
            None
        );
        assert_eq!(
            date_state(&DateStateResponse {
                missing_state: true,
                ..day
            }),
            None
        );

        let wake = TimeStateResponse {
            key: 3,
            hour: 6,
            minute: 45,
            second: 0,
            ..Default::default()
        };
        let value: ClockTime = "06:45:00".parse().expect("a time");
        assert_eq!(time_state(&wake), Some(State::Time(TimeState { value })));
        let sent = time_command(3, &Service::TimeSetValue(TimeSetValue { time: value }))
            .expect("a time's");
        assert_eq!((sent.hour, sent.minute, sent.second), (6, 45, 0));

        let ends = DateTimeStateResponse {
            key: 5,
            epoch_seconds: 1_791_960_300,
            ..Default::default()
        };
        let Some(State::Datetime(state)) = datetime_state(&ends) else {
            panic!("a moment");
        };
        let sent = datetime_command(
            5,
            &Service::DatetimeSetValue(DatetimeSetValue {
                datetime: state.value,
            }),
        )
        .expect("a moment's");
        assert_eq!(sent.epoch_seconds, 1_791_960_300);
        assert!(time_command(3, &Service::SwitchTurnOn).is_none());
    }
}
