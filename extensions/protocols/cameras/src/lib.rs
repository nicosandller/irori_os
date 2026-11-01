//! IP cameras on the local network, over ONVIF and RTSP.
//!
//! Cameras are found with WS-Discovery. Each one needs its own login before anything else can
//! be asked of it, so a camera first shows up on the Add device screen asking for one
//! ([`Waiting`]). Once it has one, it becomes a device with a camera entity, and a page can ask
//! it for a picture.
//!
//! **A refused login is never sent again.** Cameras lock an account after a few failures, and
//! giving any camera its login restarts this extension, so remembering for one run isn't
//! enough. A fingerprint of each refused login is kept in the extension's store, and a camera
//! whose login still matches it waits for a different one without being asked
//! ([`refused`]). Only the camera saying no to the login counts: a timeout or a camera that
//! can't be reached is safe to try again.

mod discover;
mod events;
mod flac;
mod fmp4;
mod http;
mod live;
mod onvif;
mod ptz;
mod refused;
pub mod settings;
mod soap;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use irori_protocol::{
    AvailabilityTarget, Health, Incoming, IncomingCall, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{
    Availability, BinarySensorCapabilities, BinarySensorClass, BinarySensorState,
    CameraCapabilities, CameraPreset, CameraState, CameraStatus, Capabilities, DeviceDescription,
    EntityDescription, LiveQuality, Name, SecretRequest, Service, State, StateReport, UniqueId,
    UsernameRequest, Waiting,
};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::discover::Found;
use crate::events::Detects;
use crate::http::HttpError;
use crate::onvif::Info;
use crate::refused::Refusals;
use crate::settings::{Login, Settings};
use crate::soap::Soap;

/// The Cameras protocol.
#[derive(Debug)]
pub struct Cameras;

impl Protocol for Cameras {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let (found, incoming) = mpsc::channel(32);
        let discovery = discover::start(&settings, found);
        let outcome = drive(settings, ctx, incoming).await;
        discovery.abort();
        outcome
    }
}

/// How long a camera that couldn't be reached is left before it's tried again, when it next
/// answers discovery.
const RETRY: Duration = Duration::from_secs(60);
/// The longest setting a camera up may take, all its questions together.
const SETTING_UP: Duration = Duration::from_secs(90);
const EVENTS: usize = 32;

/// Where one camera stands.
enum Stage {
    /// It has no login yet.
    NeedsLogin,
    /// Its login can't be read.
    BadLogin(String),
    /// It turned down the login it has. Waits for a different one.
    Refused,
    /// Being logged into now.
    Connecting,
    Ready(Arc<Ready>),
    /// It couldn't be reached, or answered strangely. Tried again after `retry`.
    Unreachable {
        why: String,
        retry: Instant,
    },
}

/// A camera that's logged into.
struct Ready {
    soap: Soap,
    info: Info,
}

struct Cam {
    found: Found,
    /// Bumped on each new login attempt, so an answer from an older one is ignored.
    generation: u64,
    stage: Stage,
    /// Whether it has been described to the core in this run.
    described: bool,
    /// How many of its streams are running: it's `streaming` while any are.
    watched: u32,
    /// What follows its events and turns it, while it's logged into. Stopped when that ends.
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Its moves go here, when it turns.
    ptz: Option<mpsc::Sender<IncomingCall>>,
    /// Its presets as last read, when it has any.
    presets: Option<Vec<CameraPreset>>,
    /// The sensors described for it.
    sensors: Vec<Detects>,
}

impl Cam {
    /// Stops what was running for the last login.
    fn stop_tasks(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.ptz = None;
    }
}

/// What a background task found out.
enum Event {
    Connected {
        id: String,
        generation: u64,
        ready: Box<Ready>,
    },
    Refused {
        id: String,
        generation: u64,
    },
    Unreachable {
        id: String,
        generation: u64,
        why: String,
    },
    /// A stream started (`true`) or ended.
    Watched {
        id: String,
        started: bool,
    },
    /// One of its sensors changed.
    Detected {
        id: String,
        generation: u64,
        change: events::Change,
    },
    /// Its presets, as just read.
    Presets {
        id: String,
        generation: u64,
        presets: Vec<CameraPreset>,
    },
}

