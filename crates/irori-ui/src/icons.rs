//! The page's own small icons: one stroke weight, one size, the colour of the text around them.
//!
//! Not for extensions' icons, which are theirs and are only ever loaded as images
//! (`devices::icon`).

use leptos::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// The handle on something that can be dragged.
    Grip,
    Add,
    Edit,
    Remove,
    // The sections of Settings.
    System,
    Appearance,
    Assistant,
    Places,
    /// Where the home is: a pin.
    Place,
    Users,
    /// A program's access token.
    Key,
    Logs,
    /// How long history is kept: a clock.
    History,
    // What the machine is short of, or isn't.
    Memory,
    Disk,
    Processor,
    Temperature,
    // Saving and putting away what was being typed.
    Check,
    Close,
    Gear,
    // A player's buttons.
    Play,
    Pause,
    Stop,
    Previous,
    Next,
    Power,
    Volume,
    Muted,
    // The kinds of device on the Floorplan.
    Bulb,
    Tv,
    Speaker,
    Plug,
    /// Someone a radar can see.
    Person,
    Motion,
    Button,
    Air,
    Lock,
    Unlocked,
    Bell,
    /// A video doorbell: a bell push under a lens.
    Doorbell,
    Camera,
    /// Something that shakes: a vibration sensor.
    Vibration,
    /// A standing lamp.
    Lamp,
    CeilingLight,
    LightStrip,
    /// A wall switch.
    Switch,
    Fan,
    /// Water where it shouldn't be.
    Leak,
    Thermostat,
    /// Something that has stopped answering: a signal, struck through.
    NoSignal,
    /// Anything that isn't one of the others.
    Dot,
    /// One of the plan's glyphs that has no icon of its own elsewhere on the page.
    Of(irori_types::Glyph),
    Cast,
}

/// The glyph a marker was given in place of its own, by the name the plan writes.
pub fn of_glyph(glyph: irori_types::Glyph) -> Icon {
    use irori_types::Glyph;
    match glyph {
        Glyph::Bulb => Icon::Bulb,
        Glyph::Lamp => Icon::Lamp,
        Glyph::CeilingLight => Icon::CeilingLight,
        Glyph::LightStrip => Icon::LightStrip,
        Glyph::Plug => Icon::Plug,
        Glyph::Switch => Icon::Switch,
        Glyph::Fan => Icon::Fan,
        Glyph::Tv => Icon::Tv,
        Glyph::Speaker => Icon::Speaker,
        Glyph::Camera => Icon::Camera,
        Glyph::Doorbell => Icon::Doorbell,
        Glyph::Bell => Icon::Bell,
        Glyph::Lock => Icon::Lock,
        Glyph::Motion => Icon::Motion,
        Glyph::Person => Icon::Person,
        Glyph::Vibration => Icon::Vibration,
        Glyph::Thermostat => Icon::Thermostat,
        Glyph::Temperature => Icon::Temperature,
        Glyph::Air => Icon::Air,
        Glyph::Leak => Icon::Leak,
        Glyph::Button => Icon::Button,
        Glyph::Dot => Icon::Dot,
        other => Icon::Of(other),
    }
}

