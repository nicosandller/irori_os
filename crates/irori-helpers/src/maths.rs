//! The arithmetic of the helpers worked out from other entities, as plain functions over
//! readings and times, so it's tested without a clock or a home.

use irori_protocol::types::Timestamp;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One reading and when it was taken.
pub(crate) type Sample = (Timestamp, f64);

/// Seconds from `from` to `to`, negative when `to` is earlier.
pub(crate) fn seconds(from: Timestamp, to: Timestamp) -> f64 {
    to.as_jiff().duration_since(from.as_jiff()).as_secs_f64()
}

/// `at` moved by `by` seconds.
pub(crate) fn plus(at: Timestamp, by: f64) -> Timestamp {
    jiff::SignedDuration::try_from_secs_f64(by)
        .ok()
        .and_then(|span| at.as_jiff().checked_add(span).ok())
        .map_or(at, Timestamp::from_jiff)
}

/// Whether a threshold is on, given the reading and whether it was on before. `hysteresis`
/// keeps a reading hovering at the line from flicking it on and off: it has to go that far
/// past the line to change.
pub(crate) fn threshold(
    value: f64,
    above: Option<f64>,
    below: Option<f64>,
    hysteresis: f64,
    was_on: bool,
) -> bool {
    // Turning on needs the reading past the line by the hysteresis; turning off, back past it
    // the other way by as much.
    let margin = if was_on { -hysteresis } else { hysteresis };
    let over = |line: f64| value > line + margin;
    let under = |line: f64| value < line - margin;
    match (above, below) {
        (Some(above), Some(below)) => over(above) && under(below),
        (Some(above), None) => over(above),
        (None, Some(below)) => under(below),
        (None, None) => false,
    }
}

/// The readings within the last `over` seconds before `now`, with the last reading before
/// them standing at the window's start: a value holds until the next one comes.
pub(crate) fn window(samples: &[Sample], now: Timestamp, over: f64) -> Vec<Sample> {
    let start = plus(now, -over);
    let mut kept: Vec<Sample> = Vec::new();
    if let Some((_, before)) = samples.iter().rev().find(|(at, _)| *at <= start) {
        kept.push((start, *before));
    }
    kept.extend(
        samples
            .iter()
            .filter(|(at, _)| *at > start && *at <= now)
            .copied(),
    );
    kept
}

/// How much the readings changed over the window: the last less the first.
pub(crate) fn change(samples: &[Sample]) -> Option<f64> {
    Some(samples.last()?.1 - samples.first()?.1)
}

/// Whether it's rising (or falling) by at least `by` over the window.
pub(crate) fn trend(samples: &[Sample], rising: bool, by: f64) -> bool {
    change(samples).is_some_and(|change| if rising { change >= by } else { -change >= by })
}

/// Change per `per` seconds over the window.
pub(crate) fn derivative(samples: &[Sample], per: f64) -> Option<f64> {
    let (first, last) = (samples.first()?, samples.last()?);
    let span = seconds(first.0, last.0);
    (span > 0.0).then(|| (last.1 - first.1) / span * per)
}

/// What a statistics helper works out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Statistic {
    /// The average over the time, each reading counting for as long as it held.
    Mean,
    Min,
    Max,
}

/// The statistic of the window up to `now`.
pub(crate) fn statistic(samples: &[Sample], now: Timestamp, of: Statistic) -> Option<f64> {
    let values = samples.iter().map(|(_, value)| *value);
    match of {
        Statistic::Min => values.reduce(f64::min),
        Statistic::Max => values.reduce(f64::max),
        Statistic::Mean => {
            let start = samples.first()?.0;
            let span = seconds(start, now);
            if span <= 0.0 {
                return samples.last().map(|(_, value)| *value);
            }
            let held: f64 = samples
                .iter()
                .enumerate()
                .map(|(i, (at, value))| {
                    let until = samples.get(i + 1).map_or(now, |(next, _)| *next);
                    value * seconds(*at, until)
                })
                .sum();
            Some(held / span)
        }
    }
}

/// A running integral: the area under the readings so far, each step the average of its two
/// ends (Home Assistant's trapezoidal rule), per `per` seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Integral {
    pub total: f64,
    pub last: Option<(Timestamp, f64)>,
}

