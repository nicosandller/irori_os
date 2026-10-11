//! Telling the time at home: when a time of day next comes round, when the sun next rises, and
//! whether now is inside a window (`docs/specs/rules.md` §5.2, §5.3, §6.3, §6.4).
//!
//! Everything here is a pure function of a [`Place`] and a moment, so an engine on a virtual
//! clock (a backtest) and one on the real clock get the same answers.
//!
//! The time zone database is compiled in. An engine is a process of its own, often on an image
//! with no `/usr/share/zoneinfo`, and "07:00" must not depend on what the machine has installed.

use irori_types::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};

use crate::cron::CronSpec;
use crate::{CivilTime, SunEvent, Weekday};

/// How far ahead a next occurrence is looked for before deciding there isn't one: a cron for
/// the 29th of February needs four years, and one more for good measure.
const HORIZON_DAYS: i32 = 366 * 5;

/// Where the home is, as far as it's been said: a time zone, and maybe coordinates.
#[derive(Debug, Clone)]
pub struct Place {
    name: String,
    zone: TimeZone,
    coordinates: Option<(f64, f64)>,
}

impl PartialEq for Place {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.coordinates == other.coordinates
    }
}

impl Place {
    /// `time_zone` is an IANA name; `coordinates` are latitude and longitude in degrees.
    pub fn new(time_zone: &str, coordinates: Option<(f64, f64)>) -> Result<Self, String> {
        let zone = TimeZone::get(time_zone)
            .map_err(|_| format!("{time_zone:?} isn't a time zone Irori knows"))?;
        Ok(Self {
            name: time_zone.to_owned(),
            zone,
            coordinates,
        })
    }

    pub fn time_zone(&self) -> &str {
        &self.name
    }

    pub fn has_location(&self) -> bool {
        self.coordinates.is_some()
    }

    /// The date and time on the wall at home.
    pub fn civil(&self, at: Timestamp) -> DateTime {
        at.as_jiff().to_zoned(self.zone.clone()).datetime()
    }

    /// The moment a wall-clock time stands for, by the rules of rules.md §5.2: a time that
    /// doesn't exist that day (clocks went forward over it) is `None`, and one that happens
    /// twice (clocks went back over it) is its first occurrence.
    fn moment(&self, at: DateTime) -> Option<Timestamp> {
        let offset = match self.zone.to_ambiguous_timestamp(at).offset() {
            AmbiguousOffset::Unambiguous { offset } => offset,
            AmbiguousOffset::Gap { .. } => return None,
            AmbiguousOffset::Fold { before, .. } => before,
        };
        offset.to_timestamp(at).ok().map(Timestamp::from_jiff)
    }
}

/// Whether a time zone name is one the compiled-in database knows.
pub fn zone_exists(name: &str) -> bool {
    TimeZone::get(name).is_ok()
}

/// `HH:MM` or `HH:MM:SS` as numbers.
fn parts(time: &CivilTime) -> (i8, i8, i8) {
    let mut pieces = time
        .as_str()
        .split(':')
        .map(|piece| piece.parse::<i8>().unwrap_or(0));
    (
        pieces.next().unwrap_or(0),
        pieces.next().unwrap_or(0),
        pieces.next().unwrap_or(0),
    )
}

fn weekday_of(date: Date) -> Weekday {
    match date.weekday() {
        jiff::civil::Weekday::Monday => Weekday::Mon,
        jiff::civil::Weekday::Tuesday => Weekday::Tue,
        jiff::civil::Weekday::Wednesday => Weekday::Wed,
        jiff::civil::Weekday::Thursday => Weekday::Thu,
        jiff::civil::Weekday::Friday => Weekday::Fri,
        jiff::civil::Weekday::Saturday => Weekday::Sat,
        jiff::civil::Weekday::Sunday => Weekday::Sun,
    }
}

