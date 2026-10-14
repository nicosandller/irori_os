//! Every kind of helper in words, with its settings and an example: what the assistant reads
//! to suggest one, and what anyone writing `extensions/helpers.toml` by hand would want.

/// One kind, said for someone choosing.
#[derive(Debug)]
pub struct Guide {
    /// Its table, and the `kind` a helper is proposed as.
    pub table: &'static str,
    /// What it is and what it's for.
    pub what: &'static str,
    /// Its settings beside `name`, each with what it means.
    pub settings: &'static str,
    /// The settings of one, as JSON.
    pub example: &'static str,
}

pub const GUIDE: &[Guide] = &[
    Guide {
        table: "toggles",
        what: "On or off, set by a person or a flow: switch.<id>. \"Guests are over\".",
        settings: "initial (true or false, default false)",
        example: r#"{"name": "Guests are over"}"#,
    },
    Guide {
        table: "numbers",
        what: "A value within a range: number.<id>. A brightness for the evening, a threshold a flow compares with.",
        settings: "min and max (required), step (default 1), unit (text), mode (slider or box), initial (default min)",
        example: r#"{"name": "Evening brightness", "min": 0, "max": 100, "step": 5, "unit": "%"}"#,
    },
    Guide {
        table: "counters",
        what: "A number counted up and down by flows (number.increment and number.decrement): number.<id>.",
        settings: "initial (default 0), step (default 1), min (default 0), max (default 1000000)",
        example: r#"{"name": "Doorbell rings today"}"#,
    },
    Guide {
        table: "selects",
        what: "One of a list: select.<id>. \"Home, away or asleep\".",
        settings: "options (list of texts, required), initial (default the first)",
        example: r#"{"name": "House mode", "options": ["home", "away", "asleep"]}"#,
    },
    Guide {
        table: "texts",
        what: "A few words someone sets: text.<id>.",
        settings: "min_length (default 0), max_length (default 255), initial",
        example: r#"{"name": "Hall display message"}"#,
    },
    Guide {
        table: "buttons",
        what: "Something to press: button.<id>, and event.<id> that says `pressed` each time, which flows start from.",
        settings: "none",
        example: r#"{"name": "Movie time"}"#,
    },
    Guide {
        table: "dates",
        what: "A calendar day: date.<id>.",
        settings: "initial (like 2026-10-14)",
        example: r#"{"name": "Bins go out"}"#,
    },
    Guide {
        table: "times",
        what: "A time of day: time.<id>.",
        settings: "initial (like 06:45)",
        example: r#"{"name": "Wake up", "initial": "06:45"}"#,
    },
    Guide {
        table: "datetimes",
        what: "A day and a time: datetime.<id>.",
        settings: "initial (like 2026-10-14T06:45:00Z)",
        example: r#"{"name": "Holiday ends"}"#,
    },
    Guide {
        table: "timers",
        what: "Counts down and says when it's done: timer.<id> (idle, active, paused), and event.<id> saying started, paused, cancelled, finished.",
        settings: "duration (seconds; 0 means each start says how long), restore (default true: finishes at startup if it ran out while Irori was down)",
        example: r#"{"name": "Lights stay on", "duration": 600}"#,
    },
    Guide {
        table: "schedules",
        what: "On during the times set for each day of the week, on the home's clock: schedule.<id>.",
        settings: "mon, tue, wed, thu, fri, sat, sun: each a list of [from, until] times, like [[\"06:30\", \"08:30\"]]; 24:00 ends one at midnight",
        example: r#"{"name": "Heating hours", "mon": [["06:30", "08:30"], ["17:00", "22:00"]], "sat": [["08:00", "23:00"]]}"#,
    },
    Guide {
        table: "groups",
        what: "Several entities of one kind as one: <kind>.<id>. On while any member is (or all, with all = true); a call to it goes to every member. Sensors combine their readings.",
        settings: "kind (light, switch, fan, cover, binary_sensor or sensor), members (entity ids of that kind), all (true or false), of (for sensors: mean, min, max or sum)",
        example: r#"{"name": "Downstairs lights", "kind": "light", "members": ["light.kitchen", "light.lounge"]}"#,
    },
    Guide {
        table: "thresholds",
        what: "On while a number is above, below, or between lines: binary_sensor.<id>.",
        settings: "source (entity id with a number), field (one of its other values, optional), above, below (one or both), hysteresis (how far past a line to change, default 0)",
        example: r#"{"name": "Too warm", "source": "sensor.lounge_temperature", "above": 26, "hysteresis": 0.5}"#,
    },
    Guide {
        table: "trends",
        what: "On while a number rises (or falls) by at least `by` over `over` seconds: binary_sensor.<id>.",
        settings: "source, field (optional), direction (rising or falling), by, over (seconds, at most a week)",
        example: r#"{"name": "Shower running", "source": "sensor.bathroom_humidity", "direction": "rising", "by": 10, "over": 600}"#,
    },
    Guide {
        table: "meters",
        what: "How much a counting reading (an energy total, a water meter) went up since the hour, day, week, month or year began: sensor.<id>.",
        settings: "source (a total that only goes up), every (hour, day, week, month or year)",
        example: r#"{"name": "Energy today", "source": "sensor.house_energy", "every": "day"}"#,
    },
    Guide {
        table: "integrals",
        what: "The area under a reading over time: a power reading (W) into energy (Wh): sensor.<id>.",
        settings: "source, field (optional), per (second, minute or hour; default hour)",
        example: r#"{"name": "Heater energy", "source": "sensor.heater_power"}"#,
    },
    Guide {
        table: "derivatives",
        what: "How fast a reading changes, per hour (or minute, second), over the last `over` seconds: sensor.<id>.",
        settings: "source, field (optional), per (default hour), over (seconds)",
        example: r#"{"name": "Tank filling rate", "source": "sensor.tank_level", "over": 900}"#,
    },
    Guide {
        table: "statistics",
        what: "A reading's average, least or most over the last `over` seconds: sensor.<id>.",
        settings: "source, field (optional), of (mean, min or max), over (seconds)",
        example: r#"{"name": "Average outside temperature", "source": "sensor.outside_temperature", "of": "mean", "over": 3600}"#,
    },
    Guide {
        table: "history",
        what: "How long (hours), how often (count) or how much of the time (%) something was a value, since midnight or over `over` seconds: sensor.<id>.",
        settings: "source (any entity), is (true, false, or a text like \"heat\"), measure (time, count or ratio), over (seconds, optional)",
        example: r#"{"name": "Heating hours today", "source": "climate.hall", "is": "heat", "measure": "time"}"#,
    },
];

/// The whole guide as text, a kind to a paragraph.
pub fn guide() -> String {
    GUIDE
        .iter()
        .map(|kind| {
            format!(
                "{}: {}\n  settings: {}\n  example: {}",
                kind.table, kind.what, kind.settings, kind.example
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KINDS;

    #[test]
    fn every_kind_is_in_the_guide_and_its_example_is_one() {
        for kind in KINDS {
            let said = GUIDE
                .iter()
                .find(|guide| guide.table == kind.table)
                .unwrap_or_else(|| panic!("{} isn't in the guide", kind.table));
            let example: serde_json::Value = serde_json::from_str(said.example).expect("JSON");
            let id = irori_protocol::types::ObjectId::try_from("example").expect("an id");
            kind.check(&id, &example)
                .unwrap_or_else(|why| panic!("{}'s example: {why}", kind.table));
        }
        assert_eq!(GUIDE.len(), KINDS.len());
    }
}
