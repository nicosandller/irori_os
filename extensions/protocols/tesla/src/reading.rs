//! Reading the cars and sites, as often as the polling mode and the month's money allow.
//!
//! **Cars.** One call (`GET /vehicles`) says which cars are awake. A car is read when it wakes,
//! then as often as the plan allows while it's doing something (charging, driving, warming the
//! cabin), and otherwise only every few hours, so an idle car is left to fall asleep. A car
//! that's asleep is never read: a read would be billed and answer "asleep", and waking it to
//! read it costs ten times as much and drains its battery. What it said last stays on show.
//!
//! **Sites.** Their power flows on the plan's gap, their day so far every few of those, their
//! settings when the account is listed.
//!
//! What goes wrong with one car or site stays with it: a car Tesla won't let Irori read
//! (Home Assistant's #189753 took the whole account down) says so on its own line, and the
//! others carry on. Only the account's own troubles (signed out, billing, Tesla down) stop
//! everything.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use irori_protocol::{IncomingCall, ServiceError};
use irori_types::{DeviceDescription, EntityDescription, Name, StateReport, UniqueId};
use serde_json::Value;
use tokio::time::Instant;

use crate::account::ACCOUNT_DEVICE;
use crate::api::{ApiError, Car, Site};
use crate::energy::{self, SiteReading};
use crate::entity::{self, Entity};
use crate::usage::Why;
use crate::usage::pace::{self, HISTORY_EVERY, LONGEST_GAP, Mode, Plan};
use crate::usage::price::{self, Endpoint};
use crate::vehicle::{self, CarReading};
use crate::{Done, World, load};

/// The shortest a site's day so far waits between reads, however much money is left.
const HISTORY_FLOOR: Duration = Duration::from_secs(30 * 60);

/// How long a car or site Tesla refused waits before it's tried again.
const REFUSED_WAIT: Duration = Duration::from_secs(6 * 3600);

/// How many reads after Wake look for the car awake, each [`crate::Timing::woken`] apart.
const WAKE_TRIES: u8 = 3;

/// Someone pressed Wake: the car is read until it answers awake, on their account.
#[derive(Debug, Clone)]
struct Woken {
    why: Why,
    tries: u8,
}

/// How often a site's day so far is read, given how often its power flows are.
fn history_gap(site: Option<Duration>) -> Option<Duration> {
    site.map(|gap| (gap * HISTORY_EVERY).clamp(HISTORY_FLOOR, LONGEST_GAP.max(HISTORY_FLOOR)))
}

/// A car Irori reads.
pub(crate) struct CarSlot {
    pub car: Car,
    pub reading: CarReading,
    pub described: BTreeMap<&'static str, Entity>,
    device: Option<DeviceDescription>,
    pub reading_now: bool,
    pub next_read: Option<Instant>,
    /// Asked for the cached data once since starting, for a car that's never been read.
    cached_asked: bool,
    /// What's wrong with this car alone, until a read works.
    pub trouble: Option<String>,
    /// Refresh presses waiting for the read under way.
    refreshes: Vec<IncomingCall>,
    /// Woken by a press, and not yet seen awake.
    woken: Option<Woken>,
}

