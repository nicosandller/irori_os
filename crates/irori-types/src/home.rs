//! Where the home is and what time it is there (`home.toml`, `docs/specs/config.md` §3.9).
//!
//! Two answers only a person can give. The time zone is what makes "07:00" a moment; the
//! coordinates are what make "sunset" one. Either may be missing: a home with a zone and no
//! coordinates has time triggers and no sun triggers (`docs/specs/rules.md` K13).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InvariantError;

/// How many decimals a coordinate keeps: about a metre, and a file that diffs cleanly.
const DECIMALS: f64 = 100_000.0;

/// The longest label a location carries.
const LABEL_MAX_LEN: usize = 120;

/// What `home.toml` says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HomeSettings {
    /// The home's IANA time zone, e.g. `Europe/Brussels`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<TimeZoneName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

impl HomeSettings {
    pub fn is_empty(&self) -> bool {
        self.time_zone.is_none() && self.location.is_none()
    }
}

/// Where the home is on the earth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "RawLocation")]
pub struct Location {
    /// Degrees north, −90 to 90.
    pub latitude: f64,
    /// Degrees east, −180 to 180.
    pub longitude: f64,
    /// What it was found as, for a person to read: "Brussels, Belgium".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The shape as it's written, before its ranges are checked. Also what the schema is made
/// from, so the schema carries the same ranges.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Location")]
struct RawLocation {
    /// Degrees north, −90 to 90.
    #[schemars(range(min = -90.0, max = 90.0))]
    latitude: f64,
    /// Degrees east, −180 to 180.
    #[schemars(range(min = -180.0, max = 180.0))]
    longitude: f64,
    /// What it was found as, for a person to read: "Brussels, Belgium".
    #[serde(default)]
    #[schemars(length(max = 120))]
    label: Option<String>,
}

impl TryFrom<RawLocation> for Location {
    type Error = InvariantError;

    fn try_from(raw: RawLocation) -> Result<Self, Self::Error> {
        Location::new(raw.latitude, raw.longitude, raw.label)
    }
}

impl Location {
    /// A checked location, its coordinates rounded to five decimals.
    pub fn new(
        latitude: f64,
        longitude: f64,
        label: Option<String>,
    ) -> Result<Self, InvariantError> {
        if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
            return Err(InvariantError::new(format!(
                "latitude {latitude} is out of range: it goes from -90 to 90"
            )));
        }
        if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
            return Err(InvariantError::new(format!(
                "longitude {longitude} is out of range: it goes from -180 to 180"
            )));
        }
        let label = label
            .map(|label| label.trim().to_owned())
            .filter(|label| !label.is_empty());
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > LABEL_MAX_LEN)
        {
            return Err(InvariantError::new(format!(
                "a location's label is at most {LABEL_MAX_LEN} characters"
            )));
        }
        Ok(Self {
            latitude: round(latitude),
            longitude: round(longitude),
            label,
        })
    }
}

fn round(degrees: f64) -> f64 {
    (degrees * DECIMALS).round() / DECIMALS
}

/// An IANA time zone name, e.g. `Europe/Brussels`.
///
/// Checked here for its shape only: this crate compiles to the browser, which carries no time
/// zone database. Whether the zone exists is checked where there is one (the server, and the
/// automations engine).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TimeZoneName(String);

impl TimeZoneName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TimeZoneName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<TimeZoneName> for String {
    fn from(name: TimeZoneName) -> Self {
        name.0
    }
}

impl TryFrom<String> for TimeZoneName {
    type Error = InvariantError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let fits = !text.is_empty()
            && text.len() <= 64
            && text.split('/').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+'))
            });
        if fits {
            Ok(Self(text))
        } else {
            Err(InvariantError::new(format!(
                "{text:?} isn't a time zone name; one looks like Europe/Brussels"
            )))
        }
    }
}

impl TryFrom<&str> for TimeZoneName {
    type Error = InvariantError;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        Self::try_from(text.to_owned())
    }
}

impl JsonSchema for TimeZoneName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TimeZoneName".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "An IANA time zone name, e.g. Europe/Brussels.",
            "pattern": r"^[A-Za-z0-9_+-]+(/[A-Za-z0-9_+-]+)*$",
            "maxLength": 64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_are_kept_to_about_a_metre() -> Result<(), InvariantError> {
        let place = Location::new(50.846_712_345, 4.352_498_7, Some("  Brussels ".into()))?;
        assert_eq!(place.latitude, 50.84671);
        assert_eq!(place.longitude, 4.3525);
        assert_eq!(place.label.as_deref(), Some("Brussels"));
        Ok(())
    }

    #[test]
    fn a_place_off_the_earth_is_refused_and_says_why() {
        let north = Location::new(91.0, 0.0, None).expect_err("too far north");
        assert!(north.to_string().contains("-90 to 90"), "{north}");
        let east = Location::new(0.0, 181.0, None).expect_err("too far east");
        assert!(east.to_string().contains("-180 to 180"), "{east}");
        assert!(Location::new(f64::NAN, 0.0, None).is_err());
    }

    #[test]
    fn a_zone_is_a_name_in_iana_s_shape() {
        for good in [
            "Europe/Brussels",
            "UTC",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+3",
        ] {
            assert!(TimeZoneName::try_from(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "Europe/",
            "/Brussels",
            "Europe Brussels",
            "../etc/passwd",
        ] {
            assert!(TimeZoneName::try_from(bad).is_err(), "{bad}");
        }
    }
}
