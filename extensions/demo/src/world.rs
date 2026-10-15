//! The demo house's day: what the weather, the rooms and the batteries are doing, `hour` hours
//! in. A day lasts a minute ([`DAY_SECS`]), so everything a flow might wait for comes round
//! often. Each device reads what it needs from here and keeps its own habits (when the door
//! opens, when the fridge runs) in its own module.

/// How long a day lasts here.
pub(crate) const DAY_SECS: u64 = 60;

/// The house and the garden at one moment, as the devices feel them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct World {
    /// Seconds since the demo started, on the readings' clock. Habits slower than a day (the
    /// sofa, the TV's script, the doorbell) count these.
    pub(crate) secs: u64,
    /// Hours into the day, 0 to 24.
    pub(crate) hour: f64,
    /// Indoors, in °C.
    pub(crate) indoors: f64,
    /// Indoors, relative humidity in %.
    pub(crate) indoor_humidity: f64,
    /// In the garden, in °C.
    pub(crate) outdoors: f64,
    /// In the garden, relative humidity in %.
    pub(crate) outdoor_humidity: f64,
    /// Daylight in the garden, in lx: about nothing at night, tens of thousands at noon.
    pub(crate) daylight: f64,
    /// Every battery, in %: full at midnight, empty by the end of the day.
    pub(crate) battery: f64,
}

impl World {
    /// The world `secs` seconds after the demo started.
    pub(crate) fn at(secs: u64) -> Self {
        let hour = (secs % DAY_SECS) as f64 / DAY_SECS as f64 * 24.0;
        Self {
            secs,
            hour,
            indoors: indoors(hour),
            indoor_humidity: indoor_humidity(hour),
            outdoors: outdoors(hour),
            outdoor_humidity: outdoor_humidity(hour),
            daylight: daylight(hour),
            battery: battery(hour),
        }
    }
}

/// Whether `hour` falls in one of `spans`, each from one hour to (not including) another.
pub(crate) fn during(hour: f64, spans: &[(f64, f64)]) -> bool {
    spans.iter().any(|(from, to)| (*from..*to).contains(&hour))
}

/// Something that peaks at `peak` o'clock and bottoms out twelve hours away, from -1 to 1.
pub(crate) fn daily(hour: f64, peak: f64) -> f64 {
    ((hour - peak) / 24.0 * std::f64::consts::TAU).cos()
}

pub(crate) fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

pub(crate) fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

/// Full at midnight, empty by the end of the day, and full again.
fn battery(hour: f64) -> f64 {
    (100.0 * (1.0 - hour / 24.0)).round().clamp(0.0, 100.0)
}

/// About 21 °C, a little warmer through the afternoon and with people in in the evening.
fn indoors(hour: f64) -> f64 {
    let people = if during(hour, &[(18.0, 23.0)]) {
        0.8
    } else {
        0.0
    };
    round1(21.0 + 1.2 * daily(hour, 16.0) + people)
}

/// Highest in the early morning, a shower's worth after getting up.
fn indoor_humidity(hour: f64) -> f64 {
    let shower = if during(hour, &[(7.0, 8.2)]) {
        14.0
    } else {
        0.0
    };
    round1((47.0 + 7.0 * daily(hour, 5.0) + shower).clamp(20.0, 90.0))
}

/// An autumn day: about 3 °C before dawn, 15 °C mid-afternoon.
fn outdoors(hour: f64) -> f64 {
    round1(9.0 + 6.0 * daily(hour, 15.0))
}

/// Damp at night, drier as it warms up.
fn outdoor_humidity(hour: f64) -> f64 {
    round1((76.0 - 16.0 * daily(hour, 15.0)).clamp(20.0, 98.0))
}

/// From dawn at 6 to dusk at 20, up to 25 000 lx at midday; under one lux at night.
fn daylight(hour: f64) -> f64 {
    let sun = ((hour - 6.0) / 14.0 * std::f64::consts::PI).sin().max(0.0);
    (0.4 + 25_000.0 * sun.powf(1.6)).round()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The day's hours, as the readings see them at the default interval.
    pub(crate) fn day() -> impl Iterator<Item = World> {
        (0..DAY_SECS / 2).map(|tick| World::at(tick * 2))
    }

    #[test]
    fn the_weather_and_the_rooms_stay_believable() {
        for world in day() {
            assert!((19.0..=23.5).contains(&world.indoors), "{world:?}");
            assert!((2.0..=16.0).contains(&world.outdoors), "{world:?}");
            assert!((20.0..=98.0).contains(&world.outdoor_humidity), "{world:?}");
            assert!((20.0..=90.0).contains(&world.indoor_humidity), "{world:?}");
        }
        assert!(
            World::at(30).outdoors > World::at(5).outdoors,
            "warmer at noon"
        );
        let lux: Vec<f64> = day().map(|world| world.daylight).collect();
        assert!(lux.iter().any(|lx| *lx < 1.0) && lux.iter().any(|lx| *lx > 20_000.0));
        // Dusk crosses the lux a flow might call "getting dark".
        assert!(lux.iter().any(|lx| (50.0..600.0).contains(lx)), "{lux:?}");
    }

    #[test]
    fn batteries_run_down_over_the_day_and_start_again() {
        assert_eq!(battery(0.0), 100.0);
        assert!(battery(12.0) < 60.0 && battery(12.0) > 40.0);
        assert!(battery(23.9) <= 1.0);
        let charges: Vec<f64> = day().map(|world| world.battery).collect();
        assert!(charges.windows(2).all(|w| w[1] <= w[0]));
    }
}
