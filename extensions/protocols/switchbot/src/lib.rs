//! SwitchBot devices, through SwitchBot's cloud API and over Bluetooth.
//!
//! One device is one SwitchBot device, keyed by its MAC address, which is both its cloud
//! `deviceId` and the address its Bluetooth adverts carry. The cloud is polled one device at a
//! time, paced so the day's share of SwitchBot's 10 000 calls lasts until midnight UTC, with
//! doors, leaks and locks polled twice as often as the rest. A device heard over Bluetooth lately
//! isn't polled at all: its adverts say the same, sooner. Everything said about a device, by
//! either route, is merged into one reading per device, which outlives a restart.

mod ble;
mod cloud;
mod map;
mod model;
mod network;
pub mod settings;
mod sign;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use irori_cloud::{Backoff, Budget, Period};
use irori_protocol::{
    AvailabilityTarget, Health, IncomingCall, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{
    Availability, ContextId, DeviceDescription, EntityDescription, Name, ObjectId, StateReport,
    UniqueId, Unmodeled,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::cloud::{Cloud, CloudError, DeviceList, Listed};
use crate::model::{Family, Reading};
use crate::settings::Settings;

/// The SwitchBot protocol.
#[derive(Debug)]
pub struct SwitchBot;

impl Protocol for SwitchBot {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let cloud = match settings.account() {
            Some(account) => {
                let http =
                    irori_cloud::client(concat!("irori-switchbot/", env!("CARGO_PKG_VERSION")))
                        .map_err(|error| ProtocolError::new(format!("no HTTPS client: {error}")))?;
                Some(Cloud::new(http, cloud::API, account))
            }
            None => None,
        };
        let radio = settings.bluetooth.then(|| {
            let (radio, heard) = mpsc::channel(256);
            tokio::spawn(ble::keep_listening(
                usize::from(settings.bluetooth_adapter),
                radio,
            ));
            heard
        });
        drive(settings, ctx, cloud, radio, Timing::REAL).await
    }
}

/// How long things take. Tests shorten them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// After a command, how long until the device is read again to see it happened.
    pub recheck: Duration,
    /// How often the device list is fetched again, for devices added or removed in the app.
    pub relist: Duration,
    /// The shortest gap between two status calls, whatever the budget allows.
    pub min_gap: Duration,
    /// The shortest time between two reads of the same device.
    pub per_device: Duration,
    /// How long to wait before asking again with a token SwitchBot refused.
    pub refused: Duration,
    /// How long a device counts as nearby after its last Bluetooth advert. Nearby devices aren't
    /// polled; one that only Bluetooth knows is unavailable once it's been quiet this long.
    pub heard: Duration,
}

impl Timing {
    const REAL: Timing = Timing {
        recheck: Duration::from_secs(6),
        relist: Duration::from_secs(6 * 3600),
        min_gap: Duration::from_secs(5),
        per_device: Duration::from_secs(60),
        refused: Duration::from_secs(3600),
        heard: Duration::from_secs(600),
    };
}

/// Calls kept back from polling for commands, out of every hundred.
const COMMAND_RESERVE_PERCENT: u64 = 10;

/// What the extension keeps between runs, beside each device's reading.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ledger {
    budget: Budget,
    /// The UTC day (days since 1970) the token last worked. A refusal after it worked the same
    /// day is the day's calls running out, not a wrong token: SwitchBot says the same for both.
    worked_on: Option<u64>,
}

const LEDGER_KEY: &str = "ledger";
const DEVICES_KEY: &str = "devices";

/// A device as the last run knew it: enough to describe it and report its states before
/// SwitchBot answers, which on a day whose calls are spent is after midnight UTC.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Saved {
    listed: Listed,
    reading: Reading,
    #[serde(default = "yes")]
    from_cloud: bool,
}

fn yes() -> bool {
    true
}

/// A device Irori can use: listed by the account, heard over Bluetooth, or both.
#[derive(Debug)]
struct Known {
    /// What the account lists, or for one only Bluetooth knows, what its adverts say.
    listed: Listed,
    /// The account lists it. One that isn't is left alone by the account's listings.
    from_cloud: bool,
    family: Family,
    reading: Reading,
    /// The entity keys described for it, so ones that drop out can be removed.
    described: BTreeSet<&'static str>,
    /// The firmware version it was described with.
    version: Option<String>,
    available: bool,
    last_read: Option<Instant>,
}

impl Known {
    fn new(listed: Listed, family: Family, reading: Reading) -> Self {
        Self {
            listed,
            from_cloud: true,
            family,
            reading,
            described: BTreeSet::new(),
            version: None,
            available: true,
            last_read: None,
        }
    }
}