/// Other words a glyph is found by in the picker's search, beside its label.
pub fn also_called(glyph: irori_types::Glyph) -> &'static str {
    use irori_types::Glyph;
    match glyph {
        Glyph::Bulb => "light lamp",
        Glyph::Lamp => "light floor lamp table lamp",
        Glyph::CeilingLight => "light pendant ceiling",
        Glyph::LightStrip => "light led strip tape",
        Glyph::Spotlight => "light spot downlight",
        Glyph::Chandelier => "light pendant",
        Glyph::OutdoorLight => "light garden porch wall",
        Glyph::Plug => "power socket smart plug",
        Glyph::Switch => "light switch wall relay",
        Glyph::Outlet => "power socket wall",
        Glyph::Fan => "ceiling fan air",
        Glyph::Tv => "television screen media",
        Glyph::Speaker => "music audio media sonos",
        Glyph::Remote => "control ir",
        Glyph::Computer => "pc laptop desktop monitor",
        Glyph::Router => "wifi network hub bridge",
        Glyph::GameConsole => "playstation xbox games",
        Glyph::Camera => "cctv security video",
        Glyph::Doorbell => "door bell video ring",
        Glyph::Bell => "chime",
        Glyph::Lock => "door security",
        Glyph::Alarm => "security shield panel",
        Glyph::Siren => "alarm security",
        Glyph::Motion => "pir presence sensor",
        Glyph::Person => "presence mmwave radar occupancy",
        Glyph::Vibration => "shake sensor",
        Glyph::Door => "contact opening",
        Glyph::Window => "contact opening",
        Glyph::GarageDoor => "garage cover opener",
        Glyph::Gate => "fence driveway",
        Glyph::Blind => "cover shade shutter roller",
        Glyph::Curtain => "cover drapes",
        Glyph::Thermostat => "heating climate hvac",
        Glyph::Temperature => "thermometer climate",
        Glyph::Humidity => "humidifier dehumidifier water drop",
        Glyph::Radiator => "heater heating",
        Glyph::AirConditioner => "ac cooling climate",
        Glyph::Air => "co2 quality purifier",
        Glyph::Smoke => "fire detector co alarm",
        Glyph::Leak => "water flood",
        Glyph::Valve => "water shutoff pipe",
        Glyph::WaterHeater => "boiler tank hot water",
        Glyph::Sprinkler => "irrigation garden water",
        Glyph::Plant => "garden soil pot",
        Glyph::Washer => "laundry washing dryer",
        Glyph::Dishwasher => "kitchen dishes",
        Glyph::Fridge => "kitchen freezer refrigerator",
        Glyph::Oven => "kitchen cooker stove",
        Glyph::Kettle => "kitchen tea",
        Glyph::Coffee => "kitchen espresso",
        Glyph::Vacuum => "robot cleaner roomba",
        Glyph::Mower => "lawn garden robot",
        Glyph::Battery => "power",
        Glyph::Solar => "panel pv energy",
        Glyph::Charger => "ev car electric",
        Glyph::Energy => "power meter electricity bolt",
        Glyph::Sun => "light level lux illuminance",
        Glyph::Bed => "bedroom sleep",
        Glyph::Pet => "dog cat feeder",
        Glyph::Button => "press remote",
        Glyph::Dot => "other",
    }
}

