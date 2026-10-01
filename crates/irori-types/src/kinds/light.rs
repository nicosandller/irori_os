//! `light`: on/off, optionally dimmable and colored.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InvariantError;
use crate::num::{Num, whole};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightCapabilities {
    /// Supports dimming.
    #[serde(default)]
    pub brightness: bool,
    /// Supports color temperature, within this range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_temp_kelvin: Option<ColorTempRange>,
    /// Supports RGB color.
    #[serde(default)]
    pub rgb: bool,
}

/// Supported color temperatures in kelvin, `min` (warmest) to `max` (coolest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColorTempRange {
    #[schemars(range(min = 1000, max = 20000))]
    pub min: u16,
    #[schemars(range(min = 1000, max = 20000))]
    pub max: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawColorTempRange {
    // Wider than `u16` so out-of-range values get the range message, not "expected u16".
    min: Num,
    max: Num,
}

impl<'de> Deserialize<'de> for ColorTempRange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawColorTempRange::deserialize(deserializer)?;
        use serde::de::Error as _;
        let range = ColorTempRange {
            min: whole("color_temp_kelvin.min", raw.min, 1000, 20000).map_err(D::Error::custom)?,
            max: whole("color_temp_kelvin.max", raw.max, 1000, 20000).map_err(D::Error::custom)?,
        };
        range.validate().map_err(serde::de::Error::custom)?;
        Ok(range)
    }
}

