//! `device_tracker`: where something is, as far as the home cares: at home or not. A phone seen
//! by the router, a tag heard over Bluetooth, a car reporting its position.
//!
//! Its value is whether it's home. One that knows where it is says that too, as fields. Named
//! places other than home wait for Irori to have zones; a tracker in one is simply not home.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Field, Typed};
use crate::InvariantError;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackerCapabilities {
    /// How it knows, when it says. Home Assistant's source types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<TrackerSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackerSource {
    /// A position from satellites, or from a phone that says where it is.
    Gps,
    /// Seen on the home's network.
    Router,
    Bluetooth,
    BluetoothLe,
}

impl TrackerSource {
    /// The source Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackerState {
    /// At home.
    pub home: bool,
    /// Degrees north, -90 to 90.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = -90.0, max = 90.0))]
    pub latitude: Option<f64>,
    /// Degrees east, -180 to 180.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = -180.0, max = 180.0))]
    pub longitude: Option<f64>,
    /// How far off the position may be, in metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub accuracy: Option<f64>,
}

/// How near the home's own point a tracker counts as home, before its accuracy: a driveway or
/// a garage across the street is home, the end of the road isn't.
pub const HOME_RADIUS_METRES: f64 = 100.0;

/// The most of a tracker's own accuracy that widens the home circle. A position a kilometre out
/// says nothing about whether it's home.
pub const MOST_ACCURACY_METRES: f64 = 200.0;

/// The distance between two points on the earth, in metres (haversine, on a round earth: close
/// enough for "is it home").
pub fn distance_metres(from: (f64, f64), to: (f64, f64)) -> f64 {
    const EARTH_RADIUS_METRES: f64 = 6_371_000.0;
    let (lat1, lon1) = (from.0.to_radians(), from.1.to_radians());
    let (lat2, lon2) = (to.0.to_radians(), to.1.to_radians());
    let a = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_METRES * a.sqrt().min(1.0).asin()
}

impl TrackerState {
    /// Whether a tracker that says where it is is at a home whose point is `home`: within
    /// [`HOME_RADIUS_METRES`], widened by its accuracy (up to [`MOST_ACCURACY_METRES`]).
    /// `None` when it doesn't say where it is.
    pub fn is_at(&self, home: (f64, f64)) -> Option<bool> {
        let here = (self.latitude?, self.longitude?);
        let slack = self
            .accuracy
            .unwrap_or(0.0)
            .clamp(0.0, MOST_ACCURACY_METRES);
        Some(distance_metres(here, home) <= HOME_RADIUS_METRES + slack)
    }

    /// A tracker that says only whether it's home.
    pub fn at_home(home: bool) -> Self {
        Self {
            home,
            latitude: None,
            longitude: None,
            accuracy: None,
        }
    }

    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        let within = |what: &str, value: Option<f64>, least: f64, most: f64| match value {
            Some(value) if !(least..=most).contains(&value) => Err(InvariantError(format!(
                "a tracker's {what} is {least} to {most}, not {value}"
            ))),
            _ => Ok(()),
        };
        within("latitude", self.latitude, -90.0, 90.0)?;
        within("longitude", self.longitude, -180.0, 180.0)?;
        match self.accuracy {
            Some(accuracy) if !accuracy.is_finite() || accuracy < 0.0 => Err(InvariantError(
                format!("a tracker's accuracy is metres, nought or more, not {accuracy}"),
            )),
            _ => Ok(()),
        }
    }
}

/// What a tracker has beside whether it's home: where it is, for one that knows.
pub(crate) fn fields(caps: &TrackerCapabilities) -> Vec<Field> {
    match caps.source_type {
        // Seen on the network or heard over Bluetooth: there, or not, and nothing more.
        Some(TrackerSource::Router | TrackerSource::Bluetooth | TrackerSource::BluetoothLe) => {
            Vec::new()
        }
        Some(TrackerSource::Gps) | None => vec![
            Field::number("latitude"),
            Field::number("longitude"),
            Field::number("accuracy"),
        ],
    }
}

pub(crate) fn field(state: &TrackerState, name: &str) -> Option<Typed> {
    match name {
        "latitude" => state.latitude,
        "longitude" => state.longitude,
        "accuracy" => state.accuracy,
        _ => None,
    }
    .map(Typed::Number)
}

pub(crate) fn primary(state: &TrackerState) -> Typed {
    Typed::Bool(state.home)
}

/// Keeps the position it said last.
pub(crate) fn with_primary(previous: Option<&TrackerState>, value: &Typed) -> Option<TrackerState> {
    let Typed::Bool(home) = value else {
        return None;
    };
    Some(TrackerState {
        home: *home,
        ..previous
            .cloned()
            .unwrap_or_else(|| TrackerState::at_home(*home))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whether_it_is_home_is_its_value_and_where_it_is_are_fields() {
        let phone = TrackerState {
            home: false,
            latitude: Some(50.8467),
            longitude: Some(4.3525),
            accuracy: Some(12.0),
        };
        assert_eq!(primary(&phone), Typed::Bool(false));
        assert_eq!(field(&phone, "latitude"), Some(Typed::Number(50.8467)));
        assert_eq!(field(&TrackerState::at_home(true), "latitude"), None);
        let home = with_primary(Some(&phone), &Typed::Bool(true)).expect("valid");
        assert!(home.home && home.longitude == Some(4.3525));
        assert_eq!(with_primary(None, &Typed::Text("home".into())), None);

        let gps = TrackerCapabilities {
            source_type: Some(TrackerSource::Gps),
        };
        assert_eq!(fields(&gps).len(), 3);
        let router = TrackerCapabilities {
            source_type: Some(TrackerSource::Router),
        };
        assert!(fields(&router).is_empty());
        assert_eq!(
            TrackerSource::from_ha("bluetooth_le"),
            Some(TrackerSource::BluetoothLe)
        );
    }

    #[test]
    fn a_position_has_to_be_on_the_globe() {
        let off = TrackerState {
            latitude: Some(91.0),
            ..TrackerState::at_home(false)
        };
        assert!(off.validate().is_err());
        let vague = TrackerState {
            accuracy: Some(-1.0),
            ..TrackerState::at_home(false)
        };
        assert!(vague.validate().is_err());
        assert!(TrackerState::at_home(true).validate().is_ok());
    }
}

#[cfg(test)]
mod home_tests {
    use super::*;

    fn at(latitude: f64, longitude: f64, accuracy: Option<f64>) -> TrackerState {
        TrackerState {
            home: false,
            latitude: Some(latitude),
            longitude: Some(longitude),
            accuracy,
        }
    }

    #[test]
    fn a_position_near_the_home_is_home() {
        let home = (51.0, 4.0);
        // About 55 m north.
        assert_eq!(at(51.0005, 4.0, None).is_at(home), Some(true));
        // About 550 m north.
        assert_eq!(at(51.005, 4.0, None).is_at(home), Some(false));
        // About 220 m north: home only if the fix could be that far off.
        assert_eq!(at(51.002, 4.0, Some(150.0)).is_at(home), Some(true));
        assert_eq!(at(51.002, 4.0, Some(5_000.0)).is_at(home), Some(true));
        assert_eq!(at(51.004, 4.0, Some(5_000.0)).is_at(home), Some(false));
        assert_eq!(TrackerState::at_home(true).is_at(home), None);
        let km = distance_metres((51.0, 4.0), (51.009, 4.0));
        assert!((km - 1_000.0).abs() < 10.0, "{km}");
    }
}