impl CarSlot {
    fn new(car: Car, reading: CarReading) -> Self {
        Self {
            car,
            reading,
            described: BTreeMap::new(),
            device: None,
            reading_now: false,
            next_read: None,
            cached_asked: false,
            trouble: None,
            refreshes: Vec::new(),
            woken: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Part {
    /// `site_info`: its settings.
    Info,
    /// `live_status`: power flows now.
    Live,
    /// `calendar_history`: the day so far.
    History,
}

/// An energy site Irori reads.
pub(crate) struct SiteSlot {
    pub site: Site,
    pub reading: SiteReading,
    pub described: BTreeMap<&'static str, Entity>,
    device: Option<DeviceDescription>,
    reading_now: BTreeSet<Part>,
    next_live: Option<Instant>,
    next_history: Option<Instant>,
    pub trouble: Option<String>,
    /// Refresh presses waiting for the power flows being read.
    refreshes: Vec<IncomingCall>,
}

impl SiteSlot {
    fn new(site: Site, reading: SiteReading) -> Self {
        Self {
            site,
            reading,
            described: BTreeMap::new(),
            device: None,
            reading_now: BTreeSet::new(),
            next_live: None,
            next_history: None,
            trouble: None,
            refreshes: Vec::new(),
        }
    }
}

/// What a state check means for a car's next read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Read it now: it just woke (its driver, the app, a schedule), so see why.
    Now,
    /// Leave whatever's planned.
    Keep,
    /// Don't read it: it's asleep or offline.
    Never,
}

fn on_state(was_awake: bool, awake: bool, mode: Mode) -> Next {
    match (was_awake, awake) {
        (_, false) => Next::Never,
        (false, true) if mode != Mode::Off => Next::Now,
        _ => Next::Keep,
    }
}

/// How long after a read the car is read again. Busy: as often as the plan allows. Idle: a
/// dozen of those gaps, an hour at least, `cap` at most, so it can fall asleep, and a drive that
/// starts while it's awake (kept up by sentry mode, or another app reading it) is still seen
/// within the hour when there's money to spare. Saving looks at an idle car only every `cap`.
/// Nothing when reading is off or the month's money is spent.
fn after_read(busy: bool, mode: Mode, car: Option<Duration>, cap: Duration) -> Option<Duration> {
    let gap = car?;
    if busy {
        return Some(gap);
    }
    if mode == Mode::Saving {
        return Some(cap);
    }
    let floor = IDLE_FLOOR.min(cap);
    Some((gap * 12).clamp(floor, cap))
}

/// The soonest an idle car is read again.
const IDLE_FLOOR: Duration = Duration::from_secs(3600);

/// Whether an error is the account's (and stops everything) or this one car's or site's.
fn account_wide(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::SignedOut(_)
            | ApiError::Billing(_)
            | ApiError::Server(_)
            | ApiError::Unreachable(_)
            | ApiError::RateLimited(_)
            | ApiError::WrongRegion
    )
}

impl World {
    /// The polling mode, and how often each kind of reading may happen now.
    pub(crate) fn plan(&self) -> (Mode, Plan) {
        let now = self.now();
        let Ok(usage) = self.meter.lock() else {
            return (
                Mode::Off,
                Plan {
                    state: None,
                    car: None,
                    site: None,
                    starved: false,
                },
            );
        };
        let mode = usage.prefs.mode;
        let outlook = usage.outlook(now);
        let plan = pace::plan(
            &outlook,
            mode,
            usage.prices,
            pace::Reading {
                cars: self.cars.len(),
                sites: self.sites.len(),
                active_share: None,
            },
        );
        (mode, plan)
    }

    /// The soonest anything is due.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        let cars = self.cars.values().filter_map(|slot| slot.next_read);
        let sites = self
            .sites
            .values()
            .flat_map(|slot| [slot.next_live, slot.next_history])
            .flatten();
        [self.next_list, self.next_states]
            .into_iter()
            .flatten()
            .chain(cars)
            .chain(sites)
            .min()
    }

    /// Starts whatever is due.
    pub(crate) fn tick(&mut self) {
        let now = Instant::now();
        let due = |at: Option<Instant>| at.is_some_and(|at| at <= now);
        if due(self.next_list) {
            self.next_list = None;
            self.list();
        }
        if due(self.next_states) {
            self.next_states = None;
            self.check_states();
        }
        let cars: Vec<String> = self
            .cars
            .iter()
            .filter(|(_, slot)| due(slot.next_read))
            .map(|(vin, _)| vin.clone())
            .collect();
        for vin in cars {
            let mut why = Why::Schedule;
            if let Some(slot) = self.cars.get_mut(&vin) {
                slot.next_read = None;
                if let Some(woken) = &slot.woken {
                    why = woken.why.clone();
                }
            }
            self.read_car(&vin, false, why);
        }
        let sites: Vec<(String, Part)> = self
            .sites
            .iter()
            .flat_map(|(id, slot)| {
                [
                    due(slot.next_live).then(|| (id.clone(), Part::Live)),
                    due(slot.next_history).then(|| (id.clone(), Part::History)),
                ]
            })
            .flatten()
            .collect();
        for (id, part) in sites {
            if let Some(slot) = self.sites.get_mut(&id) {
                match part {
                    Part::Live => slot.next_live = None,
                    Part::History => slot.next_history = None,
                    Part::Info => {}
                }
            }
            self.read_site(&id, part, Why::Schedule);
        }
    }