/// What's keeping the extension from working as it should, worst first.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Trouble {
    /// No token and secret key yet.
    NoAccount,
    /// SwitchBot refused the token and secret key, and they never worked today.
    Refused,
    /// The day's calls are spent: by Irori, or by another app on the account.
    Spent,
    Unreachable(String),
}

/// The last Bluetooth advert from a device.
#[derive(Debug, Clone, Copy)]
struct Ear {
    at: Instant,
    rssi: Option<i16>,
}

/// How the Bluetooth radio is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RadioState {
    Off,
    Listening,
    Failed(String),
}

/// Work done off the loop, coming back.
#[derive(Debug)]
enum Done {
    Listed(Result<DeviceList, CloudError>),
    Status {
        id: String,
        result: Result<serde_json::Map<String, serde_json::Value>, CloudError>,
        caused_by: Option<ContextId>,
    },
    /// A command went through: read the device again soon, on behalf of the call.
    Commanded {
        id: String,
        caused_by: ContextId,
    },
    /// A call failed in a way that says something about the account (refused, too many).
    Failed(CloudError),
    Recheck {
        id: String,
        caused_by: ContextId,
    },
}

struct World {
    ctx: ProtocolContext,
    cloud: Option<Cloud>,
    timing: Timing,
    ledger: Ledger,
    devices: BTreeMap<String, Known>,
    /// A device list is on its way.
    listing: bool,
    /// The order devices are polled in; urgent ones appear twice.
    rotation: Vec<String>,
    next_in_rotation: usize,
    /// Devices new to this run, read once each before the paced rotation, so their states show
    /// within seconds of starting.
    sweep: VecDeque<String>,
    /// Nothing is polled before this.
    quiet_until: Option<Instant>,
    trouble: Option<Trouble>,
    reported: Option<Health>,
    readings_dirty: bool,
    /// The call count changed since it was last stored. It's stored once a minute, not after
    /// every call; a crash loses at most a minute of counting.
    ledger_dirty: bool,
    radio: RadioState,
    /// The devices heard over Bluetooth, and when last.
    ears: BTreeMap<String, Ear>,
    /// Something on the map changed since it was last drawn.
    map_dirty: bool,
    done: mpsc::Sender<Done>,
}

pub(crate) async fn drive(
    settings: Settings,
    ctx: ProtocolContext,
    cloud: Option<Cloud>,
    mut radio: Option<mpsc::Receiver<ble::Radio>>,
    timing: Timing,
) -> Result<(), ProtocolError> {
    let now = unix_now();
    let ledger: Option<Ledger> = load(&ctx, LEDGER_KEY).await;
    let limit = u64::from(settings.daily_calls);
    let ledger = Ledger {
        budget: Budget::restored(
            ledger.as_ref().map(|l| l.budget.clone()),
            Period::Day,
            limit,
            now,
        ),
        worked_on: ledger.and_then(|l| l.worked_on),
    };
    let saved: Vec<Saved> = load(&ctx, DEVICES_KEY).await.unwrap_or_default();
    let (done, mut finished) = mpsc::channel(64);
    let mut world = World {
        ctx,
        trouble: cloud.is_none().then_some(Trouble::NoAccount),
        cloud,
        timing,
        ledger,
        devices: BTreeMap::new(),
        listing: false,
        rotation: Vec::new(),
        next_in_rotation: 0,
        sweep: VecDeque::new(),
        quiet_until: None,
        reported: None,
        readings_dirty: false,
        ledger_dirty: false,
        radio: if radio.is_some() {
            RadioState::Listening
        } else {
            RadioState::Off
        },
        ears: BTreeMap::new(),
        map_dirty: false,
        done,
    };
    world.restore(saved).await;
    world.report_health().await;

    let mut list_backoff = Backoff::new();
    let mut next_list = world.cloud.is_some().then(Instant::now);
    let mut next_poll = Instant::now() + timing.min_gap;
    let mut save = tokio::time::interval(Duration::from_secs(60));
    save.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut redraw = tokio::time::interval(Duration::from_secs(10));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let list_at = next_list.unwrap_or_else(far_future);
        tokio::select! {
            call = world.ctx.next_call() => {
                let Some(call) = call else { break };
                world.command(call).await;
            }
            _ = tokio::time::sleep_until(list_at), if next_list.is_some() => {
                next_list = None;
                world.list();
            }
            _ = tokio::time::sleep_until(next_poll) => {
                world.poll_next().await;
                next_poll = Instant::now() + world.gap();
            }
            Some(done) = finished.recv() => match done {
                Done::Listed(Ok(list)) => {
                    world.listing = false;
                    list_backoff.succeeded();
                    world.worked().await;
                    world.adopt(list).await;
                    next_list = Some(Instant::now() + timing.relist);
                }
                Done::Listed(Err(error)) => {
                    world.listing = false;
                    let wait = match &error {
                        CloudError::Unauthorized if !world.worked_today() => timing.refused,
                        _ => list_backoff.failed().max(world.wait_for(&error)),
                    };
                    world.failed(error).await;
                    next_list = Some(Instant::now() + wait);
                }
                Done::Status { id, result, caused_by } => {
                    world.status(&id, result, caused_by).await;
                }
                Done::Commanded { id, caused_by } => {
                    world.worked().await;
                    let done = world.done.clone();
                    let wait = timing.recheck;
                    tokio::spawn(async move {
                        tokio::time::sleep(wait).await;
                        let _ = done.send(Done::Recheck { id, caused_by }).await;
                    });
                }
                Done::Recheck { id, caused_by } => {
                    world.read(&id, Some(caused_by)).await;
                }
                Done::Failed(error) => world.failed(error).await,
            },
            event = next_radio(&mut radio) => match event {
                Some(ble::Radio::Advert(advert)) => world.hear(&advert).await,
                Some(ble::Radio::Failed(why)) => world.deaf(why),
                None => radio = None,
            },
            _ = redraw.tick() => world.redraw().await,
            _ = save.tick() => world.save().await,
        }
        world.report_health().await;
    }
    world.save().await;
    Ok(())
}

