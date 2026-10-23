//! A light: On/Off, with Level Control to dim it and Color Control for its colour temperature
//! and colour, as its features say.

use serde_json::json;

use super::Reading;
use crate::model::{Command, Node, cluster, device_type};
use irori_types::{ColorMode, ColorTempRange, LightCapabilities, LightState, Service, State};

/// What a light can do: dim, unless it says it's only an On/Off Light; colour temperature and
/// colour, as its Color Control features say.
pub(super) fn reading(node: &Node, endpoint: u16) -> Reading {
    let brightness = node.has(endpoint, cluster::LEVEL_CONTROL, 0)
        && node.device_types(endpoint) != [device_type::ON_OFF_LIGHT];
    let color = node.has_cluster(endpoint, cluster::COLOR_CONTROL);
    let features = node.features(endpoint, cluster::COLOR_CONTROL);
    let rgb = color && features & (color_feature::HUE_SATURATION | color_feature::XY) != 0;
    let color_temp = color && features & color_feature::COLOR_TEMPERATURE != 0;
    Reading::Light(LightCapabilities {
        brightness,
        color_temp_kelvin: color_temp.then(|| color_temp_range(node, endpoint)),
        rgb,
    })
}

/// Color Control's feature bits.
mod color_feature {
    pub const HUE_SATURATION: u64 = 1;
    pub const XY: u64 = 8;
    pub const COLOR_TEMPERATURE: u64 = 16;
}

/// The light's own colour temperature limits, or Home Assistant's 153-500 mireds when it gives
/// none.
fn color_temp_range(node: &Node, endpoint: u16) -> ColorTempRange {
    let mireds = |attribute, fallback| {
        node.number(endpoint, cluster::COLOR_CONTROL, attribute)
            .filter(|mireds| *mireds > 0.0)
            .unwrap_or(fallback)
    };
    // The fewest mireds are the coolest light, the most kelvin.
    let warmest = kelvin(mireds(0x400C, 500.0));
    let coolest = kelvin(mireds(0x400B, 153.0));
    ColorTempRange {
        min: warmest.min(coolest),
        max: warmest.max(coolest),
    }
}

/// Mireds to kelvin, within Irori's 1000-20000 K.
pub(super) fn kelvin(mireds: f64) -> u16 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1000..=20000 first"
    )]
    let kelvin = (1_000_000.0 / mireds).round().clamp(1000.0, 20000.0) as u16;
    kelvin
}

/// A light's state, trimmed to what it was described as doing: the core refuses a reading the
/// entity didn't declare.
pub(super) fn state(node: &Node, endpoint: u16, known: &LightCapabilities) -> Option<State> {
    let on = node.value(endpoint, cluster::ON_OFF, 0)?.as_bool()?;
    let brightness = known
        .brightness
        .then(|| node.number(endpoint, cluster::LEVEL_CONTROL, 0))
        .flatten()
        .and_then(level);
    let color = |attribute| node.number(endpoint, cluster::COLOR_CONTROL, attribute);
    // 0 hue and saturation, 1 XY, 2 colour temperature.
    let mode = color(8);
    let showing_ct = known.color_temp_kelvin.is_some() && (mode == Some(2.0) || !known.rgb);
    let showing_rgb = known.rgb && !showing_ct;
    let color_temp_kelvin = known
        .color_temp_kelvin
        .filter(|_| showing_ct)
        .and_then(|range| {
            let mireds = color(7).filter(|mireds| *mireds > 0.0)?;
            Some(kelvin(mireds).clamp(range.min, range.max))
        });
    let rgb = showing_rgb
        .then(|| {
            if mode == Some(1.0) {
                xy_to_rgb(color(3)? / 65535.0, color(4)? / 65535.0)
            } else {
                Some(hs_to_rgb(color(0)? / 254.0 * 360.0, color(1)? / 254.0))
            }
        })
        .flatten();
    let both = known.rgb && known.color_temp_kelvin.is_some();
    let color_mode = match (both, rgb.is_some(), color_temp_kelvin.is_some()) {
        (true, true, _) => Some(ColorMode::Rgb),
        (true, false, true) => Some(ColorMode::ColorTemp),
        _ => None,
    };
    Some(State::Light(LightState {
        on,
        brightness,
        color_mode,
        color_temp_kelvin,
        rgb,
    }))
}