/// Runs the protocol against an injected discovery stream. Tests use this so nothing probes the
/// real network.
pub(crate) async fn drive(
    settings: Settings,
    mut ctx: ProtocolContext,
    mut discovery: mpsc::Receiver<Found>,
) -> Result<(), ProtocolError> {
    let mut refusals = Refusals::load(&ctx).await;
    let (events_tx, mut events) = mpsc::channel(EVENTS);
    let mut cams: BTreeMap<String, Cam> = BTreeMap::new();
    let mut generations = 0u64;
    let mut reported_health = Health::Running;
    ctx.set_health(reported_health.clone()).await;
    let mut reported_waiting: Vec<Waiting> = Vec::new();

    loop {
        tokio::select! {
            incoming = ctx.next() => {
                let Some(incoming) = incoming else { break Ok(()) };
                handle(incoming, &cams, &events_tx);
            }
            found = discovery.recv() => {
                let Some(found) = found else { break Ok(()) };
                arrive(found, &settings, &refusals, &mut cams, &mut generations, &events_tx);
            }
            Some(event) = events.recv() => {
                apply(event, &settings, &ctx, &mut refusals, &mut cams, &events_tx).await?;
            }
        }
        let health = health(&cams);
        if health != reported_health {
            ctx.set_health(health.clone()).await;
            reported_health = health;
        }
        let waiting = waiting(&cams);
        if waiting != reported_waiting {
            ctx.set_waiting(waiting.clone()).await;
            reported_waiting = waiting;
        }
    }
}

/// A camera answered discovery.
fn arrive(
    found: Found,
    settings: &Settings,
    refusals: &Refusals,
    cams: &mut BTreeMap<String, Cam>,
    generations: &mut u64,
    events: &mpsc::Sender<Event>,
) {
    if !cams.contains_key(&found.id) {
        tracing::info!(camera = %found.id, ip = %found.ip, "found a camera");
    }
    let cam = cams.entry(found.id.clone()).or_insert_with(|| Cam {
        found: found.clone(),
        generation: 0,
        stage: Stage::NeedsLogin,
        described: false,
        watched: 0,
        tasks: Vec::new(),
        ptz: None,
        presets: None,
        sensors: Vec::new(),
    });
    let moved = cam.found.ip != found.ip || cam.found.device_service != found.device_service;
    cam.found = found;
    let start = match &cam.stage {
        // A first sighting, or one still without a usable login: see what the settings have.
        Stage::NeedsLogin | Stage::BadLogin(_) => true,
        // Waiting for a different login; this run's settings can't have one.
        Stage::Refused => false,
        // Already being logged into: one attempt at a time, however many probes it answers.
        Stage::Connecting => false,
        Stage::Ready(_) => moved,
        Stage::Unreachable { retry, .. } => Instant::now() >= *retry || moved,
    };
    if !start {
        return;
    }
    cam.stage = match settings.cameras.get(&cam.found.id).map(|given| &given.0) {
        None => Stage::NeedsLogin,
        Some(Err(why)) => Stage::BadLogin(why.clone()),
        Some(Ok(login)) if refusals.refused(&cam.found.id, login) => Stage::Refused,
        Some(Ok(login)) => {
            cam.stop_tasks();
            *generations += 1;
            cam.generation = *generations;
            tracing::info!(camera = %cam.found.id, ip = %cam.found.ip, "logging into a camera");
            connect(
                cam.found.clone(),
                login.clone(),
                cam.generation,
                events.clone(),
            );
            Stage::Connecting
        }
    };
}

/// Logs into a camera in the background.
fn connect(found: Found, login: Login, generation: u64, events: mpsc::Sender<Event>) {
    tokio::spawn(async move {
        let id = found.id.clone();
        let connected = tokio::time::timeout(SETTING_UP, onvif::connect(&found, &login)).await;
        let connected = connected.unwrap_or_else(|_| {
            Err(HttpError::Unreachable(
                "it took too long to answer while being set up".into(),
            ))
        });
        let event = match connected {
            Ok((soap, info)) => Event::Connected {
                id,
                generation,
                ready: Box::new(Ready { soap, info }),
            },
            Err(HttpError::Refused) => Event::Refused { id, generation },
            Err(HttpError::Unreachable(why) | HttpError::Failed(why)) => Event::Unreachable {
                id,
                generation,
                why,
            },
        };
        let _ = events.send(event).await;
    });
}

