//! Virtual devices, for trying Irori without hardware. Also the reference integration: copy this
//! to start a new one (`docs/specs/integrations.md`).
//!
//! Study: a dimmable lamp and a plug. Hallway: a ceiling light, a PIR, an illuminance sensor,
//! and an mmWave with occupancy and target distance — a scene for designing automations. Sensor
//! readings keep moving on a timer.

use std::collections::BTreeMap;
use std::time::Duration;

use irori_integration::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, ColorMode,
    ColorTempRange, ContextId, DeviceDescription, EntityDescription, LightCapabilities, LightState,
    LightTurnOn, Name, ObjectId, SensorCapabilities, SensorClass, SensorState, SensorValue,
    SensorValueType, Service, State, StateClass, StateReport, SwitchCapabilities, SwitchClass,
    SwitchState, UniqueId,
};
use irori_integration::{Integration, IntegrationContext, IntegrationError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;

/// The demo integration.
#[derive(Debug)]
pub struct Demo;

/// Settings for the demo integration.
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
            // Fast enough that a hallway scene visibly cycles while someone is writing a rule.
            sensor_interval_secs: 3,
        }
    }
}

impl Integration for Demo {
    type Config = Config;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(config: Config, ctx: IntegrationContext) -> Result<(), IntegrationError> {
        run(config, ctx).await
    }
}

// Device ids are the integration and these handles (`demo_lamp`), so they don't repeat "demo".
const LAMP: &str = "lamp";
const LAMP_LIGHT: &str = "lamp-light";
const PLUG: &str = "plug";
const PLUG_SWITCH: &str = "plug-switch";
const SENSOR: &str = "hallway-sensor";
const SENSOR_MOTION: &str = "hallway-sensor-motion";
const SENSOR_TEMPERATURE: &str = "hallway-sensor-temperature";
const HALL_LIGHT: &str = "hall-light";
const HALL_LIGHT_ENTITY: &str = "hall-light-light";
const MOVEMENT: &str = "movement";
const MOVEMENT_MOTION: &str = "movement-motion";
const LUMINOSITY: &str = "luminosity";
const LUMINOSITY_LX: &str = "luminosity-illuminance";
const MMWAVE: &str = "mmwave";
const MMWAVE_OCCUPANCY: &str = "mmwave-occupancy";
const MMWAVE_DISTANCE: &str = "mmwave-target-distance";

async fn run(config: Config, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
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
    let mut plug_on = false;
    ctx.report_state(report(LAMP_LIGHT, Some(State::Light(lamp.clone())), None)?);
    ctx.report_state(report(
        HALL_LIGHT_ENTITY,
        Some(State::Light(hall.clone())),
        None,
    )?);
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
                report_sensors(&ctx, tick).await?;
                tick += 1;
            }
        }
    }
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

async fn report_sensors(ctx: &IntegrationContext, tick: u64) -> Result<(), IntegrationError> {
    ctx.report_state(report(
        SENSOR_MOTION,
        Some(State::BinarySensor(BinarySensorState {
            on: tick.is_multiple_of(3),
        })),
        None,
    )?);
    ctx.report_state(report(
        SENSOR_TEMPERATURE,
        Some(State::Sensor(SensorState {
            value: SensorValue::Number(temperature(tick)),
        })),
        None,
    )?);
    ctx.report_state(report(
        MOVEMENT_MOTION,
        Some(State::BinarySensor(BinarySensorState {
            on: movement(tick),
        })),
        None,
    )?);
    ctx.report_state(report(
        LUMINOSITY_LX,
        Some(State::Sensor(SensorState {
            value: SensorValue::Number(illuminance(tick)),
        })),
        None,
    )?);
    let occupied = occupancy(tick);
    ctx.report_state(report(
        MMWAVE_OCCUPANCY,
        Some(State::BinarySensor(BinarySensorState { on: occupied })),
        None,
    )?);
    ctx.report_state(report(
        MMWAVE_DISTANCE,
        target_distance(tick, occupied).map(|metres| {
            State::Sensor(SensorState {
                value: SensorValue::Number(metres),
            })
        }),
        None,
    )?);
    Ok(())
}

/// Around 21 °C, drifting ±1.5 °C over a couple of minutes of readings, to one decimal.
fn temperature(tick: u64) -> f64 {
    let wave = (tick as f64 * std::f64::consts::TAU / 24.0).sin();
    round1(21.0 + 1.5 * wave)
}