/// Level Control's 1-254 to Irori's 1-255. Level 0 isn't a brightness.
fn level(value: f64) -> Option<u8> {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1..=255 first"
    )]
    (value >= 1.0).then(|| (value * 255.0 / 254.0).round().clamp(1.0, 255.0) as u8)
}

/// Hue in degrees and saturation 0-1, at full value.
fn hs_to_rgb(hue: f64, saturation: f64) -> [u8; 3] {
    let saturation = saturation.clamp(0.0, 1.0);
    let sector = (hue.rem_euclid(360.0)) / 60.0;
    let chroma = saturation;
    let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u8 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = 1.0 - chroma;
    [byte(r + m), byte(g + m), byte(b + m)]
}

/// CIE xy at full brightness to sRGB, as Home Assistant's `color_xy_to_RGB` does it.
fn xy_to_rgb(x: f64, y: f64) -> Option<[u8; 3]> {
    if y <= 0.0 {
        return None;
    }
    let (big_x, big_y, big_z) = (x / y, 1.0, (1.0 - x - y) / y);
    let linear = [
        big_x * 1.656_492 - big_y * 0.354_851 - big_z * 0.255_038,
        -big_x * 0.707_196 + big_y * 1.655_397 + big_z * 0.036_152,
        big_x * 0.051_713 - big_y * 0.121_364 + big_z * 1.011_530,
    ];
    let gamma = linear.map(|c| {
        let c = c.max(0.0);
        if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    });
    let max = gamma.iter().copied().fold(0.0_f64, f64::max);
    let scale = if max > 1.0 { max } else { 1.0 };
    Some(gamma.map(|c| byte(c / scale)))
}

fn byte(fraction: f64) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255 first"
    )]
    let byte = (fraction * 255.0).round().clamp(0.0, 255.0) as u8;
    byte
}

/// Color Control's options: act even while the light is off, so a colour set with the light off
/// is the colour it comes on in.
const EXECUTE_IF_OFF: u8 = 1;

/// What `light.turn_on` and `light.turn_off` send: the colour first, then the level, which also
/// turns it on (`moveToLevelWithOnOff`), or a plain `on`.
pub(super) fn command(
    node: &Node,
    endpoint: u16,
    known: &LightCapabilities,
    service: &Service,
) -> Result<Vec<Command>, String> {
    let turn_on = match service {
        Service::LightTurnOff => {
            return Ok(vec![Command::invoke(
                endpoint,
                cluster::ON_OFF,
                "off",
                json!({}),
            )]);
        }
        Service::LightTurnOn(turn_on) => turn_on,
        other => return Err(format!("a light doesn't take {}", other.name())),
    };
    let colour = |name, mut payload: serde_json::Value| {
        payload["transitionTime"] = json!(0);
        payload["optionsMask"] = json!(EXECUTE_IF_OFF);
        payload["optionsOverride"] = json!(EXECUTE_IF_OFF);
        Command::invoke(endpoint, cluster::COLOR_CONTROL, name, payload)
    };
    let mut commands = Vec::new();
    if let Some(kelvin) = turn_on
        .color_temp_kelvin
        .filter(|_| known.color_temp_kelvin.is_some())
    {
        commands.push(colour(
            "moveToColorTemperature",
            json!({"colorTemperatureMireds": mireds(kelvin)}),
        ));
    }
    if let Some(rgb) = turn_on.rgb.filter(|_| known.rgb) {
        let features = node.features(endpoint, cluster::COLOR_CONTROL);
        if features & color_feature::XY != 0 {
            let (x, y) = rgb_to_xy(rgb);
            commands.push(colour(
                "moveToColor",
                json!({"colorX": fraction(x), "colorY": fraction(y)}),
            ));
        } else {
            let (hue, saturation) = rgb_to_hs(rgb);
            commands.push(colour(
                "moveToHueAndSaturation",
                json!({
                    "hue": scale(hue / 360.0, 254.0),
                    "saturation": scale(saturation, 254.0),
                }),
            ));
        }
    }
    match turn_on.brightness.filter(|_| known.brightness) {
        Some(brightness) => commands.push(Command::invoke(
            endpoint,
            cluster::LEVEL_CONTROL,
            "moveToLevelWithOnOff",
            json!({
                "level": matter_level(brightness),
                "transitionTime": 0,
                "optionsMask": 0,
                "optionsOverride": 0,
            }),
        )),
        None => commands.push(Command::invoke(endpoint, cluster::ON_OFF, "on", json!({}))),
    }
    Ok(commands)
}

