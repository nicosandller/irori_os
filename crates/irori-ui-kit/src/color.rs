//! A light's colour, between the page and the light: the "#rrggbb" a colour well speaks and the
//! `[r, g, b]` a service takes, and a colour temperature in the words people use for it.

/// The color an `<input type="color">` gives, "#rrggbb", as the `[r, g, b]` the data takes.
pub fn rgb_from_hex(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let read = |from: usize| u8::from_str_radix(&hex[from..from + 2], 16).ok();
    Some([read(0)?, read(2)?, read(4)?])
}

/// `[r, g, b]` back into the "#rrggbb" a color input wants.
pub fn hex_from_rgb(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// A colour temperature as it's said: a candle is warm, an overcast sky is cool.
pub fn kelvin_word(kelvin: u16) -> &'static str {
    match kelvin {
        ..2400 => "candlelight",
        2400..3100 => "warm white",
        3100..4200 => "neutral white",
        4200..5600 => "cool white",
        _ => "daylight",
    }
}

/// Roughly the colour of white light at a colour temperature, for a swatch: amber when warm,
/// through white, to a faint blue when cool. Good enough to tell one from another by eye.
pub fn kelvin_rgb(kelvin: u16) -> [u8; 3] {
    // Tanner Helland's fit to the black-body curve, in hundreds of kelvin.
    let t = f64::from(kelvin.clamp(1000, 20000)) / 100.0;
    let red = if t <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
    };
    let green = if t <= 66.0 {
        99.470_802_586_1 * t.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to a byte
    let byte = |value: f64| value.round().clamp(0.0, 255.0) as u8;
    [byte(red), byte(green), byte(blue)]
}