async fn apply(
    event: Event,
    settings: &Settings,
    ctx: &ProtocolContext,
    refusals: &mut Refusals,
    cams: &mut BTreeMap<String, Cam>,
    events_tx: &mpsc::Sender<Event>,
) -> Result<(), ProtocolError> {
    let (id, generation) = match &event {
        Event::Connected { id, generation, .. }
        | Event::Refused { id, generation }
        | Event::Unreachable { id, generation, .. }
        | Event::Detected { id, generation, .. }
        | Event::Presets { id, generation, .. } => (id.clone(), *generation),
        Event::Watched { id, started } => {
            if let Some(cam) = cams.get_mut(id) {
                let before = cam.watched;
                cam.watched = if *started {
                    cam.watched.saturating_add(1)
                } else {
                    cam.watched.saturating_sub(1)
                };
                if cam.described && (before == 0) != (cam.watched == 0) {
                    report(ctx, id, cam)?;
                }
            }
            return Ok(());
        }
    };
    let Some(cam) = cams.get_mut(&id) else {
        return Ok(());
    };
    if cam.generation != generation {
        return Ok(());
    }
    match event {
        Event::Connected { ready, .. } => {
            let info = &ready.info;
            if let Some(Ok(login)) = settings.cameras.get(&id).map(|given| &given.0) {
                refusals.accepted(ctx, &id, login).await;
            }
            tracing::info!(
                camera = %id,
                profiles = %info
                    .profiles
                    .iter()
                    .map(|profile| format!("{} {}x{}", profile.encoding, profile.width, profile.height))
                    .collect::<Vec<_>>()
                    .join(", "),
                pictures = info.snapshot.is_some(),
                "logged into a camera"
            );
            let (device, entities) = describe(&cam.found, info)?;
            let mut added = ctx.describe_device(device).await.is_ok();
            for entity in entities {
                added = added && ctx.describe_entity(entity).await.is_ok();
            }
            if !added {
                tracing::warn!(camera = %id, "a camera couldn't be added");
                cam.stage = Stage::Unreachable {
                    why: "it couldn't be added".into(),
                    retry: Instant::now() + RETRY,
                };
                return Ok(());
            }
            cam.described = true;
            cam.sensors = info.detects.clone();
            cam.presets = info
                .turns
                .as_ref()
                .filter(|turning| turning.abilities.presets)
                .map(|_| Vec::new());
            available(ctx, &id, Availability::Available).await;
            report(ctx, &id, cam)?;
            let ready = Arc::new(*ready);
            start_tasks(cam, &id, generation, &ready, events_tx);
            cam.stage = Stage::Ready(ready);
        }
        Event::Refused { .. } => {
            tracing::warn!(camera = %id, "a camera refused its login; it won't be tried again until it's given a different one");
            if let Some(Ok(login)) = settings.cameras.get(&id).map(|given| &given.0) {
                refusals.refuse(ctx, &id, login).await;
            }
            cam.stop_tasks();
            if cam.described {
                available(ctx, &id, Availability::Unavailable).await;
            }
            cam.stage = Stage::Refused;
        }
        Event::Unreachable { why, .. } => {
            tracing::warn!(camera = %id, "a camera couldn't be set up: {why}");
            cam.stop_tasks();
            if cam.described {
                available(ctx, &id, Availability::Unavailable).await;
            }
            cam.stage = Stage::Unreachable {
                why,
                retry: Instant::now() + RETRY,
            };
        }
        Event::Detected { change, .. } => {
            if !cam.described || !matches!(cam.stage, Stage::Ready(_)) {
                return Ok(());
            }
            // A camera can notice something it didn't list: its sensor is added then.
            if !cam.sensors.contains(&change.detects) {
                if ctx
                    .describe_entity(sensor(&id, change.detects)?)
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                cam.sensors.push(change.detects);
            }
            ctx.report_state(StateReport {
                unique_id: sensor_entity(&id, change.detects)?,
                state: Some(State::BinarySensor(BinarySensorState { on: change.on })),
                attributes: BTreeMap::new(),
                caused_by: None,
                replayed: false,
            });
        }
        Event::Presets { presets, .. } => {
            if cam.presets.is_some() && cam.presets.as_ref() != Some(&presets) {
                cam.presets = Some(presets);
                report(ctx, &id, cam)?;
            }
        }
        // Handled above, before the generation check.
        Event::Watched { .. } => {}
    }
    Ok(())
}

