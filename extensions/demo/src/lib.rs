//! Virtual devices, for trying Irori without hardware. Also the reference protocol: copy this
//! to start a new one (`docs/specs/protocols.md`).
//!
//! Study: a dimmable lamp and a plug that measures what it powers. Hallway: a ceiling light, two
//! presence sensors, an illuminance sensor, and an mmWave with occupancy and target distance.
//! Living room: an air monitor, a TV, and a window; and the front and back doors. A scene for
//! designing automations.
//!
//! Everything moves the way it would over a day, but a day lasts a minute: dark until dawn, the
//! house waking up, everyone out, back in the evening. Batteries run down from full to empty over
//! that minute, then start again.

use std::collections::BTreeMap;
use std::time::Duration;

use irori_protocol::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, ColorMode,
    ColorTempRange, ContextId, DeviceDescription, EntityDescription, LightCapabilities, LightState,
    LightTurnOn, Name, ObjectId, SensorCapabilities, SensorClass, SensorState, SensorValue,
    SensorValueType, Service, State, StateClass, StateReport, SwitchCapabilities, SwitchClass,
    SwitchState, UniqueId,
};
use irori_protocol::{Protocol, ProtocolContext, ProtocolError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;

/// The demo protocol.
#[derive(Debug)]
pub struct Demo;

/// Settings for the demo protocol.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seconds between sensor readings.
    #[schemars(range(min = 1, max = 3600))]
    #[serde(deserialize_with = "interval_secs")]
    pub sensor_interval_secs: u64,
}

/// Serde doesn't read the schema, so what it advertises is checked here too: a whole number from
/// 1 to 3600, however it's written (`10` or `10.0`, as JSON Schema's `integer` allows).
fn interval_secs<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let secs = f64::deserialize(deserializer)?;
    if secs.fract() == 0.0 && (1.0..=3600.0).contains(&secs) {
        Ok(secs as u64)
    } else {
        Err(serde::de::Error::custom(format!(
            "sensor_interval_secs must be a whole number from 1 to 3600 (got {secs})"
        )))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Thirty readings through each minute-long day: enough to see it go by.
            sensor_interval_secs: 2,
        }
    }
}

impl Protocol for Demo {
    type Config = Config;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(config: Config, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        run(config, ctx).await
    }
}

// Device ids are the protocol and these handles (`demo_lamp`), so they don't repeat "demo".
const LAMP: &str = "lamp";
const LAMP_LIGHT: &str = "lamp-light";
const PLUG: &str = "plug";
const PLUG_SWITCH: &str = "plug-switch";
const PLUG_CURRENT: &str = "plug-current";
const PLUG_POWER: &str = "plug-power";
const PLUG_VOLTAGE: &str = "plug-voltage";
const SENSOR: &str = "hallway-sensor";
const SENSOR_OCCUPANCY: &str = "hallway-sensor-occupancy";
const SENSOR_TEMPERATURE: &str = "hallway-sensor-temperature";
const HALL_LIGHT: &str = "hall-light";
const HALL_LIGHT_ENTITY: &str = "hall-light-light";
const MOVEMENT: &str = "movement";
const MOVEMENT_OCCUPANCY: &str = "movement-occupancy";
const LUMINOSITY: &str = "luminosity";
const LUMINOSITY_LX: &str = "luminosity-illuminance";
const MMWAVE: &str = "mmwave";
const MMWAVE_OCCUPANCY: &str = "mmwave-occupancy";
const MMWAVE_DISTANCE: &str = "mmwave-target-distance";
const AIR: &str = "air-monitor";
const AIR_BATTERY: &str = "air-monitor-battery";
const AIR_TEMPERATURE: &str = "air-monitor-temperature";
const AIR_HUMIDITY: &str = "air-monitor-humidity";
const AIR_CO2: &str = "air-monitor-co2";
const TV: &str = "tv";
const TV_STATE: &str = "tv-state";
const TV_AREA: &str = "tv-area-mmwave-sensor";
const TV_AREA_OCCUPANCY: &str = "tv-area-mmwave-sensor-occupancy";