impl World {
    fn worked_today(&self) -> bool {
        self.ledger.worked_on == Some(unix_now() / 86_400)
    }

    /// A call went through: the token is good today.
    async fn worked(&mut self) {
        let today = unix_now() / 86_400;
        if matches!(
            self.trouble,
            Some(Trouble::Refused | Trouble::Spent | Trouble::Unreachable(_))
        ) {
            self.trouble = None;
            self.quiet_until = None;
        }
        if self.ledger.worked_on != Some(today) {
            self.ledger.worked_on = Some(today);
            self.save_ledger().await;
        }
    }

    /// How long a failure asks the extension to stay quiet.
    fn wait_for(&self, error: &CloudError) -> Duration {
        match error {
            CloudError::TooMany(after) => after.unwrap_or(Duration::from_secs(60)),
            CloudError::Unauthorized => {
                Duration::from_secs(self.ledger.budget.resets_at().saturating_sub(unix_now()) + 60)
            }
            _ => Duration::ZERO,
        }
    }

    async fn failed(&mut self, error: CloudError) {
        match &error {
            CloudError::Unauthorized if self.worked_today() => {
                // It worked earlier today: the day's calls are spent, maybe by another app.
                self.ledger.budget.exhaust(unix_now());
                self.save_ledger().await;
                self.trouble = Some(Trouble::Spent);
                self.quiet_until = Some(Instant::now() + self.wait_for(&error));
            }
            CloudError::Unauthorized => {
                self.trouble = Some(Trouble::Refused);
                self.quiet_until = Some(Instant::now() + self.timing.refused);
            }
            CloudError::TooMany(_) => {
                self.quiet_until = Some(Instant::now() + self.wait_for(&error));
            }
            CloudError::Unreachable(why)
                if !matches!(self.trouble, Some(Trouble::Refused | Trouble::Spent)) =>
            {
                self.trouble = Some(Trouble::Unreachable(why.clone()));
            }
            _ => {}
        }
    }

    /// Fetches the device list, off the loop.
    fn list(&mut self) {
        let Some(cloud) = self.cloud.clone() else {
            return;
        };
        if self.listing {
            return;
        }
        self.listing = true;
        self.ledger.budget.spend(1, unix_now());
        self.ledger_dirty = true;
        let done = self.done.clone();
        tokio::spawn(async move {
            let _ = done.send(Done::Listed(cloud.devices().await)).await;
        });
    }