impl Integral {
    pub(crate) fn add(&mut self, at: Timestamp, value: f64, per: f64) {
        if let Some((then, before)) = self.last {
            let span = seconds(then, at);
            if span > 0.0 {
                self.total += (before + value) / 2.0 * span / per;
            }
        }
        self.last = Some((at, value));
    }
}

/// How often a meter starts again from nought.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    Hour,
    Day,
    Week,
    Month,
    Year,
}

/// When the period `at` falls in began, on the home's clock.
pub(crate) fn period_start(period: Period, at: Timestamp, zone: &TimeZone) -> Timestamp {
    let wall = at.as_jiff().to_zoned(zone.clone()).datetime();
    let date = wall.date();
    let start = match period {
        Period::Hour => {
            date.to_datetime(Time::new(wall.hour(), 0, 0, 0).unwrap_or(Time::midnight()))
        }
        Period::Day => date.to_datetime(Time::midnight()),
        Period::Week => {
            let back = i64::from(date.weekday().to_monday_zero_offset());
            date.checked_sub(jiff::Span::new().days(back))
                .unwrap_or(date)
                .to_datetime(Time::midnight())
        }
        Period::Month => Date::new(date.year(), date.month(), 1)
            .unwrap_or(date)
            .to_datetime(Time::midnight()),
        Period::Year => Date::new(date.year(), 1, 1)
            .unwrap_or(date)
            .to_datetime(Time::midnight()),
    };
    start
        .to_zoned(zone.clone())
        .map_or(at, |zoned| Timestamp::from_jiff(zoned.timestamp()))
}

/// A meter: how much a counting reading (an energy total) has gone up since the period began.
/// A reading lower than the last means the device's count started again, and the new reading
/// is all used since.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Meter {
    pub since: Timestamp,
    pub used: f64,
    pub last: Option<f64>,
}

impl Meter {
    pub(crate) fn new(since: Timestamp) -> Self {
        Self {
            since,
            used: 0.0,
            last: None,
        }
    }

    /// Starts again if `now` is in a later period than the one it's counting.
    pub(crate) fn roll(&mut self, period: Period, now: Timestamp, zone: &TimeZone) {
        let start = period_start(period, now, zone);
        if start > self.since {
            self.since = start;
            self.used = 0.0;
        }
    }

    pub(crate) fn add(&mut self, reading: f64) {
        if let Some(last) = self.last {
            self.used += if reading >= last {
                reading - last
            } else {
                reading
            };
        }
        self.last = Some(reading);
    }
}

/// What a history helper says about the time it looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Measure {
    /// Hours it was the value.
    Time,
    /// Times it became the value.
    Count,
    /// Share of the time it was the value, in %.
    Ratio,
}

/// Over `samples` from `start` to `now` (the first standing at `start`), how much the source
/// was what `is_it` says, by `measure`.
pub(crate) fn history<T>(
    samples: &[(Timestamp, T)],
    start: Timestamp,
    now: Timestamp,
    is_it: impl Fn(&T) -> bool,
    measure: Measure,
) -> f64 {
    let mut time = 0.0;
    let mut count = 0_u32;
    let mut was = false;
    for (i, (at, value)) in samples.iter().enumerate() {
        let from = if *at < start { start } else { *at };
        let until = samples.get(i + 1).map_or(now, |(next, _)| *next).min(now);
        let it = is_it(value);
        if it && !was && *at >= start {
            count += 1;
        }
        if it && until > from {
            time += seconds(from, until);
        }
        was = it;
    }
    match measure {
        Measure::Time => time / 3600.0,
        Measure::Count => f64::from(count),
        Measure::Ratio => {
            let span = seconds(start, now);
            if span > 0.0 { time / span * 100.0 } else { 0.0 }
        }
    }
}

