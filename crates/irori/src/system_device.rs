//! Irori itself as a device in the home (`irori_hub`): its version, how long it has been up,
//! and how busy the machine is, as sensors an automation can watch like any other — "when the
//! disk is above 90 %, turn the plug's light red". A built-in protocol, like helpers, and never
//! waiting to be added (`irori_core::SYSTEM_PROTOCOL`).
//!
//! Readings change slowly and are sent every 30 seconds: history, and an SD card, shouldn't
//! take a write for every tick of a load average.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use irori_protocol::types::{
    Capabilities, DeviceDescription, EntityDescription, Name, ObjectId, SensorCapabilities,
    SensorClass, SensorState, SensorValue, SensorValueType, State, StateClass, StateReport,
    UniqueId,
};
use irori_protocol::{Protocol, ProtocolContext, ProtocolError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

/// Where the instance keeps its data, for the disk reading. Set once before extensions start.
pub static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

const EVERY: Duration = Duration::from_secs(30);
const DEVICE: &str = "hub";

/// Irori's own device.
#[derive(Debug)]
pub struct IroriDevice;

/// Nothing to set.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {}

impl Protocol for IroriDevice {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../assets/system-extension.toml");
    const ICON: Option<&'static str> = None;

    async fn run(_: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        run(ctx).await
    }
}

/// One reading: its id (`sensor.irori_<id>`), name, and how it's measured.
struct Reading {
    id: &'static str,
    name: &'static str,
    unit: Option<&'static str>,
    class: Option<SensorClass>,
    text: bool,
}

const READINGS: [Reading; 5] = [
    Reading {
        id: "version",
        name: "Version",
        unit: None,
        class: None,
        text: true,
    },
    Reading {
        id: "uptime",
        name: "Uptime",
        unit: Some("min"),
        class: None,
        text: false,
    },
    Reading {
        id: "cpu",
        name: "Processor use",
        unit: Some("%"),
        class: None,
        text: false,
    },
    Reading {
        id: "memory",
        name: "Memory use",
        unit: Some("%"),
        class: None,
        text: false,
    },
    Reading {
        id: "disk",
        name: "Disk use",
        unit: Some("%"),
        class: None,
        text: false,
    },
];

/// The processor's temperature, on a machine that says (a Raspberry Pi does).
const TEMPERATURE: Reading = Reading {
    id: "temperature",
    name: "Processor temperature",
    unit: Some("°C"),
    class: Some(SensorClass::Temperature),
    text: false,
};

fn unique(id: &str) -> Result<UniqueId, ProtocolError> {
    Ok(UniqueId::try_from(format!("{DEVICE}-{id}"))?)
}

async fn describe(ctx: &ProtocolContext, reading: &Reading) -> Result<(), ProtocolError> {
    ctx.describe_entity(EntityDescription {
        unique_id: unique(reading.id)?,
        name: Some(Name::try_from(reading.name)?),
        device_unique_id: Some(UniqueId::try_from(DEVICE)?),
        // `sensor.irori_uptime`: the device is Irori, so its entities read as Irori's.
        suggested_object_id: Some(ObjectId::try_from(format!("irori_{}", reading.id))?),
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: if reading.text {
                SensorValueType::Text
            } else {
                SensorValueType::Number
            },
            device_class: reading.class,
            unit: reading.unit.map(str::to_owned),
            state_class: (!reading.text).then_some(StateClass::Measurement),
            options: Vec::new(),
        }),
        entity_category: None,
    })
    .await?;
    Ok(())
}

async fn run(mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
    ctx.describe_device(DeviceDescription {
        unique_id: UniqueId::try_from(DEVICE)?,
        name: Name::try_from("Irori")?,
        manufacturer: Some("Irori".into()),
        model: Some("Home hub".into()),
        sw_version: Some(env!("CARGO_PKG_VERSION").into()),
        hw_version: None,
        suggested_area: None,
        via_device_unique_id: None,
    })
    .await?;
    for reading in &READINGS {
        describe(&ctx, reading).await?;
    }
    let has_temperature = temperature().is_some();
    if has_temperature {
        describe(&ctx, &TEMPERATURE).await?;
    }

    let started = Instant::now();
    let mut system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage()),
    );
    ctx.report_state(text("version", env!("CARGO_PKG_VERSION"))?);
    let mut every = tokio::time::interval(EVERY);
    loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(incoming) = call else {
                    return Ok(()); // told to stop
                };
                incoming.reply(Err(ServiceError::failed("Irori's readings can't be switched")));
            }
            _ = every.tick() => {
                system.refresh_cpu_usage();
                system.refresh_memory();
                #[allow(clippy::cast_precision_loss)] // minutes and percentages, rounded anyway
                {
                    ctx.report_state(number("uptime", (started.elapsed().as_secs() / 60) as f64)?);
                    ctx.report_state(number("cpu", round1(f64::from(system.global_cpu_usage())))?);
                    // The same memory Settings shows: a container's allowance, where there is one.
                    let (total, used) = crate::host_info::memory(&system);
                    if total > 0 {
                        let used = used as f64 / total as f64 * 100.0;
                        ctx.report_state(number("memory", round1(used))?);
                    }
                    if let Some(dir) = DATA_DIR.get() {
                        let disk = crate::host_info::disk(dir);
                        if disk.total > 0 {
                            ctx.report_state(number("disk", round1(disk.used as f64 / disk.total as f64 * 100.0))?);
                        }
                    }
                }
                if has_temperature && let Some(celsius) = temperature() {
                    ctx.report_state(number("temperature", round1(celsius))?);
                }
            }
        }
    }
}

fn report(id: &str, value: SensorValue) -> Result<StateReport, ProtocolError> {
    Ok(StateReport {
        unique_id: unique(id)?,
        state: Some(State::Sensor(SensorState { value })),
        attributes: Default::default(),
        caused_by: None,
        replayed: false,
    })
}

fn number(id: &str, value: f64) -> Result<StateReport, ProtocolError> {
    report(id, SensorValue::Number(value))
}

fn text(id: &str, value: &str) -> Result<StateReport, ProtocolError> {
    report(id, SensorValue::Text(value.to_owned()))
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

/// The processor's temperature in °C, from Linux's first thermal zone.
fn temperature() -> Option<f64> {
    let milli = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").ok()?;
    let milli: f64 = milli.trim().parse().ok()?;
    Some(milli / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_a_valid_built_in() {
        let builtin = irori_protocol::builtin::<IroriDevice>().expect("a valid built-in");
        assert_eq!(
            builtin.manifest.extension.id.as_str(),
            irori_core::SYSTEM_PROTOCOL
        );
        for reading in READINGS.iter().chain([&TEMPERATURE]) {
            assert!(unique(reading.id).is_ok());
            assert!(ObjectId::try_from(format!("irori_{}", reading.id)).is_ok());
        }
    }
}
