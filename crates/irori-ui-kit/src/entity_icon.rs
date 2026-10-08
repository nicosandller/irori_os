//! The icon an entity has by default: what kind of thing it is, at a glance, beside its name.
//!
//! Chosen from what the entity says about itself — its kind, and its device class where that
//! says more (a sensor that measures temperature, a cover that is a garage door). The shell's
//! lists and an extension's pickers draw the same one, so a thing looks the same wherever it's
//! named.
//!
//! Each is what goes inside a 24 × 24 box, drawn as strokes in the colour of the text around it
//! (`fill="none" stroke="currentColor"`, round caps and joins). Written here and nowhere else,
//! so it's safe to hand to the browser as markup.

use irori_types::{
    BinarySensorClass, Capabilities, CoverClass, MediaPlayerClass, SensorClass, SwitchClass,
};

const BULB: &str = concat!(
    r#"<path d="M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0 0 12 3Z"/>"#,
    r#"<path d="M9.5 18.5h5M10.5 21h3"/>"#,
);
const TOGGLE: &str =
    r#"<rect x="3" y="7" width="18" height="10" rx="5"/><circle cx="16" cy="12" r="2.5"/>"#;
const OUTLET: &str =
    r#"<rect x="4" y="4" width="16" height="16" rx="3"/><path d="M9.5 10v3M14.5 10v3"/>"#;
const GAUGE: &str = concat!(
    r#"<path d="M4.6 17a8.5 8.5 0 1 1 14.8 0"/><path d="M12 14l3.5-4.5"/>"#,
    r#"<path d="M12 14h.01"/>"#,
);
const THERMOMETER: &str =
    r#"<path d="M14 14.8V5a2 2 0 0 0-4 0v9.8a4 4 0 1 0 4 0Z"/><path d="M12 9v8"/>"#;
const DROPLET: &str =
    r#"<path d="M12 3.5c3 3.6 5.5 6.6 5.5 10a5.5 5.5 0 0 1-11 0c0-3.4 2.5-6.4 5.5-10Z"/>"#;
const SUN: &str = concat!(
    r#"<circle cx="12" cy="12" r="3.5"/>"#,
    r#"<path d="M12 3v2.5M12 18.5V21M3 12h2.5M18.5 12H21M5.6 5.6l1.8 1.8"#,
    r#"M16.6 16.6l1.8 1.8M5.6 18.4l1.8-1.8M16.6 7.4l1.8-1.8"/>"#,
);
const BATTERY: &str = concat!(
    r#"<rect x="3" y="8" width="16" height="8" rx="1.5"/>"#,
    r#"<path d="M21.5 11v2M6.5 11v2M9.5 11v2"/>"#,
);
const BOLT: &str = r#"<path d="M13 3 5 13.5h6L10 21l8-10.5h-6Z"/>"#;
const CLOUD: &str =
    r#"<path d="M7 18.5a4 4 0 0 1-.6-7.96 5.5 5.5 0 0 1 10.7 1.2A3.4 3.4 0 0 1 16.5 18.5Z"/>"#;
const SIGNAL: &str = concat!(
    r#"<path d="M4 9.5a12 12 0 0 1 16 0M7 13a7.5 7.5 0 0 1 10 0M10 16.5a3 3 0 0 1 4 0"/>"#,
    r#"<path d="M12 19.5h.01"/>"#,
);
const CLOCK: &str = r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>"#;
const WAVES: &str = concat!(
    r#"<path d="M12 12h.01"/>"#,
    r#"<path d="M8.5 8.5a5 5 0 0 0 0 7M15.5 8.5a5 5 0 0 1 0 7"/>"#,
    r#"<path d="M5.6 5.6a9 9 0 0 0 0 12.8M18.4 5.6a9 9 0 0 1 0 12.8"/>"#,
);
const DOOR: &str = concat!(
    r#"<path d="M6 21V4.5A1.5 1.5 0 0 1 7.5 3h9A1.5 1.5 0 0 1 18 4.5V21M3.5 21h17"/>"#,
    r#"<path d="M14.5 12.5h.01"/>"#,
);
const GARAGE: &str = concat!(
    r#"<path d="M3 21V9.5L12 4l9 5.5V21"/>"#,
    r#"<path d="M7 21v-8h10v8M7 15.7h10M7 18.3h10"/>"#,
);
const WINDOW: &str =
    r#"<rect x="4" y="3.5" width="16" height="17" rx="1.5"/><path d="M12 3.5v17M4 12h16"/>"#;
