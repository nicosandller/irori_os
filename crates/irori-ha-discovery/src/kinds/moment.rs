//! Home Assistant's MQTT date, time and datetime: a day, a time of day or a moment, published
//! the ISO 8601 way and read back through `value_template`.

use irori_types::{
    Capabilities, DateState, DatetimeState, EntityKind, MomentCapabilities, Service, State,
    TimeState,
};

use crate::discovery::{EntityTopics, plain_command, str_field};
use crate::state::{Message, Publish, decode_text, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MomentTopics {
    /// Which of the three it is: `Date`, `Time` or `Datetime`.
    pub kind: EntityKind,
    pub state_topic: Option<String>,
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub value_template: ValueTemplate,
}

pub(crate) fn parse(
    kind: EntityKind,
    root: &serde_json::Value,
) -> Result<(Capabilities, EntityTopics), String> {
    let (what, capabilities) = match kind {
        EntityKind::Date => ("a date", Capabilities::Date(MomentCapabilities {})),
        EntityKind::Time => ("a time", Capabilities::Time(MomentCapabilities {})),
        _ => ("a datetime", Capabilities::Datetime(MomentCapabilities {})),
    };
    let (command_topic, command_template) = plain_command(root, what)?;
    Ok((
        capabilities,
        EntityTopics::Moment(MomentTopics {
            kind,
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            command_template,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        }),
    ))
}

pub(crate) fn decode(topics: &MomentTopics, message: Message) -> Option<Result<State, String>> {
    message.on(topics.state_topic.as_deref()).then(|| {
        let said = decode_text(message.payload, &topics.value_template)?;
        let said = said.trim();
        match topics.kind {
            EntityKind::Date => said
                .parse()
                .map(|value| State::Date(DateState { value }))
                .map_err(|e| e.to_string()),
            EntityKind::Time => said
                .parse()
                .map(|value| State::Time(TimeState { value }))
                .map_err(|e| e.to_string()),
            _ => said
                .parse()
                .map(|value| State::Datetime(DatetimeState { value }))
                .map_err(|e: irori_types::TimestampError| e.to_string()),
        }
    })
}

pub(crate) fn encode(topics: &MomentTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let value = match (topics.kind, service) {
        (EntityKind::Date, Service::DateSetValue(data)) => data.date.to_string(),
        (EntityKind::Time, Service::TimeSetValue(data)) => data.time.to_string(),
        (EntityKind::Datetime, Service::DatetimeSetValue(data)) => {
            serde_json::to_value(data.datetime)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or("that moment can't be written")?
        }
        (_, service) => return Err(super::no_service("a date or a time", service)),
    };
    Ok(vec![text_publish(
        &topics.command_topic,
        &topics.command_template.render(&value),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;
    use irori_types::{DateSetValue, DatetimeSetValue, TimeSetValue};

    fn config(what: &str) -> Vec<u8> {
        format!(
            r#"{{"unique_id": "x_{what}", "name": "X", "state_topic": "clock/{what}",
                "command_topic": "clock/{what}/set"}}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_time_of_day_is_read_and_sent_the_iso_way() {
        let parsed = parse(Component::Time, &config("time")).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Time(MomentCapabilities {})
        );
        let wake: irori_types::ClockTime = "06:45:00".parse().expect("a time");
        assert_eq!(
            crate::state::decode(&parsed.topics, "clock/time", b"06:45:00", None),
            Some(Ok(State::Time(TimeState { value: wake })))
        );
        assert!(
            crate::state::decode(&parsed.topics, "clock/time", b"quarter to seven", None)
                .expect("its topic")
                .is_err()
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &Service::TimeSetValue(TimeSetValue { time: wake }),
        )
        .expect("sets");
        assert_eq!(
            (sent[0].topic.as_str(), sent[0].payload.as_slice()),
            ("clock/time/set", &b"06:45:00"[..])
        );
    }

    #[test]
    fn a_day_and_a_moment_are_too() {
        let parsed = parse(Component::Date, &config("date")).expect("valid");
        let day: irori_types::CivilDate = "2026-10-14".parse().expect("a date");
        assert_eq!(
            crate::state::decode(&parsed.topics, "clock/date", b"2026-10-14", None),
            Some(Ok(State::Date(DateState { value: day })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &Service::DateSetValue(DateSetValue { date: day }),
        )
        .expect("sets");
        assert_eq!(sent[0].payload, b"2026-10-14");
        // A date can't be asked for a time.
        let wake: irori_types::ClockTime = "06:45:00".parse().expect("a time");
        assert!(
            crate::state::encode(
                &parsed.topics,
                &Service::TimeSetValue(TimeSetValue { time: wake })
            )
            .is_err()
        );

        let parsed = parse(Component::Datetime, &config("datetime")).expect("valid");
        let decoded = crate::state::decode(
            &parsed.topics,
            "clock/datetime",
            b"2026-10-14T07:45:00+01:00",
            None,
        );
        let Some(Ok(State::Datetime(state))) = decoded else {
            panic!("a moment, not {decoded:?}");
        };
        let sent = crate::state::encode(
            &parsed.topics,
            &Service::DatetimeSetValue(DatetimeSetValue {
                datetime: state.value,
            }),
        )
        .expect("sets");
        assert_eq!(sent[0].payload, b"2026-10-14T06:45:00Z");
    }
}