/// The living room's lights: a device, its light, the id it asks for, its name, and whether it
/// dims. The ceiling light is on a relay, so it's only on or off.
const ROOM_LIGHTS: [(&str, &str, &str, &str, bool); 3] = [
    (
        "tv-area-lights",
        "tv-area-lights-light",
        "demo_tv_area_lights",
        "Demo TV area lights",
        true,
    ),
    (
        "living-room-light",
        "living-room-light-light",
        "demo_living_room_light",
        "Demo living room light",
        false,
    ),
    (
        "dining-light",
        "dining-light-light",
        "demo_dining_light",
        "Demo dining light",
        true,
    ),
];

/// The contact sensors: a device, its contact and its battery, what it's on, and where.
const CONTACTS: [(&str, &str, &str, &str, BinarySensorClass, &str); 3] = [
    (
        "front-door",
        "front-door-contact",
        "front-door-battery",
        "Demo front door",
        BinarySensorClass::Door,
        "Hallway",
    ),
    (
        "back-door",
        "back-door-contact",
        "back-door-battery",
        "Demo back door",
        BinarySensorClass::Door,
        "Kitchen",
    ),
    (
        "window",
        "window-contact",
        "window-battery",
        "Demo living room window",
        BinarySensorClass::Window,
        "Living room",
    ),
];

/// How long a day lasts here.
const DAY_SECS: u64 = 60;
async fn run(config: Config, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
    describe(&ctx).await?;

    let mut lamp = LightState {
        on: false,
        brightness: Some(180),
        color_mode: Some(ColorMode::ColorTemp),
        color_temp_kelvin: Some(2700),
        rgb: None,
    };
    let mut hall = LightState {
        on: false,
        brightness: Some(120),
        color_mode: None,
        color_temp_kelvin: None,
        rgb: None,
    };
    let mut room: BTreeMap<&'static str, LightState> = ROOM_LIGHTS
        .iter()
        .map(|(_, light, _, _, dims)| {
            (
                *light,
                LightState {
                    on: false,
                    brightness: dims.then_some(150),
                    color_mode: None,
                    color_temp_kelvin: None,
                    rgb: None,
                },
            )
        })
        .collect();
    let mut plug_on = false;
    ctx.report_state(report(LAMP_LIGHT, Some(State::Light(lamp.clone())), None)?);
    ctx.report_state(report(
        HALL_LIGHT_ENTITY,
        Some(State::Light(hall.clone())),
        None,
    )?);
    for (light, state) in &room {
        ctx.report_state(report(light, Some(State::Light(state.clone())), None)?);
    }
    ctx.report_state(report(
        PLUG_SWITCH,
        Some(State::Switch(SwitchState { on: plug_on })),
        None,
    )?);

    let mut readings = tokio::time::interval(Duration::from_secs(config.sensor_interval_secs));
    let mut tick: u64 = 0;
    loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(incoming) = call else {
                    return Ok(()); // told to stop
                };
                let call = &incoming.call;
                let caused_by = Some(call.context.id.clone());
                let result = match (call.unique_id.as_str(), &call.service) {
                    (LAMP_LIGHT, Service::LightTurnOn(data)) => {
                        apply_light(&mut lamp, Some(data));
                        Ok((LAMP_LIGHT, State::Light(lamp.clone())))
                    }
                    (LAMP_LIGHT, Service::LightTurnOff) => {
                        apply_light(&mut lamp, None);
                        Ok((LAMP_LIGHT, State::Light(lamp.clone())))
                    }
                    (HALL_LIGHT_ENTITY, Service::LightTurnOn(data)) => {
                        apply_light(&mut hall, Some(data));
                        Ok((HALL_LIGHT_ENTITY, State::Light(hall.clone())))
                    }
                    (HALL_LIGHT_ENTITY, Service::LightTurnOff) => {
                        apply_light(&mut hall, None);
                        Ok((HALL_LIGHT_ENTITY, State::Light(hall.clone())))
                    }
                    (light, Service::LightTurnOn(_) | Service::LightTurnOff)
                        if room.contains_key(light) =>
                    {
                        room_light(&mut room, light, &call.service)
                    }
                    (PLUG_SWITCH, Service::SwitchTurnOn | Service::SwitchTurnOff) => {
                        plug_on = matches!(call.service, Service::SwitchTurnOn);
                        Ok((PLUG_SWITCH, State::Switch(SwitchState { on: plug_on })))
                    }
                    (_, _) => Err(format!(
                        "the demo has no `{}` for {}",
                        call.unique_id,
                        call.service.name()
                    )),
                };
                match result {
                    Ok((entity, state)) => {
                        incoming.reply(Ok(()));
                        ctx.report_state(report(entity, Some(state), caused_by)?);
                    }
                    Err(message) => incoming.reply(Err(ServiceError::failed(message))),
                }
            }
            _ = readings.tick() => {
                let secs = tick * config.sensor_interval_secs;
                let hour = (secs % DAY_SECS) as f64 / DAY_SECS as f64 * 24.0;
                report_sensors(&ctx, hour, secs, plug_on)?;
                tick += 1;
            }
        }
    }
}