/// The drawings of the glyphs that are only ever on the plan.
fn glyph_drawing(glyph: irori_types::Glyph) -> &'static str {
    use irori_types::Glyph;
    use irori_ui_kit::entity_icon as kit;
    match glyph {
        Glyph::Spotlight => {
            r#"<path d="M8 3.5h8l-1.5 6h-5Z"/><path d="M9.5 9.5 6 20.5h12l-3.5-11"/>"#
        }
        Glyph::Chandelier => concat!(
            r#"<path d="M12 2.5v5M4.5 11.5c0 2.5 2 3.5 4 3.5h7c2 0 4-1 4-3.5M12 7.5v7.5"/>"#,
            r#"<path d="M4.5 11.5v-2M19.5 11.5v-2M12 15v3"/><circle cx="12" cy="19.5" r="1.5"/>"#,
        ),
        Glyph::OutdoorLight => concat!(
            r#"<path d="M5 3.5v17"/><path d="M5 6.5h4l1.5 2v6h-7"/>"#,
            r#"<path d="M14 10h2M14.5 6.5l1.5-1M14.5 13.5l1.5 1"/>"#,
        ),
        Glyph::Outlet => kit::OUTLET,
        Glyph::Remote => concat!(
            r#"<rect x="8" y="2.5" width="8" height="19" rx="2.5"/>"#,
            r#"<circle cx="12" cy="7" r="1.5"/><path d="M10.5 12h.01M13.5 12h.01M10.5 15h.01M13.5 15h.01"/>"#,
        ),
        Glyph::Computer => {
            r#"<rect x="3" y="4" width="18" height="12" rx="1.5"/><path d="M9 20.5h6M12 16v4.5"/>"#
        }
        Glyph::Router => concat!(
            r#"<rect x="3" y="13" width="18" height="6" rx="1.5"/><path d="M7 16h.01M10 16h.01"/>"#,
            r#"<path d="M15 13V8M8.5 8.5a5 5 0 0 1 7 0M6 6a8.5 8.5 0 0 1 12 0"/>"#,
        ),
        Glyph::GameConsole => concat!(
            r#"<path d="M7 7.5h10a4 4 0 0 1 4 4v3a3 3 0 0 1-5.4 1.8L14.5 15h-5l-1.1 1.3A3 3 0 0 1 3 14.5v-3a4 4 0 0 1 4-4Z"/>"#,
            r#"<path d="M7.5 10v3M6 11.5h3M15.5 11h.01M17.5 12.5h.01"/>"#,
        ),
        Glyph::Alarm => kit::SHIELD,
        Glyph::Siren => kit::SIREN,
        Glyph::Door => kit::DOOR,
        Glyph::Window => kit::WINDOW,
        Glyph::GarageDoor => kit::GARAGE,
        Glyph::Gate => concat!(
            r#"<path d="M3 20.5V5M21 20.5V5M3 8h18M3 17h18"/>"#,
            r#"<path d="M7.5 8v9M12 8v9M16.5 8v9"/>"#,
        ),
        Glyph::Blind => kit::BLIND,
        Glyph::Curtain => concat!(
            r#"<path d="M3 3.5h18"/><path d="M5 3.5c0 6 1 12 4 17H5M19 3.5c0 6-1 12-4 17h4"/>"#,
            r#"<path d="M5 20.5V3.5M19 20.5V3.5"/>"#,
        ),
        Glyph::Humidity => kit::DROPLET,
        Glyph::Radiator => concat!(
            r#"<path d="M4 20.5V18M20 20.5V18"/><rect x="3" y="5" width="18" height="13" rx="1.5"/>"#,
            r#"<path d="M7.5 5v13M12 5v13M16.5 5v13"/>"#,
        ),
        Glyph::AirConditioner => concat!(
            r#"<rect x="2.5" y="4" width="19" height="8" rx="1.5"/><path d="M6 9.5h12"/>"#,
            r#"<path d="M8 15.5c0 2-1 3-2 4M12 15.5v5M16 15.5c0 2 1 3 2 4"/>"#,
        ),
        Glyph::Smoke => concat!(
            r#"<circle cx="12" cy="12" r="8.5"/><circle cx="12" cy="12" r="3.5"/>"#,
            r#"<path d="M12 3.5v2M12 18.5v2"/>"#,
        ),
        Glyph::Valve => kit::VALVE,
        Glyph::WaterHeater => kit::TANK,
        Glyph::Sprinkler => concat!(
            r#"<path d="M12 20.5V12M8.5 20.5h7"/><path d="M12 12 6 6M12 12l6-6M12 12V4"/>"#,
            r#"<path d="M4.5 9h.01M19.5 9h.01M8 4h.01M16 4h.01"/>"#,
        ),
        Glyph::Plant => concat!(
            r#"<path d="M7 14.5h10l-1.5 6h-7Z"/><path d="M12 14.5V8"/>"#,
            r#"<path d="M12 10c0-3.5-2.5-5.5-6-5.5 0 3.5 2.5 5.5 6 5.5ZM12 8c0-3 2-4.5 5-4.5 0 3-2 4.5-5 4.5Z"/>"#,
        ),
        Glyph::Washer => concat!(
            r#"<rect x="4" y="2.5" width="16" height="19" rx="2"/><path d="M4 7h16M7.5 4.8h.01"/>"#,
            r#"<circle cx="12" cy="14" r="4.5"/>"#,
        ),
        Glyph::Dishwasher => concat!(
            r#"<rect x="4" y="2.5" width="16" height="19" rx="2"/><path d="M4 7h16M7.5 4.8h.01M11 4.8h6"/>"#,
            r#"<path d="M7.5 11h9M7.5 14.5h9M7.5 18h9"/>"#,
        ),
        Glyph::Fridge => concat!(
            r#"<rect x="5" y="2.5" width="14" height="19" rx="2"/><path d="M5 9.5h14"/>"#,
            r#"<path d="M8.5 5.5v2M8.5 12.5v3"/>"#,
        ),
        Glyph::Oven => concat!(
            r#"<rect x="3.5" y="3" width="17" height="18" rx="2"/><path d="M3.5 8h17"/>"#,
            r#"<rect x="7" y="11" width="10" height="7" rx="1"/><path d="M7 5.5h.01M10 5.5h.01"/>"#,
        ),
        Glyph::Kettle => concat!(
            r#"<path d="M6 20.5h12l-1.5-11h-9Z"/><path d="M7.5 9.5c0-3 2-5 4.5-5s4.5 2 4.5 5"/>"#,
            r#"<path d="M16.5 12h1.5a2 2 0 0 1 0 4h-1"/>"#,
        ),
        Glyph::Coffee => concat!(
            r#"<path d="M5 9.5h11v5a5 5 0 0 1-5 5h-1a5 5 0 0 1-5-5Z"/>"#,
            r#"<path d="M16 11h1.5a2.5 2.5 0 0 1 0 5H16M8 3.5v3M11 3.5v3M14 3.5v3"/>"#,
        ),
        Glyph::Vacuum => kit::ROBOT,
        Glyph::Mower => kit::MOWER,
        Glyph::Battery => kit::BATTERY,
        Glyph::Solar => concat!(
            r#"<path d="M5 12.5h14l2 8H3Z"/><path d="M4 16.5h16M12 12.5v8M8.5 12.5l-1 8M15.5 12.5l1 8"/>"#,
            r#"<path d="M12 3v2M7 5l1 1.5M17 5l-1 1.5"/>"#,
        ),
        Glyph::Charger => concat!(
            r#"<rect x="4" y="3" width="10" height="18" rx="1.5"/><path d="M9.5 7 7.5 11h3l-2 4"/>"#,
            r#"<path d="M14 9h2.5a1.5 1.5 0 0 1 1.5 1.5V17a1.5 1.5 0 0 0 3 0V8l-2-2"/>"#,
        ),
        Glyph::Energy => kit::BOLT,
        Glyph::Sun => kit::SUN,
        Glyph::Bed => concat!(
            r#"<path d="M3 19V6M3 15.5h18V19M21 15.5v-3a3 3 0 0 0-3-3h-7v6"/>"#,
            r#"<circle cx="7" cy="12" r="2"/>"#,
        ),
        Glyph::Pet => concat!(
            r#"<circle cx="7" cy="9" r="1.8"/><circle cx="10.5" cy="5.5" r="1.8"/>"#,
            r#"<circle cx="14.5" cy="5.5" r="1.8"/><circle cx="18" cy="9" r="1.8"/>"#,
            r#"<path d="M12.5 11.5c-3 0-5.5 3.5-5.5 6a2.5 2.5 0 0 0 3 2.3c1.5-.4 3.5-.4 5 0a2.5 2.5 0 0 0 3-2.3c0-2.5-2.5-6-5.5-6Z"/>"#,
        ),
        // The rest have icons of their own, reached through `of_glyph`.
        other => drawing(of_glyph(other)),
    }
}

