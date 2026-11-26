//! Home Assistant's history of an entity from before Irori recorded it
//! (`docs/specs/protocols.md` §5.5). Read-only: `history/history_during_period` for changes and
//! `recorder/statistics_during_period` for hourly summaries, one entity at a time, one question
//! at a time. Irori keeps none of it; it asks again when it wants it.

use irori_protocol::history::{MAX_PAST_CHANGES, MAX_PAST_HOURS};
use irori_protocol::{PastChange, PastHour};
use irori_types::{Availability, Capabilities, StateClass, Timestamp};
use serde_json::{Map, Value};

use crate::model::{self, Reading};

/// What Home Assistant's recorder keeps hourly statistics for, and what Irori's hourly chart
/// draws: the hour's average, or for a counter how much it changed.
pub fn hourly_value(caps: &Capabilities) -> Option<&'static str> {
    match caps {
        Capabilities::Sensor(sensor) => match sensor.state_class? {
            StateClass::Measurement => Some("mean"),
            StateClass::Total | StateClass::TotalIncreasing => Some("change"),
        },
        _ => None,
    }
}

/// The changes in a `history/history_during_period` answer for `entity_id`, as Irori's values
/// of the entity. Rows come compressed: `s` the state, `lu` when (seconds since the epoch), and
/// `a` the attributes, which a minimal answer only sends on the first row; later rows keep the
/// last ones seen. The first row is the state as `since` began, dated `since`. The newest
/// [`MAX_PAST_CHANGES`] are kept.
pub fn changes(
    answer: &Value,
    entity_id: &str,
    caps: &Capabilities,
    since: Timestamp,
    until: Timestamp,
) -> Vec<PastChange> {
    let rows = answer
        .get(entity_id)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut attributes = Map::new();
    let mut changes: Vec<PastChange> = Vec::new();
    for row in rows {
        if let Some(a) = row.get("a").and_then(Value::as_object) {
            attributes = a.clone();
        }
        let Some(s) = row.get("s").and_then(Value::as_str) else {
            continue;
        };
        let Some(at) = row
            .get("lu")
            .or_else(|| row.get("lc"))
            .and_then(Value::as_f64)
            .and_then(from_seconds)
        else {
            continue;
        };
        let at = at.max(since);
        if at >= until {
            continue;
        }
        let (availability, state) = match model::reading(caps, s, &attributes) {
            Reading::Unavailable => (Availability::Unavailable, None),
            Reading::Nothing => continue,
            Reading::Value(state) => (Availability::Available, state),
        };
        // An attribute changed that Irori doesn't keep: nothing to say.
        if changes
            .last()
            .is_some_and(|last| last.state == state && last.availability == availability)
        {
            continue;
        }
        changes.push(PastChange {
            at,
            availability,
            state,
        });
    }
    let extra = changes.len().saturating_sub(MAX_PAST_CHANGES);
    changes.drain(..extra);
    changes
}

/// The hours in a `recorder/statistics_during_period` answer for `entity_id`. `start` is in
/// milliseconds since the epoch; `value` is the statistic [`hourly_value`] names.
pub fn hours(
    answer: &Value,
    entity_id: &str,
    value: &str,
    since: Timestamp,
    until: Timestamp,
) -> Vec<PastHour> {
    let rows = answer
        .get(entity_id)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut hours: Vec<PastHour> = rows
        .iter()
        .filter_map(|row| {
            let start = row
                .get("start")
                .and_then(Value::as_f64)
                .and_then(|ms| from_seconds(ms / 1000.0))?;
            let number = |key: &str| {
                row.get(key)
                    .and_then(Value::as_f64)
                    .filter(|n| n.is_finite())
            };
            let measured = value == "mean";
            Some(PastHour {
                start,
                value: number(value)?,
                min: measured.then(|| number("min")).flatten(),
                max: measured.then(|| number("max")).flatten(),
            })
        })
        .filter(|hour| hour.start >= since && hour.start < until)
        .collect();
    let extra = hours.len().saturating_sub(MAX_PAST_HOURS);
    hours.drain(..extra);
    hours
}

fn from_seconds(seconds: f64) -> Option<Timestamp> {
    if !seconds.is_finite() {
        return None;
    }
    let micros = (seconds * 1_000_000.0).round();
    if micros.abs() > 9.0e15 {
        return None;
    }
    jiff::Timestamp::from_microsecond(micros as i64)
        .ok()
        .map(Timestamp::from_jiff)
}