    /// Describes the devices the last run knew and reports their last readings, before SwitchBot
    /// has answered anything. Each is read again first thing once SwitchBot answers.
    async fn restore(&mut self, saved: Vec<Saved>) {
        let listed: Vec<Listed> = saved.iter().map(|s| s.listed.clone()).collect();
        let mut readings: BTreeMap<String, (Reading, bool)> = saved
            .into_iter()
            .map(|s| (s.listed.id, (s.reading, s.from_cloud)))
            .collect();
        for listed in hubs_first(&listed) {
            let Some(family) = Family::of(&listed.device_type) else {
                continue;
            };
            let (reading, from_cloud) = readings.remove(&listed.id).unwrap_or_default();
            let mut known = Known::new(listed.clone(), family, reading);
            known.from_cloud = from_cloud;
            if self.describe(&mut known).await.is_ok() {
                self.report(&known, None, true);
            }
            if !family.silent() {
                self.sweep.push_back(known.listed.id.clone());
            }
            self.devices.insert(known.listed.id.clone(), known);
        }
        self.draw().await;
        self.rotate();
    }

    async fn draw(&mut self) {
        let devices = self.devices.values().map(|known| network::Drawn {
            listed: &known.listed,
            through_cloud: known.from_cloud,
            heard: self
                .ears
                .get(&known.listed.id)
                .filter(|ear| ear.at.elapsed() < self.timing.heard)
                .map(|ear| ear.rssi),
        });
        let map = network::draw(devices, self.cloud.is_some());
        self.ctx.set_networks(vec![map]).await;
        self.map_dirty = false;
    }

    fn heard_lately(&self, id: &str) -> bool {
        self.ears
            .get(id)
            .is_some_and(|ear| ear.at.elapsed() < self.timing.heard)
    }

    /// Every ten seconds: forgets devices quiet for too long (one only Bluetooth knows goes
    /// unavailable, one the account lists is polled again), and draws the map if it changed.
    async fn redraw(&mut self) {
        let quiet: Vec<String> = self
            .ears
            .iter()
            .filter(|(_, ear)| ear.at.elapsed() >= self.timing.heard)
            .map(|(id, _)| id.clone())
            .collect();
        for id in quiet {
            self.ears.remove(&id);
            self.map_dirty = true;
            if self.devices.get(&id).is_some_and(|known| !known.from_cloud) {
                self.set_available(&id, false).await;
            }
        }
        if self.map_dirty {
            self.draw().await;
        }
    }

    /// Bluetooth can't be used.
    fn deaf(&mut self, why: String) {
        if self.radio != RadioState::Failed(why.clone()) {
            tracing::warn!("SwitchBot over Bluetooth: {why}");
            self.radio = RadioState::Failed(why);
        }
    }

    /// Takes in a Bluetooth advert: a device the account lists, or with no account, any
    /// SwitchBot device nearby (the core keeps a new one aside until someone adds it).
    async fn hear(&mut self, advert: &ble::Advert) {
        let Some(mac) = advert.mac() else {
            return;
        };
        let hint = self
            .devices
            .get(&mac)
            .map(|known| known.listed.device_type.clone());
        let Some(heard) = ble::parse(advert, hint.as_deref()) else {
            return;
        };
        self.radio = RadioState::Listening;
        if !self.devices.contains_key(&mac) {
            // With an account, only its devices: a neighbour's SwitchBot is the neighbour's.
            if self.cloud.is_some() {
                return;
            }
            let Some(family) = Family::of(heard.device_type) else {
                return;
            };
            let listed = Listed {
                name: format!(
                    "{} {}",
                    crate::model::readable(heard.device_type),
                    &mac[mac.len().saturating_sub(4)..]
                ),
                id: mac.clone(),
                device_type: heard.device_type.to_owned(),
                hub: None,
            };
            let mut known = Known::new(listed, family, Reading::default());
            known.from_cloud = false;
            if self.describe(&mut known).await.is_err() {
                return;
            }
            self.devices.insert(mac.clone(), known);
            self.readings_dirty = true;
        }
        let ear = Ear {
            at: Instant::now(),
            rssi: heard.rssi,
        };
        let quality = |ear: &Ear| {
            ear.rssi
                .map(|dbm| irori_types::LinkQuality::from_rssi(i64::from(dbm)))
        };
        match self.ears.insert(mac.clone(), ear) {
            Some(before) if quality(&before) == quality(&ear) => {}
            _ => self.map_dirty = true,
        }
        let Some(mut known) = self.devices.remove(&mac) else {
            return;
        };
        let before = known.reading.clone();
        known.reading.merge(heard.reading);
        if known.reading != before {
            self.readings_dirty = true;
            let keys = map::entities(known.family, &known.reading);
            if keys.len() != known.described.len()
                || keys.iter().any(|e| !known.described.contains(e.key))
            {
                let _ = self.describe(&mut known).await;
            }
            self.report(&known, None, false);
        }
        self.devices.insert(mac.clone(), known);
        self.set_available(&mac, true).await;
    }