/// Irori's 1-255 to Level Control's 1-254.
fn matter_level(brightness: u8) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1..=254 first"
    )]
    let level = (f64::from(brightness) * 254.0 / 255.0)
        .round()
        .clamp(1.0, 254.0) as u8;
    level
}

fn mireds(kelvin: u16) -> u16 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to Color Control's range first"
    )]
    let mireds = (1_000_000.0 / f64::from(kelvin.max(1)))
        .round()
        .clamp(1.0, 65279.0) as u16;
    mireds
}

/// A 0-1 fraction of `top`, rounded.
fn scale(fraction: f64, top: f64) -> u16 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=top first"
    )]
    let value = (fraction * top).round().clamp(0.0, top) as u16;
    value
}

/// A CIE coordinate as Color Control's 0-65279.
fn fraction(coordinate: f64) -> u16 {
    scale(coordinate, 65279.0)
}

/// Hue in degrees and saturation 0-1 of an RGB colour, its brightness left aside.
fn rgb_to_hs([r, g, b]: [u8; 3]) -> (f64, f64) {
    let [r, g, b] = [r, g, b].map(|c| f64::from(c) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if max <= 0.0 || delta <= 0.0 {
        return (0.0, 0.0);
    }
    let hue = if (max - r).abs() < f64::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f64::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    (hue, delta / max)
}

/// sRGB to CIE xy, as Home Assistant's `color_RGB_to_xy` does it.
fn rgb_to_xy(rgb: [u8; 3]) -> (f64, f64) {
    let [r, g, b] = rgb.map(|c| {
        let c = f64::from(c) / 255.0;
        if c > 0.04045 {
            ((c + 0.055) / 1.055).powf(2.4)
        } else {
            c / 12.92
        }
    });
    let x = r * 0.664_511 + g * 0.154_324 + b * 0.162_028;
    let y = r * 0.283_881 + g * 0.668_433 + b * 0.047_685;
    let z = r * 0.000_088 + g * 0.072_310 + b * 0.986_039;
    let sum = x + y + z;
    if sum <= 0.0 {
        // Black has no colour; D65 white's point stands in.
        return (0.3127, 0.3290);
    }
    (x / sum, y / sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_cross_to_rgb() {
        assert_eq!(hs_to_rgb(0.0, 1.0), [255, 0, 0]);
        assert_eq!(hs_to_rgb(120.0, 1.0), [0, 255, 0]);
        assert_eq!(hs_to_rgb(0.0, 0.0), [255, 255, 255]);
        // D65 white comes out near white; a red point comes out red.
        let white = xy_to_rgb(0.3127, 0.3290).expect("a colour");
        assert!(white.iter().all(|c| *c >= 240), "{white:?}");
        let red = xy_to_rgb(0.68, 0.31).expect("a colour");
        assert!(red[0] == 255 && red[1] < 60 && red[2] < 60, "{red:?}");
        assert_eq!(xy_to_rgb(0.3, 0.0), None);
        assert_eq!(level(254.0), Some(255));
        assert_eq!(level(1.0), Some(1));
        assert_eq!(level(0.0), None);
    }

    #[test]
    fn colours_cross_back_to_matter() {
        assert_eq!(rgb_to_hs([255, 0, 0]), (0.0, 1.0));
        let (hue, saturation) = rgb_to_hs([0, 0, 255]);
        assert!((hue - 240.0).abs() < 0.01 && (saturation - 1.0).abs() < 0.01);
        assert_eq!(rgb_to_hs([255, 255, 255]), (0.0, 0.0));
        // Red there and back lands on red.
        let (x, y) = rgb_to_xy([255, 0, 0]);
        let back = xy_to_rgb(x, y).expect("a colour");
        assert!(back[0] == 255 && back[1] < 30 && back[2] < 30, "{back:?}");
        assert_eq!((matter_level(255), matter_level(1)), (254, 1));
        assert_eq!(mireds(2700), 370);
    }
}