/// The first moment after `after` that the clock at home reads `at`, on one of `weekdays` if
/// any are given.
pub fn next_time(
    place: &Place,
    at: &CivilTime,
    weekdays: Option<&[Weekday]>,
    after: Timestamp,
) -> Option<Timestamp> {
    let (hour, minute, second) = parts(at);
    let mut date = place.civil(after).date();
    for _ in 0..HORIZON_DAYS {
        if weekdays.is_none_or(|days| days.contains(&weekday_of(date)))
            && let Some(moment) = place.moment(date.at(hour, minute, second, 0))
            && moment > after
        {
            return Some(moment);
        }
        date = date.tomorrow().ok()?;
    }
    None
}

/// The first moment after `after` that a cron names.
pub fn next_cron(place: &Place, cron: &CronSpec, after: Timestamp) -> Option<Timestamp> {
    let mut date = place.civil(after).date();
    for _ in 0..HORIZON_DAYS {
        let month = u8::try_from(date.month()).unwrap_or(0);
        let day = u8::try_from(date.day()).unwrap_or(0);
        let weekday = u8::try_from(date.weekday().to_sunday_zero_offset()).unwrap_or(0);
        if cron.on_day(month, day, weekday) {
            for hour in &cron.hours {
                for minute in &cron.minutes {
                    let at = date.at(
                        i8::try_from(*hour).unwrap_or(0),
                        i8::try_from(*minute).unwrap_or(0),
                        0,
                        0,
                    );
                    if let Some(moment) = place.moment(at)
                        && moment > after
                    {
                        return Some(moment);
                    }
                }
            }
        }
        date = date.tomorrow().ok()?;
    }
    None
}

/// Whether now is inside a time window (the truth table of rules.md §6.3).
pub fn in_time_window(
    place: &Place,
    after: Option<&CivilTime>,
    before: Option<&CivilTime>,
    weekdays: Option<&[Weekday]>,
    now: Timestamp,
) -> bool {
    let here = place.civil(now);
    if weekdays.is_some_and(|days| !days.contains(&weekday_of(here.date()))) {
        return false;
    }
    let seconds = |(hour, minute, second): (i8, i8, i8)| {
        i32::from(hour) * 3600 + i32::from(minute) * 60 + i32::from(second)
    };
    let at = seconds((here.hour(), here.minute(), here.second()));
    match (
        after.map(parts).map(seconds),
        before.map(parts).map(seconds),
    ) {
        (Some(after), Some(before)) if after < before => after <= at && at < before,
        // Overnight: 22:00 to 06:00 holds at 23:00 and at 05:59.
        (Some(after), Some(before)) if after > before => at >= after || at < before,
        // The same time twice is an empty window, not all day.
        (Some(_), Some(_)) => false,
        (Some(after), None) => at >= after,
        (None, Some(before)) => at < before,
        (None, None) => true,
    }
}

/// The hour (0–23) and minute (0–59) on the wall at home.
pub fn hour_and_minute(place: &Place, now: Timestamp) -> (i64, i64) {
    let here = place.civil(now);
    (i64::from(here.hour()), i64::from(here.minute()))
}

// ---- the sun ------------------------------------------------------------------------------

/// How far below the horizon the sun's middle is when its top edge shows, with the air's
/// bending of the light: the zenith angle of sunrise and sunset.
const ZENITH_HORIZON: f64 = 90.833;

/// Civil twilight: the sun six degrees down. Dawn and dusk.
const ZENITH_CIVIL: f64 = 96.0;

/// Days from the civil calendar's 1970-01-01 to the Julian day count's start, at midnight UTC.
const JULIAN_AT_UNIX_EPOCH: f64 = 2_440_587.5;