    /// Takes in a fresh device list: describes what's new or changed, removes what's gone,
    /// and lists what Irori can't use.
    async fn adopt(&mut self, list: DeviceList) {
        let ids: BTreeSet<String> = list.devices.iter().map(|d| d.id.clone()).collect();
        let hubs: BTreeSet<String> = list
            .devices
            .iter()
            .filter(|d| is_hub(d))
            .map(|d| d.id.clone())
            .collect();
        let gone: Vec<String> = self
            .devices
            .iter()
            .filter(|(id, known)| known.from_cloud && !ids.contains(*id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            // Deleted in the app but still nearby: Bluetooth keeps it.
            if self.heard_lately(&id)
                && let Some(known) = self.devices.get_mut(&id)
            {
                known.from_cloud = false;
                known.listed.hub = None;
                continue;
            }
            self.devices.remove(&id);
            self.readings_dirty = true;
            if let Ok(unique_id) = UniqueId::try_from(id.as_str()) {
                let _ = self.ctx.remove_device(unique_id).await;
            }
        }

        let mut unmodeled = Vec::new();
        for listed in hubs_first(&list.devices) {
            let Some(family) = Family::of(&listed.device_type) else {
                unmodeled.push(Unmodeled {
                    device_unique_id: None,
                    platform: platform(&listed.device_type),
                    name: Name::try_from(listed.name.as_str()).ok(),
                    reason: None,
                });
                continue;
            };
            let mut listed = listed.clone();
            listed.hub = listed.hub.filter(|hub| hubs.contains(hub));
            if let Some(known) = self.devices.get_mut(&listed.id) {
                known.from_cloud = true;
            }
            let changed = self
                .devices
                .get(&listed.id)
                .is_none_or(|known| known.listed != listed);
            if !changed {
                continue;
            }
            if !self.devices.contains_key(&listed.id) && !family.silent() {
                self.sweep.push_back(listed.id.clone());
            }
            let mut known = match self.devices.remove(&listed.id) {
                Some(known) => Known {
                    family,
                    listed,
                    ..known
                },
                None => Known::new(listed, family, Reading::default()),
            };
            self.readings_dirty = true;
            if self.describe(&mut known).await.is_ok() {
                self.report(&known, None, true);
            }
            self.devices.insert(known.listed.id.clone(), known);
        }
        for remote in &list.remotes {
            unmodeled.push(Unmodeled {
                device_unique_id: None,
                platform: platform("infrared remote"),
                name: Name::try_from(remote.name.as_str()).ok(),
                reason: Some("Infrared remotes come in a later version".into()),
            });
        }
        self.ctx.set_unmodeled(unmodeled).await;
        self.draw().await;
        self.rotate();
    }

    /// Describes a device and the entities it has now, and removes entities it no longer has.
    async fn describe(&self, known: &mut Known) -> Result<(), ()> {
        let device_id = UniqueId::try_from(known.listed.id.as_str()).map_err(drop)?;
        let name = Name::try_from(known.listed.name.as_str())
            .or_else(|_| Name::try_from(known.listed.device_type.as_str()))
            .map_err(drop)?;
        let device = DeviceDescription {
            unique_id: device_id.clone(),
            name,
            manufacturer: Some("SwitchBot".into()),
            model: Some(crate::model::readable(&known.listed.device_type).to_owned()),
            sw_version: known.reading.version.clone(),
            hw_version: None,
            serial_number: None,
            suggested_area: None,
            via_device_unique_id: known
                .listed
                .hub
                .as_deref()
                .and_then(|hub| UniqueId::try_from(hub).ok()),
        };
        if self.ctx.describe_device(device).await.is_err() {
            tracing::warn!("a SwitchBot device couldn't be added");
            return Err(());
        }
        known.version = known.reading.version.clone();
        let entities = map::entities(known.family, &known.reading);
        let keys: BTreeSet<&'static str> = entities.iter().map(|e| e.key).collect();
        for entity in entities {
            if known.described.contains(entity.key) {
                continue;
            }
            let description = EntityDescription {
                unique_id: entity_id(&known.listed.id, entity.key)?,
                name: entity.name.and_then(|name| Name::try_from(name).ok()),
                device_unique_id: Some(device_id.clone()),
                suggested_object_id: None,
                capabilities: entity.capabilities,
                entity_category: entity.category,
            };
            if self.ctx.describe_entity(description).await.is_err() {
                tracing::warn!("a SwitchBot entity couldn't be added");
            }
        }
        for stale in known.described.difference(&keys) {
            if let Ok(id) = entity_id(&known.listed.id, stale) {
                let _ = self.ctx.remove_entity(id).await;
            }
        }
        known.described = keys;
        Ok(())
    }

    /// Reports every entity's state from the device's reading.
    fn report(&self, known: &Known, caused_by: Option<ContextId>, replayed: bool) {
        for key in &known.described {
            let Some(state) = map::state(known.family, key, &known.reading) else {
                continue;
            };
            let Ok(unique_id) = entity_id(&known.listed.id, key) else {
                continue;
            };
            self.ctx.report_state(StateReport {
                unique_id,
                state: Some(state),
                attributes: BTreeMap::new(),
                caused_by: caused_by.clone(),
                replayed,
            });
        }
    }

    /// The polling order: every device that has something to read, the urgent ones twice.
    fn rotate(&mut self) {
        let pollable: Vec<(&String, &Known)> = self
            .devices
            .iter()
            .filter(|(_, known)| known.from_cloud && !known.family.silent())
            .collect();
        let mut rotation: Vec<String> = pollable.iter().map(|(id, _)| (*id).clone()).collect();
        rotation.extend(
            pollable
                .iter()
                .filter(|(_, known)| known.family.urgent())
                .map(|(id, _)| (*id).clone()),
        );
        self.rotation = rotation;
        self.next_in_rotation = 0;
    }

    /// The wait before the next status call: what the day's budget allows, never faster than
    /// the floor, and never reading one device more often than once a minute.
    fn gap(&self) -> Duration {
        let reserve = self.ledger.budget.limit() * COMMAND_RESERVE_PERCENT / 100;
        let per_device =
            self.timing.per_device / u32::try_from(self.rotation.len().max(1)).unwrap_or(u32::MAX);
        let floor = self.timing.min_gap.max(per_device);
        let pace = if self.sweep.is_empty() {
            self.ledger.budget.pace(unix_now(), 1, reserve, floor)
        } else {
            self.timing.min_gap
        };
        match self.quiet_until {
            Some(until) if until > Instant::now() => pace.max(until - Instant::now()),
            _ => pace,
        }
    }

    async fn poll_next(&mut self) {
        if self.cloud.is_none() || self.rotation.is_empty() {
            return;
        }
        if self.quiet_until.is_some_and(|until| until > Instant::now()) {
            return;
        }
        // A new day starts a new count, even when nothing was spent since midnight.
        self.ledger.budget.roll(unix_now());
        let reserve = self.ledger.budget.limit() * COMMAND_RESERVE_PERCENT / 100;
        if self.ledger.budget.remaining() <= reserve {
            return;
        }
        while let Some(id) = self.sweep.pop_front() {
            if self.wants_reading(&id) {
                self.read(&id, None).await;
                return;
            }
        }
        // The next device in the rotation that needs the cloud: one heard nearby is read from
        // its adverts, and one read a moment ago (an urgent one's second turn) can wait.
        for _ in 0..self.rotation.len() {
            let id = self.rotation[self.next_in_rotation % self.rotation.len()].clone();
            self.next_in_rotation = (self.next_in_rotation + 1) % self.rotation.len();
            let fresh = self
                .devices
                .get(&id)
                .and_then(|known| known.last_read)
                .is_some_and(|at| at.elapsed() < self.timing.per_device);
            if !fresh && self.wants_reading(&id) {
                self.read(&id, None).await;
                return;
            }
        }
    }

    /// Whether the cloud should be asked about `id`: the account lists it, and Bluetooth hasn't
    /// heard it lately.
    fn wants_reading(&self, id: &str) -> bool {
        self.devices.get(id).is_some_and(|known| known.from_cloud) && !self.heard_lately(id)
    }

    /// Reads one device's status, off the loop.
    async fn read(&mut self, id: &str, caused_by: Option<ContextId>) {
        let Some(cloud) = self.cloud.clone() else {
            return;
        };
        let Some(known) = self.devices.get_mut(id) else {
            return;
        };
        known.last_read = Some(Instant::now());
        self.ledger.budget.spend(1, unix_now());
        self.ledger_dirty = true;
        let done = self.done.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let result = cloud.status(&id).await;
            let _ = done
                .send(Done::Status {
                    id,
                    result,
                    caused_by,
                })
                .await;
        });
    }

    async fn status(
        &mut self,
        id: &str,
        result: Result<serde_json::Map<String, serde_json::Value>, CloudError>,
        caused_by: Option<ContextId>,
    ) {
        // Any answer from SwitchBot says it's reachable and the token good, even one about the
        // device; and every answer but 171 says the device's hub is up.
        let hub_up = match &result {
            Ok(_) | Err(CloudError::DeviceOffline | CloudError::Refused { .. }) => Some(true),
            Err(CloudError::HubOffline) => Some(false),
            _ => None,
        };
        let available = match result {
            Ok(status) => {
                self.worked().await;
                let Some(mut known) = self.devices.remove(id) else {
                    return;
                };
                let reading = Reading::from_status(known.family, &status);
                let online = reading.online.unwrap_or(true);
                known.reading.merge(reading);
                self.readings_dirty = true;
                let keys = map::entities(known.family, &known.reading);
                let reshaped = keys.len() != known.described.len()
                    || keys.iter().any(|e| !known.described.contains(e.key))
                    || known.version != known.reading.version;
                if reshaped {
                    let _ = self.describe(&mut known).await;
                }
                self.report(&known, caused_by, false);
                self.devices.insert(id.to_owned(), known);
                Some(online)
            }
            Err(CloudError::DeviceOffline | CloudError::HubOffline) => {
                self.worked().await;
                Some(false)
            }
            Err(CloudError::NotFound) => {
                // Gone from the account: a fresh list removes it properly.
                self.worked().await;
                self.list();
                None
            }
            Err(error @ CloudError::Refused { .. }) => {
                self.worked().await;
                tracing::debug!("a SwitchBot status read failed: {}", error.describe());
                None
            }
            Err(error) => {
                self.failed(error).await;
                None
            }
        };
        if let Some(available) = available {
            self.set_available(id, available).await;
        }
        // A hub with nothing of its own to read is only known to be up or down through the
        // devices behind it.
        let hub = self
            .devices
            .get(id)
            .and_then(|known| known.listed.hub.clone());
        if let (Some(hub), Some(up)) = (hub, hub_up) {
            self.set_available(&hub, up).await;
        }
    }

    async fn set_available(&mut self, id: &str, available: bool) {
        let Some(known) = self.devices.get_mut(id) else {
            return;
        };
        if known.available == available {
            return;
        }
        known.available = available;
        if let Ok(device) = UniqueId::try_from(id) {
            let _ = self
                .ctx
                .set_availability(
                    AvailabilityTarget::Device(device),
                    if available {
                        Availability::Available
                    } else {
                        Availability::Unavailable
                    },
                )
                .await;
        }
    }

    /// Carries out a service call: SwitchBot commands, sent off the loop, answered when SwitchBot
    /// took them. The device is read again a few seconds later to report what happened.
    async fn command(&mut self, call: IncomingCall) {
        let Some((id, key)) = call.call.unique_id.as_str().rsplit_once(':') else {
            call.reply(Err(ServiceError::failed(
                "no SwitchBot device has that entity",
            )));
            return;
        };
        let Some(known) = self.devices.get(id) else {
            call.reply(Err(ServiceError::failed(
                "no SwitchBot device has that entity",
            )));
            return;
        };
        let Some(cloud) = self.cloud.clone() else {
            call.reply(Err(ServiceError::unavailable(
                "Controlling SwitchBot devices goes through SwitchBot's cloud for now: add the token and secret key in the extension's settings",
            )));
            return;
        };
        let commands = match map::command(known.family, key, &call.call.service, &known.reading) {
            Ok(commands) => commands,
            Err(error) => {
                call.reply(Err(error));
                return;
            }
        };
        let now = unix_now();
        self.ledger
            .budget
            .spend(u64::try_from(commands.len()).unwrap_or(u64::MAX), now);
        self.ledger_dirty = true;
        let done = self.done.clone();
        let id = id.to_owned();
        let caused_by = call.call.context.id.clone();
        tokio::spawn(async move {
            for command in &commands {
                let mut sent = cloud.command(&id, command).await;
                // 190 is SwitchBot's "something went wrong on our side": worth one more try.
                if matches!(sent, Err(CloudError::Refused { code: 190, .. })) {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    sent = cloud.command(&id, command).await;
                }
                if let Err(error) = sent {
                    let reply = match &error {
                        CloudError::DeviceOffline | CloudError::HubOffline => {
                            ServiceError::unavailable(error.describe())
                        }
                        CloudError::Unauthorized => ServiceError::unavailable(
                            "SwitchBot isn't taking calls: the token is wrong, or the day's calls are used up",
                        ),
                        _ => ServiceError::failed(error.describe()),
                    };
                    call.reply(Err(reply));
                    if matches!(error, CloudError::Unauthorized | CloudError::TooMany(_)) {
                        let _ = done.send(Done::Failed(error)).await;
                    }
                    return;
                }
            }
            call.reply(Ok(()));
            let _ = done.send(Done::Commanded { id, caused_by }).await;
        });
    }

    async fn report_health(&mut self) {
        // A device offline shows on its own page. A hub offline takes the devices behind it
        // with it, so it's worth saying here.
        let hubs: Vec<&str> = self
            .devices
            .values()
            .filter(|d| !d.available && matches!(d.family, Family::Hub(_)))
            .map(|d| d.listed.name.as_str())
            .collect();
        let health = match &self.trouble {
            // Bluetooth alone is a way to work; it's only trouble when Bluetooth isn't working.
            Some(Trouble::NoAccount) => match &self.radio {
                RadioState::Listening => Health::Running,
                RadioState::Off => Health::Degraded(
                    "Add the SwitchBot token and secret key in this extension's settings, or turn on Bluetooth, to see your devices.".into(),
                ),
                RadioState::Failed(why) => Health::Degraded(format!(
                    "Bluetooth isn't working ({why}). Add the SwitchBot token and secret key in this extension's settings to reach your devices through SwitchBot's cloud."
                )),
            },
            Some(Trouble::Refused) => Health::Degraded(
                "SwitchBot refused the token and secret key. Check them in this extension's settings.".into(),
            ),
            Some(Trouble::Spent) => Health::Degraded(
                "SwitchBot's calls for today are used up (another app on the account, such as Home Assistant, may share them). Readings resume after midnight UTC.".into(),
            ),
            Some(Trouble::Unreachable(why)) => {
                Health::Degraded(format!("Can't reach SwitchBot's cloud: {why}."))
            }
            None => match hubs.as_slice() {
                [] => Health::Running,
                [hub] => Health::Degraded(format!(
                    "The hub {hub} is offline, so the devices it reaches can't be read. Check its power and Wi-Fi."
                )),
                _ => Health::Degraded(format!(
                    "{} SwitchBot hubs are offline, so the devices they reach can't be read. Check their power and Wi-Fi.",
                    hubs.len()
                )),
            },
        };
        if self.reported.as_ref() != Some(&health) {
            self.ctx.set_health(health.clone()).await;
            self.reported = Some(health);
        }
    }

    async fn save_ledger(&mut self) {
        if let Ok(value) = serde_json::to_value(&self.ledger)
            && self.ctx.store(LEDGER_KEY, value).await.is_ok()
        {
            self.ledger_dirty = false;
        }
    }

    async fn save(&mut self) {
        if self.ledger_dirty {
            self.save_ledger().await;
        }
        self.save_readings().await;
    }

    async fn save_readings(&mut self) {
        if !self.readings_dirty {
            return;
        }
        let saved: Vec<Saved> = self
            .devices
            .values()
            .map(|known| Saved {
                listed: known.listed.clone(),
                reading: known.reading.clone(),
                from_cloud: known.from_cloud,
            })
            .collect();
        if let Ok(value) = serde_json::to_value(&saved)
            && self.ctx.store(DEVICES_KEY, value).await.is_ok()
        {
            self.readings_dirty = false;
        }
    }
}