/// Turns one of the living room's lights on or off; a relay has no brightness to take.
fn room_light(
    room: &mut BTreeMap<&'static str, LightState>,
    light: &str,
    service: &Service,
) -> Result<(&'static str, State), String> {
    let (key, state) = room
        .iter_mut()
        .find(|(key, _)| **key == light)
        .ok_or_else(|| format!("the demo has no light `{light}`"))?;
    let dims = state.brightness.is_some();
    match service {
        Service::LightTurnOn(data) => apply_light(state, Some(data)),
        _ => apply_light(state, None),
    }
    if !dims {
        state.brightness = None;
    }
    Ok((*key, State::Light(state.clone())))
}

fn apply_light(light: &mut LightState, on: Option<&LightTurnOn>) {
    match on {
        Some(data) => {
            light.on = true;
            light.brightness = data.brightness.or(light.brightness);
            if let Some(kelvin) = data.color_temp_kelvin {
                light.color_temp_kelvin = Some(kelvin);
                light.color_mode = Some(ColorMode::ColorTemp);
            }
        }
        None => light.on = false,
    }
}

fn number(unique_id: &str, value: f64) -> Result<StateReport, ProtocolError> {
    report(
        unique_id,
        Some(State::Sensor(SensorState {
            value: SensorValue::Number(value),
        })),
        None,
    )
}

fn flag(unique_id: &str, on: bool) -> Result<StateReport, ProtocolError> {
    report(
        unique_id,
        Some(State::BinarySensor(BinarySensorState { on })),
        None,
    )
}

/// Every reading, `hour` hours into the day and `secs` seconds since the demo started.
fn report_sensors(
    ctx: &ProtocolContext,
    hour: f64,
    secs: u64,
    plug_on: bool,
) -> Result<(), ProtocolError> {
    let charge = battery(hour);
    ctx.report_state(flag(SENSOR_OCCUPANCY, hallway(hour))?);
    ctx.report_state(number(SENSOR_TEMPERATURE, temperature(hour))?);
    ctx.report_state(flag(MOVEMENT_OCCUPANCY, walk_by(hour))?);
    ctx.report_state(number(LUMINOSITY_LX, illuminance(hour))?);
    let occupied = occupancy(hour);
    ctx.report_state(flag(MMWAVE_OCCUPANCY, occupied)?);
    ctx.report_state(report(
        MMWAVE_DISTANCE,
        target_distance(hour, occupied).map(|metres| {
            State::Sensor(SensorState {
                value: SensorValue::Number(metres),
            })
        }),
        None,
    )?);

    let volts = voltage(hour);
    let watts = if plug_on { power(hour) } else { 0.0 };
    ctx.report_state(number(PLUG_VOLTAGE, volts)?);
    ctx.report_state(number(PLUG_POWER, watts)?);
    ctx.report_state(number(PLUG_CURRENT, round2(watts / volts))?);

    ctx.report_state(number(AIR_BATTERY, charge)?);
    ctx.report_state(number(AIR_TEMPERATURE, living_temperature(hour))?);
    ctx.report_state(number(AIR_HUMIDITY, humidity(hour))?);
    ctx.report_state(number(AIR_CO2, co2(hour))?);

    for (_, contact, battery_id, _, _, _) in CONTACTS {
        ctx.report_state(flag(contact, open(contact, hour))?);
        ctx.report_state(number(battery_id, charge)?);
    }
    ctx.report_state(report(
        TV_STATE,
        Some(State::Sensor(SensorState {
            value: SensorValue::Text(tv(secs).into()),
        })),
        None,
    )?);
    ctx.report_state(flag(TV_AREA_OCCUPANCY, watching(secs))?);
    Ok(())
}