impl ColorTempRange {
    /// Deserialization runs this; call it yourself when building a range in code.
    pub fn validate(self) -> Result<(), InvariantError> {
        let valid = |k: u16| (1000..=20000).contains(&k);
        if !valid(self.min) || !valid(self.max) {
            return Err(InvariantError(format!(
                "color temperature range {}-{} K must be within 1000-20000 K",
                self.min, self.max
            )));
        }
        if self.min > self.max {
            return Err(InvariantError(format!(
                "color temperature range has min {} K above max {} K",
                self.min, self.max
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightState {
    pub on: bool,
    /// 1-255, when the light supports dimming. Kept while off: the level it returns to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 255))]
    pub brightness: Option<u8>,
    /// Which color setting is active, when the light supports more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_mode: Option<ColorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1000, max = 20000))]
    pub color_temp_kelvin: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLightState {
    on: bool,
    // Wider than the real types so out-of-range values get a message naming the field.
    // Numbers as written, so the checks below can name the field (see `crate::num`).
    #[serde(default)]
    brightness: Option<Num>,
    #[serde(default)]
    color_mode: Option<ColorMode>,
    #[serde(default)]
    color_temp_kelvin: Option<Num>,
    #[serde(default)]
    rgb: Option<[Num; 3]>,
}

impl<'de> Deserialize<'de> for LightState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = RawLightState::deserialize(deserializer)?;
        if matches!(raw.brightness, Some(Num::Int(0) | Num::UInt(0)))
            || matches!(raw.brightness, Some(Num::Float(b)) if b == 0.0)
        {
            return Err(D::Error::custom(BRIGHTNESS_ZERO));
        }
        let rgb = match raw.rgb {
            Some([r, g, b]) => Some([
                whole("rgb[0]", r, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[1]", g, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[2]", b, 0, 255).map_err(D::Error::custom)?,
            ]),
            None => None,
        };
        let light = LightState {
            on: raw.on,
            brightness: raw
                .brightness
                .map(|n| whole("brightness", n, 1, 255))
                .transpose()
                .map_err(D::Error::custom)?,
            color_mode: raw.color_mode,
            color_temp_kelvin: raw
                .color_temp_kelvin
                .map(|n| whole("color_temp_kelvin", n, 1000, 20000))
                .transpose()
                .map_err(D::Error::custom)?,
            rgb,
        };
        light.validate().map_err(D::Error::custom)?;
        Ok(light)
    }
}

const BRIGHTNESS_ZERO: &str =
    "brightness 0 is invalid; brightness is 1-255 (use `on: false` for off)";

impl LightState {
    /// Deserialization runs this; call it yourself when building a `LightState` in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.brightness == Some(0) {
            return Err(InvariantError(BRIGHTNESS_ZERO.into()));
        }
        if let Some(kelvin) = self.color_temp_kelvin
            && !(1000..=20000).contains(&kelvin)
        {
            return Err(InvariantError(format!(
                "color_temp_kelvin {kelvin} is out of range; it must be 1000-20000"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    ColorTemp,
    Rgb,
}

/// Data for `light.turn_on`. Everything is optional: with no data, the light turns on at its
/// last brightness and color.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::one_color_setting)]
pub struct LightTurnOn {
    /// 1-255. Needs a dimmable light.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 255))]
    pub brightness: Option<u8>,
    /// Needs color temperature support, within the light's range. Not together with `rgb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1000, max = 20000))]
    pub color_temp_kelvin: Option<u16>,
    /// Needs RGB support. Not together with `color_temp_kelvin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLightTurnOn {
    // Numbers as written, so the checks below can name the field (see `crate::num`).
    #[serde(default)]
    brightness: Option<Num>,
    #[serde(default)]
    color_temp_kelvin: Option<Num>,
    #[serde(default)]
    rgb: Option<[Num; 3]>,
}

impl<'de> Deserialize<'de> for LightTurnOn {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = RawLightTurnOn::deserialize(deserializer)?;
        if raw
            .brightness
            .is_some_and(|n| whole::<u8>("brightness", n, 0, 0).is_ok())
        {
            return Err(D::Error::custom(TURN_ON_BRIGHTNESS_ZERO));
        }
        let rgb = match raw.rgb {
            Some([r, g, b]) => Some([
                whole("rgb[0]", r, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[1]", g, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[2]", b, 0, 255).map_err(D::Error::custom)?,
            ]),
            None => None,
        };
        let data = LightTurnOn {
            brightness: raw
                .brightness
                .map(|n| whole("brightness", n, 1, 255))
                .transpose()
                .map_err(D::Error::custom)?,
            color_temp_kelvin: raw
                .color_temp_kelvin
                .map(|n| whole("color_temp_kelvin", n, 1000, 20000))
                .transpose()
                .map_err(D::Error::custom)?,
            rgb,
        };
        data.validate().map_err(D::Error::custom)?;
        Ok(data)
    }
}

const TURN_ON_BRIGHTNESS_ZERO: &str =
    "brightness 0 is invalid; brightness is 1-255 (use `light.turn_off` to turn a light off)";

impl LightTurnOn {
    /// Deserialization runs this; call it yourself when building the data in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.brightness == Some(0) {
            return Err(InvariantError(TURN_ON_BRIGHTNESS_ZERO.into()));
        }
        if let Some(kelvin) = self.color_temp_kelvin
            && !(1000..=20000).contains(&kelvin)
        {
            return Err(InvariantError(format!(
                "color_temp_kelvin {kelvin} is out of range; it must be from 1000 to 20000"
            )));
        }
        if self.color_temp_kelvin.is_some() && self.rgb.is_some() {
            return Err(InvariantError(
                "set `color_temp_kelvin` or `rgb`, not both".into(),
            ));
        }
        Ok(())
    }
}

/// Whether a light can do what `data` asks: `Err` says what it can't, after the entity's name
/// ("isn't dimmable").
pub(crate) fn supports(caps: &LightCapabilities, data: &LightTurnOn) -> Result<(), String> {
    if data.brightness.is_some() && !caps.brightness {
        return Err("isn't dimmable".into());
    }
    if let Some(kelvin) = data.color_temp_kelvin {
        match caps.color_temp_kelvin {
            None => return Err("doesn't support color temperature".into()),
            Some(range) if !(range.min..=range.max).contains(&kelvin) => {
                return Err(format!(
                    "supports color temperatures from {} to {} K, not {kelvin} K",
                    range.min, range.max
                ));
            }
            Some(_) => {}
        }
    }
    if data.rgb.is_some() && !caps.rgb {
        return Err("doesn't support RGB color".into());
    }
    Ok(())
}

/// Whether a reported state is one this light can be in.
pub(crate) fn fits(caps: &LightCapabilities, light: &LightState) -> Result<(), String> {
    match light.color_mode {
        Some(ColorMode::ColorTemp) if caps.color_temp_kelvin.is_none() => {
            return Err("it's in color_temp mode, but doesn't support color temperature".into());
        }
        Some(ColorMode::Rgb) if !caps.rgb => {
            return Err("it's in rgb mode, but doesn't support RGB color".into());
        }
        _ => {}
    }
    let data = LightTurnOn {
        brightness: light.brightness,
        color_temp_kelvin: light.color_temp_kelvin,
        rgb: light.rgb,
    };
    supports(caps, &data).map_err(|what| format!("it {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capabilities, State};

    #[test]
    fn color_temp_range_is_checked_when_deserialized_on_its_own() {
        let inverted = serde_json::from_str::<ColorTempRange>(r#"{"min": 6500, "max": 2200}"#);
        assert!(inverted.is_err_and(|e| e.to_string().contains("min 6500 K above max 2200 K")));

        let via_capabilities = serde_json::from_str::<Capabilities>(
            r#"{"kind": "light", "color_temp_kelvin": {"min": 500, "max": 2200}}"#,
        );
        assert!(via_capabilities.is_err_and(|e| {
            e.to_string().contains(
                "color_temp_kelvin.min 500 is out of range; it must be from 1000 to 20000",
            )
        }));
    }

    #[test]
    fn light_rules_hold_when_deserialized_on_their_own() {
        let zero = serde_json::from_str::<LightState>(r#"{"on": true, "brightness": 0}"#);
        assert!(zero.is_err_and(|e| e.to_string().contains("brightness 0 is invalid")));

        let hot = serde_json::from_str::<State>(
            r#"{"kind": "light", "on": true, "color_temp_kelvin": 20001}"#,
        );
        assert!(hot.is_err_and(|e| {
            e.to_string()
                .contains("color_temp_kelvin 20001 is out of range")
        }));

        let bright = serde_json::from_str::<LightState>(r#"{"on": true, "brightness": 300}"#);
        assert!(bright.is_err_and(|e| {
            e.to_string()
                .contains("brightness 300 is out of range; it must be from 1 to 255")
        }));

        assert!(
            serde_json::from_str::<LightState>(
                r#"{"on": false, "brightness": 1, "color_temp_kelvin": 1000}"#
            )
            .is_ok()
        );
    }
}