/// The sun's declination (degrees) and the equation of time (minutes) at a Julian day.
/// NOAA's solar equations, good to well under a minute for the next few centuries.
fn sun_at(julian_day: f64) -> (f64, f64) {
    let t = (julian_day - 2_451_545.0) / 36_525.0;
    let mean_longitude = (280.466_46 + t * (36_000.769_83 + t * 0.000_303_2)).rem_euclid(360.0);
    let mean_anomaly = 357.529_11 + t * (35_999.050_29 - 0.000_153_7 * t);
    let eccentricity = 0.016_708_634 - t * (0.000_042_037 + 0.000_000_126_7 * t);
    let m = mean_anomaly.to_radians();
    let centre = m.sin() * (1.914_602 - t * (0.004_817 + 0.000_014 * t))
        + (2.0 * m).sin() * (0.019_993 - 0.000_101 * t)
        + (3.0 * m).sin() * 0.000_289;
    let omega = (125.04 - 1934.136 * t).to_radians();
    let apparent_longitude =
        (mean_longitude + centre - 0.005_69 - 0.004_78 * omega.sin()).to_radians();
    let mean_obliquity =
        23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.000_59 - t * 0.001_813))) / 60.0) / 60.0;
    let obliquity = (mean_obliquity + 0.002_56 * omega.cos()).to_radians();
    let declination = (obliquity.sin() * apparent_longitude.sin())
        .asin()
        .to_degrees();
    let y = (obliquity / 2.0).tan().powi(2);
    let l = mean_longitude.to_radians();
    let equation = 4.0
        * (y * (2.0 * l).sin() - 2.0 * eccentricity * m.sin()
            + 4.0 * eccentricity * y * m.sin() * (2.0 * l).cos()
            - 0.5 * y * y * (4.0 * l).sin()
            - 1.25 * eccentricity * eccentricity * (2.0 * m).sin())
        .to_degrees();
    (declination, equation)
}

/// How long before and after solar noon the sun stands at `zenith`, in minutes. `None` on a
/// day it never gets there: the midnight sun, or the polar night.
fn half_day(latitude: f64, declination: f64, zenith: f64) -> Option<f64> {
    let (lat, dec) = (latitude.to_radians(), declination.to_radians());
    let cos_hour_angle =
        zenith.to_radians().cos() / (lat.cos() * dec.cos()) - lat.tan() * dec.tan();
    (-1.0..=1.0)
        .contains(&cos_hour_angle)
        .then(|| cos_hour_angle.acos().to_degrees() * 4.0)
}

/// A sun event of one solar day, as minutes from that date's midnight UTC. The solar day is
/// the one whose noon falls on `date` in UTC, which is the calendar day at home or next to it.
fn sun_minutes(event: SunEvent, date: Date, latitude: f64, longitude: f64) -> Option<f64> {
    let days = f64::from(date.since(Date::constant(1970, 1, 1)).ok()?.get_days());
    let midnight = JULIAN_AT_UNIX_EPOCH + days;
    let at = |minutes: f64| -> Option<f64> {
        let (declination, equation) = sun_at(midnight + minutes / 1440.0);
        let noon = 720.0 - 4.0 * longitude - equation;
        Some(match event {
            SunEvent::Noon => noon,
            SunEvent::Midnight => noon - 720.0,
            SunEvent::Sunrise => noon - half_day(latitude, declination, ZENITH_HORIZON)?,
            SunEvent::Sunset => noon + half_day(latitude, declination, ZENITH_HORIZON)?,
            SunEvent::Dawn => noon - half_day(latitude, declination, ZENITH_CIVIL)?,
            SunEvent::Dusk => noon + half_day(latitude, declination, ZENITH_CIVIL)?,
        })
    };
    // Once from the day's middle, then again from where that landed: the sun's own position
    // moves a little over the day, and the second pass is taken at the moment itself.
    let first = at(720.0 - 4.0 * longitude)?;
    at(first)
}

fn sun_moment(event: SunEvent, date: Date, latitude: f64, longitude: f64) -> Option<Timestamp> {
    let minutes = sun_minutes(event, date, latitude, longitude)?;
    let days = i64::from(date.since(Date::constant(1970, 1, 1)).ok()?.get_days());
    #[allow(clippy::cast_possible_truncation)] // minutes within a few days of the date
    let millis = days * 86_400_000 + (minutes * 60_000.0).round() as i64;
    jiff::Timestamp::from_millisecond(millis)
        .ok()
        .map(Timestamp::from_jiff)
}

fn shifted(at: Timestamp, offset_ms: i64) -> Timestamp {
    Timestamp::from_jiff(
        at.as_jiff()
            .checked_add(jiff::SignedDuration::from_millis(offset_ms))
            .unwrap_or(at.as_jiff()),
    )
}

