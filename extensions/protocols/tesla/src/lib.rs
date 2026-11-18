//! Tesla cars and Powerwall energy sites, through Tesla's Fleet API, with what the API costs in
//! view.
//!
//! One loop owns the protocol's context. Every call to Tesla runs in a task of its own and sends
//! what came of it back to the loop, so the loop is always free: to save new tokens the moment a
//! refresh asks (the refresh waits for that), to answer the page, to take the next command.

mod account;
mod api;
mod auth;
mod commands;
mod energy;
mod entity;
#[cfg(test)]
mod fake;
mod keys;
mod network;
mod page;
mod reading;
mod sending;
pub mod settings;
mod setup;
mod signed;
#[cfg(test)]
mod tests;
mod usage;
mod vehicle;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_cloud::Backoff;
use irori_protocol::IncomingCall;
use irori_protocol::{
    Health, Incoming, IncomingAction, IncomingPage, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{DeviceDescription, Name, Service, State, UniqueId, Waiting};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::api::{Api, ApiError, Clock, Hosts, KeyStatus, Meter, Products};
use crate::auth::{Keep, Session};
use crate::commands::{CarCommand, SiteCommand};
use crate::keys::Key;
use crate::reading::{CarSlot, Part, SiteSlot};
use crate::settings::Settings;
use crate::setup::Registered;
use crate::usage::price::{self, Prices};
use crate::usage::{Usage, Why};

/// The Tesla protocol.
#[derive(Debug)]
pub struct Tesla;

impl Protocol for Tesla {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        drive(settings, ctx, Timing::real()).await
    }
}

/// Where Tesla is and how long things take. Tests point it at a fake and fix the time.
#[derive(Debug, Clone)]
pub(crate) struct Timing {
    pub hosts: Hosts,
    pub now: Clock,
    /// How often the cars and sites are listed again, for ones added or sold.
    pub relist: Duration,
    /// How often the meter is saved while it changes.
    pub save: Duration,
    /// How often a car that's awake but doing nothing is read: rarely, so it can fall asleep.
    pub idle: Duration,
    /// How long after Wake the car is read to see it awake, and between tries.
    pub woken: Duration,
    /// How long a command may take, and how often it's tried while the car wakes.
    pub patience: sending::Patience,
}

impl Timing {
    fn real() -> Self {
        Self {
            hosts: Hosts::tesla(),
            now: unix_now,
            relist: Duration::from_secs(24 * 3600),
            save: Duration::from_secs(60),
            idle: usage::pace::LONGEST_GAP,
            woken: Duration::from_secs(25),
            patience: sending::Patience::real(),
        }
    }
}

/// Store keys, beside the meter's own (`usage::keys`).
mod stored {
    /// The key that was registered with Tesla last, as hex: no need to register it again.
    pub const REGISTERED: &str = "registered";
    /// The `state` of the sign-in link the person was last given, so a restart between
    /// opening it and pasting the address doesn't turn the paste away.
    pub const SIGN_IN_STATE: &str = "sign_in_state";
    pub const PRODUCTS: &str = "products";
}

/// What a task sends back to the loop.
enum Done {
    Registered(Result<Registered, ApiError>, Option<IncomingAction>),
    SignedIn(Result<api::Region, ApiError>, IncomingAction),
    Listed(Result<(Products, Option<KeyStatus>), ApiError>),
    /// Which cars are awake.
    States(Result<Vec<api::Car>, ApiError>),
    /// A car's data, or (`cached`) what Tesla last heard from it.
    CarRead {
        vin: String,
        cached: bool,
        result: Result<Option<serde_json::Value>, ApiError>,
    },
    SiteRead {
        id: String,
        part: Part,
        result: Result<serde_json::Value, ApiError>,
    },
    /// Tesla's answer to Wake, and the press it answers.
    Woken {
        vin: String,
        result: Result<String, ApiError>,
        call: IncomingCall,
    },
    /// What came of a command to a car or a site.
    Commanded {
        device: String,
        command: sending::Command,
        result: Result<Option<serde_json::Value>, sending::Failure>,
        call: IncomingCall,
    },
}

