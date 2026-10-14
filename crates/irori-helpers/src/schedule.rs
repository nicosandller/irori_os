//! A schedule: `schedule.<id>`, on during the times a week sets out and off between them.
//! "Heating hours", "the children's bedtime". A flow reads it (`on('schedule.heating_hours')`)
//! or starts from it turning on or off.
//!
//! The times are the home's (`home.toml`'s time zone), or this machine's while the home hasn't
//! said where it is. A time that doesn't happen one day, because the clocks go forward over
//! it, is taken as the first moment after.

use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, ClockTime, Name, ObjectId, ScheduleCapabilities, ScheduleState, Timestamp,
};
use jiff::civil::{DateTime, Time, Weekday};
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[schedules.<id>]`: for each day, its times as `["07:00", "09:00"]` pairs. `24:00` ends a
/// time at midnight.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub name: Name,
    #[serde(default)]
    pub mon: Vec<[String; 2]>,
    #[serde(default)]
    pub tue: Vec<[String; 2]>,
    #[serde(default)]
    pub wed: Vec<[String; 2]>,
    #[serde(default)]
    pub thu: Vec<[String; 2]>,
    #[serde(default)]
    pub fri: Vec<[String; 2]>,
    #[serde(default)]
    pub sat: Vec<[String; 2]>,
    #[serde(default)]
    pub sun: Vec<[String; 2]>,
}

/// A week as minutes of the day each day is on, from and until, Monday first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Week([Vec<(u32, u32)>; 7]);

/// Seconds into the day of a time written `07:00`, or the day's end for `24:00`.
fn seconds_of(text: &str) -> Result<u32, String> {
    if text == "24:00" || text == "24:00:00" {
        return Ok(24 * 3600);
    }
    let time: ClockTime = text
        .parse()
        .map_err(|e: irori_protocol::types::InvariantError| e.to_string())?;
    let time = time.as_jiff();
    Ok(u32::try_from(time.hour()).unwrap_or(0) * 3600
        + u32::try_from(time.minute()).unwrap_or(0) * 60
        + u32::try_from(time.second()).unwrap_or(0))
}

impl Week {
    pub(crate) fn of(schedule: &Schedule) -> Result<Self, String> {
        let mut week = Week::default();
        let days = [
            ("mon", &schedule.mon),
            ("tue", &schedule.tue),
            ("wed", &schedule.wed),
            ("thu", &schedule.thu),
            ("fri", &schedule.fri),
            ("sat", &schedule.sat),
            ("sun", &schedule.sun),
        ];
        for (i, (day, times)) in days.into_iter().enumerate() {
            let mut blocks = Vec::new();
            for [from, until] in times {
                let (start, end) = (seconds_of(from)?, seconds_of(until)?);
                if start >= end {
                    return Err(format!(
                        "on {day}, {from} to {until} ends before it starts; a time past midnight \
                         is two, one ending at 24:00 and one starting at 00:00"
                    ));
                }
                blocks.push((start, end));
            }
            blocks.sort_unstable();
            if blocks.windows(2).any(|pair| pair[0].1 > pair[1].0) {
                return Err(format!("on {day}, two of its times overlap"));
            }
            week.0[i] = blocks;
        }
        Ok(week)
    }

    fn day(&self, weekday: Weekday) -> &[(u32, u32)] {
        &self.0[usize::try_from(weekday.to_monday_zero_offset()).unwrap_or(0)]
    }

    /// Whether it's on at `at` on the wall.
    fn on(&self, at: DateTime) -> bool {
        let now = seconds_into(at.time());
        self.day(at.weekday())
            .iter()
            .any(|(start, end)| (*start..*end).contains(&now))
    }

    /// Whether it's on at `at` on the wall, and when that next changes (`None` for a week with
    /// no times at all, or one that's on all week).
    pub(crate) fn at(&self, at: DateTime) -> (bool, Option<DateTime>) {
        let on = self.on(at);
        // Every start and end over the next week and a day, in order: the first where it is
        // something else than now is the change. One time ending as the next begins, or a day
        // ending at midnight where the next starts, isn't one.
        let mut edges: Vec<DateTime> = (0..8_i64)
            .filter_map(|ahead| at.date().checked_add(jiff::Span::new().days(ahead)).ok())
            .flat_map(|day| {
                self.day(day.weekday())
                    .iter()
                    .flat_map(|(start, end)| [*start, *end])
                    .filter_map(move |edge| {
                        let midnight = day.to_datetime(Time::midnight());
                        midnight
                            .checked_add(jiff::Span::new().seconds(i64::from(edge)))
                            .ok()
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|edge| *edge > at)
            .collect();
        edges.sort_unstable();
        edges.dedup();
        let next = edges.into_iter().find(|edge| self.on(*edge) != on);
        (on, next)
    }
}

fn seconds_into(time: Time) -> u32 {
    u32::try_from(time.hour()).unwrap_or(0) * 3600
        + u32::try_from(time.minute()).unwrap_or(0) * 60
        + u32::try_from(time.second()).unwrap_or(0)
}

/// The home's zone, or this machine's while the home hasn't said.
pub(crate) fn zone(time_zone: Option<&str>) -> TimeZone {
    time_zone
        .and_then(|name| TimeZone::get(name).ok())
        .unwrap_or_else(TimeZone::system)
}

/// The schedule's state at `now` in `zone`.
pub(crate) fn state(week: &Week, zone: &TimeZone, now: Timestamp) -> ScheduleState {
    let wall = now.as_jiff().to_zoned(zone.clone()).datetime();
    let (on, next) = week.at(wall);
    ScheduleState {
        on,
        // A wall time the clocks skip is its first moment after.
        next_change: next
            .and_then(|next| next.to_zoned(zone.clone()).ok())
            .map(|zoned| Timestamp::from_jiff(zoned.timestamp())),
    }
}

pub(crate) fn made(id: &ObjectId, schedule: &Schedule) -> Result<Vec<Made>, ProtocolError> {
    let week = Week::of(schedule)
        .map_err(|why| ProtocolError::new(format!("the schedule `{id}`: {why}")))?;
    Ok(vec![Made::new_schedule(
        id,
        &schedule.name,
        Capabilities::Schedule(ScheduleCapabilities {}),
        week,
    )?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(toml_text: &str) -> Week {
        let schedule: Schedule =
            toml::from_str(&format!("name = \"S\"\n{toml_text}")).expect("valid");
        Week::of(&schedule).expect("a week")
    }

    fn wall(text: &str) -> DateTime {
        text.parse().expect("a wall time")
    }

    #[test]
    fn a_schedule_is_on_in_its_times_and_says_when_that_changes() {
        // 2026-10-12 is a Monday.
        let week = schedule(
            r#"mon = [["07:00", "09:00"], ["18:00", "22:00"]]
            tue = [["07:00", "09:00"]]"#,
        );
        assert_eq!(
            week.at(wall("2026-10-12T06:00")),
            (false, Some(wall("2026-10-12T07:00")))
        );
        assert_eq!(
            week.at(wall("2026-10-12T08:30")),
            (true, Some(wall("2026-10-12T09:00")))
        );
        assert_eq!(
            week.at(wall("2026-10-12T09:00")),
            (false, Some(wall("2026-10-12T18:00")))
        );
        assert_eq!(
            week.at(wall("2026-10-12T23:00")),
            (false, Some(wall("2026-10-13T07:00")))
        );
        // After Tuesday's time, the next is a week on.
        assert_eq!(
            week.at(wall("2026-10-13T10:00")),
            (false, Some(wall("2026-10-19T07:00")))
        );
        assert_eq!(Week::default().at(wall("2026-10-12T10:00")), (false, None));
    }

    #[test]
    fn a_time_through_midnight_is_on_without_a_break() {
        let week = schedule(
            r#"fri = [["22:00", "24:00"]]
            sat = [["00:00", "02:00"]]"#,
        );
        // Friday 2026-10-16 at 23:00: it carries on into Saturday, off at 02:00.
        assert_eq!(
            week.at(wall("2026-10-16T23:00")),
            (true, Some(wall("2026-10-17T02:00")))
        );
    }

    #[test]
    fn a_time_that_ends_before_it_starts_or_overlaps_is_refused() {
        let bad = |toml_text: &str| {
            let schedule: Schedule =
                toml::from_str(&format!("name = \"S\"\n{toml_text}")).expect("parses");
            Week::of(&schedule).expect_err("refused")
        };
        assert!(bad(r#"mon = [["22:00", "02:00"]]"#).contains("ends before it starts"));
        assert!(bad(r#"mon = [["07:00", "09:00"], ["08:00", "10:00"]]"#).contains("overlap"));
        assert!(bad(r#"mon = [["07:00", "25:00"]]"#).contains("isn't a time of day"));
    }

    #[test]
    fn its_state_is_read_on_the_homes_clock() {
        let week = schedule(r#"mon = [["07:00", "09:00"]]"#);
        let brussels = zone(Some("Europe/Brussels"));
        // 06:30 UTC is 08:30 in Brussels in summer time.
        let now: Timestamp = "2026-10-12T06:30:00Z".parse().expect("an instant");
        let state = state(&week, &brussels, now);
        assert!(state.on);
        assert_eq!(
            state.next_change,
            Some("2026-10-12T07:00:00Z".parse().expect("an instant"))
        );
    }
}