/// Someone on the sofa by the TV, then not, three and a half minutes each: slower than the
/// day, so a trigger waiting for the sofa to be empty for 3 minutes gets to fire.
fn watching(secs: u64) -> bool {
    (secs / SOFA_SECS).is_multiple_of(2)
}

/// How long the sofa stays taken, and then empty.
const SOFA_SECS: u64 = 210;

/// Whether `hour` falls in one of `spans`, each from one hour to (not including) another.
fn during(hour: f64, spans: &[(f64, f64)]) -> bool {
    spans.iter().any(|(from, to)| (*from..*to).contains(&hour))
}

/// Something that peaks at `peak` o'clock and bottoms out twelve hours away, from -1 to 1.
fn daily(hour: f64, peak: f64) -> f64 {
    ((hour - peak) / 24.0 * std::f64::consts::TAU).cos()
}

/// Full at midnight, empty by the end of the day, and full again.
fn battery(hour: f64) -> f64 {
    (100.0 * (1.0 - hour / 24.0)).round().clamp(0.0, 100.0)
}

/// The hallway, around 20 °C: coolest before dawn, warmest mid-afternoon.
fn temperature(hour: f64) -> f64 {
    round1(20.5 + 1.5 * daily(hour, 15.0))
}

/// Someone in the hallway: up in the night, getting up, heading out, home, and the evening.
fn hallway(hour: f64) -> bool {
    during(
        hour,
        &[
            (2.5, 3.4),
            (6.5, 7.4),
            (8.0, 9.0),
            (17.5, 18.5),
            (21.0, 21.9),
        ],
    )
}

/// The PIR further down the hall: fewer, shorter walk-bys.
fn walk_by(hour: f64) -> bool {
    during(hour, &[(7.0, 7.9), (17.8, 18.7), (22.2, 23.1)])
}

/// The mmWave: presence that lingers while people are home and up.
fn occupancy(hour: f64) -> bool {
    during(hour, &[(6.5, 9.0), (17.5, 23.5)])
}

/// Daylight: 8 lx through the night, up to 400 lx at midday, from dawn at 6 to dusk at 20.
fn illuminance(hour: f64) -> f64 {
    let sun = ((hour - 6.0) / 14.0 * std::f64::consts::PI).sin().max(0.0);
    round1(8.0 + 392.0 * sun)
}

/// Distance to the nearest target while occupied, 0.6–3.0 m. Unknown when the room is empty.
fn target_distance(hour: f64, occupied: bool) -> Option<f64> {
    occupied.then(|| {
        let wave = (hour * std::f64::consts::TAU / 1.5).sin();
        round1(1.8 + 1.2 * wave)
    })
}

/// The mains, wandering a little around 230 V.
fn voltage(hour: f64) -> f64 {
    round1(230.0 + 2.5 * (hour * std::f64::consts::TAU / 5.0).sin() - 1.5 * daily(hour, 19.0))
}

/// What the plug powers, while it's on: a desk's worth, more in the evening.
fn power(hour: f64) -> f64 {
    let busy = if during(hour, &[(17.5, 23.5)]) {
        45.0
    } else {
        0.0
    };
    round1(62.0 + busy + 8.0 * (hour * std::f64::consts::TAU / 2.0).sin())
}

/// The living room, a little warmer than the hall, and warmer still with people in it.
fn living_temperature(hour: f64) -> f64 {
    let people = if during(hour, &[(18.0, 23.0)]) {
        0.8
    } else {
        0.0
    };
    round1(21.0 + 1.2 * daily(hour, 16.0) + people)
}

/// Relative humidity: highest in the early morning, a shower's worth after getting up.
fn humidity(hour: f64) -> f64 {
    let shower = if during(hour, &[(7.0, 8.2)]) {
        14.0
    } else {
        0.0
    };
    round1((47.0 + 7.0 * daily(hour, 5.0) + shower).clamp(20.0, 90.0))
}