    /// Takes the cars and sites kept from last time, and shows what they said last: no call to
    /// Tesla, so the entities have their values the moment Irori starts.
    pub(crate) async fn restore(&mut self) {
        for car in self.known.cars.clone() {
            let reading: CarReading = load(&self.ctx, &vehicle::stored(&car.vin))
                .await
                .unwrap_or_default();
            let vin = car.vin.clone();
            self.cars.insert(vin.clone(), CarSlot::new(car, reading));
            self.show_car(&vin, true).await;
        }
        for site in self.known.sites.clone() {
            let reading: SiteReading = load(&self.ctx, &energy::stored(&site.id))
                .await
                .unwrap_or_default();
            let id = site.id.clone();
            self.sites.insert(id.clone(), SiteSlot::new(site, reading));
            self.show_site(&id, true).await;
        }
    }

    /// Takes in a fresh listing: new cars and sites are added and read, gone ones removed.
    pub(crate) async fn adopt(&mut self) {
        let (mode, plan) = self.plan();
        let now = Instant::now();
        let listed: BTreeSet<String> = self.known.cars.iter().map(|car| car.vin.clone()).collect();
        let gone: Vec<String> = self
            .cars
            .keys()
            .filter(|vin| !listed.contains(*vin))
            .cloned()
            .collect();
        for vin in gone {
            self.cars.remove(&vin);
            if let Ok(id) = UniqueId::try_from(vin.as_str()) {
                let _ = self.ctx.remove_device(id).await;
            }
            let _ = self.ctx.forget(&vehicle::stored(&vin)).await;
        }
        for car in self.known.cars.clone() {
            let vin = car.vin.clone();
            let slot = self
                .cars
                .entry(vin.clone())
                .or_insert_with(|| CarSlot::new(car.clone(), CarReading::default()));
            slot.car = car.clone();
            slot.reading.state = Some(car.state.clone());
            if slot.reading.awake() {
                if mode != Mode::Off && !slot.reading_now && slot.next_read.is_none() {
                    slot.next_read = Some(now);
                }
            } else {
                slot.next_read = None;
                // A car that's never been read shows what Tesla last heard from it, once.
                if slot.reading.fields.is_empty() && !slot.cached_asked && !slot.reading_now {
                    slot.cached_asked = true;
                    self.read_car(&vin, true, Why::Start);
                }
            }
            self.show_car(&vin, false).await;
        }
        if !self.cars.is_empty() && self.next_states.is_none() {
            self.next_states = plan.state.map(|gap| now + gap);
        }

        let listed: BTreeSet<String> = self
            .known
            .sites
            .iter()
            .map(|site| site.id.clone())
            .collect();
        let gone: Vec<String> = self
            .sites
            .keys()
            .filter(|id| !listed.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.sites.remove(&id);
            if let Ok(unique_id) = UniqueId::try_from(id.as_str()) {
                let _ = self.ctx.remove_device(unique_id).await;
            }
            let _ = self.ctx.forget(&energy::stored(&id)).await;
        }
        for site in self.known.sites.clone() {
            let id = site.id.clone();
            let slot = self
                .sites
                .entry(id.clone())
                .or_insert_with(|| SiteSlot::new(site.clone(), SiteReading::default()));
            slot.site = site;
            if mode != Mode::Off {
                if slot.next_live.is_none() && !slot.reading_now.contains(&Part::Live) {
                    slot.next_live = Some(now);
                }
                if slot.next_history.is_none() && !slot.reading_now.contains(&Part::History) {
                    slot.next_history = Some(now);
                }
            }
            self.read_site(&id, Part::Info, Why::Start);
            self.show_site(&id, false).await;
        }
    }

    /// Asks which cars are awake.
    fn check_states(&mut self) {
        if self.checking_states || !self.signed_in || self.cars.is_empty() {
            return;
        }
        self.checking_states = true;
        let api = self.api.clone();
        let done = self.done.clone();
        tokio::spawn(async move {
            let result = api.vehicles(Why::Schedule).await;
            let _ = done.send(Done::States(result)).await;
        });
    }