#[cfg(test)]
mod tests {
    use irori_types::{LightCapabilities, SensorCapabilities, SensorValueType, State};
    use serde_json::json;

    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a timestamp")
    }

    fn seconds(text: &str) -> f64 {
        at(text).as_jiff().as_millisecond() as f64 / 1000.0
    }

    fn light() -> Capabilities {
        Capabilities::Light(LightCapabilities {
            brightness: true,
            color_temp_kelvin: None,
            rgb: false,
        })
    }

    #[test]
    fn a_minimal_answer_keeps_the_first_rows_attributes() {
        let answer = json!({ "light.desk": [
            // The state as the stretch began, from before it.
            { "s": "on", "a": { "brightness": 255, "color_mode": "brightness" }, "lu": seconds("2026-11-19T20:00:00Z") },
            { "s": "off", "lu": seconds("2026-11-20T01:00:00Z") },
            { "s": "unavailable", "lu": seconds("2026-11-20T02:00:00Z") },
            { "s": "on", "lu": seconds("2026-11-20T03:00:00.5Z") },
            // After Irori took over: left out.
            { "s": "off", "lu": seconds("2026-11-20T12:00:00Z") },
        ]});
        let changes = changes(
            &answer,
            "light.desk",
            &light(),
            at("2026-11-20T00:00:00Z"),
            at("2026-11-20T12:00:00Z"),
        );
        let said: Vec<(String, Availability, Option<bool>)> = changes
            .iter()
            .map(|change| {
                let on = match &change.state {
                    Some(State::Light(light)) => Some(light.on),
                    _ => None,
                };
                (change.at.to_string(), change.availability, on)
            })
            .collect();
        assert_eq!(
            said,
            [
                (
                    "2026-11-20T00:00:00Z".into(),
                    Availability::Available,
                    Some(true)
                ),
                (
                    "2026-11-20T01:00:00Z".into(),
                    Availability::Available,
                    Some(false)
                ),
                (
                    "2026-11-20T02:00:00Z".into(),
                    Availability::Unavailable,
                    None
                ),
                (
                    "2026-11-20T03:00:00.5Z".into(),
                    Availability::Available,
                    Some(true)
                ),
            ]
        );
        // The brightness came from the first row's attributes.
        assert!(matches!(
            &changes[3].state,
            Some(State::Light(light)) if light.brightness == Some(255)
        ));
    }

    #[test]
    fn an_unknown_entity_or_odd_rows_are_nothing() {
        let answer = json!({ "light.other": [{ "s": "on", "lu": 1.0 }],
            "light.desk": [{ "lu": 1.0 }, { "s": "on" }, { "s": "on", "lu": "soon" }] });
        let since = at("1970-01-01T00:00:00Z");
        let until = at("2030-01-01T00:00:00Z");
        assert!(changes(&answer, "light.desk", &light(), since, until).is_empty());
        assert!(changes(&json!(null), "light.desk", &light(), since, until).is_empty());
    }

    #[test]
    fn hours_are_the_mean_of_a_measurement_and_the_change_of_a_counter() {
        let sensor = |class| {
            Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Number,
                device_class: None,
                unit: Some("W".into()),
                state_class: Some(class),
                options: Vec::new(),
            })
        };
        assert_eq!(hourly_value(&sensor(StateClass::Measurement)), Some("mean"));
        assert_eq!(
            hourly_value(&sensor(StateClass::TotalIncreasing)),
            Some("change")
        );
        assert_eq!(hourly_value(&light()), None);

        let ms = |text: &str| at(text).as_jiff().as_millisecond();
        let answer = json!({ "sensor.power": [
            { "start": ms("2026-11-19T10:00:00Z"), "end": ms("2026-11-19T11:00:00Z"), "mean": 120.5, "min": 80.0, "max": 300.0, "change": 0.1 },
            { "start": ms("2026-11-19T11:00:00Z"), "end": ms("2026-11-19T12:00:00Z"), "mean": null },
            { "start": ms("2026-11-19T12:00:00Z"), "end": ms("2026-11-19T13:00:00Z"), "mean": 99.0 },
        ]});
        let since = at("2026-11-19T00:00:00Z");
        let until = at("2026-11-19T12:00:00Z");
        let measured = hours(&answer, "sensor.power", "mean", since, until);
        assert_eq!(measured.len(), 1, "no mean, or Irori's own hour: left out");
        assert_eq!(measured[0].value, 120.5);
        assert_eq!(measured[0].min, Some(80.0));
        let counted = hours(&answer, "sensor.power", "change", since, until);
        assert_eq!(counted[0].value, 0.1);
        assert_eq!(counted[0].min, None);
    }
}