/// The camera's state: `streaming` while anyone watches, else `idle`, with its presets.
fn report(ctx: &ProtocolContext, id: &str, cam: &Cam) -> Result<(), ProtocolError> {
    ctx.report_state(StateReport {
        unique_id: camera_entity(id)?,
        state: Some(State::Camera(CameraState {
            state: if cam.watched > 0 {
                CameraStatus::Streaming
            } else {
                CameraStatus::Idle
            },
            motion_detection: None,
            presets: cam.presets.clone(),
        })),
        attributes: BTreeMap::new(),
        caused_by: None,
        replayed: false,
    });
    Ok(())
}

/// Follows a camera's events and starts its PTZ worker, for this login.
fn start_tasks(
    cam: &mut Cam,
    id: &str,
    generation: u64,
    ready: &Arc<Ready>,
    events_tx: &mpsc::Sender<Event>,
) {
    if let Some(target) = ready.info.events.clone()
        && !ready.info.detects.is_empty()
    {
        let (changes_tx, mut changes) = mpsc::channel(32);
        let soap = ready.soap.clone();
        let tx = events_tx.clone();
        let who = id.to_owned();
        cam.tasks.push(tokio::spawn(async move {
            if let Err(HttpError::Refused) = events::follow(&soap, &target, &changes_tx).await {
                let _ = tx
                    .send(Event::Refused {
                        id: who,
                        generation,
                    })
                    .await;
            }
        }));
        let tx = events_tx.clone();
        let who = id.to_owned();
        cam.tasks.push(tokio::spawn(async move {
            while let Some(change) = changes.recv().await {
                let event = Event::Detected {
                    id: who.clone(),
                    generation,
                    change,
                };
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        }));
    }
    if let (Some(turning), Some(service)) =
        (ready.info.turns.clone(), ready.info.ptz_service.clone())
    {
        let (calls_tx, calls) = mpsc::channel(4);
        let (reports_tx, mut reports) = mpsc::channel(4);
        let worker = ptz::Ptz {
            soap: ready.soap.clone(),
            service,
            token: turning.token,
        };
        cam.tasks.push(tokio::spawn(ptz::run(
            worker,
            turning.abilities.presets,
            calls,
            reports_tx,
        )));
        let tx = events_tx.clone();
        let who = id.to_owned();
        cam.tasks.push(tokio::spawn(async move {
            while let Some(report) = reports.recv().await {
                let event = match report {
                    ptz::Report::Presets(presets) => Event::Presets {
                        id: who.clone(),
                        generation,
                        presets,
                    },
                    ptz::Report::Refused => Event::Refused {
                        id: who.clone(),
                        generation,
                    },
                };
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        }));
        cam.ptz = Some(calls_tx);
    }
}

async fn available(ctx: &ProtocolContext, id: &str, availability: Availability) {
    let Ok(device) = UniqueId::try_from(id) else {
        return;
    };
    if ctx
        .set_availability(AvailabilityTarget::Device(device), availability)
        .await
        .is_err()
    {
        tracing::warn!(camera = %id, "a camera's availability couldn't be set");
    }
}

/// Handles something from the core.
fn handle(incoming: Incoming, cams: &BTreeMap<String, Cam>, events: &mpsc::Sender<Event>) {
    match incoming {
        Incoming::Call(call) => {
            let moves = matches!(
                call.call.service,
                Service::CameraMove(_) | Service::CameraStop | Service::CameraGoToPreset(_)
            );
            let worker = call
                .call
                .unique_id
                .as_str()
                .strip_prefix("camera:")
                .and_then(|id| cams.get(id))
                .and_then(|cam| cam.ptz.as_ref());
            match (moves, worker) {
                (true, Some(worker)) => {
                    if let Err(error) = worker.try_send(call) {
                        let call = match error {
                            mpsc::error::TrySendError::Full(call)
                            | mpsc::error::TrySendError::Closed(call) => call,
                        };
                        call.reply(Err(ServiceError::failed(
                            "the camera is still busy with the last move",
                        )));
                    }
                }
                (true, None) => call.reply(Err(ServiceError::unavailable(
                    "the camera can't be reached",
                ))),
                (false, _) => call.reply(Err(ServiceError::failed("this camera can't do that"))),
            }
        }
        Incoming::Action(action) => action.reply(Err("this extension has no actions".into())),
        Incoming::Unpair(unpair) => unpair.reply(Err("cameras aren't paired".into())),
        Incoming::Picture(picture) => {
            let ready = ready_for(cams, picture.unique_id.as_str());
            let (id, ready) = match ready {
                Ok(pair) => pair,
                Err(why) => {
                    picture.reply(Err(why));
                    return;
                }
            };
            let Some(snapshot) = ready.info.snapshot.clone() else {
                picture.reply(Err("this camera only sends live video".into()));
                return;
            };
            let generation = cams.get(&id).map_or(0, |cam| cam.generation);
            let events = events.clone();
            tokio::spawn(async move {
                match onvif::picture(&ready.soap, &snapshot).await {
                    Ok(taken) => picture.reply(Ok(taken)),
                    Err(HttpError::Refused) => {
                        picture.reply(Err("the camera refused its login".into()));
                        let _ = events.send(Event::Refused { id, generation }).await;
                    }
                    Err(error) => picture.reply(Err(error.to_string())),
                }
            });
        }
        Incoming::Live(live) => {
            let (id, ready) = match ready_for(cams, live.unique_id.as_str()) {
                Ok(pair) => pair,
                Err(why) => {
                    live.feed.end(Some(why));
                    return;
                }
            };
            let profile = match live.quality {
                LiveQuality::Low => ready.info.low().or_else(|| ready.info.high()),
                LiveQuality::High => ready.info.high(),
            };
            let (Some(stream), Some(login)) = (
                profile.and_then(|profile| profile.stream.clone()),
                ready.soap.login.clone(),
            ) else {
                live.feed
                    .end(Some("this camera has no video stream".into()));
                return;
            };
            let generation = cams.get(&id).map_or(0, |cam| cam.generation);
            let ip = ready.soap.camera.ip;
            let events = events.clone();
            tokio::spawn(async move {
                let _ = events
                    .send(Event::Watched {
                        id: id.clone(),
                        started: true,
                    })
                    .await;
                let ended = live::run(&stream, ip, &login, live).await;
                let _ = events
                    .send(Event::Watched {
                        id: id.clone(),
                        started: false,
                    })
                    .await;
                if ended == live::Ended::Refused {
                    let _ = events.send(Event::Refused { id, generation }).await;
                }
            });
        }
    }
}

/// The logged-in camera an entity belongs to, or why there isn't one.
fn ready_for(cams: &BTreeMap<String, Cam>, entity: &str) -> Result<(String, Arc<Ready>), String> {
    let id = entity
        .strip_prefix("camera:")
        .ok_or_else(|| "no camera has that entity".to_owned())?;
    let cam = cams
        .get(id)
        .ok_or_else(|| "no camera has that entity".to_owned())?;
    match &cam.stage {
        Stage::Ready(ready) => Ok((id.to_owned(), Arc::clone(ready))),
        Stage::Refused => Err("the camera refused its login".into()),
        _ => Err("the camera can't be reached".into()),
    }
}

fn camera_entity(id: &str) -> Result<UniqueId, ProtocolError> {
    UniqueId::try_from(format!("camera:{id}"))
        .map_err(|error| ProtocolError::new(error.to_string()))
}

/// The device, its camera entity, and a sensor for each thing it notices.
fn describe(
    found: &Found,
    info: &Info,
) -> Result<(DeviceDescription, Vec<EntityDescription>), ProtocolError> {
    let device_id = UniqueId::try_from(found.id.as_str())
        .map_err(|error| ProtocolError::new(error.to_string()))?;
    let device = DeviceDescription {
        unique_id: device_id.clone(),
        name: device_name(found, info),
        manufacturer: info.manufacturer.clone(),
        model: info.model.clone().or_else(|| found.hardware.clone()),
        sw_version: info.firmware.clone(),
        hw_version: None,
        serial_number: info.serial.clone(),
        suggested_area: None,
        via_device_unique_id: None,
    };
    let entity = EntityDescription {
        unique_id: camera_entity(&found.id)?,
        name: None,
        device_unique_id: Some(device_id),
        suggested_object_id: None,
        capabilities: Capabilities::Camera(capabilities(info)),
        entity_category: None,
    };
    let mut entities = vec![entity];
    for detects in &info.detects {
        entities.push(sensor(&found.id, *detects)?);
    }
    Ok((device, entities))
}

fn sensor_entity(id: &str, detects: Detects) -> Result<UniqueId, ProtocolError> {
    UniqueId::try_from(format!("{}:{id}", detects.key()))
        .map_err(|error| ProtocolError::new(error.to_string()))
}

/// The binary sensor for one thing a camera notices.
fn sensor(id: &str, detects: Detects) -> Result<EntityDescription, ProtocolError> {
    let device = UniqueId::try_from(id).map_err(|error| ProtocolError::new(error.to_string()))?;
    Ok(EntityDescription {
        unique_id: sensor_entity(id, detects)?,
        name: Some(name_of(detects.name())),
        device_unique_id: Some(device),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: match detects {
                Detects::Motion => Some(BinarySensorClass::Motion),
                Detects::Person => Some(BinarySensorClass::Occupancy),
                Detects::Vehicle | Detects::Animal => None,
            },
        }),
        entity_category: None,
    })
}