    /// Starts reading a car, unless it can't be or is being read already. Says whether it
    /// started.
    fn read_car(&mut self, vin: &str, cached: bool, why: Why) -> bool {
        if !self.signed_in {
            return false;
        }
        let Some(slot) = self.cars.get_mut(vin) else {
            return false;
        };
        if slot.reading_now {
            return false;
        }
        slot.reading_now = true;
        let api = self.api.clone();
        let done = self.done.clone();
        let vin = vin.to_owned();
        tokio::spawn(async move {
            let result = if cached {
                api.vehicle_data_cached(&vin, why).await
            } else {
                api.vehicle_data(&vin, true, why).await.map(Some)
            };
            let _ = done
                .send(Done::CarRead {
                    vin,
                    cached,
                    result,
                })
                .await;
        });
        true
    }

    fn read_site(&mut self, id: &str, part: Part, why: Why) {
        if !self.signed_in {
            return;
        }
        let Some(slot) = self.sites.get_mut(id) else {
            return;
        };
        if !slot.reading_now.insert(part) {
            return;
        }
        let api = self.api.clone();
        let done = self.done.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let result = match part {
                Part::Info => api.site_info(&id, why).await,
                Part::Live => api.live_status(&id, why).await,
                Part::History => api.energy_history(&id, why).await,
            };
            let _ = done.send(Done::SiteRead { id, part, result }).await;
        });
    }

    /// When to try again after the account's own trouble.
    fn retry_after(&mut self, error: &ApiError) -> Option<Instant> {
        match error {
            ApiError::SignedOut(_) => None,
            ApiError::RateLimited(Some(wait)) => Some(Instant::now() + *wait),
            _ => Some(Instant::now() + self.read_backoff.failed()),
        }
    }

    /// A call to Tesla worked: whatever was wrong with the account isn't any more.
    pub(crate) fn account_worked(&mut self) {
        self.read_backoff.succeeded();
        self.trouble = None;
    }

    pub(crate) async fn states_checked(&mut self, result: Result<Vec<Car>, ApiError>) {
        self.checking_states = false;
        let (mode, plan) = self.plan();
        let now = Instant::now();
        let cars = match result {
            Ok(cars) => cars,
            Err(error) => {
                self.next_states = self.retry_after(&error);
                self.trouble_from(error);
                return;
            }
        };
        self.account_worked();
        let mut changed = Vec::new();
        for car in cars {
            let Some(slot) = self.cars.get_mut(&car.vin) else {
                // A car that wasn't there at the last listing: list again.
                self.next_list = Some(now);
                continue;
            };
            let was_awake = slot.reading.awake();
            if slot.reading.state.as_deref() != Some(car.state.as_str()) {
                slot.reading.state = Some(car.state.clone());
                changed.push(car.vin.clone());
            }
            match on_state(was_awake, slot.reading.awake(), mode) {
                Next::Now if !slot.reading_now => slot.next_read = Some(now),
                Next::Now | Next::Keep => {}
                // A car someone just woke is still coming up: its reads stand.
                Next::Never if slot.woken.is_some() => {}
                Next::Never => slot.next_read = None,
            }
        }
        for vin in changed {
            self.show_car(&vin, false).await;
            self.keep_car(&vin).await;
        }
        self.next_states = plan.state.map(|gap| now + gap);
    }

    pub(crate) async fn car_read(
        &mut self,
        vin: String,
        cached: bool,
        result: Result<Option<Value>, ApiError>,
    ) {
        let (mode, plan) = self.plan();
        let now = Instant::now();
        let unix_now = self.now();
        let Some(slot) = self.cars.get_mut(&vin) else {
            return;
        };
        slot.reading_now = false;
        let answer: Result<(), String> = match &result {
            Ok(_) => Ok(()),
            Err(error) => Err(error.describe()),
        };
        let fell_asleep = matches!(result, Err(ApiError::Asleep)) && !cached;
        match result {
            Ok(data) => {
                slot.trouble = None;
                if let Some(data) = data {
                    // The cached data says what state the car was in when it last reported, not
                    // now: a sleeping car stays asleep.
                    let state = slot.reading.state.clone();
                    slot.reading.merge(&data);
                    if cached && state.is_some() {
                        slot.reading.state = state;
                    }
                }
                if !cached {
                    slot.reading.read_at = Some(unix_now);
                    slot.reading.state = Some("online".into());
                    slot.woken = None;
                    let saving = mode == Mode::Saving;
                    let busy = slot.reading.busy(saving);
                    slot.next_read =
                        after_read(busy, mode, plan.car, self.timing.idle).map(|gap| now + gap);
                }
                self.account_worked();
            }
            Err(ApiError::Asleep) => {
                // Billed all the same; it fell asleep since the last check, or hasn't woken yet.
                slot.reading.state = Some("asleep".into());
                slot.next_read = None;
                if let Some(woken) = &mut slot.woken
                    && !cached
                {
                    woken.tries = woken.tries.saturating_sub(1);
                    if woken.tries > 0 {
                        slot.next_read = Some(now + self.timing.woken);
                    } else {
                        slot.woken = None;
                    }
                }
                self.account_worked();
            }
            Err(ApiError::NotFound) => {
                slot.next_read = None;
                self.next_list = Some(now);
                self.account_worked();
            }
            Err(error) if !account_wide(&error) => {
                slot.trouble = Some(error.describe());
                slot.next_read = Some(now + REFUSED_WAIT);
                self.account_worked();
            }
            Err(error) => {
                let retry = self.retry_after(&error);
                if let Some(slot) = self.cars.get_mut(&vin) {
                    slot.next_read = if cached { None } else { retry };
                }
                self.trouble_from(error);
            }
        }
        self.show_car(&vin, false).await;
        self.keep_car(&vin).await;
        let Some(slot) = self.cars.get_mut(&vin) else {
            return;
        };
        if slot.refreshes.is_empty() {
            return;
        }
        // Refresh never wakes a car: one that fell asleep shows what Tesla last heard from it.
        if fell_asleep && self.read_car(&vin, true, Why::Refresh) {
            return;
        }
        let Some(slot) = self.cars.get_mut(&vin) else {
            return;
        };
        for call in std::mem::take(&mut slot.refreshes) {
            call.reply(answer.clone().map_err(ServiceError::failed));
        }
    }

    pub(crate) async fn site_read(
        &mut self,
        id: String,
        part: Part,
        result: Result<Value, ApiError>,
    ) {
        let (_, plan) = self.plan();
        let now = Instant::now();
        let Some(slot) = self.sites.get_mut(&id) else {
            return;
        };
        slot.reading_now.remove(&part);
        if part == Part::Live {
            let answer = match &result {
                Ok(_) => Ok(()),
                Err(error) => Err(ServiceError::failed(error.describe())),
            };
            for call in std::mem::take(&mut slot.refreshes) {
                call.reply(answer.clone());
            }
        }
        let next = |gap: Option<Duration>| gap.map(|gap| now + gap);
        let history_gap = history_gap(plan.site);
        match result {
            Ok(data) => {
                slot.trouble = None;
                match part {
                    Part::Info => {
                        slot.reading.take_info(&data);
                    }
                    Part::Live => {
                        slot.reading.take_live(&data);
                        slot.next_live = next(plan.site);
                    }
                    Part::History => {
                        slot.reading.take_history(&data);
                        slot.next_history = next(history_gap);
                    }
                }
                self.account_worked();
            }
            Err(error) if !account_wide(&error) => {
                slot.trouble = Some(error.describe());
                match part {
                    Part::Info => {}
                    Part::Live => slot.next_live = Some(now + REFUSED_WAIT),
                    Part::History => slot.next_history = Some(now + REFUSED_WAIT),
                }
                self.account_worked();
            }
            Err(error) => {
                let retry = self.retry_after(&error);
                if let Some(slot) = self.sites.get_mut(&id) {
                    match part {
                        Part::Info => {}
                        Part::Live => slot.next_live = retry,
                        Part::History => slot.next_history = retry,
                    }
                }
                self.trouble_from(error);
            }
        }
        self.show_site(&id, false).await;
        self.keep_site(&id).await;
    }

    pub(crate) async fn keep_car(&mut self, vin: &str) {
        if let Some(slot) = self.cars.get(vin)
            && let Ok(value) = serde_json::to_value(&slot.reading)
        {
            let _ = self.ctx.store(&vehicle::stored(vin), value).await;
        }
    }

    pub(crate) async fn keep_site(&mut self, id: &str) {
        if let Some(slot) = self.sites.get(id)
            && let Ok(value) = serde_json::to_value(&slot.reading)
        {
            let _ = self.ctx.store(&energy::stored(id), value).await;
        }
    }

    /// Describes the car and its entities as they are now, and reports every state.
    async fn show_car(&mut self, vin: &str, replayed: bool) {
        let (prices, currency) = self.prices();
        let Some(slot) = self.cars.get_mut(vin) else {
            return;
        };
        let Ok(device_id) = UniqueId::try_from(vin) else {
            return;
        };
        let device = DeviceDescription {
            unique_id: device_id.clone(),
            name: Name::try_from(slot.car.name.trim())
                .or_else(|_| Name::try_from(vin))
                .expect("a VIN is a name"),
            manufacturer: Some("Tesla".into()),
            model: slot.reading.model().map(str::to_owned),
            sw_version: slot.reading.version(),
            hw_version: None,
            serial_number: Some(vin.to_owned()),
            suggested_area: None,
            via_device_unique_id: UniqueId::try_from(ACCOUNT_DEVICE).ok(),
        };
        let mut entities = vehicle::entities(&slot.reading);
        entities.push(entity::button(
            "refresh",
            format!(
                "Refresh (≈{})",
                price::money(prices.call(Endpoint::VehicleData), &currency)
            ),
        ));
        entities.push(entity::button(
            "wake",
            format!(
                "Wake (≈{})",
                price::money(prices.call(Endpoint::WakeUp), &currency)
            ),
        ));
        if !describe(
            &self.ctx,
            device,
            &mut slot.device,
            &mut slot.described,
            entities,
        )
        .await
        {
            return;
        }
        for key in slot.described.keys() {
            if let Some(state) = vehicle::state(key, &slot.reading) {
                report(&self.ctx, vin, key, state, replayed);
            }
        }
    }

    async fn show_site(&mut self, id: &str, replayed: bool) {
        let (prices, currency) = self.prices();
        let Some(slot) = self.sites.get_mut(id) else {
            return;
        };
        let Ok(device_id) = UniqueId::try_from(id) else {
            return;
        };
        let device = DeviceDescription {
            unique_id: device_id.clone(),
            name: Name::try_from(slot.site.name.trim())
                .or_else(|_| Name::try_from("Energy site"))
                .expect("a name"),
            manufacturer: Some("Tesla".into()),
            model: Some(
                if slot.site.battery {
                    "Powerwall"
                } else {
                    "Solar"
                }
                .to_owned(),
            ),
            sw_version: slot.reading.version(),
            hw_version: None,
            serial_number: None,
            suggested_area: None,
            via_device_unique_id: UniqueId::try_from(ACCOUNT_DEVICE).ok(),
        };
        let mut entities = energy::entities(&slot.site, &slot.reading);
        let refresh = prices.call(Endpoint::LiveStatus) + prices.call(Endpoint::EnergyHistory);
        entities.push(entity::button(
            "refresh",
            format!("Refresh (≈{})", price::money(refresh, &currency)),
        ));
        if !describe(
            &self.ctx,
            device,
            &mut slot.device,
            &mut slot.described,
            entities,
        )
        .await
        {
            return;
        }
        for key in slot.described.keys() {
            if let Some(state) = energy::state(key, &slot.reading) {
                report(&self.ctx, id, key, state, replayed);
            }
        }
    }

    /// What each call costs, and in what currency.
    fn prices(&self) -> (price::Prices, String) {
        self.meter
            .lock()
            .map(|usage| (usage.prices, usage.currency.clone()))
            .unwrap_or_else(|_| {
                (
                    price::Prices::from_settings(&self.settings),
                    self.settings.currency.clone(),
                )
            })
    }

    /// Refresh on a car: read it now if it's awake, else show what Tesla last heard from it.
    /// Never wakes it. Answered once the read is done.
    pub(crate) fn refresh_car(&mut self, vin: &str, call: IncomingCall) {
        if !self.signed_in {
            call.reply(Err(ServiceError::unavailable("sign in to Tesla first")));
            return;
        }
        let Some(slot) = self.cars.get_mut(vin) else {
            call.reply(Err(ServiceError::failed("no such car")));
            return;
        };
        slot.refreshes.push(call);
        if slot.reading_now {
            return;
        }
        // Live first, even for a car thought asleep (the last check may be most of an hour
        // old); one that's asleep answers so, and its last report is shown instead.
        self.read_car(vin, false, Why::Refresh);
    }

    pub(crate) fn refresh_site(&mut self, id: &str, call: IncomingCall) {
        if !self.signed_in {
            call.reply(Err(ServiceError::unavailable("sign in to Tesla first")));
            return;
        }
        let Some(slot) = self.sites.get_mut(id) else {
            call.reply(Err(ServiceError::failed("no such site")));
            return;
        };
        slot.refreshes.push(call);
        self.read_site(id, Part::Live, Why::Refresh);
        self.read_site(id, Part::History, Why::Refresh);
    }

    /// Wake: asks the car to wake, then reads it once it's had time to.
    pub(crate) fn wake_car(&mut self, vin: &str, call: IncomingCall) {
        if !self.signed_in {
            call.reply(Err(ServiceError::unavailable("sign in to Tesla first")));
            return;
        }
        if !self.cars.contains_key(vin) {
            call.reply(Err(ServiceError::failed("no such car")));
            return;
        }
        let why = Why::Command {
            what: "Wake".into(),
            by: crate::account::asked_by(&call.call.context.origin),
        };
        let api = self.api.clone();
        let done = self.done.clone();
        let vin = vin.to_owned();
        tokio::spawn(async move {
            let result = api.wake_up(&vin, why).await;
            let _ = done.send(Done::Woken { vin, result, call }).await;
        });
    }

    pub(crate) async fn woken(
        &mut self,
        vin: String,
        result: Result<String, ApiError>,
        call: IncomingCall,
    ) {
        match result {
            Ok(_) => {
                self.account_worked();
                let wait = self.timing.woken;
                if let Some(slot) = self.cars.get_mut(&vin) {
                    let by = crate::account::asked_by(&call.call.context.origin);
                    slot.woken = Some(Woken {
                        why: Why::Command {
                            what: "Wake".into(),
                            by,
                        },
                        tries: WAKE_TRIES,
                    });
                    slot.next_read = Some(Instant::now() + wait);
                }
                call.reply(Ok(()));
            }
            Err(error) => {
                call.reply(Err(ServiceError::failed(error.describe())));
                if account_wide(&error) {
                    self.trouble_from(error);
                }
            }
        }
    }

    /// The polling mode or the allowance changed: everything is scheduled again by the new plan.
    pub(crate) fn replan(&mut self) {
        let (mode, plan) = self.plan();
        let now = Instant::now();
        let soonest = |at: Option<Instant>, gap: Option<Duration>| match (at, gap) {
            (_, None) => None,
            (Some(at), Some(gap)) => Some(at.min(now + gap)),
            (None, Some(_)) => Some(now),
        };
        self.next_states = match plan.state {
            Some(gap) if !self.cars.is_empty() => {
                Some(self.next_states.map_or(now + gap, |at| at.min(now + gap)))
            }
            _ => None,
        };
        let idle = self.timing.idle;
        for slot in self.cars.values_mut() {
            if slot.woken.is_some() {
                continue;
            }
            if mode == Mode::Off || !slot.reading.awake() {
                slot.next_read = None;
            } else if !slot.reading_now {
                let gap = after_read(
                    slot.reading.busy(mode == Mode::Saving),
                    mode,
                    plan.car,
                    idle,
                );
                slot.next_read = soonest(slot.next_read, gap);
            }
        }
        for slot in self.sites.values_mut() {
            slot.next_live = soonest(slot.next_live, plan.site);
            slot.next_history = soonest(slot.next_history, history_gap(plan.site));
        }
    }

    /// The cars and sites Tesla won't let Irori read, by name, with why.
    pub(crate) fn refused(&self) -> Vec<String> {
        let cars = self
            .cars
            .values()
            .filter_map(|slot| Some(format!("{}: {}", slot.car.name, slot.trouble.as_ref()?)));
        let sites = self
            .sites
            .values()
            .filter_map(|slot| Some(format!("{}: {}", slot.site.name, slot.trouble.as_ref()?)));
        cars.chain(sites).collect()
    }
}