/// PIR walk-by: on for two ticks of every eight, then still.
fn movement(tick: u64) -> bool {
    tick % 8 < 2
}

/// Presence that lingers after the PIR: occupied for five ticks of every eight.
fn occupancy(tick: u64) -> bool {
    tick % 8 < 5
}

/// Night through a bright hallway, 8–400 lx. Below ~30 lx is the usual "turn the light on" band.
fn illuminance(tick: u64) -> f64 {
    let wave = (tick as f64 * std::f64::consts::TAU / 30.0).sin();
    round1((204.0 + 196.0 * wave).max(8.0))
}

/// Distance to the nearest target while occupied, 0.6–3.0 m. Unknown when the room is empty.
fn target_distance(tick: u64, occupied: bool) -> Option<f64> {
    occupied.then(|| {
        let wave = (tick as f64 * std::f64::consts::TAU / 6.0).sin();
        round1(1.8 + 1.2 * wave)
    })
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

async fn describe(ctx: &IntegrationContext) -> Result<(), IntegrationError> {
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
    })
    .await?;

    ctx.describe_device(device(
        SENSOR,
        "Demo hallway sensor",
        "Virtual sensor",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(SENSOR_MOTION)?,
        name: Some(Name::try_from("Motion")?),
        device_unique_id: Some(id(SENSOR)?),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: Some(BinarySensorClass::Motion),
        }),
    })
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
        }),
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
    })
    .await?;

    ctx.describe_device(device(
        MOVEMENT,
        "Demo movement sensor",
        "Virtual PIR",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(EntityDescription {
        unique_id: id(MOVEMENT_MOTION)?,
        name: Some(Name::try_from("Motion")?),
        device_unique_id: Some(id(MOVEMENT)?),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: Some(BinarySensorClass::Motion),
        }),
    })
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
        }),
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
        }),
    })
    .await?;
    Ok(())
}

fn id(unique_id: &str) -> Result<UniqueId, IntegrationError> {
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
) -> Result<DeviceDescription, IntegrationError> {
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
) -> Result<StateReport, IntegrationError> {
    Ok(StateReport {
        unique_id: id(unique_id)?,
        state,
        attributes: BTreeMap::new(),
        caused_by,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_and_config_are_valid() {
        let builtin = irori_integration::builtin::<Demo>().expect("valid built-in");
        assert_eq!(builtin.manifest.extension.id.as_str(), "demo");
        assert!(builtin.manifest.warnings().is_empty());
    }

    #[tokio::test]
    async fn settings_outside_the_advertised_range_are_refused() {
        let builtin = irori_integration::builtin::<Demo>().expect("valid built-in");
        for secs in [
            serde_json::json!(0),
            serde_json::json!(3601),
            serde_json::json!(1.5),
        ] {
            let (ctx, _host) = irori_integration::host::connect();
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
            let (ctx, _host) = irori_integration::host::connect();
            let started = builtin.start(serde_json::json!({ "sensor_interval_secs": secs }), ctx);
            assert!(started.is_ok());
        }
    }

    #[test]
    fn temperature_stays_near_21() {
        for tick in 0..100 {
            let t = temperature(tick);
            assert!((19.5..=22.5).contains(&t), "{t}");
        }
    }

    /// PIR walk-bys are brief; mmWave occupancy lingers over them; distance is only a number
    /// while someone is there; lux crosses the "dark enough to light" band.
    #[test]
    fn hallway_scene_cycles_for_automations() {
        let mut motion = 0;
        let mut occupied = 0;
        let mut distances = Vec::new();
        let mut lux = Vec::new();
        for tick in 0..40 {
            if movement(tick) {
                motion += 1;
            }
            if occupancy(tick) {
                occupied += 1;
            }
            if let Some(metres) = target_distance(tick, occupancy(tick)) {
                assert!((0.6..=3.0).contains(&metres), "{metres}");
                distances.push(metres);
            } else {
                assert!(!occupancy(tick));
            }
            lux.push(illuminance(tick));
        }
        assert!(motion > 0 && motion < occupied);
        assert!(distances.iter().any(|&a| distances.iter().any(|&b| a != b)));
        assert!(lux.iter().copied().any(|lx| lx < 30.0));
        assert!(lux.iter().copied().any(|lx| lx > 200.0));
        assert!(occupancy(0) && movement(0));
        assert!(
            occupancy(3) && !movement(3),
            "presence lingers after the PIR"
        );
        assert!(!occupancy(7) && !movement(7));
    }
}