/// What it can do: live video at the qualities it has a stream for.
fn capabilities(info: &Info) -> CameraCapabilities {
    let streams = |profile: Option<&onvif::Profile>| profile.is_some_and(|p| p.stream.is_some());
    let mut qualities = Vec::new();
    if streams(info.low()) {
        qualities.push(LiveQuality::Low);
    }
    if streams(info.high()) {
        qualities.push(LiveQuality::High);
    }
    let abilities = info
        .turns
        .as_ref()
        .map(|turning| turning.abilities)
        .unwrap_or_default();
    CameraCapabilities {
        live: !qualities.is_empty(),
        qualities,
        pan_tilt: abilities.pan_tilt,
        zoom: abilities.zoom,
        presets: abilities.presets,
        ..CameraCapabilities::default()
    }
}

/// What to call it: its maker and model, which is what cameras' own names rarely are
/// (a Reolink announces itself as `IPC-BO`).
fn device_name(found: &Found, info: &Info) -> Name {
    let maker = info.manufacturer.as_deref().unwrap_or_default().trim();
    let text = match info.model.as_deref().or(found.hardware.as_deref()) {
        Some(model)
            if !maker.is_empty()
                && !model
                    .to_ascii_lowercase()
                    .contains(&maker.to_ascii_lowercase()) =>
        {
            format!("{maker} {model}")
        }
        Some(model) => model.to_owned(),
        None => found.name.clone(),
    };
    name_of(&text)
}