const ALERT: &str = r#"<path d="M12 4 2.8 19.5h18.4Z"/><path d="M12 10v4.5M12 17.2h.01"/>"#;
const DOT: &str = concat!(
    r#"<circle cx="12" cy="12" r="8.5"/>"#,
    r#"<circle cx="12" cy="12" r="3" fill="currentColor" stroke="none"/>"#,
);
const SLIDERS: &str = concat!(
    r#"<path d="M4 8h9M17 8h3M4 16h3M11 16h9"/>"#,
    r#"<circle cx="15" cy="8" r="2"/><circle cx="9" cy="16" r="2"/>"#,
);
const LIST: &str = r#"<path d="M8.5 7h11M8.5 12h11M8.5 17h11M4.5 7h.01M4.5 12h.01M4.5 17h.01"/>"#;
const LETTERS: &str = r#"<path d="M5 7V5h14v2M12 5v14M9.5 19h5"/>"#;
const PRESS: &str = r#"<circle cx="12" cy="12" r="8.5"/><circle cx="12" cy="12" r="4.5"/>"#;
const BELL: &str = r#"<path d="M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15Z"/><path d="M10.5 21h3"/>"#;
const BLIND: &str = r#"<path d="M4 4h16M6 4v13h12V4M6 8.5h12M6 13h12M12 17v3"/>"#;
const PADLOCK: &str = concat!(
    r#"<rect x="5" y="11" width="14" height="9.5" rx="1.5"/>"#,
    r#"<path d="M8 11V8a4 4 0 0 1 8 0v3"/>"#,
);
const FAN: &str = concat!(
    r#"<path d="M12 12h.01"/>"#,
    r#"<path d="M12 10.5C11 7.5 12 4 14.5 3.5c2 .9 1.5 4.5-1 7.200"/>"#,
    r#"<path d="M13.5 12c3-1 6.500 0 7 2.5-.9 2-4.500 1.500-7.200-1"/>"#,
    r#"<path d="M12 13.5c1 3 0 6.500-2.500 7-2-.9-1.500-4.500 1-7.200"/>"#,
    r#"<path d="M10.500 12c-3 1-6.500 0-7-2.500.9-2 4.500-1.500 7.200 1"/>"#,
);
const VALVE: &str = r#"<path d="M3 14h5l1.5-2h5l1.5 2h5v5H3Z"/><path d="M12 12V7M8.5 7h7"/>"#;
const SIREN: &str = concat!(
    r#"<path d="M7 18v-5a5 5 0 0 1 10 0v5M4.5 18h15v2.5h-15Z"/>"#,
    r#"<path d="M12 3v2M4.5 6.500 6 8M19.500 6.500 18 8"/>"#,
);
const DIAL: &str = r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 12l3-4M8.500 16.500h7"/>"#;
const TANK: &str = concat!(
    r#"<rect x="6" y="3" width="12" height="16" rx="3"/><path d="M9 21.500V19M15 21.500V19"/>"#,
    r#"<path d="M12 8c1.200 1.500 2 2.500 2 3.700a2 2 0 0 1-4 0c0-1.200.8-2.200 2-3.700Z"/>"#,
);
const SCREEN: &str =
    r#"<rect x="3" y="5" width="18" height="12" rx="1.5"/><path d="M8 20.500h8"/>"#;
const SPEAKER: &str = concat!(
    r#"<rect x="6" y="3" width="12" height="18" rx="2"/><circle cx="12" cy="14" r="3"/>"#,
    r#"<path d="M12 7.500h.01"/>"#,
);
const PLAYER: &str = concat!(
    r#"<rect x="3" y="5" width="18" height="12" rx="1.5"/><path d="M8 20.500h8"/>"#,
    r#"<path d="M10.500 8.700v4.600l4-2.300Z"/>"#,
);

