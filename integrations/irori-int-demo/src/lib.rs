//! Virtual devices, for trying Irori without hardware. Also the reference integration: copy this
//! to start a new one (`docs/specs/integrations.md`).
//!
//! - **Demo lamp**: a dimmable light with color temperature.
//! - **Demo plug**: a switch.
//! - **Demo hallway sensor**: motion (on for one reading out of three) and a temperature that
//!   drifts around 21 °C.

use std::collections::BTreeMap;
use std::time::Duration;

use irori_integration::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, ColorMode,
    ColorTempRange, ContextId, DeviceDescription, EntityDescription, LightCapabilities, LightState,
    Name, ObjectId, SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType,
    Service, State, StateClass, StateReport, SwitchCapabilities, SwitchClass, SwitchState,
    UniqueId,
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
    pub sensor_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sensor_interval_secs: 10,
        }
    }
}

impl Integration for Demo {
    type Config = Config;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");

    async fn run(config: Config, ctx: IntegrationContext) -> Result<(), IntegrationError> {
        run(config, ctx).await
    }
}

const LAMP: &str = "demo-lamp";
const LAMP_LIGHT: &str = "demo-lamp-light";
const PLUG: &str = "demo-plug";
const PLUG_SWITCH: &str = "demo-plug-switch";
const SENSOR: &str = "demo-hallway-sensor";
const SENSOR_MOTION: &str = "demo-hallway-sensor-motion";
const SENSOR_TEMPERATURE: &str = "demo-hallway-sensor-temperature";

async fn run(config: Config, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
    describe(&ctx).await?;

    let mut lamp = LightState {
        on: false,
        brightness: Some(180),
        color_mode: Some(ColorMode::ColorTemp),
        color_temp_kelvin: Some(2700),
        rgb: None,
    };
    let mut plug_on = false;
    ctx.report_state(report(LAMP_LIGHT, State::Light(lamp.clone()), None)?);
    ctx.report_state(report(
        PLUG_SWITCH,
        State::Switch(SwitchState { on: plug_on }),
        None,
    )?);

    let mut readings =
        tokio::time::interval(Duration::from_secs(config.sensor_interval_secs.max(1)));
    let mut tick: u64 = 0;
    loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(incoming) = call else {
                    return Ok(()); // told to stop
                };
                let call = &incoming.call;
                let caused_by = Some(call.context.id.clone());
                let target = match call.service {
                    Service::LightTurnOn(_) | Service::LightTurnOff => LAMP_LIGHT,
                    Service::SwitchTurnOn | Service::SwitchTurnOff => PLUG_SWITCH,
                };
                if call.unique_id.as_str() != target {
                    // The core only sends calls for entities we described, so this is a bug.
                    let message = format!("the demo has no `{}` for {}", call.unique_id, call.service.name());
                    incoming.reply(Err(ServiceError::failed(message)));
                    continue;
                }
                let (entity, state) = match &call.service {
                    Service::LightTurnOn(data) => {
                        lamp.on = true;
                        lamp.brightness = data.brightness.or(lamp.brightness);
                        if let Some(kelvin) = data.color_temp_kelvin {
                            lamp.color_temp_kelvin = Some(kelvin);
                        }
                        (LAMP_LIGHT, State::Light(lamp.clone()))
                    }
                    Service::LightTurnOff => {
                        lamp.on = false;
                        (LAMP_LIGHT, State::Light(lamp.clone()))
                    }
                    Service::SwitchTurnOn | Service::SwitchTurnOff => {
                        plug_on = matches!(call.service, Service::SwitchTurnOn);
                        (PLUG_SWITCH, State::Switch(SwitchState { on: plug_on }))
                    }
                };
                incoming.reply(Ok(()));
                // A real device would confirm; the demo's virtual device always does.
                ctx.report_state(report(entity, state, caused_by)?);
            }
            _ = readings.tick() => {
                ctx.report_state(report(
                    SENSOR_MOTION,
                    State::BinarySensor(BinarySensorState { on: tick.is_multiple_of(3) }),
                    None,
                )?);
                ctx.report_state(report(
                    SENSOR_TEMPERATURE,
                    State::Sensor(SensorState { value: SensorValue::Number(temperature(tick)) }),
                    None,
                )?);
                tick += 1;
            }
        }
    }
}

/// Around 21 °C, drifting ±1.5 °C over about four minutes of readings, to one decimal.
fn temperature(tick: u64) -> f64 {
    let wave = (tick as f64 * std::f64::consts::TAU / 24.0).sin();
    ((21.0 + 1.5 * wave) * 10.0).round() / 10.0
}

async fn describe(ctx: &IntegrationContext) -> Result<(), IntegrationError> {
    ctx.describe_device(device(LAMP, "Demo lamp", "Virtual lamp")?)
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

    ctx.describe_device(device(PLUG, "Demo plug", "Virtual plug")?)
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

    ctx.describe_device(device(SENSOR, "Demo hallway sensor", "Virtual sensor")?)
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
    Ok(())
}

fn id(unique_id: &str) -> Result<UniqueId, IntegrationError> {
    Ok(UniqueId::try_from(unique_id)?)
}

fn device(unique_id: &str, name: &str, model: &str) -> Result<DeviceDescription, IntegrationError> {
    Ok(DeviceDescription {
        unique_id: id(unique_id)?,
        name: Name::try_from(name)?,
        manufacturer: Some("Irori".into()),
        model: Some(model.into()),
        sw_version: Some(env!("CARGO_PKG_VERSION").into()),
        hw_version: None,
        suggested_area: None,
        via_device_unique_id: None,
    })
}

fn report(
    unique_id: &str,
    state: State,
    caused_by: Option<ContextId>,
) -> Result<StateReport, IntegrationError> {
    Ok(StateReport {
        unique_id: id(unique_id)?,
        state: Some(state),
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

    #[test]
    fn temperature_stays_near_21() {
        for tick in 0..100 {
            let t = temperature(tick);
            assert!((19.5..=22.5).contains(&t), "{t}");
        }
    }
}