/// Describes a device and whichever of its entities are new or changed, and removes the ones it
/// no longer has. False when the core wouldn't take the device.
pub(crate) async fn describe(
    ctx: &irori_protocol::ProtocolContext,
    device: DeviceDescription,
    described_device: &mut Option<DeviceDescription>,
    described: &mut BTreeMap<&'static str, Entity>,
    entities: Vec<Entity>,
) -> bool {
    let device_id = device.unique_id.clone();
    if described_device.as_ref() != Some(&device) {
        if ctx.describe_device(device.clone()).await.is_err() {
            tracing::warn!("a Tesla device couldn't be added");
            return false;
        }
        *described_device = Some(device);
    }
    let keys: BTreeSet<&'static str> = entities.iter().map(|entity| entity.key).collect();
    for entity in entities {
        if described.get(entity.key) == Some(&entity) {
            continue;
        }
        let Some(unique_id) = entity::id(device_id.as_str(), entity.key) else {
            continue;
        };
        let description = EntityDescription {
            unique_id,
            name: Name::try_from(entity.name.as_str()).ok(),
            device_unique_id: Some(device_id.clone()),
            suggested_object_id: None,
            capabilities: entity.capabilities.clone(),
            entity_category: entity.category,
        };
        if ctx.describe_entity(description).await.is_ok() {
            described.insert(entity.key, entity);
        } else {
            tracing::warn!(key = entity.key, "a Tesla entity couldn't be added");
        }
    }
    let stale: Vec<&'static str> = described
        .keys()
        .filter(|key| !keys.contains(*key))
        .copied()
        .collect();
    for key in stale {
        described.remove(key);
        if let Some(unique_id) = entity::id(device_id.as_str(), key) {
            let _ = ctx.remove_entity(unique_id).await;
        }
    }
    true
}

