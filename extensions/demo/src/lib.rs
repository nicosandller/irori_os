//! Virtual devices, for trying Irori without hardware: a whole house of them. Also the reference
//! protocol: copy this to start a new one (`docs/specs/protocols.md`).
//!
//! The house, room by room (`gadgets`):
//!
//! - **Dining room:** two door contacts, an mmWave presence sensor and a vibration sensor.
//! - **Garage:** the garage door opener, a ceiling light and a camera.
//! - **Foyer:** a smart lock with a door sensor, a ceiling light and a movement sensor.
//! - **Approach:** a video doorbell, the switch for the outdoor lights and a sensor in the
//!   mailbox.
//! - **Living room:** a door contact, a wall light, a ceiling light, a colour lamp, the TV, a
//!   camera, a temperature and humidity sensor and an mmWave presence sensor.
//! - **Kitchen:** plugs that meter the fridge and the dishwasher, a ceiling light, a thermostat,
//!   a smoke detector and a water leak sensor.
//! - **Garden:** a temperature and humidity sensor, a soil sensor, a light sensor and two
//!   cameras.
//! - **Basement:** an electric water boiler.
//!
//! Everything moves the way it would over a day, but a day lasts a minute (`world`): dark until
//! dawn, the house waking up, everyone out, back in the evening. Batteries run down from full to
//! empty over that minute, then start again.

mod gadgets;
mod world;

use std::collections::BTreeMap;
use std::time::Duration;

use irori_protocol::types::{ContextId, DeviceDescription, Name, State, StateReport, UniqueId};
use irori_protocol::{Incoming, Protocol, ProtocolContext, ProtocolError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::world::World;

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

async fn run(config: Config, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
    gadgets::describe(&ctx).await?;
    let mut house = gadgets::Gadgets::new();
    let started = tokio::time::Instant::now();
    for (entity, state) in house.tick(started, &World::at(0)) {
        ctx.report_state(report(entity, Some(state), None)?);
    }
    for (entity, state) in house.states() {
        ctx.report_state(report(entity, Some(state), None)?);
    }

    let mut readings = tokio::time::interval(Duration::from_secs(config.sensor_interval_secs));
    let mut tick: u64 = 0;
    loop {
        tokio::select! {
            incoming = ctx.next() => {
                match incoming {
                    None => return Ok(()), // told to stop
                    Some(Incoming::Call(incoming)) => {
                        let call = &incoming.call;
                        let caused_by = Some(call.context.id.clone());
                        match house.call(
                            call.unique_id.as_str(),
                            &call.service,
                            tokio::time::Instant::now(),
                        ) {
                            Ok((entity, state)) => {
                                incoming.reply(Ok(()));
                                ctx.report_state(report(entity, Some(state), caused_by)?);
                            }
                            Err(message) => incoming.reply(Err(ServiceError::failed(message))),
                        }
                    }
                    Some(Incoming::Picture(asked)) => {
                        let picture = house.picture(asked.unique_id.as_str());
                        asked.reply(picture);
                    }
                    Some(Incoming::Action(action)) => {
                        action.reply(Err("the demo has no actions".into()));
                    }
                    Some(Incoming::Unpair(unpair)) => {
                        unpair.reply(Err("the demo's devices aren't paired".into()));
                    }
                }
            }
            _ = readings.tick() => {
                let secs = tick * config.sensor_interval_secs;
                let world = World::at(secs);
                for (entity, state) in house.tick(tokio::time::Instant::now(), &world) {
                    ctx.report_state(report(entity, Some(state), None)?);
                }
                if tick > 0 {
                    let since = secs - config.sensor_interval_secs;
                    for (entity, state) in gadgets::rings(since, secs) {
                        ctx.report_state(report(entity, Some(state), None)?);
                    }
                }
                tick += 1;
            }
        }
    }
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

    /// The TV goes through every state once a minute.
    #[test]
    fn the_tv_goes_through_its_script() {
        for state in ["off", "idle", "playing", "paused"] {
            assert!((0..300).any(|s| gadgets::tv::word(s) == state), "{state}");
        }
        assert_eq!(
            (
                gadgets::tv::word(0),
                gadgets::tv::word(59),
                gadgets::tv::word(60)
            ),
            ("playing", "playing", "paused")
        );
    }
}