/// The first time after `after` that `event` happens, moved by `offset_ms` (negative is
/// before it). `None` without coordinates, or if it doesn't happen in the coming year.
pub fn next_sun(
    place: &Place,
    event: SunEvent,
    offset_ms: i64,
    after: Timestamp,
) -> Option<Timestamp> {
    let (latitude, longitude) = place.coordinates?;
    // From two days back: an offset of a day, or a home a long way from Greenwich, can put the
    // next one on a solar day that started before today.
    let mut date = after.as_jiff().to_zoned(TimeZone::UTC).date();
    date = date.yesterday().ok()?.yesterday().ok()?;
    for _ in 0..370 {
        if let Some(moment) = sun_moment(event, date, latitude, longitude)
            && shifted(moment, offset_ms) > after
        {
            return Some(shifted(moment, offset_ms));
        }
        date = date.tomorrow().ok()?;
    }
    None
}

/// When `event` happens on the calendar day at home that `now` is in. `Ok(None)` on a day it
/// doesn't happen; `Err` without coordinates.
pub fn sun_today(
    place: &Place,
    event: SunEvent,
    now: Timestamp,
) -> Result<Option<Timestamp>, String> {
    let (latitude, longitude) = place
        .coordinates
        .ok_or_else(|| "the sun needs the home's location".to_owned())?;
    let today = place.civil(now).date();
    let mut date = today.yesterday().map_err(|e| e.to_string())?;
    for _ in 0..3 {
        if let Some(moment) = sun_moment(event, date, latitude, longitude)
            && place.civil(moment).date() == today
        {
            return Ok(Some(moment));
        }
        date = date.tomorrow().map_err(|e| e.to_string())?;
    }
    Ok(None)
}

/// Whether now is inside a sun window (rules.md §6.4): after one event, before another, or
/// both, each moved by the same offset. An event that doesn't happen today is an error, which
/// a condition counts as not holding.
pub fn in_sun_window(
    place: &Place,
    after: Option<SunEvent>,
    before: Option<SunEvent>,
    after_offset_ms: i64,
    before_offset_ms: i64,
    now: Timestamp,
) -> Result<bool, String> {
    let today = |event: SunEvent, offset_ms: i64| -> Result<Timestamp, String> {
        sun_today(place, event, now)?
            .map(|at| shifted(at, offset_ms))
            .ok_or_else(|| format!("there's no {} at home today", sun_word(event)))
    };
    Ok(match (after, before) {
        (Some(after), Some(before)) => {
            let (after, before) = (
                today(after, after_offset_ms)?,
                today(before, before_offset_ms)?,
            );
            if after <= before {
                after <= now && now < before
            } else {
                now >= after || now < before
            }
        }
        (Some(after), None) => now >= today(after, after_offset_ms)?,
        (None, Some(before)) => now < today(before, before_offset_ms)?,
        (None, None) => true,
    })
}

/// A sun event as a person says it.
pub fn sun_word(event: SunEvent) -> &'static str {
    match event {
        SunEvent::Sunrise => "sunrise",
        SunEvent::Sunset => "sunset",
        SunEvent::Dawn => "dawn",
        SunEvent::Dusk => "dusk",
        SunEvent::Noon => "solar noon",
        SunEvent::Midnight => "solar midnight",
    }
}

/// A moment as the clock at home shows it: "18:42".
pub fn wall(place: &Place, at: Timestamp) -> String {
    let here = place.civil(at);
    format!("{:02}:{:02}", here.hour(), here.minute())
}