/// CO₂ in ppm: climbs while people are in, falls once they leave or open the window.
fn co2(hour: f64) -> f64 {
    let ppm = match hour {
        h if h < 6.5 => 900.0 - 60.0 * h,
        h if h < 8.5 => 510.0 + 190.0 * (h - 6.5),
        h if h < 17.5 => 430.0 + 460.0 * (-(h - 8.5)).exp(),
        h if h < 23.0 => 430.0 + 110.0 * (h - 17.5),
        h => 1035.0 - 135.0 * (h - 23.0),
    };
    let aired = if during(hour, &[(12.0, 14.5), (20.5, 21.4)]) {
        0.7
    } else {
        1.0
    };
    (ppm * aired).max(410.0).round()
}

/// Whether a contact reads open: out in the morning and back in the evening through the front,
/// the bins out the back, the window for some air.
fn open(contact: &str, hour: f64) -> bool {
    match contact {
        "front-door-contact" => during(hour, &[(8.0, 8.9), (17.4, 18.3)]),
        "back-door-contact" => during(hour, &[(19.5, 20.4)]),
        _ => during(hour, &[(12.0, 14.5), (20.5, 21.4)]),
    }
}

/// The TV, a state a minute rather than following the day, so a trigger waiting for it to play
/// for 20 seconds gets to fire: a show, a pause, more of it, the menu, then off.
fn tv(secs: u64) -> &'static str {
    const SHOW: [&str; 5] = ["playing", "paused", "playing", "idle", "off"];
    SHOW[usize::try_from(secs / TV_SECS).unwrap_or(0) % SHOW.len()]
}

/// How long the TV stays in each state.
const TV_SECS: u64 = 60;

fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