/// What to draw inside the icon's 24 × 24 box for an entity that has these capabilities.
pub fn drawing(capabilities: &Capabilities) -> &'static str {
    match capabilities {
        Capabilities::Light(_) => BULB,
        Capabilities::Switch(switch) => match switch.device_class {
            Some(SwitchClass::Outlet) => OUTLET,
            _ => TOGGLE,
        },
        Capabilities::Sensor(sensor) => measured(sensor.device_class),
        Capabilities::Number(number) => match number.device_class {
            Some(class) => measured(Some(class)),
            None => SLIDERS,
        },
        Capabilities::BinarySensor(sensor) => noticed(sensor.device_class),
        Capabilities::Select(_) => LIST,
        Capabilities::Text(_) => LETTERS,
        Capabilities::Button(_) => PRESS,
        Capabilities::Event(_) => BELL,
        Capabilities::Cover(cover) => match cover.device_class {
            Some(CoverClass::Door | CoverClass::Gate) => DOOR,
            Some(CoverClass::Garage) => GARAGE,
            Some(CoverClass::Window) => WINDOW,
            _ => BLIND,
        },
        Capabilities::Lock(_) => PADLOCK,
        Capabilities::Fan(_) => FAN,
        Capabilities::Valve(_) => VALVE,
        Capabilities::Siren(_) => SIREN,
        Capabilities::Climate(_) => DIAL,
        Capabilities::WaterHeater(_) => TANK,
        Capabilities::Humidifier(_) => DROPLET,
        Capabilities::MediaPlayer(player) => match player.device_class {
            Some(MediaPlayerClass::Tv) => SCREEN,
            Some(MediaPlayerClass::Speaker | MediaPlayerClass::Receiver) => SPEAKER,
            None => PLAYER,
        },
    }
}

/// A sensor, by what it measures.
fn measured(class: Option<SensorClass>) -> &'static str {
    use SensorClass::*;
    match class {
        Some(Temperature | TemperatureDelta) => THERMOMETER,
        Some(
            Humidity
            | AbsoluteHumidity
            | Moisture
            | Water
            | Precipitation
            | PrecipitationIntensity
            | Volume
            | VolumeFlowRate
            | VolumeStorage,
        ) => DROPLET,
        Some(Illuminance | Irradiance) => SUN,
        Some(Battery) => BATTERY,
        Some(
            Power | ApparentPower | ReactivePower | PowerFactor | Energy | EnergyDistance
            | EnergyStorage | ReactiveEnergy | Voltage | Current | Frequency,
        ) => BOLT,
        Some(
            Aqi
            | Co2
            | CarbonMonoxide
            | Gas
            | NitrogenDioxide
            | NitrogenMonoxide
            | NitrousOxide
            | Ozone
            | Pm1
            | Pm10
            | Pm25
            | Pm4
            | Radon
            | SulphurDioxide
            | VolatileOrganicCompounds
            | VolatileOrganicCompoundsParts
            | WindDirection
            | WindSpeed,
        ) => CLOUD,
        Some(SignalStrength | DataRate | DataSize) => SIGNAL,
        Some(Timestamp | Date | Duration | Uptime) => CLOCK,
        _ => GAUGE,
    }
}

/// An on/off sensor, by what it notices.
fn noticed(class: Option<BinarySensorClass>) -> &'static str {
    use BinarySensorClass::*;
    match class {
        Some(Motion | Moving | Occupancy | Presence | Vibration | Sound) => WAVES,
        Some(Door | Opening) => DOOR,
        Some(GarageDoor) => GARAGE,
        Some(Window) => WINDOW,
        Some(Battery | BatteryCharging) => BATTERY,
        Some(Plug) => OUTLET,
        Some(Power) => BOLT,
        Some(Connectivity) => SIGNAL,
        Some(Lock) => PADLOCK,
        Some(Light) => SUN,
        Some(Moisture) => DROPLET,
        Some(Cold | Heat) => THERMOMETER,
        Some(CarbonMonoxide | Gas | Smoke) => CLOUD,
        Some(GlassBreak | Problem | Safety | Tamper) => ALERT,
        _ => DOT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every drawing is strokes and shapes: nothing in it can run or load.
    #[test]
    fn a_drawing_is_only_shapes() {
        for drawing in [
            BULB,
            TOGGLE,
            OUTLET,
            GAUGE,
            THERMOMETER,
            DROPLET,
            SUN,
            BATTERY,
            BOLT,
            CLOUD,
            SIGNAL,
            CLOCK,
            WAVES,
            DOOR,
            GARAGE,
            WINDOW,
            ALERT,
            DOT,
            SLIDERS,
            LIST,
            LETTERS,
            PRESS,
            BELL,
            BLIND,
            PADLOCK,
            FAN,
            VALVE,
            SIREN,
            DIAL,
            TANK,
            SCREEN,
            SPEAKER,
            PLAYER,
        ] {
            for tag in drawing.split('<').skip(1) {
                let name = tag.split([' ', '/', '>']).next().unwrap_or_default();
                assert!(
                    matches!(name, "path" | "rect" | "circle" | ""),
                    "{name} in {drawing}"
                );
            }
            assert!(
                !drawing.contains("href") && !drawing.contains(" on"),
                "{drawing}"
            );
        }
    }
}