/// The cars and sites as the last listing found them, kept for the next start.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Known {
    cars: Vec<api::Car>,
    sites: Vec<api::Site>,
    /// Cars without Irori's key on them.
    #[serde(default)]
    unpaired: BTreeSet<String>,
    /// Cars that take only unsigned commands, which Irori doesn't send.
    #[serde(default)]
    unsigned: BTreeSet<String>,
}

/// Where registering the app with Tesla stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Registration {
    Checking,
    Done(Registered),
    /// Tesla couldn't be asked; it's asked again with the next listing.
    Later,
}

struct World {
    ctx: ProtocolContext,
    settings: Settings,
    timing: Timing,
    api: Api,
    meter: Meter,
    /// The private key, or why the one in the settings can't be used.
    key: Result<Key, String>,
    registration: Registration,
    signed_in: bool,
    /// Signed in once, and Tesla signed Irori out since.
    signed_out: bool,
    sign_in_state: Option<String>,
    /// What went wrong for the whole account last, until a call works again.
    trouble: Option<ApiError>,
    known: Known,
    listing: bool,
    next_list: Option<Instant>,
    list_backoff: Backoff,
    cars: BTreeMap<String, CarSlot>,
    sites: BTreeMap<String, SiteSlot>,
    /// Each car's sessions, for commands.
    links: BTreeMap<String, Arc<sending::Link>>,
    checking_states: bool,
    next_states: Option<Instant>,
    /// For reads that failed for the whole account.
    read_backoff: Backoff,
    /// How the cars spend their time, for pacing.
    activity: usage::pace::Activity,
    account: AccountShown,
    done: mpsc::Sender<Done>,
    reported: Reported,
}

/// The account device as last described and reported.
#[derive(Default)]
struct AccountShown {
    device: Option<DeviceDescription>,
    described: BTreeMap<&'static str, entity::Entity>,
    states: BTreeMap<&'static str, State>,
}

#[derive(Default)]
struct Reported {
    waiting: Option<Vec<Waiting>>,
    actions: Option<Vec<String>>,
    health: Option<Health>,
    map: Option<irori_types::Network>,
}

pub(crate) async fn drive(
    settings: Settings,
    ctx: ProtocolContext,
    timing: Timing,
) -> Result<(), ProtocolError> {
    let now = (timing.now)();
    let key = load_key(&settings, &ctx).await;
    let usage = load_usage(&ctx, &settings, now).await;
    let meter: Meter = Arc::new(Mutex::new(usage));
    let tokens: Option<auth::Kept> = load(&ctx, auth::STORED).await;
    let (keep, mut to_keep) = mpsc::unbounded_channel::<Keep>();
    let http = irori_cloud::client(concat!("irori-tesla/", env!("CARGO_PKG_VERSION")))
        .map_err(|error| ProtocolError::new(format!("no HTTPS client: {error}")))?;
    let session = Session::new(
        http.clone(),
        timing.hosts.clone(),
        &settings,
        tokens,
        keep,
        timing.now,
    );
    let signed_in = session.signed_in().await;
    let api = Api::new(
        http,
        timing.hosts.clone(),
        session,
        Arc::clone(&meter),
        timing.now,
    );
    let registered: Option<String> = load(&ctx, stored::REGISTERED).await;
    let sign_in_state: Option<String> = load(&ctx, stored::SIGN_IN_STATE).await;
    let known: Known = load(&ctx, stored::PRODUCTS).await.unwrap_or_default();
    let (done, mut finished) = mpsc::channel(64);
    let mut world = World {
        ctx,
        settings,
        api,
        meter,
        registration: match (&key, registered) {
            (Ok(key), Some(hex)) if hex == key.public_hex() => Registration::Done(Registered::Yes),
            _ => Registration::Later,
        },
        key,
        signed_in,
        signed_out: false,
        sign_in_state,
        trouble: None,
        known,
        listing: false,
        next_list: signed_in.then(Instant::now),
        list_backoff: Backoff::new(),
        cars: BTreeMap::new(),
        sites: BTreeMap::new(),
        links: BTreeMap::new(),
        checking_states: false,
        next_states: None,
        read_backoff: Backoff::new(),
        activity: usage::pace::Activity::default(),
        account: AccountShown::default(),
        done,
        reported: Reported::default(),
        timing,
    };
    world.show_account().await;
    world.restore().await;
    world.register(None);
    world.report().await;

    let mut save = tokio::time::interval(world.timing.save);
    save.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let due = world.next_due();
        tokio::select! {
            incoming = world.ctx.next() => {
                let Some(incoming) = incoming else { break };
                world.incoming(incoming).await;
            }
            Some(keep) = to_keep.recv() => world.keep_tokens(keep).await,
            Some(done) = finished.recv() => world.finished(done).await,
            _ = tokio::time::sleep_until(due.unwrap_or_else(far_future)), if due.is_some() => {
                world.tick();
            }
            _ = save.tick() => world.save().await,
        }
        world.report().await;
    }
    world.save().await;
    Ok(())
}