async fn describe(ctx: &ProtocolContext) -> Result<(), ProtocolError> {
    ctx.describe_device(device(LAMP, "Demo lamp", "Virtual lamp", "Study")?)
        .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(LAMP_LIGHT)?,
        name: None,
        device_unique_id: Some(id(LAMP)?),
        suggested_object_id: Some(ObjectId::try_from("demo_lamp")?),
        capabilities: Capabilities::Light(LightCapabilities {
            brightness: true,
            color_temp_kelvin: Some(ColorTempRange {
                min: 2200,
                max: 6500,
            }),
            rgb: false,
        }),
        entity_category: None,
    })
    .await?;

    ctx.describe_device(device(PLUG, "Demo plug", "Virtual plug", "Study")?)
        .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(PLUG_SWITCH)?,
        name: None,
        device_unique_id: Some(id(PLUG)?),
        suggested_object_id: Some(ObjectId::try_from("demo_plug")?),
        capabilities: Capabilities::Switch(SwitchCapabilities {
            device_class: Some(SwitchClass::Outlet),
        }),
        entity_category: None,
    })
    .await?;
    for (unique_id, name, class, unit) in [
        (PLUG_CURRENT, "Current", SensorClass::Current, "A"),
        (PLUG_POWER, "Power", SensorClass::Power, "W"),
        (PLUG_VOLTAGE, "Voltage", SensorClass::Voltage, "V"),
    ] {
        ctx.describe_entity(measurement(unique_id, name, PLUG, class, unit)?)
            .await?;
    }

    ctx.describe_device(device(
        SENSOR,
        "Demo hallway sensor",
        "Virtual sensor",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(flag_entity(
        SENSOR_OCCUPANCY,
        "Occupancy",
        SENSOR,
        BinarySensorClass::Occupancy,
    )?)
    .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(SENSOR_TEMPERATURE)?,
        name: Some(Name::try_from("Temperature")?),
        device_unique_id: Some(id(SENSOR)?),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: Some(SensorClass::Temperature),
            unit: Some("°C".into()),
            state_class: Some(StateClass::Measurement),
            options: Vec::new(),
        }),
        entity_category: None,
    })
    .await?;

    ctx.describe_device(device(
        HALL_LIGHT,
        "Demo hallway light",
        "Virtual ceiling light",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(HALL_LIGHT_ENTITY)?,
        name: None,
        device_unique_id: Some(id(HALL_LIGHT)?),
        suggested_object_id: Some(ObjectId::try_from("demo_hall_light")?),
        capabilities: Capabilities::Light(LightCapabilities {
            brightness: true,
            color_temp_kelvin: None,
            rgb: false,
        }),
        entity_category: None,
    })
    .await?;

    ctx.describe_device(device(
        MOVEMENT,
        "Demo movement sensor",
        "Virtual PIR",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(flag_entity(
        MOVEMENT_OCCUPANCY,
        "Occupancy",
        MOVEMENT,
        BinarySensorClass::Occupancy,
    )?)
    .await?;

    ctx.describe_device(device(
        LUMINOSITY,
        "Demo luminosity sensor",
        "Virtual lux sensor",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(LUMINOSITY_LX)?,
        name: Some(Name::try_from("Illuminance")?),
        device_unique_id: Some(id(LUMINOSITY)?),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: Some(SensorClass::Illuminance),
            unit: Some("lx".into()),
            state_class: Some(StateClass::Measurement),
            options: Vec::new(),
        }),
        entity_category: None,
    })
    .await?;

    ctx.describe_device(device(
        MMWAVE,
        "Demo mmWave sensor",
        "Virtual mmWave",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(MMWAVE_OCCUPANCY)?,
        name: Some(Name::try_from("Occupancy")?),
        device_unique_id: Some(id(MMWAVE)?),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: Some(BinarySensorClass::Occupancy),
        }),
        entity_category: None,
    })
    .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(MMWAVE_DISTANCE)?,
        name: Some(Name::try_from("Target distance")?),
        device_unique_id: Some(id(MMWAVE)?),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: Some(SensorClass::Distance),
            unit: Some("m".into()),
            state_class: Some(StateClass::Measurement),
            options: Vec::new(),
        }),
        entity_category: None,
    })
    .await?;

    ctx.describe_device(device(
        TV_AREA,
        "Demo TV area mmWave sensor",
        "Virtual mmWave",
        "Living room",
    )?)
    .await?;
    ctx.describe_entity(flag_entity(
        TV_AREA_OCCUPANCY,
        "Occupancy",
        TV_AREA,
        BinarySensorClass::Occupancy,
    )?)
    .await?;

    for (handle, light, object_id, name, dims) in ROOM_LIGHTS {
        let model = if dims {
            "Virtual dimmable light"
        } else {
            "Virtual light on a relay"
        };
        ctx.describe_device(device(handle, name, model, "Living room")?)
            .await?;
        ctx.describe_entity(EntityDescription {
            unique_id: id(light)?,
            name: None,
            device_unique_id: Some(id(handle)?),
            suggested_object_id: Some(ObjectId::try_from(object_id)?),
            capabilities: Capabilities::Light(LightCapabilities {
                brightness: dims,
                color_temp_kelvin: None,
                rgb: false,
            }),
            entity_category: None,
        })
        .await?;
    }

    ctx.describe_device(device(
        AIR,
        "Demo air monitor",
        "Virtual air quality monitor",
        "Living room",
    )?)
    .await?;
    for (unique_id, name, class, unit) in [
        (AIR_BATTERY, "Battery", SensorClass::Battery, "%"),
        (
            AIR_TEMPERATURE,
            "Temperature",
            SensorClass::Temperature,
            "°C",
        ),
        (AIR_HUMIDITY, "Humidity", SensorClass::Humidity, "%"),
        (AIR_CO2, "CO2", SensorClass::Co2, "ppm"),
    ] {
        ctx.describe_entity(measurement(unique_id, name, AIR, class, unit)?)
            .await?;
    }

    for (device_id, contact, battery_id, name, class, room) in CONTACTS {
        ctx.describe_device(device(device_id, name, "Virtual contact sensor", room)?)
            .await?;
        ctx.describe_entity(flag_entity(contact, "Contact", device_id, class)?)
            .await?;
        ctx.describe_entity(measurement(
            battery_id,
            "Battery",
            device_id,
            SensorClass::Battery,
            "%",
        )?)
        .await?;
    }

    ctx.describe_device(device(TV, "Demo TV", "Virtual TV", "Living room")?)
        .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(TV_STATE)?,
        name: Some(Name::try_from("State")?),
        device_unique_id: Some(id(TV)?),
        suggested_object_id: None,
        // off, idle, playing or paused.
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Text,
            device_class: None,
            unit: None,
            state_class: None,
            options: Vec::new(),
        }),
        entity_category: None,
    })
    .await?;
    Ok(())
}

