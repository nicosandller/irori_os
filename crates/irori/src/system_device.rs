//! Irori itself as a device in the home (`irori_hub`): its version, how long it has been up,
//! and how busy the machine is, as sensors an automation can watch like any other — "when the
//! disk is above 90 %, turn the plug's light red". A built-in protocol, like helpers, and never
//! waiting to be added (`irori_core::SYSTEM_PROTOCOL`).
//!
//! Its installed extensions are here too, each as an update (`update.irori_mqtt`) that says
//! when this build has a newer version of it and installs it (`extension_updates`). And so is
//! Irori (`update.irori`): whether a newer release is out, and, where this Irori can replace
//! itself, installing it (`self_update`).
//!
//! Readings change slowly and are sent every 30 seconds: history, and an SD card, shouldn't
//! take a write for every tick of a load average.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use irori_protocol::types::{
    Capabilities, DeviceDescription, EntityCategory, EntityDescription, ExtensionId, Name,
    ObjectId, SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, Service,
    State, StateClass, StateReport, UniqueId, UpdateCapabilities, UpdateState,
};
use irori_protocol::{Protocol, ProtocolContext, ProtocolError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

/// Where the instance keeps its data, for the disk reading. Set once before extensions start.
pub static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

const EVERY: Duration = Duration::from_secs(30);
const DEVICE: &str = "hub";

/// How often an extension that is being updated is looked at, to say so promptly when it's done.
const WHILE_UPDATING: Duration = Duration::from_secs(2);

/// How often Irori looks for a newer release of itself, and how soon after starting.
const LOOKS_EVERY: Duration = Duration::from_secs(12 * 60 * 60);
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(60);

/// How long after an update was placed its new version is waited for before the entity stops
/// saying it's installing. The host reads the new manifest as it starts the extension again.
const NEW_VERSION_WITHIN: Duration = Duration::from_secs(60);

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
    let mut extensions = Extensions::default();
    let mut while_updating = tokio::time::interval(WHILE_UPDATING);
    let (done, mut updated) = tokio::sync::mpsc::channel::<(ExtensionId, Result<(), String>)>(4);
    let mut own = Own::default();
    own.describe(&ctx).await?;
    own.report(&ctx)?;
    let (heard, mut about_irori) = tokio::sync::mpsc::channel::<Heard>(4);
    let mut looks =
        tokio::time::interval_at(tokio::time::Instant::now() + FIRST_LOOK_AFTER, LOOKS_EVERY);
    let looks_unasked = crate::self_update::LOOKS.get().copied().unwrap_or(true);
    let look = |heard: tokio::sync::mpsc::Sender<Heard>| {
        tokio::spawn(async move {
            let _ = heard
                .send(Heard::Latest(crate::self_update::latest().await))
                .await;
        });
    };
    loop {
        tokio::select! {
            _ = looks.tick(), if looks_unasked => look(heard.clone()),
            Some(said) = about_irori.recv() => {
                own.hear(said);
                own.report(&ctx)?;
            }
            call = ctx.next_call() => {
                let Some(incoming) = call else {
                    return Ok(()); // told to stop
                };
                if incoming.call.unique_id == own_id()? {
                    match (&incoming.call.service, own.wanted()) {
                        (Service::UpdateCheck, _) => {
                            incoming.reply(Ok(()));
                            look(heard.clone());
                        }
                        (Service::UpdateInstall, Some(version)) => {
                            incoming.reply(Ok(()));
                            own.begin();
                            own.report(&ctx)?;
                            let heard = heard.clone();
                            tokio::spawn(async move {
                                let result = crate::self_update::install(&version).await;
                                let _ = heard.send(Heard::Installed(result)).await;
                            });
                        }
                        _ => incoming.reply(Err(ServiceError::failed(
                            "There is no newer Irori to install.",
                        ))),
                    }
                    continue;
                }
                let extension = extensions.of(&incoming.call.unique_id);
                match (&incoming.call.service, extension) {
                    (Service::UpdateInstall, Some(extension)) => {
                        incoming.reply(Ok(()));
                        extensions.begin(&extension);
                        extensions.sync(&ctx).await?;
                        let done = done.clone();
                        tokio::spawn(async move {
                            let result = crate::extension_updates::update(&extension).await;
                            let _ = done.send((extension, result)).await;
                        });
                    }
                    _ => incoming.reply(Err(ServiceError::failed(
                        "Irori's readings can't be switched",
                    ))),
                }
            }
            Some((extension, result)) = updated.recv() => {
                extensions.finish(&extension, result.err());
                extensions.sync(&ctx).await?;
            }
            _ = while_updating.tick(), if extensions.busy() => {
                extensions.sync(&ctx).await?;
            }
            _ = every.tick() => {
                extensions.sync(&ctx).await?;
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

/// What came back about Irori's own update.
enum Heard {
    Latest(Result<crate::self_update::Release, String>),
    Installed(Result<(), String>),
}

fn own_id() -> Result<UniqueId, ProtocolError> {
    unique("update")
}

/// Irori's own update: the latest release it has heard of, and how an install is going.
#[derive(Debug, Default)]
struct Own {
    latest: Option<crate::self_update::Release>,
    installing: bool,
    failed: Option<String>,
}

impl Own {
    fn installed() -> Option<irori_protocol::types::Version> {
        irori_protocol::types::Version::try_from(crate::build_info::VERSION.to_owned()).ok()
    }

    /// The release to install, when there is a newer one.
    fn wanted(&self) -> Option<irori_protocol::types::Version> {
        let latest = &self.latest.as_ref()?.version;
        crate::self_update::newer(&Self::installed()?, latest).then(|| latest.clone())
    }

    fn begin(&mut self) {
        self.installing = true;
        self.failed = None;
    }

    fn hear(&mut self, said: Heard) {
        match said {
            Heard::Latest(Ok(release)) => self.latest = Some(release),
            // Offline, or GitHub is: what was last heard stands, and the next look tries again.
            Heard::Latest(Err(why)) => tracing::debug!(%why, "couldn't look for a newer Irori"),
            // A restart is on its way; it stays "installing" until this process is replaced.
            Heard::Installed(Ok(())) => {}
            Heard::Installed(Err(why)) => {
                self.installing = false;
                self.failed = Some(why);
            }
        }
    }

    async fn describe(&self, ctx: &ProtocolContext) -> Result<(), ProtocolError> {
        ctx.describe_entity(EntityDescription {
            unique_id: own_id()?,
            name: Some(Name::try_from("Irori")?),
            device_unique_id: Some(UniqueId::try_from(DEVICE)?),
            suggested_object_id: Some(ObjectId::try_from("irori")?),
            capabilities: Capabilities::Update(UpdateCapabilities {
                device_class: None,
                install: crate::self_update::installable(),
                check: true,
            }),
            entity_category: Some(EntityCategory::Config),
        })
        .await?;
        Ok(())
    }

    fn report(&self, ctx: &ProtocolContext) -> Result<(), ProtocolError> {
        let latest = self.latest.as_ref();
        ctx.report_state(StateReport {
            unique_id: own_id()?,
            state: Some(State::Update(UpdateState {
                available: self.wanted().is_some(),
                installed_version: Some(crate::build_info::VERSION.to_owned()),
                latest_version: latest.map(|release| release.version.to_string()),
                in_progress: self.installing,
                title: Some("Irori".to_owned()),
                release_summary: latest.and_then(|release| release.summary.clone()),
                release_url: latest.and_then(|release| release.url.clone()),
                last_error: self.failed.clone(),
                ..UpdateState::default()
            })),
            attributes: Default::default(),
            caused_by: None,
            replayed: false,
        });
        Ok(())
    }
}

/// An extension's update as it was last described and reported.
#[derive(Debug, Default)]
struct Extensions {
    /// What was last reported for each, so a report is only sent when it says something new.
    known: BTreeMap<ExtensionId, Option<UpdateState>>,
    updating: BTreeMap<ExtensionId, Updating>,
    /// Why the last update of each failed, until it's tried again or its version changes.
    failed: BTreeMap<ExtensionId, (String, String)>,
}

#[derive(Debug)]
struct Updating {
    /// The version it had when the update was asked for.
    from: Option<String>,
    /// When the new package was put in place; `None` while it's still being fetched.
    placed: Option<Instant>,
}

fn update_id(extension: &ExtensionId) -> Result<UniqueId, ProtocolError> {
    unique(&format!("update-{extension}"))
}

impl Extensions {
    /// The extension whose update `unique_id` is.
    fn of(&self, unique_id: &UniqueId) -> Option<ExtensionId> {
        self.known
            .keys()
            .find(|extension| update_id(extension).is_ok_and(|id| id == *unique_id))
            .cloned()
    }

    fn busy(&self) -> bool {
        !self.updating.is_empty()
    }

    fn begin(&mut self, extension: &ExtensionId) {
        self.failed.remove(extension);
        let from = self
            .known
            .get(extension)
            .and_then(|state| state.as_ref()?.installed_version.clone());
        self.updating
            .insert(extension.clone(), Updating { from, placed: None });
    }

    fn finish(&mut self, extension: &ExtensionId, failure: Option<String>) {
        match failure {
            Some(why) => {
                if let Some(updating) = self.updating.remove(extension) {
                    self.failed
                        .insert(extension.clone(), (updating.from.unwrap_or_default(), why));
                }
            }
            // Still installing until the extension has started again and says its new version.
            None => {
                if let Some(updating) = self.updating.get_mut(extension) {
                    updating.placed = Some(Instant::now());
                }
            }
        }
    }

    /// Describes the extensions that are installed now, forgets the ones that aren't, and
    /// reports each one whose update changed.
    async fn sync(&mut self, ctx: &ProtocolContext) -> Result<(), ProtocolError> {
        let offered = crate::extension_updates::offered_now();
        // One that was put in place and never came back with a version (it won't start, or its
        // manifest can't be read) isn't being updated any more.
        self.updating.retain(|extension, updating| {
            offered.iter().any(|offered| offered.id == *extension)
                || !updating
                    .placed
                    .is_some_and(|placed| placed.elapsed() >= NEW_VERSION_WITHIN)
        });
        let gone: Vec<ExtensionId> = self
            .known
            .keys()
            .filter(|known| !offered.iter().any(|offered| offered.id == **known))
            .cloned()
            .collect();
        for extension in gone {
            // Being replaced: it's back in a moment, and its update with it.
            if self.updating.contains_key(&extension) {
                continue;
            }
            self.known.remove(&extension);
            self.failed.remove(&extension);
            let _ = ctx.remove_entity(update_id(&extension)?).await;
        }
        for offered in offered {
            if !self.known.contains_key(&offered.id) {
                ctx.describe_entity(EntityDescription {
                    unique_id: update_id(&offered.id)?,
                    name: Some(Name::try_from(format!("{} extension", offered.name))?),
                    device_unique_id: Some(UniqueId::try_from(DEVICE)?),
                    suggested_object_id: Some(ObjectId::try_from(format!("irori_{}", offered.id))?),
                    capabilities: Capabilities::Update(UpdateCapabilities {
                        device_class: None,
                        // Full access to this machine is approved by a person, on the
                        // Extensions page, every time.
                        install: !offered.full_access,
                        check: false,
                    }),
                    entity_category: Some(EntityCategory::Config),
                })
                .await?;
                self.known.insert(offered.id.clone(), None);
            }
            if let Some(updating) = self.updating.get(&offered.id) {
                let arrived = updating.from.as_deref() != Some(offered.installed.as_str());
                let waited = updating
                    .placed
                    .is_some_and(|placed| placed.elapsed() >= NEW_VERSION_WITHIN);
                if updating.placed.is_some() && (arrived || waited) {
                    self.updating.remove(&offered.id);
                }
            }
            if self
                .failed
                .get(&offered.id)
                .is_some_and(|(from, _)| *from != offered.installed)
            {
                self.failed.remove(&offered.id);
            }
            let state = UpdateState {
                available: offered.installed != offered.latest,
                in_progress: self.updating.contains_key(&offered.id),
                title: Some(format!("{} extension", offered.name)),
                last_error: self.failed.get(&offered.id).map(|(_, why)| why.clone()),
                installed_version: Some(offered.installed),
                latest_version: Some(offered.latest),
                ..UpdateState::default()
            };
            if self.known.get(&offered.id) != Some(&Some(state.clone())) {
                ctx.report_state(StateReport {
                    unique_id: update_id(&offered.id)?,
                    state: Some(State::Update(state.clone())),
                    attributes: Default::default(),
                    caused_by: None,
                    replayed: false,
                });
                self.known.insert(offered.id, Some(state));
            }
        }
        Ok(())
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