async fn load<T: serde::de::DeserializeOwned>(ctx: &ProtocolContext, key: &str) -> Option<T> {
    let value = ctx.load(key).await.ok().flatten()?;
    serde_json::from_value(value).ok()
}

fn entity_id(device: &str, key: &str) -> Result<UniqueId, ()> {
    UniqueId::try_from(format!("{device}:{key}")).map_err(drop)
}

/// A device type as an unmodeled platform: lower case, words joined by `_`.
fn platform(device_type: &str) -> ObjectId {
    let mut slug = String::new();
    for c in device_type.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_end_matches('_');
    ObjectId::try_from(if slug.is_empty() { "device" } else { slug })
        .unwrap_or_else(|_| ObjectId::try_from("device").expect("a valid object id"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(365 * 86_400)
}

fn is_hub(listed: &Listed) -> bool {
    matches!(Family::of(&listed.device_type), Some(Family::Hub(_)))
}

/// Hubs first, so a device can say it's reached through one already described.
fn hubs_first(list: &[Listed]) -> Vec<&Listed> {
    let mut ordered: Vec<&Listed> = list.iter().filter(|d| is_hub(d)).collect();
    ordered.extend(list.iter().filter(|d| !is_hub(d)));
    ordered
}

async fn next_radio(radio: &mut Option<mpsc::Receiver<ble::Radio>>) -> Option<ble::Radio> {
    match radio {
        Some(radio) => radio.recv().await,
        None => std::future::pending().await,
    }
}