/// A sensor with a number, on one of the devices.
fn measurement(
    unique_id: &str,
    name: &str,
    device_id: &str,
    class: SensorClass,
    unit: &str,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: id(unique_id)?,
        name: Some(Name::try_from(name)?),
        device_unique_id: Some(id(device_id)?),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: Some(class),
            unit: Some(unit.into()),
            state_class: Some(StateClass::Measurement),
            options: Vec::new(),
        }),
        entity_category: None,
    })
}

/// A binary sensor on one of the devices.
fn flag_entity(
    unique_id: &str,
    name: &str,
    device_id: &str,
    class: BinarySensorClass,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: id(unique_id)?,
        name: Some(Name::try_from(name)?),
        device_unique_id: Some(id(device_id)?),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: Some(class),
        }),
        entity_category: None,
    })
}

fn id(unique_id: &str) -> Result<UniqueId, ProtocolError> {
    Ok(UniqueId::try_from(unique_id)?)
}

/// One virtual device. `room` is what a real device would say about where it is — ESPHome's
/// `area:`, for one — so the demo exercises the path a real home takes: Irori never invents the
/// room, but making one by that name collects the devices asking for it
/// (`docs/specs/config.md` §5).
fn device(
    unique_id: &str,
    name: &str,
    model: &str,
    room: &str,
) -> Result<DeviceDescription, ProtocolError> {
    Ok(DeviceDescription {
        unique_id: id(unique_id)?,
        name: Name::try_from(name)?,
        manufacturer: Some("Irori".into()),
        model: Some(model.into()),
        sw_version: Some(env!("CARGO_PKG_VERSION").into()),
        hw_version: None,
        suggested_area: Some(Name::try_from(room)?),
        via_device_unique_id: None,
    })
}