/// What's drawn inside the icon's 24 × 24 box. Written here and nowhere else, so it's safe to
/// hand to the browser as markup.
fn drawing(icon: Icon) -> &'static str {
    match icon {
        Icon::Grip => concat!(
            r#"<g fill="currentColor" stroke="none"><circle cx="9" cy="6" r="1.7"/>"#,
            r#"<circle cx="15" cy="6" r="1.7"/><circle cx="9" cy="12" r="1.7"/>"#,
            r#"<circle cx="15" cy="12" r="1.7"/><circle cx="9" cy="18" r="1.7"/>"#,
            r#"<circle cx="15" cy="18" r="1.7"/></g>"#,
        ),
        Icon::Add => r#"<path d="M12 5v14M5 12h14"/>"#,
        Icon::Edit => {
            r#"<path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/>"#
        }
        Icon::Remove => concat!(
            r#"<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/>"#,
            r#"<path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"#,
        ),
        Icon::System => concat!(
            r#"<rect x="3" y="4" width="18" height="7" rx="1.5"/>"#,
            r#"<rect x="3" y="13" width="18" height="7" rx="1.5"/>"#,
            r#"<path d="M7 7.5h.01M7 16.5h.01M11 7.5h6M11 16.5h6"/>"#,
        ),
        Icon::Appearance => concat!(
            r#"<circle cx="12" cy="12" r="3.5"/>"#,
            r#"<path d="M12 3v2.5M12 18.5V21M3 12h2.5M18.5 12H21M5.6 5.6l1.8 1.8"#,
            r#"M16.6 16.6l1.8 1.8M5.6 18.4l1.8-1.8M16.6 7.4l1.8-1.8"/>"#,
        ),
        Icon::Assistant => {
            r#"<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4h13A1.5 1.5 0 0 1 20 5.5v9a1.5 1.5 0 0 1-1.5 1.5H10l-5 4v-4.2A1.5 1.5 0 0 1 4 14.5Z"/>"#
        }
        Icon::Places => {
            r#"<path d="M12 3 3 8l9 5 9-5-9-5Z"/><path d="m3 12.5 9 5 9-5"/><path d="m3 17 9 5 9-5"/>"#
        }
        Icon::Place => concat!(
            r#"<path d="M12 21.5s7-6.2 7-11.5a7 7 0 0 0-14 0c0 5.3 7 11.5 7 11.5Z"/>"#,
            r#"<circle cx="12" cy="10" r="2.5"/>"#,
        ),
        Icon::Users => r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#,
        Icon::Key => r#"<circle cx="8" cy="12" r="4"/><path d="M12 12h9M18 12v3M21 12v2"/>"#,
        Icon::Logs => {
            r#"<rect x="4" y="3" width="16" height="18" rx="1.5"/><path d="M8 8h8M8 12h8M8 16h5"/>"#
        }
        Icon::History => r#"<circle cx="12" cy="12" r="8"/><path d="M12 8v5l3 2"/>"#,
        Icon::Memory => concat!(
            r#"<rect x="3" y="7" width="18" height="10" rx="1.5"/>"#,
            r#"<path d="M7 10.5v3M11 10.5v3M15 10.5v3M6 17v2.5M10 17v2.5M14 17v2.5M18 17v2.5"/>"#,
        ),
        Icon::Disk => concat!(
            r#"<ellipse cx="12" cy="6" rx="8" ry="3"/>"#,
            r#"<path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>"#,
        ),
        Icon::Processor => concat!(
            r#"<rect x="5" y="5" width="14" height="14" rx="1.5"/>"#,
            r#"<rect x="9.5" y="9.5" width="5" height="5"/>"#,
            r#"<path d="M9 2.5V5M15 2.5V5M9 19v2.5M15 19v2.5M2.5 9H5M2.5 15H5M19 9h2.5M19 15h2.5"/>"#,
        ),
        Icon::Temperature => {
            r#"<path d="M14 14.8V5a2 2 0 0 0-4 0v9.8a4 4 0 1 0 4 0Z"/><path d="M12 9v8"/>"#
        }
        Icon::Check => r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#,
        Icon::Close => r#"<path d="M6 6l12 12M18 6 6 18"/>"#,
        Icon::Gear => concat!(
            r#"<circle cx="12" cy="12" r="3"/>"#,
            r#"<path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1"#,
            r#"M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"/>"#,
        ),
        Icon::Play => r#"<path d="M8 5.5v13l10.5-6.500Z" fill="currentColor"/>"#,
        Icon::Pause => r#"<path d="M9 5.500v13M15 5.500v13" stroke-width="2.600"/>"#,
        Icon::Stop => {
            r#"<rect x="6.500" y="6.500" width="11" height="11" rx="1.500" fill="currentColor"/>"#
        }
        Icon::Previous => {
            r#"<path d="M18 6v12l-8.500-6Z" fill="currentColor"/><path d="M6.500 6v12"/>"#
        }
        Icon::Next => r#"<path d="M6 6v12l8.500-6Z" fill="currentColor"/><path d="M17.500 6v12"/>"#,
        Icon::Power => r#"<path d="M12 3.500v8"/><path d="M7.050 6.500a7 7 0 1 0 9.900 0"/>"#,
        Icon::Volume => concat!(
            r#"<path d="M4 9.500h3l4.500-4v13l-4.500-4H4Z"/>"#,
            r#"<path d="M15.500 9a4 4 0 0 1 0 6M18 6.500a7.500 7.500 0 0 1 0 11"/>"#,
        ),
        Icon::Muted => {
            r#"<path d="M4 9.500h3l4.500-4v13l-4.500-4H4Z"/><path d="m16 9.500 5 5M21 9.500l-5 5"/>"#
        }
        Icon::Bulb => concat!(
            r#"<path d="M9 17.5h6M10 20.5h4"/>"#,
            r#"<path d="M8.5 14.5a6 6 0 1 1 7 0c-.6.5-1 1.200-1 2v1h-5v-1c0-.800-.4-1.500-1-2Z"/>"#,
        ),
        Icon::Tv => {
            r#"<rect x="3" y="5" width="18" height="12" rx="1.5"/><path d="M8.5 20.5h7M12 17v3.500"/>"#
        }
        Icon::Speaker => concat!(
            r#"<rect x="6.5" y="3" width="11" height="18" rx="2"/>"#,
            r#"<circle cx="12" cy="14.5" r="2.800"/><path d="M12 7.500h.01"/>"#,
        ),
        Icon::Plug => concat!(
            r#"<path d="M9 3v5M15 3v5M12 17v4"/>"#,
            r#"<path d="M6.5 8h11v3.500a5.500 5.500 0 0 1-11 0Z"/>"#,
        ),
        Icon::Person => {
            r#"<circle cx="12" cy="7" r="3.200"/><path d="M5.5 20.500a6.500 6.500 0 0 1 13 0"/>"#
        }
        Icon::Motion => concat!(
            r#"<circle cx="12" cy="12" r="1.600" fill="currentColor"/>"#,
            r#"<path d="M8.300 8.300a5.200 5.200 0 0 0 0 7.400M15.700 8.300a5.200 5.200 0 0 1 0 7.400"/>"#,
            r#"<path d="M5.500 5.500a9.200 9.200 0 0 0 0 13M18.500 5.500a9.200 9.200 0 0 1 0 13"/>"#,
        ),
        Icon::Button => {
            r#"<circle cx="12" cy="12" r="8.500"/><circle cx="12" cy="12" r="3.500" fill="currentColor"/>"#
        }
        Icon::Air => concat!(
            r#"<path d="M3 9h9.500a2.500 2.500 0 1 0-2.400-3.200"/>"#,
            r#"<path d="M3 13h14.500a2.800 2.800 0 1 1-2.700 3.600"/><path d="M3 17h7"/>"#,
        ),
        Icon::Lock => {
            r#"<rect x="5" y="10.500" width="14" height="10" rx="2"/><path d="M8 10.500V7.500a4 4 0 0 1 8 0v3"/>"#
        }
        Icon::Unlocked => {
            r#"<rect x="5" y="10.500" width="14" height="10" rx="2"/><path d="M8 10.500V7.500a4 4 0 0 1 7.700-1.500"/>"#
        }
        Icon::Bell => concat!(
            r#"<path d="M6 16.500V11a6 6 0 0 1 12 0v5.500l1.500 2h-15Z"/>"#,
            r#"<path d="M10.200 21a2 2 0 0 0 3.600 0M12 3v2"/>"#,
        ),
        Icon::Doorbell => concat!(
            r#"<rect x="7" y="2.5" width="10" height="19" rx="3"/>"#,
            r#"<circle cx="12" cy="8" r="2.2"/><circle cx="12" cy="16" r="2.4"/>"#,
        ),
        Icon::Camera => concat!(
            r#"<path d="M3.5 7.5h11A1.5 1.5 0 0 1 16 9v6a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 2 15V9"#,
            r#"a1.5 1.5 0 0 1 1.5-1.5Z"/><path d="m16 11 5-3v8l-5-3"/><circle cx="9" cy="12" r="2.2"/>"#,
        ),
        Icon::Vibration => concat!(
            r#"<rect x="8" y="4" width="8" height="16" rx="1.8"/>"#,
            r#"<path d="M4.5 8.5 3 10l1.5 1.5L3 13l1.5 1.5M19.5 8.5 21 10l-1.5 1.5L21 13l-1.5 1.5"/>"#,
        ),
        Icon::Lamp => r#"<path d="M8 3.5h8l2.5 7h-13Z"/><path d="M12 10.5V20M8.5 20.5h7"/>"#,
        Icon::CeilingLight => concat!(
            r#"<path d="M12 3v4M5 14a7 7 0 0 1 14 0Z"/>"#,
            r#"<path d="M12 17.5v2M7.5 16.5l-1 1.7M16.5 16.5l1 1.7"/>"#,
        ),
        Icon::LightStrip => concat!(
            r#"<rect x="2.5" y="9.5" width="19" height="5" rx="2.5"/>"#,
            r#"<path d="M7 12h.01M12 12h.01M17 12h.01M7 6.5v-1M12 6.5v-1M17 6.5v-1"/>"#,
        ),
        Icon::Switch => concat!(
            r#"<rect x="6" y="3" width="12" height="18" rx="2"/>"#,
            r#"<rect x="9.5" y="6.5" width="5" height="11" rx="1.2"/><path d="M9.5 12h5"/>"#,
        ),
        Icon::Fan => concat!(
            r#"<path d="M12 12h.01"/>"#,
            r#"<path d="M12 10.5C11 7.5 12 4 14.5 3.5c2 .9 1.5 4.5-1 7.2"/>"#,
            r#"<path d="M13.5 12c3-1 6.5 0 7 2.5-.9 2-4.5 1.5-7.2-1"/>"#,
            r#"<path d="M12 13.5c1 3 0 6.5-2.5 7-2-.9-1.5-4.5 1-7.2"/>"#,
            r#"<path d="M10.5 12c-3 1-6.5 0-7-2.5.9-2 4.5-1.5 7.2 1"/>"#,
        ),
        Icon::Leak => concat!(
            r#"<path d="M12 3.5c3 3.6 5.5 6.6 5.5 10a5.5 5.5 0 0 1-11 0c0-3.4 2.5-6.4 5.5-10Z"/>"#,
            r#"<path d="M9.5 14a2.5 2.5 0 0 0 2.5 2.5"/>"#,
        ),
        Icon::Thermostat => concat!(
            r#"<circle cx="12" cy="12" r="8.500"/><path d="M12 12l3.200-3.200"/>"#,
            r#"<path d="M12 5.500v1.200M5.500 12h1.200M17.300 12h1.200"/>"#,
        ),
        // The stripe is the stylesheet's to colour (`.icon .stripe`): it is the one part of
        // any icon that isn't the colour of the text around it.
        Icon::NoSignal => concat!(
            r#"<path d="M12 18.500h.01"/><path d="M8.500 15a5 5 0 0 1 7 0"/>"#,
            r#"<path d="M5.500 12a9.200 9.200 0 0 1 13 0"/><path class="stripe" d="M5 20 19 4"/>"#,
        ),
        Icon::Dot => r#"<circle cx="12" cy="12" r="3.500" fill="currentColor"/>"#,
        Icon::Of(glyph) => glyph_drawing(glyph),
        Icon::Cast => concat!(
            r#"<path d="M3 8V6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-6"/>"#,
            r#"<path d="M3 12a8 8 0 0 1 8 8M3 16a4 4 0 0 1 4 4M3 20h.01"/>"#,
        ),
    }
}

pub fn icon(icon: Icon) -> AnyView {
    view! {
        <svg
            class="icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            inner_html=drawing(icon)
        ></svg>
    }
    .into_any()
}

/// An entity's own icon: what kind of thing it is, beside its name. The same one an
/// extension's page draws for it (`irori_ui_kit::entity_icon`).
pub fn entity(capabilities: &irori_types::Capabilities) -> AnyView {
    view! {
        <svg
            class="icon entity-icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            inner_html=irori_ui_kit::entity_icon::drawing(capabilities)
        ></svg>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph a plan can name has a drawing, and finding it ends.
    #[test]
    fn every_glyph_is_drawn() {
        for glyph in irori_types::Glyph::ALL {
            assert!(
                drawing(of_glyph(*glyph)).contains('<'),
                "{glyph:?} has no drawing"
            );
        }
    }
}