fn name_of(text: &str) -> Name {
    let short: String = text.trim().chars().take(100).collect();
    Name::try_from(short.trim()).unwrap_or_else(|_| Name::try_from("Camera").expect("a name"))
}

/// What the Add device screen shows for cameras that need a login.
fn waiting(cams: &BTreeMap<String, Cam>) -> Vec<Waiting> {
    cams.values()
        .filter_map(|cam| {
            let reason = match &cam.stage {
                Stage::NeedsLogin => "it needs the camera's login".to_owned(),
                Stage::BadLogin(why) => why.clone(),
                Stage::Refused => "the camera refused that login".to_owned(),
                _ => return None,
            };
            Some(Waiting {
                unique_id: UniqueId::try_from(cam.found.id.as_str()).ok()?,
                name: name_of(cam.found.hardware.as_deref().unwrap_or(&cam.found.name)),
                reason,
                secret: Some(SecretRequest {
                    path: vec!["cameras".to_owned(), cam.found.id.clone()],
                    label: "Password".to_owned(),
                    hint: Some(
                        "the camera's own login; for a Tapo camera, make one in the Tapo app, \
                         under the camera's Advanced settings, then Camera account"
                            .to_owned(),
                    ),
                    username: Some(UsernameRequest {
                        label: "Username".to_owned(),
                        placeholder: Some("admin".to_owned()),
                    }),
                }),
            })
        })
        .collect()
}

fn health(cams: &BTreeMap<String, Cam>) -> Health {
    let unreachable: Vec<&str> = cams
        .values()
        .filter_map(|cam| match &cam.stage {
            Stage::Unreachable { why, .. } => Some(why.as_str()),
            _ => None,
        })
        .collect();
    match unreachable.as_slice() {
        [] => Health::Running,
        [why] => Health::Degraded(format!("a camera can't be set up: {why}")),
        many => Health::Degraded(format!("{} cameras can't be set up", many.len())),
    }
}