impl World {
    fn now(&self) -> u64 {
        (self.timing.now)()
    }

    async fn incoming(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::Action(action) => self.action(action).await,
            Incoming::Call(call) => self.call(call).await,
            Incoming::Page(asked) => self.page(asked).await,
            Incoming::Unpair(unpair) => {
                unpair.reply(Err("a Tesla car or site is removed in the Tesla app".into()));
            }
            Incoming::Picture(picture) => picture.reply(Err("Tesla has no cameras here".into())),
            Incoming::Live(live) => live.feed.end(Some("Tesla has no cameras here".into())),
            Incoming::Talk(talk) => talk.line.end(Some("Tesla has no cameras here".into())),
        }
    }

    /// A service call: a control on the account device, Refresh or Wake, or a command to a car
    /// or a site.
    async fn call(&mut self, call: IncomingCall) {
        let id = call.call.unique_id.as_str().to_owned();
        let Some((device, key)) = id.split_once(':') else {
            call.reply(Err(ServiceError::failed("no such entity")));
            return;
        };
        let is_car = self.cars.contains_key(device);
        let is_site = self.sites.contains_key(device);
        match (key, &call.call.service) {
            ("polling", Service::SelectSelectOption(choice))
                if device == account::ACCOUNT_DEVICE =>
            {
                let result = account::mode(&choice.option);
                let result = self.set_prefs(result.map(|mode| (Some(mode), None))).await;
                call.reply(result.map_err(ServiceError::failed));
            }
            ("allowance", Service::NumberSetValue(set)) if device == account::ACCOUNT_DEVICE => {
                let result = account::allowance(set.value);
                let result = self
                    .set_prefs(result.map(|allowance| (None, Some(allowance))))
                    .await;
                call.reply(result.map_err(ServiceError::failed));
            }
            ("refresh", Service::ButtonPress) if is_car => self.refresh_car(device, call),
            ("refresh", Service::ButtonPress) if is_site => self.refresh_site(device, call),
            ("wake", Service::ButtonPress) if is_car => self.wake_car(device, call),
            (key, service) if is_car => match CarCommand::from_call(key, service) {
                Some(Ok(command)) => self.command_car(device, command, call),
                Some(Err(why)) => call.reply(Err(ServiceError::failed(why))),
                None => call.reply(Err(ServiceError::failed("a Tesla can't do that"))),
            },
            (key, service) if is_site => match SiteCommand::from_call(key, service) {
                Some(Ok(command)) => self.command_site(device, command, call),
                Some(Err(why)) => call.reply(Err(ServiceError::failed(why))),
                None => call.reply(Err(ServiceError::failed("an energy site can't do that"))),
            },
            _ => call.reply(Err(ServiceError::failed("no such entity"))),
        }
    }

    /// Changes the polling mode or the allowance, keeps it, and plans reading again. The
    /// account's entities, the page and its warning all see the change at once.
    async fn set_prefs(
        &mut self,
        change: Result<(Option<usage::Mode>, Option<price::Micros>), String>,
    ) -> Result<(), String> {
        let (mode, allowance) = change?;
        {
            let mut usage = self
                .meter
                .lock()
                .map_err(|_| "the meter is unavailable".to_owned())?;
            if let Some(mode) = mode {
                usage.prefs.mode = mode;
            }
            if let Some(allowance) = allowance {
                usage.prefs.allowance = allowance;
            }
            usage.dirty = true;
        }
        self.save().await;
        self.replan();
        self.show_account().await;
        Ok(())
    }

    /// The person read the account's spend off Tesla's dashboard: other apps' share and rate
    /// change, so reading is planned again.
    async fn calibrate(&mut self, spent: Result<price::Micros, String>) -> Result<(), String> {
        let spent = spent?;
        let now = self.now();
        self.meter
            .lock()
            .map_err(|_| "the meter is unavailable".to_owned())?
            .calibrate(now, spent)?;
        self.save().await;
        self.replan();
        self.show_account().await;
        Ok(())
    }

    /// A question from the usage page. Anyone who can open it can read it; changing the mode,
    /// the allowance or the dashboard figure is the home owner's.
    async fn page(&mut self, asked: IncomingPage) {
        let owner = asked.caller.owner;
        let writes = matches!(asked.method.as_str(), "usage.set" | "usage.calibrate");
        if writes && !owner {
            asked.answer(Err("Only the home's owner can change this.".into()));
            return;
        }
        let answer = match asked.method.as_str() {
            "usage.summary" => self.summary(owner),
            "usage.days" => self.usage_with(page::days),
            "usage.calls" => self.usage_with(page::calls),
            "usage.set" => {
                let change = page::change(&asked.params);
                match self.set_prefs(change).await {
                    Ok(()) => self.summary(owner),
                    Err(why) => Err(why),
                }
            }
            "usage.calibrate" => {
                let spent = page::spent(&asked.params);
                match self.calibrate(spent).await {
                    Ok(()) => self.summary(owner),
                    Err(why) => Err(why),
                }
            }
            other => Err(format!("no page method called {other}")),
        };
        asked.answer(answer);
    }

    fn summary(&self, can_change: bool) -> Result<serde_json::Value, String> {
        let now = self.now();
        let (_, plan) = self.plan();
        let mut usage = self
            .meter
            .lock()
            .map_err(|_| "the meter is unavailable".to_owned())?;
        usage.roll(now);
        let outlook = usage.outlook(now);
        let warning = account::warning(&outlook, &plan, &usage.currency);
        Ok(page::summary(&usage, &outlook, &plan, warning, can_change))
    }

    fn usage_with(
        &self,
        answer: impl FnOnce(&Usage, &page::Names) -> serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let names = self.names();
        let mut usage = self
            .meter
            .lock()
            .map_err(|_| "the meter is unavailable".to_owned())?;
        usage.roll(self.now());
        Ok(answer(&usage, &names))
    }

    /// What the ledger's targets are called: every car and site the account has listed.
    fn names(&self) -> page::Names {
        let cars = self.known.cars.iter().map(|car| {
            (
                car.vin.clone(),
                page::Target {
                    name: car.name.clone(),
                    kind: "car",
                },
            )
        });
        let sites = self.known.sites.iter().map(|site| {
            (
                site.id.clone(),
                page::Target {
                    name: site.name.clone(),
                    kind: "site",
                },
            )
        });
        cars.chain(sites).collect()
    }

    /// Describes the account device and reports whichever of its states changed.
    async fn show_account(&mut self) {
        let now = self.now();
        let (currency, states) = {
            let Ok(mut usage) = self.meter.lock() else {
                return;
            };
            usage.roll(now);
            let outlook = usage.outlook(now);
            let states: Vec<(&'static str, State)> = account::entities(&usage.currency)
                .iter()
                .filter_map(|entity| {
                    Some((entity.key, account::state(entity.key, &usage, &outlook)?))
                })
                .collect();
            (usage.currency.clone(), states)
        };
        let device = DeviceDescription {
            unique_id: UniqueId::try_from(account::ACCOUNT_DEVICE).expect("a valid id"),
            name: Name::try_from("Tesla account").expect("a valid name"),
            manufacturer: Some("Tesla".into()),
            model: Some("Fleet API".into()),
            sw_version: None,
            hw_version: None,
            serial_number: None,
            suggested_area: None,
            via_device_unique_id: None,
        };
        if !reading::describe(
            &self.ctx,
            device,
            &mut self.account.device,
            &mut self.account.described,
            account::entities(&currency),
        )
        .await
        {
            return;
        }
        for (key, state) in states {
            if self.account.states.get(key) == Some(&state) {
                continue;
            }
            reading::report(
                &self.ctx,
                account::ACCOUNT_DEVICE,
                key,
                state.clone(),
                false,
            );
            self.account.states.insert(key, state);
        }
    }

    async fn action(&mut self, action: IncomingAction) {
        match action.action_id.clone().as_str() {
            "sign_in" => self.finish_sign_in(action).await,
            "check_key" => {
                if self.key.is_err() || self.registration == Registration::Checking {
                    action.reply(Err("the key is being checked already".into()));
                } else {
                    self.register(Some(action));
                }
            }
            "update_spend" => {
                let spent = action
                    .field("spent")
                    .ok_or_else(|| "say what Tesla's dashboard shows".to_owned())
                    .and_then(price::parse_money);
                let result = self.calibrate(spent).await;
                action.reply(result);
            }
            other => action.reply(Err(format!("no action called {other}"))),
        }
    }

    /// Registers the app with Tesla, unless that's done or under way.
    fn register(&mut self, asked: Option<IncomingAction>) {
        let Ok(key) = self.key.clone() else {
            if let Some(action) = asked {
                action.reply(Err("the private key in the settings can't be used".into()));
            }
            return;
        };
        if asked.is_none() && self.registration != Registration::Later {
            return;
        }
        self.registration = Registration::Checking;
        let api = self.api.clone();
        let domain = self.settings.domain.clone();
        let done = self.done.clone();
        tokio::spawn(async move {
            let result = setup::register(api, domain, key).await;
            let _ = done.send(Done::Registered(result, asked)).await;
        });
    }

    async fn finish_sign_in(&mut self, action: IncomingAction) {
        let Some(state) = self.sign_in_state.clone() else {
            action.reply(Err(
                "open the sign-in link first, then paste the address you land on".into(),
            ));
            return;
        };
        let code = match action
            .field("address")
            .ok_or_else(|| "paste the address you landed on".to_owned())
            .and_then(|address| auth::parse_landed(address, &state))
        {
            Ok(code) => code,
            Err(why) => {
                action.reply(Err(why));
                return;
            }
        };
        let api = self.api.clone();
        let done = self.done.clone();
        tokio::spawn(async move {
            let meter = Arc::clone(api.meter());
            let result = api.session().exchange(&code, &meter).await;
            let _ = done.send(Done::SignedIn(result, action)).await;
        });
    }

    /// Saves new tokens, then lets the refresh that got them go on.
    async fn keep_tokens(&mut self, keep: Keep) {
        let value = serde_json::to_value(&keep.tokens).unwrap_or_default();
        let saved = self.ctx.store(auth::STORED, value).await.is_ok();
        let _ = keep.done.send(saved);
    }

    async fn finished(&mut self, done: Done) {
        match done {
            Done::Registered(result, asked) => {
                let answer = match &result {
                    Ok(Registered::Yes) => Ok(()),
                    Ok(Registered::KeyDiffers) => {
                        Err("Tesla has a different public key for the domain".to_owned())
                    }
                    Ok(Registered::Refused(words)) => Err(format!("Tesla refused: {words}")),
                    Err(error) => Err(error.describe()),
                };
                match result {
                    Ok(registered) => {
                        if registered == Registered::Yes
                            && let Ok(key) = &self.key
                        {
                            let hex = serde_json::Value::from(key.public_hex());
                            let _ = self.ctx.store(stored::REGISTERED, hex).await;
                        }
                        self.registration = Registration::Done(registered);
                    }
                    Err(error) => {
                        self.registration = Registration::Later;
                        self.trouble_from(error);
                    }
                }
                if let Some(action) = asked {
                    action.reply(answer);
                }
            }
            Done::SignedIn(result, action) => match result {
                Ok(_) => {
                    self.signed_in = true;
                    self.signed_out = false;
                    self.trouble = None;
                    self.sign_in_state = None;
                    let _ = self
                        .ctx
                        .store(stored::SIGN_IN_STATE, serde_json::Value::Null)
                        .await;
                    self.next_list = Some(Instant::now());
                    action.reply(Ok(()));
                }
                Err(error) => action.reply(Err(error.describe())),
            },
            Done::Listed(result) => {
                self.listing = false;
                match result {
                    Ok((products, keys)) => {
                        self.list_backoff.succeeded();
                        self.trouble = None;
                        self.known.cars = products.cars;
                        self.known.sites = products.sites;
                        if let Some(keys) = keys {
                            self.known.unpaired = keys.unpaired.into_iter().collect();
                            self.known.unsigned = keys.unsigned.into_iter().collect();
                        }
                        self.keep_known().await;
                        self.adopt().await;
                        self.next_list = Some(Instant::now() + self.timing.relist);
                        if self.registration == Registration::Later {
                            self.register(None);
                        }
                    }
                    Err(error) => {
                        let wait = match &error {
                            ApiError::RateLimited(Some(wait)) => *wait,
                            _ => self.list_backoff.failed(),
                        };
                        let signed_out = matches!(error, ApiError::SignedOut(_));
                        self.trouble_from(error);
                        if !signed_out {
                            self.next_list = Some(Instant::now() + wait);
                        }
                    }
                }
            }
            Done::States(result) => self.states_checked(result).await,
            Done::CarRead {
                vin,
                cached,
                result,
            } => self.car_read(vin, cached, result).await,
            Done::SiteRead { id, part, result } => self.site_read(id, part, result).await,
            Done::Woken { vin, result, call } => self.woken(vin, result, call).await,
            Done::Commanded {
                device,
                command,
                result,
                call,
            } => self.commanded(device, command, result, call).await,
        }
    }

    /// Keeps the cars and sites as last listed, and which have Irori's key, for the next start.
    async fn keep_known(&mut self) {
        if let Ok(value) = serde_json::to_value(&self.known) {
            let _ = self.ctx.store(stored::PRODUCTS, value).await;
        }
    }

    /// Notes an account-wide failure.
    fn trouble_from(&mut self, error: ApiError) {
        if matches!(error, ApiError::SignedOut(_)) {
            self.signed_out = self.signed_in;
            self.signed_in = false;
        }
        self.trouble = Some(error);
    }

    /// Lists the cars and sites, and which cars have Irori's key.
    fn list(&mut self) {
        if self.listing || !self.signed_in {
            return;
        }
        self.listing = true;
        let api = self.api.clone();
        let done = self.done.clone();
        tokio::spawn(async move {
            let result = async {
                let products = api.products(Why::Start).await?;
                let vins: Vec<String> = products.cars.iter().map(|car| car.vin.clone()).collect();
                let keys = if vins.is_empty() {
                    None
                } else {
                    // A key check that fails doesn't hide the cars.
                    api.fleet_status(&vins, Why::Start).await.ok()
                };
                Ok((products, keys))
            }
            .await;
            let _ = done.send(Done::Listed(result)).await;
        });
    }

    /// The setup steps still to do.
    async fn waiting(&mut self) -> Vec<Waiting> {
        let mut steps = Vec::new();
        if let Ok(key) = &self.key
            && let Registration::Done(why @ (Registered::KeyDiffers | Registered::Refused(_))) =
                &self.registration
        {
            steps.push(setup::key_step(&self.settings.domain, key, why));
        }
        if !self.signed_in {
            let state = match &self.sign_in_state {
                Some(state) => state.clone(),
                None => {
                    let state = auth::new_state();
                    let _ = self
                        .ctx
                        .store(
                            stored::SIGN_IN_STATE,
                            serde_json::Value::from(state.clone()),
                        )
                        .await;
                    self.sign_in_state = Some(state.clone());
                    state
                }
            };
            let url = self.api.session().authorize_url(&state);
            steps.push(setup::sign_in_step(url, self.signed_out));
        }
        for car in &self.known.cars {
            if self.known.unpaired.contains(&car.vin)
                && let Some(step) =
                    setup::virtual_key_step(&self.settings.domain, &car.vin, &car.name)
            {
                steps.push(step);
            }
        }
        steps
    }

    fn health(&self) -> Health {
        if let Err(why) = &self.key {
            return Health::Degraded(format!(
                "The private key in the settings can't be used: {why}. Paste all of \
                 tesla_fleet.key, or clear it and Irori makes its own."
            ));
        }
        match &self.trouble {
            Some(ApiError::Billing(_)) => Health::Degraded(format!(
                "Tesla suspended the API: the account's billing limit is reached. Raise it on \
                 developer.tesla.com (Billing and usage), or readings resume on {}.",
                usage::next_month_label(self.now())
            )),
            Some(ApiError::SignedOut(_)) if self.signed_out => {
                Health::Degraded("Tesla signed Irori out: sign in again.".into())
            }
            Some(ApiError::Forbidden(why)) => Health::Degraded(format!(
                "Tesla refused Irori a permission ({why}). Sign in again and allow everything \
                 Irori asks for."
            )),
            Some(
                error @ (ApiError::Server(_) | ApiError::Unreachable(_) | ApiError::RateLimited(_)),
            ) => Health::Degraded(format!("{}; trying again.", capitalise(&error.describe()))),
            _ => {
                let mut notes = Vec::new();
                let (_, plan) = self.plan();
                if let Ok(usage) = self.meter.lock() {
                    let outlook = usage.outlook(self.now());
                    notes.extend(account::warning(&outlook, &plan, &usage.currency));
                }
                let refused = self.refused();
                if !refused.is_empty() {
                    notes.push(format!(
                        "Tesla won't let Irori read {}.",
                        refused.join("; ")
                    ));
                }
                if notes.is_empty() {
                    Health::Running
                } else {
                    Health::Degraded(notes.join(" "))
                }
            }
        }
    }

    fn available_actions(&self) -> Vec<String> {
        let mut actions = Vec::new();
        if !self.signed_in {
            actions.push("sign_in".to_owned());
        }
        if self.key.is_ok() && self.registration != Registration::Done(Registered::Yes) {
            actions.push("check_key".to_owned());
        }
        actions.push("update_spend".to_owned());
        actions
    }

    /// Tells the core what changed: setup steps, actions, health, the month's figures.
    async fn report(&mut self) {
        self.show_account().await;
        let waiting = self.waiting().await;
        if self.reported.waiting.as_ref() != Some(&waiting) {
            self.ctx.set_waiting(waiting.clone()).await;
            self.reported.waiting = Some(waiting);
        }
        let actions = self.available_actions();
        if self.reported.actions.as_ref() != Some(&actions) {
            self.ctx.set_available_actions(actions.clone()).await;
            self.reported.actions = Some(actions);
        }
        let health = self.health();
        if self.reported.health.as_ref() != Some(&health) {
            self.ctx.set_health(health.clone()).await;
            self.reported.health = Some(health);
        }
        let map = self.map();
        if self.reported.map.as_ref() != Some(&map) {
            self.ctx.set_networks(vec![map.clone()]).await;
            self.reported.map = Some(map);
        }
    }

    /// The map: the cars and sites, through Tesla's cloud.
    fn map(&self) -> irori_types::Network {
        let cars: Vec<(String, String, String)> = self
            .cars
            .iter()
            .map(|(vin, slot)| {
                let state = slot.reading.state.as_deref().unwrap_or("offline");
                let detail = match slot.reading.model() {
                    Some(model) => format!("{model}, {state}"),
                    None => state.to_owned(),
                };
                (vin.clone(), slot.car.name.clone(), detail)
            })
            .collect();
        let sites: Vec<(String, String, String)> = self
            .sites
            .iter()
            .map(|(id, slot)| {
                let model = if slot.site.battery {
                    "Powerwall"
                } else {
                    "Solar"
                };
                (id.clone(), slot.site.name.clone(), model.to_owned())
            })
            .collect();
        let drawn = cars
            .iter()
            .chain(&sites)
            .map(|(id, name, detail)| network::Drawn {
                id,
                name,
                detail: Some(detail),
            });
        network::draw(drawn, self.signed_in)
    }

    /// Stores the meter, if it changed.
    async fn save(&mut self) {
        let values = {
            let Ok(mut usage) = self.meter.lock() else {
                return;
            };
            if !usage.dirty {
                return;
            }
            usage.dirty = false;
            usage.to_store()
        };
        for (key, value) in values {
            if self.ctx.store(&key, value).await.is_err()
                && let Ok(mut usage) = self.meter.lock()
            {
                usage.dirty = true;
            }
        }
    }
}