/// `value` to `places` decimals, so a worked-out value doesn't show 21.111111.
pub(crate) fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_790_000_000 + second).expect("an instant")
    }

    #[test]
    fn a_threshold_needs_the_reading_past_the_line_by_its_hysteresis() {
        assert!(!threshold(26.2, Some(26.0), None, 0.5, false));
        assert!(threshold(26.6, Some(26.0), None, 0.5, false));
        // On, it stays on until the reading is back past the line by as much.
        assert!(threshold(25.7, Some(26.0), None, 0.5, true));
        assert!(!threshold(25.4, Some(26.0), None, 0.5, true));
        // Between two lines.
        assert!(threshold(20.0, Some(18.0), Some(22.0), 0.0, false));
        assert!(!threshold(23.0, Some(18.0), Some(22.0), 0.0, false));
        assert!(threshold(10.0, None, Some(15.0), 0.0, false));
    }

    #[test]
    fn a_window_starts_with_the_reading_that_held_when_it_began() {
        let samples = [(at(0), 10.0), (at(50), 12.0), (at(90), 15.0)];
        let kept = window(&samples, at(100), 60.0);
        assert_eq!(kept, [(at(40), 10.0), (at(50), 12.0), (at(90), 15.0)]);
        assert_eq!(change(&kept), Some(5.0));
        assert!(trend(&kept, true, 5.0) && !trend(&kept, true, 6.0) && !trend(&kept, false, 1.0));
        // 5 over 50 seconds is 360 an hour.
        assert_eq!(derivative(&kept, 3600.0), Some(360.0));
    }

    #[test]
    fn statistics_count_each_reading_for_as_long_as_it_held() {
        let samples = [(at(0), 10.0), (at(30), 20.0)];
        // 10 for 30 seconds, 20 for 10.
        assert_eq!(statistic(&samples, at(40), Statistic::Mean), Some(12.5));
        assert_eq!(statistic(&samples, at(40), Statistic::Min), Some(10.0));
        assert_eq!(statistic(&samples, at(40), Statistic::Max), Some(20.0));
        assert_eq!(statistic(&[], at(40), Statistic::Max), None);
    }

    #[test]
    fn an_integral_turns_power_into_energy() {
        let mut energy = Integral::default();
        energy.add(at(0), 1000.0, 3600.0);
        energy.add(at(1800), 1000.0, 3600.0);
        energy.add(at(3600), 2000.0, 3600.0);
        // 1000 W for half an hour, then ramping to 2000 over the next: 500 + 750 Wh.
        assert_eq!(energy.total, 1250.0);
    }

    #[test]
    fn a_meter_counts_what_was_used_and_starts_again_each_period() {
        let zone = TimeZone::get("Europe/Brussels").expect("a zone");
        // 2026-10-14T10:00Z is noon in Brussels.
        let noon: Timestamp = "2026-10-14T10:00:00Z".parse().expect("an instant");
        let mut meter = Meter::new(period_start(Period::Day, noon, &zone));
        assert_eq!(
            meter.since,
            "2026-10-13T22:00:00Z"
                .parse::<Timestamp>()
                .expect("an instant")
        );
        meter.add(100.0);
        meter.add(104.5);
        // The device's count started again from nought, then went on.
        meter.add(1.0);
        meter.add(3.0);
        assert_eq!(meter.used, 7.5);
        meter.roll(
            Period::Day,
            "2026-10-14T21:00:00Z".parse().expect("an instant"),
            &zone,
        );
        assert_eq!(meter.used, 7.5, "still the same day in Brussels");
        meter.roll(
            Period::Day,
            "2026-10-14T22:30:00Z".parse().expect("an instant"),
            &zone,
        );
        assert_eq!(meter.used, 0.0, "a new day");
        assert_eq!(
            period_start(Period::Month, noon, &zone),
            "2026-09-30T22:00:00Z"
                .parse::<Timestamp>()
                .expect("an instant")
        );
    }

    #[test]
    fn history_says_how_long_how_often_and_how_much_of_the_time() {
        // Off, on at 10 for 20 seconds, off, on again at 50 until now (60).
        let samples = [
            (at(0), false),
            (at(10), true),
            (at(30), false),
            (at(50), true),
        ];
        let on = |value: &bool| *value;
        assert_eq!(history(&samples, at(0), at(60), on, Measure::Count), 2.0);
        assert_eq!(
            history(&samples, at(0), at(60), on, Measure::Time),
            30.0 / 3600.0
        );
        assert_eq!(history(&samples, at(0), at(60), on, Measure::Ratio), 50.0);
        // From 20: on for 10 of the first stretch, then 10 more; it was already on at 20.
        assert_eq!(history(&samples, at(20), at(60), on, Measure::Count), 1.0);
        assert_eq!(history(&samples, at(20), at(60), on, Measure::Ratio), 50.0);
    }
}