/// A moment as a person would say it from `now`: "today at 18:42", "tomorrow at 06:52",
/// "Monday at 07:00", "12 Mar at 07:00".
pub fn spoken(place: &Place, at: Timestamp, now: Timestamp) -> String {
    const DAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (then, today) = (place.civil(at).date(), place.civil(now).date());
    let days = then.since(today).map(|span| span.get_days()).unwrap_or(0);
    let day = match days {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        2..=6 => DAYS[usize::try_from(then.weekday().to_monday_zero_offset()).unwrap_or(0) % 7]
            .to_owned(),
        _ => format!(
            "{} {}",
            then.day(),
            MONTHS[usize::try_from(then.month() - 1).unwrap_or(0) % 12]
        ),
    };
    format!("{day} at {}", wall(place, at))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        Timestamp::from_jiff(text.parse().unwrap())
    }

    fn time(text: &str) -> CivilTime {
        serde_json::from_value(serde_json::json!(text)).unwrap()
    }

    fn brussels() -> Place {
        Place::new("Europe/Brussels", Some((50.8467, 4.3525))).unwrap()
    }

    fn minutes_apart(a: Timestamp, b: Timestamp) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let ms = (a.as_jiff().as_millisecond() - b.as_jiff().as_millisecond()) as f64;
        (ms / 60_000.0).abs()
    }

    #[test]
    fn a_zone_nobody_has_heard_of_is_refused() {
        assert!(zone_exists("Europe/Brussels"));
        assert!(!zone_exists("Europe/Atlantis"));
        let why = Place::new("Europe/Atlantis", None).unwrap_err();
        assert!(why.contains("isn't a time zone"), "{why}");
    }

    #[test]
    fn seven_comes_round_today_or_tomorrow() {
        let place = brussels();
        // 06:00 in Brussels (summer, UTC+2): 07:00 is still to come today.
        let early = at("2026-07-01T04:00:00Z");
        assert_eq!(
            next_time(&place, &time("07:00"), None, early),
            Some(at("2026-07-01T05:00:00Z"))
        );
        // At 07:00 exactly it has happened: the next one is tomorrow's.
        let on_it = at("2026-07-01T05:00:00Z");
        assert_eq!(
            next_time(&place, &time("07:00"), None, on_it),
            Some(at("2026-07-02T05:00:00Z"))
        );
    }

    #[test]
    fn only_the_days_asked_for() {
        let place = brussels();
        // Wednesday 1 July 2026. The next Monday is the 6th.
        let now = at("2026-07-01T12:00:00Z");
        let next = next_time(&place, &time("07:00"), Some(&[Weekday::Mon]), now);
        assert_eq!(next, Some(at("2026-07-06T05:00:00Z")));
    }

    #[test]
    fn a_time_the_clocks_skip_is_skipped_that_day() {
        let place = brussels();
        // Clocks go forward on 29 March 2026: 02:30 doesn't exist. The next 02:30 is the 30th.
        let now = at("2026-03-28T12:00:00Z");
        let first = next_time(&place, &time("02:30"), None, now).unwrap();
        assert_eq!(first, at("2026-03-30T00:30:00Z"));
    }

    #[test]
    fn a_time_that_happens_twice_fires_at_the_first() {
        let place = brussels();
        // Clocks go back on 25 October 2026: 02:30 happens at 00:30Z (summer) and 01:30Z.
        let now = at("2026-10-24T12:00:00Z");
        let first = next_time(&place, &time("02:30"), None, now).unwrap();
        assert_eq!(first, at("2026-10-25T00:30:00Z"));
        // And not again an hour later: the one after is the next day's.
        let second = next_time(&place, &time("02:30"), None, first).unwrap();
        assert_eq!(second, at("2026-10-26T01:30:00Z"));
    }

    #[test]
    fn a_cron_comes_round_when_its_fields_say() {
        let place = brussels();
        let spec = CronSpec::parse("*/15 8 * * MON-FRI").unwrap();
        // Friday 3 July 2026, 08:50 at home: the 08:45 has gone, so Monday at 08:00.
        let now = at("2026-07-03T06:50:00Z");
        assert_eq!(
            next_cron(&place, &spec, now),
            Some(at("2026-07-06T06:00:00Z"))
        );
        // The 29th of February, years away.
        let leap = CronSpec::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            next_cron(&place, &leap, now),
            Some(at("2028-02-28T23:00:00Z"))
        );
    }

    #[test]
    fn a_window_holds_inside_it_and_wraps_overnight() {
        let place = brussels();
        let (ten, six) = (time("22:00"), time("06:00"));
        let holds = |text: &str| in_time_window(&place, Some(&ten), Some(&six), None, at(text));
        // Winter, UTC+1.
        assert!(holds("2026-01-10T22:00:00Z")); // 23:00
        assert!(holds("2026-01-10T04:59:00Z")); // 05:59
        assert!(!holds("2026-01-10T05:00:00Z")); // 06:00
        assert!(!holds("2026-01-10T20:59:00Z")); // 21:59
        // The same time twice is never.
        assert!(!in_time_window(
            &place,
            Some(&ten),
            Some(&ten),
            None,
            at("2026-01-10T21:00:00Z")
        ));
        // Days alone: all of that day, at home. Saturday 10 January.
        let day = |days: &[Weekday]| {
            in_time_window(&place, None, None, Some(days), at("2026-01-10T12:00:00Z"))
        };
        assert!(day(&[Weekday::Sat]));
        assert!(!day(&[Weekday::Sun]));
    }

    #[test]
    fn the_hour_is_the_one_on_the_wall_at_home() {
        let place = brussels();
        assert_eq!(hour_and_minute(&place, at("2026-07-01T21:05:00Z")), (23, 5));
        assert_eq!(hour_and_minute(&place, at("2026-01-01T21:05:00Z")), (22, 5));
    }

    /// Against published almanac times, to within two minutes.
    #[test]
    fn the_sun_rises_and_sets_when_the_almanac_says() {
        let place = brussels();
        let midsummer = at("2026-06-21T00:00:00Z");
        let rise = next_sun(&place, SunEvent::Sunrise, 0, midsummer).unwrap();
        let set = next_sun(&place, SunEvent::Sunset, 0, midsummer).unwrap();
        // Brussels, 21 June: up at 05:29, down at 22:00 (CEST, UTC+2).
        assert!(
            minutes_apart(rise, at("2026-06-21T03:29:00Z")) < 2.0,
            "{rise:?}"
        );
        assert!(
            minutes_apart(set, at("2026-06-21T20:00:00Z")) < 2.0,
            "{set:?}"
        );

        let midwinter = at("2026-12-21T00:00:00Z");
        let rise = next_sun(&place, SunEvent::Sunrise, 0, midwinter).unwrap();
        let set = next_sun(&place, SunEvent::Sunset, 0, midwinter).unwrap();
        // 21 December: up at 08:42, down at 16:39 (CET, UTC+1).
        assert!(
            minutes_apart(rise, at("2026-12-21T07:42:00Z")) < 2.0,
            "{rise:?}"
        );
        assert!(
            minutes_apart(set, at("2026-12-21T15:39:00Z")) < 2.0,
            "{set:?}"
        );
    }

    #[test]
    fn the_day_is_in_order_and_an_offset_moves_it() {
        let place = brussels();
        let now = at("2026-09-22T00:00:00Z");
        let next = |event| next_sun(&place, event, 0, now).unwrap();
        assert!(next(SunEvent::Dawn) < next(SunEvent::Sunrise));
        assert!(next(SunEvent::Sunrise) < next(SunEvent::Noon));
        assert!(next(SunEvent::Noon) < next(SunEvent::Sunset));
        assert!(next(SunEvent::Sunset) < next(SunEvent::Dusk));
        let early = next_sun(&place, SunEvent::Sunset, -30 * 60_000, now).unwrap();
        assert!((minutes_apart(early, next(SunEvent::Sunset)) - 30.0).abs() < 0.01);
    }

    #[test]
    fn far_from_greenwich_the_next_one_is_still_the_next_one() {
        // Auckland, twelve or thirteen hours ahead: its morning is yesterday evening in UTC.
        let place = Place::new("Pacific/Auckland", Some((-36.8485, 174.7633))).unwrap();
        let now = at("2026-03-10T00:00:00Z"); // 13:00 at home
        let set = next_sun(&place, SunEvent::Sunset, 0, now).unwrap();
        let rise = next_sun(&place, SunEvent::Sunrise, 0, now).unwrap();
        assert!(set > now && rise > set, "{set:?} {rise:?}");
        // Sunset is that same evening: within twelve hours.
        assert!(minutes_apart(set, now) < 12.0 * 60.0);
        assert_eq!(place.civil(set).date(), place.civil(now).date());
    }

    #[test]
    fn where_the_sun_doesn_t_set_there_is_no_sunset_to_wait_for_today() {
        // Tromsø at midsummer: the midnight sun.
        let place = Place::new("Europe/Oslo", Some((69.6492, 18.9553))).unwrap();
        let now = at("2026-06-21T10:00:00Z");
        assert_eq!(sun_today(&place, SunEvent::Sunset, now), Ok(None));
        // It comes back: the next sunset is weeks away, not never.
        let next = next_sun(&place, SunEvent::Sunset, 0, now).unwrap();
        assert!(minutes_apart(next, now) > 20.0 * 24.0 * 60.0);
        // And a window on it doesn't hold, and says why.
        let why = in_sun_window(&place, Some(SunEvent::Sunset), None, 0, 0, now).unwrap_err();
        assert!(why.contains("no sunset"), "{why}");
    }

    #[test]
    fn after_sunset_holds_until_the_day_ends() {
        let place = brussels();
        let after =
            |text: &str| in_sun_window(&place, Some(SunEvent::Sunset), None, 0, 0, at(text));
        // 21 June: sunset at 22:00 at home (20:00Z).
        assert_eq!(after("2026-06-21T19:00:00Z"), Ok(false));
        assert_eq!(after("2026-06-21T20:30:00Z"), Ok(true));
        // Between sunset and sunrise wraps the night.
        let night = |text: &str| {
            in_sun_window(
                &place,
                Some(SunEvent::Sunset),
                Some(SunEvent::Sunrise),
                0,
                0,
                at(text),
            )
        };
        assert_eq!(night("2026-06-21T01:00:00Z"), Ok(true)); // 03:00, before sunrise
        assert_eq!(night("2026-06-21T12:00:00Z"), Ok(false));
        assert_eq!(night("2026-06-21T20:30:00Z"), Ok(true));
    }

    #[test]
    fn each_end_of_a_sun_window_moves_on_its_own() {
        let place = brussels();
        const HOUR: i64 = 3_600_000;
        // 21 June: sunset at 22:00 at home (20:00Z). An hour before it opens the window early
        // and leaves where it closes alone.
        let early = |text: &str| {
            in_sun_window(
                &place,
                Some(SunEvent::Sunset),
                Some(SunEvent::Sunrise),
                -HOUR,
                0,
                at(text),
            )
        };
        assert_eq!(early("2026-06-21T18:30:00Z"), Ok(false));
        assert_eq!(early("2026-06-21T19:30:00Z"), Ok(true));
        // Sunrise is about 05:30 at home (03:30Z): two hours on, 07:00 is still inside.
        let late = |text: &str| {
            in_sun_window(
                &place,
                Some(SunEvent::Sunset),
                Some(SunEvent::Sunrise),
                0,
                2 * HOUR,
                at(text),
            )
        };
        assert_eq!(late("2026-06-21T05:00:00Z"), Ok(true));
        assert_eq!(early("2026-06-21T05:00:00Z"), Ok(false));
    }

    #[test]
    fn without_a_location_the_sun_is_out_of_reach() {
        let place = Place::new("Europe/Brussels", None).unwrap();
        let now = at("2026-06-21T10:00:00Z");
        assert_eq!(next_sun(&place, SunEvent::Sunset, 0, now), None);
        assert!(sun_today(&place, SunEvent::Sunset, now).is_err());
    }

    #[test]
    fn a_moment_is_said_the_way_a_person_would() {
        let place = brussels();
        let now = at("2026-07-01T10:00:00Z"); // Wednesday, 12:00 at home
        assert_eq!(
            spoken(&place, at("2026-07-01T16:42:00Z"), now),
            "today at 18:42"
        );
        assert_eq!(
            spoken(&place, at("2026-07-02T04:52:00Z"), now),
            "tomorrow at 06:52"
        );
        assert_eq!(
            spoken(&place, at("2026-07-06T05:00:00Z"), now),
            "Monday at 07:00"
        );
        assert_eq!(
            spoken(&place, at("2026-08-12T05:00:00Z"), now),
            "12 Aug at 07:00"
        );
    }
}