fn report(
    unique_id: &str,
    state: Option<State>,
    caused_by: Option<ContextId>,
) -> Result<StateReport, ProtocolError> {
    Ok(StateReport {
        unique_id: id(unique_id)?,
        state,
        attributes: BTreeMap::new(),
        caused_by,
        replayed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_and_config_are_valid() {
        let builtin = irori_protocol::builtin::<Demo>().expect("valid built-in");
        assert_eq!(builtin.manifest.extension.id.as_str(), "demo");
        assert!(builtin.manifest.warnings().is_empty());
    }

    #[tokio::test]
    async fn settings_outside_the_advertised_range_are_refused() {
        let builtin = irori_protocol::builtin::<Demo>().expect("valid built-in");
        for secs in [
            serde_json::json!(0),
            serde_json::json!(3601),
            serde_json::json!(1.5),
        ] {
            let (ctx, _host) = irori_protocol::host::connect();
            let err = builtin
                .start(serde_json::json!({ "sensor_interval_secs": secs }), ctx)
                .err()
                .expect("not a whole number from 1 to 3600");
            assert!(
                err.contains("must be a whole number from 1 to 3600"),
                "{err}"
            );
        }
        for secs in [serde_json::json!(60), serde_json::json!(60.0)] {
            let (ctx, _host) = irori_protocol::host::connect();
            let started = builtin.start(serde_json::json!({ "sensor_interval_secs": secs }), ctx);
            assert!(started.is_ok());
        }
    }

    /// The day's hours, as the readings see them.
    fn day() -> impl Iterator<Item = f64> {
        (0..DAY_SECS / 2).map(|tick| (tick * 2) as f64 / DAY_SECS as f64 * 24.0)
    }

    #[test]
    fn temperatures_stay_believable() {
        for hour in day() {
            assert!((18.5..=22.5).contains(&temperature(hour)), "{hour}");
            assert!((19.0..=23.5).contains(&living_temperature(hour)), "{hour}");
        }
        assert!(
            temperature(15.0) > temperature(3.0),
            "warmest in the afternoon"
        );
    }

    #[test]
    fn batteries_run_down_over_the_day_and_start_again() {
        assert_eq!(battery(0.0), 100.0);
        assert!(battery(12.0) < 60.0 && battery(12.0) > 40.0);
        assert!(battery(23.9) <= 1.0);
        let charges: Vec<f64> = day().map(battery).collect();
        assert!(charges.windows(2).all(|w| w[1] <= w[0]));
    }

    /// Every reading the scene is meant to show turns up at the default interval: presence
    /// lingers over walk-bys, distance is only a number while someone is there, lux crosses the
    /// "dark enough to light" band, doors open and close, the TV goes through its states.
    #[test]
    fn a_day_shows_everything_an_automation_might_use() {
        let hours: Vec<f64> = day().collect();
        let count = |f: &dyn Fn(f64) -> bool| hours.iter().filter(|h| f(**h)).count();
        assert!(count(&hallway) > 0 && count(&hallway) < hours.len() / 2);
        assert!(count(&walk_by) > 0 && count(&walk_by) < count(&occupancy));
        for hour in &hours {
            assert_eq!(
                target_distance(*hour, occupancy(*hour)).is_some(),
                occupancy(*hour)
            );
            if let Some(metres) = target_distance(*hour, true) {
                assert!((0.6..=3.0).contains(&metres), "{metres}");
            }
        }
        assert!(hours.iter().any(|h| illuminance(*h) < 30.0));
        assert!(hours.iter().any(|h| illuminance(*h) > 300.0));
        for (_, contact, ..) in CONTACTS {
            let opened = count(&|h| open(contact, h));
            assert!(opened > 0 && opened < hours.len() / 3, "{contact}");
        }
        // The TV changes once a minute, through every state; the sofa every 3.5 minutes.
        for state in ["off", "idle", "playing", "paused"] {
            assert!((0..300).any(|s| tv(s) == state), "{state}");
        }
        assert_eq!((tv(0), tv(59), tv(60)), ("playing", "playing", "paused"));
        assert!(watching(0) && watching(209) && !watching(210) && watching(420));
        let co2s: Vec<f64> = hours.iter().map(|h| co2(*h)).collect();
        assert!(co2s.iter().any(|c| *c > 900.0) && co2s.iter().any(|c| *c < 500.0));
        assert!(hours.iter().all(|h| (20.0..=90.0).contains(&humidity(*h))));
    }

    #[test]
    fn the_living_room_lights_dim_except_the_one_on_a_relay() {
        let mut room: BTreeMap<&'static str, LightState> = ROOM_LIGHTS
            .iter()
            .map(|(_, light, _, _, dims)| {
                let state = LightState {
                    on: false,
                    brightness: dims.then_some(150),
                    color_mode: None,
                    color_temp_kelvin: None,
                    rgb: None,
                };
                (*light, state)
            })
            .collect();
        let dim = Service::LightTurnOn(LightTurnOn {
            brightness: Some(64),
            ..LightTurnOn::default()
        });
        let Ok((_, State::Light(tv_area))) = room_light(&mut room, "tv-area-lights-light", &dim)
        else {
            panic!("the TV area lights take a call");
        };
        assert!(tv_area.on);
        assert_eq!(tv_area.brightness, Some(64));
        let Ok((_, State::Light(ceiling))) = room_light(&mut room, "living-room-light-light", &dim)
        else {
            panic!("the ceiling light takes a call");
        };
        assert!(ceiling.on);
        assert_eq!(ceiling.brightness, None);
        let Ok((_, State::Light(off))) =
            room_light(&mut room, "tv-area-lights-light", &Service::LightTurnOff)
        else {
            panic!("and turns off");
        };
        assert!(!off.on);
        assert!(room_light(&mut room, "garage-light", &Service::LightTurnOff).is_err());
    }

    #[test]
    fn the_plug_draws_what_it_says() {
        for hour in day() {
            let (v, w) = (voltage(hour), power(hour));
            assert!((225.0..=235.0).contains(&v), "{v}");
            assert!((50.0..=120.0).contains(&w), "{w}");
            assert!((round2(w / v) - w / v).abs() < 0.01);
        }
    }
}