/// The private key: the one pasted in the settings, else the one Irori made earlier, else a
/// new one, kept.
async fn load_key(settings: &Settings, ctx: &ProtocolContext) -> Result<Key, String> {
    if let Some(pasted) = &settings.private_key {
        return Key::from_pasted(pasted.expose());
    }
    let kept: Option<String> = load(ctx, keys::STORED).await;
    if let Some(key) = kept.as_deref().and_then(|kept| Key::from_stored(kept).ok()) {
        return Ok(key);
    }
    let key = Key::generate()?;
    let value = serde_json::Value::from(key.to_stored()?);
    ctx.store(keys::STORED, value)
        .await
        .map_err(|_| "couldn't keep the key Irori made".to_owned())?;
    Ok(key)
}

async fn load_usage(ctx: &ProtocolContext, settings: &Settings, now: u64) -> Usage {
    let prices = Prices::from_settings(settings);
    let credit = (settings.monthly_credit * price::MICROS as f64).round() as price::Micros;
    let fresh = Usage::new(prices, settings.currency.clone(), credit, now);
    let this = usage::month_of(now);
    let last = usage::previous_month_of(now);
    let stored = usage::Stored {
        month: load(ctx, &usage::keys::month(&this)).await,
        last_month: load(ctx, &usage::keys::month(&last)).await,
        calls: load(ctx, usage::keys::CALLS).await,
        points: load(ctx, usage::keys::POINTS).await,
        prefs: load(ctx, usage::keys::PREFS).await,
    };
    fresh.restore(now, stored)
}

async fn load<T: serde::de::DeserializeOwned>(ctx: &ProtocolContext, key: &str) -> Option<T> {
    let value = ctx.load(key).await.ok()??;
    serde_json::from_value(value).ok()
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(365 * 86_400)
}