pub(crate) fn report(
    ctx: &irori_protocol::ProtocolContext,
    device: &str,
    key: &str,
    state: irori_types::State,
    replayed: bool,
) {
    send_report(ctx, device, key, state, None, replayed);
}

/// Reports a state a service call brought about.
pub(crate) fn report_caused(
    ctx: &irori_protocol::ProtocolContext,
    device: &str,
    key: &str,
    state: irori_types::State,
    caused_by: Option<irori_types::ContextId>,
) {
    send_report(ctx, device, key, state, caused_by, false);
}

fn send_report(
    ctx: &irori_protocol::ProtocolContext,
    device: &str,
    key: &str,
    state: irori_types::State,
    caused_by: Option<irori_types::ContextId>,
    replayed: bool,
) {
    let Some(unique_id) = entity::id(device, key) else {
        return;
    };
    ctx.report_state(StateReport {
        unique_id,
        state: Some(state),
        attributes: BTreeMap::new(),
        caused_by,
        replayed,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);
    const CAP: Duration = Duration::from_secs(6 * 3600);

    #[test]
    fn a_car_is_read_when_it_wakes_and_never_while_it_sleeps() {
        use Mode::*;
        assert_eq!(on_state(false, true, Balanced), Next::Now);
        assert_eq!(on_state(false, true, Saving), Next::Now);
        assert_eq!(
            on_state(false, true, Off),
            Next::Keep,
            "off: only Refresh reads"
        );
        assert_eq!(
            on_state(true, true, Balanced),
            Next::Keep,
            "still awake: as planned"
        );
        assert_eq!(on_state(true, false, Balanced), Next::Never);
        assert_eq!(on_state(false, false, Responsive), Next::Never);
    }

    #[test]
    fn a_busy_car_is_followed_and_an_idle_one_left_to_sleep() {
        use Mode::*;
        let two = Some(2 * MINUTE);
        assert_eq!(after_read(true, Balanced, two, CAP), two);
        assert_eq!(
            after_read(true, Saving, Some(10 * MINUTE), CAP),
            Some(10 * MINUTE)
        );
        assert_eq!(
            after_read(false, Balanced, two, CAP),
            Some(60 * MINUTE),
            "an hour at least"
        );
        assert_eq!(
            after_read(false, Balanced, Some(20 * MINUTE), CAP),
            Some(240 * MINUTE)
        );
        assert_eq!(
            after_read(false, Balanced, Some(60 * MINUTE), CAP),
            Some(CAP)
        );
        assert_eq!(after_read(false, Saving, Some(10 * MINUTE), CAP), Some(CAP));
        assert_eq!(after_read(true, Off, None, CAP), None);
        assert_eq!(
            after_read(false, Balanced, None, CAP),
            None,
            "the month's money is spent"
        );
    }
}
