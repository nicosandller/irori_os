//! HTTP server: health, a temporary unauthenticated view of the core under `/api/dev/` (reads,
//! plus the commands the Devices page sends), and the embedded UI. Nothing here checks who is
//! asking, which is why `serve` binds loopback unless told otherwise; auth and the WebSocket API
//! arrive with `irori-api` in M1.5.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use irori_core::{CallError, Command, Core, Event, ExtensionHost, ExtensionOverview};
use irori_types::{
    Area, AreaId, ContextId, Description, Device, DeviceId, Entity, EntityId, EntityState,
    ExtensionId, Floor, FloorId, Floorplan, LightTurnOn, Name, Origin, Placement, UserId,
};
use tokio::sync::broadcast;

use crate::build_info::{BuildInfo, VERSION};
use crate::config::{Config, EditError, Refused};
use crate::db::Database;
use crate::history::History;

/// Who commands are attributed to until there are accounts to attribute them to (M1.5).
static UNAUTHENTICATED: LazyLock<UserId> =
    LazyLock::new(|| UserId::try_from("unauthenticated").expect("a valid user id"));

#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    started: Instant,
    /// A different value on every boot, so a page that spots it changing knows the restart it
    /// asked for happened. Uptime can't say that: a machine that boots and is opened within a
    /// minute would make a fresh-ish uptime look like a restart, and a slow restart could pass
    /// any freshness bound. The generation survives an exec (the process id is the same), so it
    /// has to be made anew inside this constructor rather than keyed off the pid.
    boot_id: String,
    db: Database,
    build: BuildInfo,
    core: Core,
    config: Config,
    host: ExtensionHost,
    history: History,
    /// Waking this asks `serve` (crates/irori/src/main.rs) to shut down and start this binary
    /// again. The atomic records that the shutdown was a requested restart: `serve` reads it
    /// once the server has stopped and re-execs itself instead of just stopping.
    restart: Arc<tokio::sync::Notify>,
    restarting: Arc<AtomicBool>,
    /// When a restart was last *accepted* this boot, for `RESTART_COOLDOWN`. Fresh (`None`) in
    /// every new process, which is the point: the window resets with the boot, so the cap is one
    /// restart per cooldown per boot rather than a permanent lockout.
    last_restart: Mutex<Option<Instant>>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Database,
        core: Core,
        config: Config,
        host: ExtensionHost,
        history: History,
        restart: Arc<tokio::sync::Notify>,
        restarting: Arc<AtomicBool>,
    ) -> Self {
        Self(Arc::new(Inner {
            started: Instant::now(),
            boot_id: boot_id(),
            db,
            build: BuildInfo::current(),
            core,
            config,
            host,
            history,
            restart,
            restarting,
            last_restart: Mutex::new(None),
        }))
    }
}

/// A fresh value after every boot: 16 random bytes from the OS, in hex. It must be
/// genuinely random rather than, say, the wall clock: the same process image re-execs on a
/// restart, so a clock that froze or stepped back could hand the new boot the old boot's id,
/// and the page would then never see the change that tells it the restart happened.
fn boot_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .expect("the OS must be able to hand over sixteen random bytes for the boot id");
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        // Unstable, for the UI and for trying things out until the real API (M0.5, M1.5) exists.
        // Everything the Devices page shows, in one response; the rest are the same data split up.
        .route("/api/dev/home", get(home))
        .route("/api/dev/command", post(command))
        .route(
            "/api/dev/devices",
            get(|State(s): State<AppState>| async move { Json(s.0.core.devices()) }),
        )
        .route(
            "/api/dev/entities",
            get(|State(s): State<AppState>| async move { Json(s.0.core.entities()) }),
        )
        .route(
            "/api/dev/states",
            get(|State(s): State<AppState>| async move { Json(s.0.core.states()) }),
        )
        .route("/api/dev/history/{entity_id}", get(entity_history))
        .route("/api/dev/system", get(host_info))
        .route("/api/dev/serial-ports", get(serial_ports))
        .route("/api/dev/restart", post(restart))
        .route(
            "/api/dev/extensions",
            get(|State(s): State<AppState>| async move { Json(s.0.core.extensions()) }),
        )
        // What a person has said about their home (`docs/specs/config.md`). These write files.
        .route("/api/dev/areas", get(areas).post(add_area))
        .route("/api/dev/floors", get(floors).post(add_floor))
        .route(
            "/api/dev/floors/{id}",
            patch(edit_floor).delete(remove_floor),
        )
        .route("/api/dev/areas/{id}", patch(edit_area).delete(remove_area))
        .route("/api/dev/floorplan", get(floorplan).put(save_floorplan))
        .route("/api/dev/devices/{id}", patch(edit_device))
        .route("/api/dev/entities/{id}", patch(edit_entity))
        .route("/api/dev/extensions/{id}/secrets", put(give_secret))
        .route(
            "/api/dev/extensions/{id}/settings",
            post(set_extension_settings),
        )
        .route(
            "/api/dev/extensions/{id}/actions/{action_id}",
            post(trigger_extension_action),
        )
        .route("/api/dev/catalog", get(catalog))
        .route("/api/dev/extensions/{id}/install", post(install_official))
        .route("/api/dev/extensions/install", post(install_url))
        .route("/api/dev/extensions/{id}", axum::routing::delete(uninstall))
        .route("/api/dev/helpers/toggles", post(add_toggle))
        .route(
            "/api/dev/helpers/toggles/{id}",
            axum::routing::delete(remove_toggle),
        )
        .route("/api/dev/extensions/{id}/icon.svg", get(extension_icon))
        .fallback(get(ui::serve))
        .with_state(state)
}

/// Everything the Devices page shows, in one response: what exists, what it's doing, and how the
/// extensions behind it are faring. The page asks for it every couple of seconds; it starts
/// listening for changes instead once the WebSocket API lands (M1.5).
#[derive(Debug, Serialize)]
struct HomeView {
    devices: Vec<Device>,
    entities: Vec<Entity>,
    states: Vec<EntityState>,
    extensions: BTreeMap<ExtensionId, ExtensionOverview>,
    areas: Vec<Area>,
    /// The levels of the home, lowest first. Empty for a home nobody has divided into floors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    floors: Vec<Floor>,
    /// Devices a person keeps out of the home, so they can be let back in.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    held: Vec<irori_core::HeldDevice>,
    /// The home as it's drawn. Left out entirely while nobody has drawn one, which is most
    /// homes: the Floorplan page then knows to offer a blank canvas rather than an empty plan.
    #[serde(skip_serializing_if = "Floorplan::is_empty")]
    floorplan: Floorplan,
}

async fn home(State(state): State<AppState>) -> Json<HomeView> {
    let core = &state.0.core;
    Json(HomeView {
        devices: core.devices(),
        entities: core.entities(),
        states: core.states(),
        extensions: core.extensions(),
        areas: core.areas(),
        floors: core.floors(),
        held: core.held_devices(),
        floorplan: core.floorplan(),
    })
}

/// The machine running Irori, for the Settings page's System menu. Read from the OS each time
/// it's asked, rather than kept: a Settings page check that cached could shrug at a disk that
/// filled or a machine that was swapped out from under it.
async fn host_info(State(state): State<AppState>) -> Json<crate::host_info::HostView> {
    Json(crate::host_info::read(&state.0.db.path))
}

/// Serial devices plugged into this machine right now, for a settings field the schema marks
/// `"format": "serial-port"` (a Zigbee dongle, say) to offer as a live-updated list of
/// candidates, alongside the plain text box a device path always was. Read fresh each time, the
/// same reasoning as `host_info`: a device plugged in or removed while the form is open should
/// show up without reopening it.
async fn serial_ports() -> Json<Vec<String>> {
    Json(crate::serial::list())
}

/// The Settings page asks for a restart with this header. Nothing in `/api/dev/*` has
/// authentication yet (that is M1.5), so this is a pre-auth stand-in and it is worth saying
/// plainly what it is and isn't: it stops the *browser* vectors — a cross-site `<form>` POST
/// carries no headers, and a fetch with a custom header is stopped by CORS preflight, so no
/// website the operator happens to have open can silently restart a loopback install — but it
/// is not authorization. A client that can already reach the server (`--allow-unauthenticated-lan`
/// puts every network client in that position, for this route and every other `/api/dev/*`
/// route) can simply send the header. What that client buys with it is capped by
/// `RESTART_COOLDOWN`, and the real boundary — authentication — arrives with M1.5.
const UI_HEADER: &str = "x-irori-ui";

/// How long after an accepted restart the endpoint answers 429 instead of accepting another
/// one. A restart is an outage, so a caller who can reach this route at all (spoofing
/// `UI_HEADER` is trivial pre-auth) should not be able to hammer it into a permanent one; this
/// bounds how often a boot can be taken down, and costs nothing for the real page — the button
/// is disabled while the restart is in flight anyway.
const RESTART_COOLDOWN: Duration = Duration::from_secs(30);

/// Restarts Irori, at the Settings page's request: wake the shutdown in `serve`
/// (crates/irori/src/main.rs), which stops the server and extensions cleanly and then starts
/// this very binary again. The button is on the Settings page because that's where someone who
/// can change how Irori runs is looking. A bare POST — the page's header missing — is refused
/// with what a caller would need to know to form a valid one, and so is a second request
/// inside `RESTART_COOLDOWN`.
async fn restart(State(state): State<AppState>, request: axum::extract::Request) -> Response {
    if !request
        .headers()
        .get(UI_HEADER)
        .is_some_and(|value| value == "1")
    {
        return refused(
            StatusCode::FORBIDDEN,
            format!("restart needs the `{UI_HEADER}: 1` header, which only the page sends"),
        );
    }
    {
        // A tiny critical section — read a clock, maybe write it — held across no await, so a
        // plain mutex rather than anything async.
        let mut last = state
            .0
            .last_restart
            .lock()
            .expect("a restart clock poisoned by a panic nobody handles");
        if last.is_some_and(|at| at.elapsed() < RESTART_COOLDOWN) {
            return refused(
                StatusCode::TOO_MANY_REQUESTS,
                "a restart was just asked for; Irori is still getting back up".to_owned(),
            );
        }
        *last = Some(Instant::now());
    }
    state.0.restarting.store(true, Ordering::SeqCst);
    tracing::info!("restart requested; shutting down");
    // notify_one, not notify_waiters: a non-retaining wake that finds no waiter is lost, and a
    // restart can be asked for before the graceful shutdown future has registered its waiter (a
    // request can reach the handler before the accept loop's first poll of it). notify_one keeps
    // the notification around until the waiter registers, so the shutdown always lands.
    state.0.restart.notify_one();
    // Accepted, not No Content: the restart itself happens a moment later, once this request
    // has drained.
    StatusCode::ACCEPTED.into_response()
}

/// The last day of an entity's changes, for the expandable table under its row on the Devices
/// page. In memory, and only what this server has seen: it starts empty at each start, and the
/// SQLite recorder (M1.3, `irori-recorder`) keeps the surviving, longer view behind the same
/// idea.
#[derive(Debug, Serialize)]
struct HistoryView {
    entity: EntityId,
    /// The changes, oldest first. Empty when the entity exists but has changed nothing since
    /// this server started.
    states: Vec<EntityState>,
}

async fn entity_history(
    State(state): State<AppState>,
    Path(entity_id): Path<EntityId>,
) -> Response {
    if state.0.core.state(&entity_id).is_none() {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no entity `{entity_id}`"),
        );
    }
    Json(HistoryView {
        entity: entity_id.clone(),
        states: state.0.history.for_entity(&entity_id),
    })
    .into_response()
}

// --- Rooms, names, and where things live ---------------------------------------------------
//
// Each of these changes a file in the config directory and then tells the core
// (`docs/specs/config.md`). None of them touch what a protocol reports: taking a name away
// gives the protocol's name back, rather than leaving whatever was on screen.

async fn areas(State(state): State<AppState>) -> Json<Vec<Area>> {
    Json(state.0.core.areas())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaRequest {
    name: Name,
    #[serde(default)]
    floor: Option<FloorId>,
}

/// A change to a room: a new name, a floor (`null` for none), or both.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaEdit {
    #[serde(default)]
    name: Option<Name>,
    #[serde(default, deserialize_with = "patched")]
    floor: Patch<FloorId>,
}

/// A room on a floor that isn't there is refused; a room with no floor is fine.
fn floor_exists(settings: &irori_types::Settings, floor: Option<&FloorId>) -> Result<(), Refused> {
    match floor {
        Some(floor) if settings.floor(floor).is_none() => {
            Err(Refused(format!("there's no floor `{floor}`")))
        }
        _ => Ok(()),
    }
}

/// Makes a room. Two rooms may share a name — homes have two bathrooms — so the id, not the
/// name, is what has to be unique.
async fn add_area(State(state): State<AppState>, Json(request): Json<AreaRequest>) -> Response {
    let made = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            floor_exists(settings, request.floor.as_ref())?;
            let area = Area {
                id: irori_core::new_area_id(&request.name, &settings.areas),
                name: request.name.clone(),
                floor_id: request.floor.clone(),
            };
            settings.areas.push(area.clone());
            settings.areas.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(area)
        })
        .await;
    match made {
        Ok(area) => (StatusCode::CREATED, Json(area)).into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn edit_area(
    State(state): State<AppState>,
    Path(id): Path<AreaId>,
    Json(request): Json<AreaEdit>,
) -> Response {
    let renamed = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            if let Some(floor) = &request.floor {
                floor_exists(settings, floor.as_ref())?;
            }
            let area = settings
                .areas
                .iter_mut()
                .find(|area| area.id == id)
                .ok_or_else(|| Refused(format!("there's no room `{id}`")))?;
            if let Some(name) = &request.name {
                area.name = name.clone();
            }
            if let Some(floor) = &request.floor {
                area.floor_id = floor.clone();
            }
            Ok(area.clone())
        })
        .await;
    match renamed {
        Ok(area) => Json(area).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Removes a room. Devices that were in it are left unplaced, and what was said about them is
/// kept: making the room again puts them back.
async fn remove_area(State(state): State<AppState>, Path(id): Path<AreaId>) -> Response {
    let removed = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let before = settings.areas.len();
            settings.areas.retain(|area| area.id != id);
            if settings.areas.len() == before {
                return Err(Refused(format!("there's no room `{id}`")));
            }
            Ok(())
        })
        .await;
    match removed {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn floorplan(State(state): State<AppState>) -> Json<Floorplan> {
    Json(state.0.core.floorplan())
}

/// Replaces the whole plan.
///
/// The whole thing at once, not a wall at a time: the editor holds a working copy while somebody
/// draws and sends it when they press Save, so a half-finished room never reaches the file, and
/// Cancel is simply not sending. It is also what makes the page's undo trivially correct — there
/// is nothing on the server to undo.
/// What saving a plan did beyond writing it down.
#[derive(Debug, Serialize)]
struct PlanSaved {
    /// Devices the plan moved into the room they are standing in.
    placed: usize,
}

async fn save_floorplan(State(state): State<AppState>, Json(plan): Json<Floorplan>) -> Response {
    // Which devices the home has heard of, including the ones it is holding back. Read before
    // the edit, because the edit only gets to see what the config directory says.
    let known: std::collections::BTreeSet<DeviceId> = state
        .0
        .core
        .devices()
        .into_iter()
        .map(|device| device.id)
        .chain(state.0.core.held_devices().into_iter().map(|held| held.id))
        .collect();
    let saved = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            plan.check().map_err(|why| Refused(why.to_string()))?;
            settings.floorplan = plan.clone();
            Ok(place_devices(settings, &known))
        })
        .await;
    match saved {
        Ok(placed) => Json(PlanSaved { placed }).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Puts each device that was drawn inside a room into that room, and says how many moved.
///
/// Dragging a device onto the kitchen floor is a person saying where it is, so it may answer the
/// question `devices.toml` asks — a plan that knew and didn't say would be a drawing rather than
/// part of the home. Two things it must not do.
///
/// It must not overrule a **deliberate** "not in a room" (`area = false`,
/// `docs/specs/config.md` §3.2). That answer exists precisely so that a guess — the device's own
/// `suggested_area` — can't put it back, and a dot standing on a floor is another guess.
///
/// And it must not place a device into a room that isn't there: rooms are made in `areas.toml`,
/// and a plan naming one that has since been deleted is kept rather than obeyed (§6).
///
/// `known` is every device the home has heard of, the ones it is holding back included. A plan
/// can name a device that no longer exists — entries outlive what they point at (§4) — and
/// writing a `devices.toml` entry for one would turn a stale drawing into a settings entry for a
/// device nobody has. A device that already has an entry counts as known: something has been
/// said about it, so there is nothing to invent.
fn place_devices(
    settings: &mut irori_types::Settings,
    known: &std::collections::BTreeSet<DeviceId>,
) -> usize {
    let rooms: Vec<(DeviceId, AreaId)> = settings
        .floorplan
        .floors
        .values()
        .flat_map(|level| {
            level.devices.iter().filter_map(|placed| {
                if !known.contains(&placed.device) && !settings.devices.contains_key(&placed.device)
                {
                    return None;
                }
                let room = level.areas.iter().find(|area| area.contains(placed.at))?;
                settings.area(&room.area)?;
                Some((placed.device.clone(), room.area.clone()))
            })
        })
        .collect();

    let mut moved = 0;
    for (device, room) in rooms {
        let settings = settings.devices.entry(device).or_default();
        if settings.area == Placement::Nowhere || settings.area == Placement::In(room.clone()) {
            continue;
        }
        settings.area = Placement::In(room);
        moved += 1;
    }
    moved
}

async fn floors(State(state): State<AppState>) -> Json<Vec<Floor>> {
    Json(state.0.core.floors())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorRequest {
    name: Name,
    /// 0 is the entrance level; negative is below ground.
    #[serde(default)]
    level: i8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorEdit {
    #[serde(default)]
    name: Option<Name>,
    #[serde(default)]
    level: Option<i8>,
}

async fn add_floor(State(state): State<AppState>, Json(request): Json<FloorRequest>) -> Response {
    let made = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let floor = Floor {
                id: irori_core::new_floor_id(&request.name, &settings.floors),
                name: request.name.clone(),
                level: request.level,
            };
            settings.floors.push(floor.clone());
            settings.floors.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(floor)
        })
        .await;
    match made {
        Ok(floor) => (StatusCode::CREATED, Json(floor)).into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn edit_floor(
    State(state): State<AppState>,
    Path(id): Path<FloorId>,
    Json(request): Json<FloorEdit>,
) -> Response {
    let edited = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let floor = settings
                .floors
                .iter_mut()
                .find(|floor| floor.id == id)
                .ok_or_else(|| Refused(format!("there's no floor `{id}`")))?;
            if let Some(name) = &request.name {
                floor.name = name.clone();
            }
            if let Some(level) = request.level {
                floor.level = level;
            }
            Ok(floor.clone())
        })
        .await;
    match edited {
        Ok(floor) => Json(floor).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Removes a floor. Its rooms stay, on no floor; what they said about it is kept, so making the
/// floor again puts them back — as with rooms and their devices.
async fn remove_floor(State(state): State<AppState>, Path(id): Path<FloorId>) -> Response {
    let removed = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let before = settings.floors.len();
            settings.floors.retain(|floor| floor.id != id);
            if settings.floors.len() == before {
                return Err(Refused(format!("there's no floor `{id}`")));
            }
            Ok(())
        })
        .await;
    match removed {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

/// A field that can be set, cleared, or left alone: absent means "don't touch", `null` means
/// "clear it", and a value means "make it this".
type Patch<T> = Option<Option<T>>;

/// serde reads a plain `Option<Option<T>>` as `None` for both an absent field and a `null` one,
/// which loses exactly the distinction a PATCH needs.
fn patched<'de, T, D>(deserializer: D) -> Result<Patch<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Where to put a device, in a PATCH body: `"hall"` for a room, `false` for deliberately no
/// room, `null` to go back to having said nothing (which lets the device's own suggestion stand
/// in again). Absent leaves it alone.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WhereTo {
    In(AreaId),
    /// `false`. `true` would mean "yes, a room" without saying which, so it's refused.
    Nowhere(bool),
}

impl WhereTo {
    fn placement(&self) -> Result<Placement, Refused> {
        match self {
            WhereTo::In(area) => Ok(Placement::In(area.clone())),
            WhereTo::Nowhere(false) => Ok(Placement::Nowhere),
            WhereTo::Nowhere(true) => Err(Refused(
                "`area: true` doesn't say which room; send a room's id, `false` for no room, or \
                 null to leave it to the device"
                    .to_owned(),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceEdit {
    #[serde(default, deserialize_with = "patched")]
    name: Patch<Name>,
    #[serde(default, deserialize_with = "patched")]
    description: Patch<Description>,
    #[serde(default, deserialize_with = "patched")]
    area: Patch<WhereTo>,
    /// `true` takes the device out of the home; `false` lets it back in.
    #[serde(default)]
    ignored: Option<bool>,
    /// `true` adds a new device while Irori asks before adding.
    #[serde(default)]
    added: Option<bool>,
}

async fn edit_device(
    State(state): State<AppState>,
    Path(id): Path<DeviceId>,
    Json(request): Json<DeviceEdit>,
) -> Response {
    let core = &state.0.core;
    // An ignored device isn't in the registry, but it's still one a person can let back in.
    let known = core.devices().iter().any(|device| device.id == id)
        || core.held_devices().iter().any(|device| device.id == id);
    if !known {
        return refused(StatusCode::NOT_FOUND, format!("there's no device `{id}`"));
    }
    let edited = state
        .0
        .config
        .edit(core, |settings| {
            let area = match request.area.clone() {
                None => None,
                // `null`: nobody has said, so the device's own suggestion may stand in again.
                Some(None) => Some(Placement::Unsaid),
                Some(Some(where_to)) => Some(where_to.placement()?),
            };
            if let Some(Placement::In(area)) = &area
                && settings.area(area).is_none()
            {
                return Err(Refused(format!("there's no room `{area}`")));
            }
            let device = settings.devices.entry(id.clone()).or_default();
            if let Some(name) = request.name.clone() {
                device.name = name;
            }
            if let Some(description) = request.description.clone() {
                device.description = description;
            }
            if let Some(ignored) = request.ignored {
                device.ignored = ignored;
                // Letting it back in is adding it. With ask mode on, clearing ignored alone
                // would leave `added = false` and put it on the waiting list instead of in
                // the home.
                if !ignored {
                    device.added = true;
                }
            }
            if let Some(added) = request.added {
                device.added = added;
            }
            if let Some(area) = area {
                device.area = area;
            }
            Ok(())
        })
        .await;
    match edited {
        // The device as it now is, so the page doesn't have to guess what the change produced.
        Ok(()) => Json(core.devices().into_iter().find(|device| device.id == id)).into_response(),
        Err(e) => edit_failed(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityEdit {
    #[serde(default, deserialize_with = "patched")]
    name: Patch<Name>,
}

async fn edit_entity(
    State(state): State<AppState>,
    Path(id): Path<EntityId>,
    Json(request): Json<EntityEdit>,
) -> Response {
    let core = &state.0.core;
    let Some(key) = core.entity_key(&id) else {
        return refused(StatusCode::NOT_FOUND, format!("there's no entity `{id}`"));
    };
    // A helper's name lives where the helper is defined. Writing it to entities.toml as well
    // would give it two names, one in each file (ROADMAP D36).
    if key.protocol.as_str() == HELPERS.as_str()
        && let Some(toggle) = key.unique_id.as_str().strip_prefix("toggle-")
    {
        let Some(Some(name)) = request.name.clone() else {
            return refused(
                StatusCode::BAD_REQUEST,
                "a helper always has a name; send the new one".to_owned(),
            );
        };
        let toggle = toggle.to_owned();
        let renamed = state
            .0
            .config
            .edit_extension(core, &HELPERS, |file| {
                let entry = file
                    .get_mut("toggles")
                    .and_then(|toggles| toggles.get_mut(&toggle))
                    .and_then(serde_json::Value::as_object_mut)
                    .ok_or_else(|| Refused(format!("there's no toggle `{toggle}`")))?;
                entry.insert("name".into(), serde_json::json!(name.as_str()));
                Ok(())
            })
            .await;
        return match renamed {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => edit_failed(e),
        };
    }
    let edited = state
        .0
        .config
        .edit(core, |settings| {
            if let Some(name) = request.name.clone() {
                settings.entities.entry(key).or_default().name = name;
            }
            Ok(())
        })
        .await;
    match edited {
        Ok(()) => Json(core.entities().into_iter().find(|entity| entity.id == id)).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// The helpers extension's id: its toggles are kept in `extensions/helpers.toml`.
static HELPERS: LazyLock<ExtensionId> =
    LazyLock::new(|| ExtensionId::try_from("helpers").expect("a valid extension id"));

/// Makes a toggle: `{"name": "Guests are over"}`. Its id comes from the name and never changes;
/// its entity is `switch.<id>`.
async fn add_toggle(State(state): State<AppState>, Json(request): Json<AreaRequest>) -> Response {
    if request.floor.is_some() {
        return refused(StatusCode::BAD_REQUEST, "a toggle has no floor".to_owned());
    }
    let taken: Vec<String> = state
        .0
        .core
        .entities()
        .iter()
        .map(|entity| entity.id.to_string())
        .collect();
    let made = state
        .0
        .config
        .edit_extension(&state.0.core, &HELPERS, |file| {
            let toggles = file
                .entry("toggles")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .ok_or_else(|| {
                    Refused("`toggles` in extensions/helpers.toml isn't a table".into())
                })?;
            // An id no toggle has, and no other switch either: `switch.<id>` has to be free.
            let base = irori_core::new_area_id(&request.name, &[]).to_string();
            let id = (1..)
                .map(|n| {
                    if n == 1 {
                        base.clone()
                    } else {
                        format!("{base}_{n}")
                    }
                })
                .find(|id| !toggles.contains_key(id) && !taken.contains(&format!("switch.{id}")))
                .expect("an unbounded range always finds a free id");
            toggles.insert(
                id.clone(),
                serde_json::json!({"name": request.name.as_str()}),
            );
            Ok(id)
        })
        .await;
    match made {
        Ok(id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"id": id, "entity_id": format!("switch.{id}")})),
        )
            .into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn remove_toggle(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let removed = state
        .0
        .config
        .edit_extension(&state.0.core, &HELPERS, |file| {
            let gone = file
                .get_mut("toggles")
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|toggles| toggles.remove(&id));
            gone.map(|_| ())
                .ok_or_else(|| Refused(format!("there's no toggle `{id}`")))
        })
        .await;
    match removed {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

/// An extension's icon. Served with a policy that forbids everything an image doesn't need, so
/// an icon from a third-party extension can't run script even when opened on its own — and the
/// page only ever shows it through `<img>`, where it couldn't anyway.
async fn extension_icon(State(state): State<AppState>, Path(id): Path<ExtensionId>) -> Response {
    match state.0.core.extension_icon(&id) {
        Some(svg) => (
            [
                (axum::http::header::CONTENT_TYPE, "image/svg+xml"),
                (
                    axum::http::header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; style-src 'unsafe-inline'; sandbox",
                ),
                (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                // Compiled into this binary, so it can't change while this build runs.
                (axum::http::header::CACHE_CONTROL, "max-age=3600"),
            ],
            svg,
        )
            .into_response(),
        None => refused(StatusCode::NOT_FOUND, format!("`{id}` has no icon")),
    }
}

/// An extension's own settings, given as one JSON object matching its `config_schema` — the
/// generic form behind the gear icon on its card (ROADMAP M1.6). Unlike [`give_secret`], this
/// isn't limited to a path an extension is currently asking for: it's scoped instead by only
/// ever accepting keys the extension's own schema actually declares, so it can't become "write
/// anything into anyone's settings" (`docs/specs/config.md` §3.6) — just this one extension's own
/// known fields. A `writeOnly` field (`docs/specs/config.md` §3.4: secrets) goes to
/// `secrets.toml`; everything else goes to `extensions/<id>.toml`.
async fn set_extension_settings(
    State(state): State<AppState>,
    Path(id): Path<ExtensionId>,
    Json(given): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let core = &state.0.core;
    let Some(overview) = core.extensions().remove(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no extension `{id}`"),
        );
    };
    let Some(schema) = overview
        .info
        .as_ref()
        .and_then(|info| info.config_schema.as_ref())
    else {
        return refused(
            StatusCode::BAD_REQUEST,
            format!("`{id}` has no settings to configure"),
        );
    };
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("`{id}`'s settings schema has no properties"),
        );
    };

    let mut non_secret = serde_json::Map::new();
    let mut secret_fields = Vec::new();
    for (key, value) in given {
        let Some(field_schema) = properties.get(&key) else {
            return refused(
                StatusCode::BAD_REQUEST,
                format!("`{key}` isn't one of `{id}`'s settings"),
            );
        };
        if is_write_only(field_schema, schema) {
            secret_fields.push((key, value));
        } else {
            non_secret.insert(key, value);
        }
    }

    // Checked before either half is written: a bad secret value must not leave the non-secret
    // half saved while the secret half fails, half-applying the request.
    let mut secrets_to_set = Vec::new();
    for (key, value) in secret_fields {
        let Some(text) = value.as_str() else {
            return refused(
                StatusCode::BAD_REQUEST,
                format!("`{key}` must be given as text"),
            );
        };
        secrets_to_set.push((key, text.to_owned()));
    }

    if !non_secret.is_empty() {
        let saved = state
            .0
            .config
            .edit_extension(core, &id, |file| {
                for (key, value) in non_secret.clone() {
                    // TOML has no `null`: a schema-valid `null` for an `Option<T>` field means
                    // "leave this unset," which in the file is the key's *absence*, not a value.
                    if value.is_null() {
                        file.remove(&key);
                    } else {
                        file.insert(key, value);
                    }
                }
                Ok(())
            })
            .await;
        if let Err(e) = saved {
            return edit_failed(e);
        }
    }
    for (key, text) in secrets_to_set {
        let saved = state
            .0
            .config
            .edit_secrets(core, |secrets| {
                secrets
                    .set(&id, std::slice::from_ref(&key), text.clone())
                    .map_err(|e| Refused(e.to_string()))
            })
            .await;
        if let Err(e) = saved {
            return edit_failed(e);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Triggers one of an extension's declared actions — the button behind "+ Add device" for a
/// protocol that has one, e.g. Zigbee's `permit_join`. Checked against both what the extension
/// *declares* (its manifest) and what it says is *usable right now* (`available_actions`), the
/// same static/dynamic split the rest of the extension model uses.
async fn trigger_extension_action(
    State(state): State<AppState>,
    Path((id, action_id)): Path<(ExtensionId, String)>,
) -> Response {
    let core = &state.0.core;
    let Some(overview) = core.extensions().remove(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no extension `{id}`"),
        );
    };
    let declared = overview
        .info
        .as_ref()
        .map(|info| info.actions.as_slice())
        .unwrap_or(&[]);
    if !declared.iter().any(|action| action.id == action_id) {
        return refused(
            StatusCode::NOT_FOUND,
            format!("`{id}` has no `{action_id}` action"),
        );
    }
    if !overview.available_actions.contains(&action_id) {
        return refused(
            StatusCode::CONFLICT,
            format!("`{action_id}` isn't available on `{id}` right now"),
        );
    }
    match core.call_action(&id, &action_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => refused(StatusCode::BAD_REQUEST, why.to_string()),
    }
}

/// Whether a JSON Schema node is (or, through `$ref`/`anyOf`/`oneOf`/`allOf`, resolves to
/// including) a `writeOnly` field — schemars' shape for `Option<Secret>` is
/// `{"anyOf": [{"$ref": "#/$defs/Secret"}, {"type": "null"}]}`, so a direct check on `field_schema`
/// alone isn't enough.
fn is_write_only(field_schema: &serde_json::Value, root: &serde_json::Value) -> bool {
    if field_schema
        .get("writeOnly")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return true;
    }
    if let Some(reference) = field_schema.get("$ref").and_then(serde_json::Value::as_str) {
        return reference
            .strip_prefix("#/$defs/")
            .and_then(|name| root.get("$defs")?.get(name))
            .is_some_and(|resolved| is_write_only(resolved, root));
    }
    ["anyOf", "oneOf", "allOf"].iter().any(|combinator| {
        field_schema
            .get(combinator)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|branches| branches.iter().any(|branch| is_write_only(branch, root)))
    })
}

/// A secret for an extension, at the place it asked for one: `{"path": [...], "value": "..."}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretGiven {
    path: Vec<String>,
    value: String,
}

/// Written by hand so the value can't reach a log through `{:?}`.
impl std::fmt::Debug for SecretGiven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretGiven")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Hands an extension a secret it asked for — an encryption key for a device it found, say — by
/// writing it into `secrets.toml`. The extension is restarted with it.
///
/// **Only where it asked.** The path has to be one the extension lists as waiting right now
/// (`docs/specs/protocols.md` §6.6). There is no sign-in yet (D12), so this endpoint must not
/// be a way to put anything into anyone's settings; this way it can only answer a question an
/// extension is actually asking. The value is never echoed back, logged, or readable afterwards.
async fn give_secret(
    State(state): State<AppState>,
    Path(id): Path<ExtensionId>,
    Json(given): Json<SecretGiven>,
) -> Response {
    let core = &state.0.core;
    let Some(extension) = core.extensions().remove(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no extension `{id}`"),
        );
    };
    let asked = extension
        .waiting
        .iter()
        .filter_map(|waiting| waiting.secret.as_ref())
        .any(|secret| secret.path == given.path);
    if !asked {
        return refused(
            StatusCode::CONFLICT,
            format!(
                "`{id}` isn't asking for a secret there right now; it may already have one, or \
                 have stopped waiting"
            ),
        );
    }
    if given.value.trim().is_empty() {
        return refused(StatusCode::BAD_REQUEST, "the secret is empty".to_owned());
    }
    let saved = state
        .0
        .config
        .edit_secrets(core, |secrets| {
            secrets
                .set(&id, &given.path, given.value.trim().to_owned())
                .map_err(|e| Refused(e.to_string()))
        })
        .await;
    match saved {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Official catalog plus whether each one is installed in this instance.
async fn catalog(State(state): State<AppState>) -> Json<Vec<CatalogEntry>> {
    let running = state.0.core.extensions();
    Json(
        crate::packages::official()
            .into_iter()
            .map(|item| {
                let overview = running.get(&item.id);
                // Live, not the catalog's own claim: an installed extension's manifest is only
                // read once its supervised task actually describes it, which can be a moment
                // after `installed` turns true, and a manifest missing `run` or a protocol
                // contribution never gets described at all (host.rs). Either way, this says
                // whether `extension_icon` actually has bytes right now.
                let icon = state.0.core.has_extension_icon(&item.id);
                CatalogEntry {
                    id: item.id,
                    name: item.name.to_string(),
                    category: item.category,
                    description: item.description,
                    version: item.version,
                    official: true,
                    installed: overview.is_some(),
                    icon,
                    state: overview.map(|o| match &o.status {
                        irori_core::ExtensionStatus::Disabled => "disabled",
                        irori_core::ExtensionStatus::Starting => "starting",
                        irori_core::ExtensionStatus::Running => "running",
                        irori_core::ExtensionStatus::Degraded { .. } => "degraded",
                        irori_core::ExtensionStatus::Failed { .. } => "failed",
                        irori_core::ExtensionStatus::NeedsSetup { .. } => "needs_setup",
                    }),
                    reason: overview.and_then(|o| match &o.status {
                        irori_core::ExtensionStatus::Degraded { reason }
                        | irori_core::ExtensionStatus::Failed { reason, .. } => {
                            Some(reason.clone())
                        }
                        irori_core::ExtensionStatus::NeedsSetup { missing } => {
                            Some(needs_setup_reason(missing))
                        }
                        _ => None,
                    }),
                    config_schema: overview
                        .and_then(|o| o.info.as_ref())
                        .and_then(|info| info.config_schema.clone()),
                }
            })
            .collect(),
    )
}

/// What a person needs to do about `needs_setup`, in the same `reason` slot every other state
/// puts its own explanation. The field names come from the extension's own `config_schema`, which
/// is also what the settings form labels them by — `serial_port` there reads as "Serial port".
fn needs_setup_reason(missing: &[String]) -> String {
    let named = missing
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "waiting on a setting it can't start without: {named}. Open its settings to fill it in."
    )
}

#[derive(Debug, Serialize)]
struct CatalogEntry {
    id: ExtensionId,
    name: String,
    category: String,
    description: String,
    version: String,
    official: bool,
    installed: bool,
    icon: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_schema: Option<serde_json::Value>,
}

async fn install_official(State(state): State<AppState>, Path(id): Path<ExtensionId>) -> Response {
    let Some(item) = crate::packages::official_by_id(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("`{id}` isn't an official extension"),
        );
    };
    // Stage out of any live package's way and let `install_package` decide where it lands, so a
    // double-click or a race can't overwrite files while a copy is running. A leftover staging
    // dir is never read as a package again (its name isn't a slug, and `installed_packages`
    // skips those).
    let stage = state.0.host.packages_dir().join(staging_dir());
    if let Err(why) = tokio::task::spawn_blocking({
        let stage = stage.clone();
        move || crate::packages::install_official(&item, &stage)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))
    {
        let _ = std::fs::remove_dir_all(&stage);
        return refused(StatusCode::BAD_GATEWAY, why);
    }
    match state.0.host.install_package(stage.clone()) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => {
            let _ = std::fs::remove_dir_all(&stage);
            refused(StatusCode::CONFLICT, why)
        }
    }
}

/// A unique directory name under the packages dir for a download in flight.
fn staging_dir() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos();
    format!("download-{nanos}")
}

#[derive(Debug, Deserialize)]
struct InstallUrl {
    url: String,
}

async fn install_url(State(state): State<AppState>, Json(body): Json<InstallUrl>) -> Response {
    let url = body.url.trim();
    if url.is_empty() {
        return refused(StatusCode::BAD_REQUEST, "url is empty".into());
    }
    // `curl` understands every scheme it was built with, so one gate here keeps the endpoint
    // from acting as a proxy for file:/ftp:/gopher: or anything else a hostile URL could reach.
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return refused(
            StatusCode::BAD_REQUEST,
            "only http:// and https:// URLs are allowed".into(),
        );
    }
    let dest = state.0.host.packages_dir().join(staging_dir());
    if let Err(why) = tokio::task::spawn_blocking({
        let dest = dest.clone();
        let url = url.to_owned();
        move || crate::packages::install_url(&url, &dest)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))
    {
        let _ = std::fs::remove_dir_all(&dest);
        return refused(StatusCode::BAD_GATEWAY, why);
    }
    match state.0.host.install_package(dest.clone()) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => {
            let _ = std::fs::remove_dir_all(&dest);
            refused(StatusCode::BAD_REQUEST, why)
        }
    }
}

async fn uninstall(State(state): State<AppState>, Path(id): Path<ExtensionId>) -> Response {
    match state.0.host.uninstall(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => refused(StatusCode::BAD_REQUEST, why),
    }
}

/// An edit that couldn't be made says why; one that couldn't be written says that instead, because
/// the two need different things from whoever is reading.
fn edit_failed(error: EditError) -> Response {
    match error {
        EditError::Refused(why) => refused(StatusCode::BAD_REQUEST, why.to_string()),
        EditError::Io(e) => {
            tracing::error!(%e, "couldn't write the config directory");
            refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("couldn't write the config directory: {e}"),
            )
        }
    }
}

/// A command from the UI: `{"entity_id": "light.hallway", "command": "toggle"}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRequest {
    entity_id: EntityId,
    command: CommandName,
    /// Brightness or color, for `turn_on` on a light that supports them.
    #[serde(default)]
    data: Option<LightTurnOn>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandName {
    TurnOn,
    TurnOff,
    Toggle,
}

/// Asks an entity to do something and answers with its state once the protocol confirms, so
/// the page can show the result without waiting for its next refresh.
///
/// There's no sign-in yet (ROADMAP D12, M1.5), so every command is attributed to one
/// unauthenticated person; with accounts it becomes the user who clicked.
async fn command(State(state): State<AppState>, Json(request): Json<CommandRequest>) -> Response {
    let command = match (request.command, request.data) {
        (CommandName::TurnOn, data) => Command::TurnOn(data.unwrap_or_default()),
        (CommandName::TurnOff, None) => Command::TurnOff,
        (CommandName::Toggle, None) => Command::Toggle,
        (_, Some(_)) => {
            return refused(
                StatusCode::BAD_REQUEST,
                "`data` is only for `turn_on`".to_owned(),
            );
        }
    };
    let core = &state.0.core;
    let who = core.new_context(Origin::User {
        user_id: UNAUTHENTICATED.clone(),
    });
    // Subscribed before the call: the protocol reports the new state and answers the call in
    // the same breath, and the report must not slip past while the call is still in flight.
    let changes = core.subscribe();
    match core
        .call_service(&request.entity_id, command, who.clone())
        .await
    {
        Ok(()) => {
            let settled = settled(changes, &request.entity_id, &who.id).await;
            Json(settled.or_else(|| core.state(&request.entity_id))).into_response()
        }
        Err(e) => refused(status_for(&e), e.to_string()),
    }
}

/// How long to wait for the change a command caused before answering with what's on file. The
/// protocol has already confirmed by this point, so the report is usually a moment away.
const SETTLE: Duration = Duration::from_millis(500);

/// Waits for the state change this command caused. Answers `None` when there wasn't one: a light
/// asked to turn on while it's already on has nothing new to report, and says so by staying quiet.
async fn settled(
    mut changes: broadcast::Receiver<Event>,
    entity_id: &EntityId,
    context_id: &ContextId,
) -> Option<EntityState> {
    let wait = async {
        loop {
            match changes.recv().await {
                Ok(Event::StateChanged {
                    entity_id: changed,
                    new_state,
                    ..
                }) if &changed == entity_id
                    && new_state.context.parent_id.as_ref() == Some(context_id) =>
                {
                    return Some(*new_state);
                }
                // A listener that falls behind skips ahead; it may have skipped past the change.
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(SETTLE, wait).await.ok().flatten()
}

/// Which HTTP status a refused command gets: the caller's fault, the device's, or time running
/// out.
fn status_for(error: &CallError) -> StatusCode {
    match error {
        CallError::UnknownEntity(_) => StatusCode::NOT_FOUND,
        CallError::NotSupported(_) => StatusCode::BAD_REQUEST,
        CallError::NotRunning(_) | CallError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        CallError::Failed(_) => StatusCode::BAD_GATEWAY,
        CallError::Timeout => StatusCode::GATEWAY_TIMEOUT,
    }
}

fn refused(status: StatusCode, error: String) -> Response {
    #[derive(Debug, Serialize)]
    struct Refused {
        error: String,
    }
    (status, Json(Refused { error })).into_response()
}

#[derive(Debug, Serialize)]
struct Health<'a> {
    status: &'static str,
    version: &'static str,
    /// What this binary was built from, so "am I running the version I just built?" has an
    /// answer even between releases, when every development build is `0.0.0`.
    commit: &'static str,
    built_at: &'static str,
    /// Which boot this is — a different value after every process start. Uptime can't tell an
    /// instance that started a minute ago from one that restarted a minute ago; this can, which
    /// is what lets the Settings page know the restart it asked for is done.
    boot_id: &'a str,
    uptime_ms: u128,
    features: &'a [&'static str],
    sqlite: SqliteHealth<'a>,
}

#[derive(Debug, Serialize)]
struct SqliteHealth<'a> {
    version: &'static str,
    journal_mode: &'a str,
}

/// Liveness: the process is up and serving. The `sqlite` fields describe the database as it was
/// opened at startup; they are not a live check. Real database health (read-only, disk full)
/// arrives with `irori-recorder` in M1.3, which keeps the connection open.
async fn health(State(state): State<AppState>) -> Response {
    let inner = &state.0;
    Json(Health {
        status: "ok",
        version: VERSION,
        commit: crate::build_info::COMMIT,
        built_at: crate::build_info::BUILT_AT,
        boot_id: &inner.boot_id,
        uptime_ms: inner.started.elapsed().as_millis(),
        features: &inner.build.features,
        sqlite: SqliteHealth {
            version: inner.build.sqlite_version,
            journal_mode: &inner.db.journal_mode,
        },
    })
    .into_response()
}

#[cfg(feature = "ui")]
mod ui {
    use axum::http::{StatusCode, Uri, header};
    use axum::response::{IntoResponse, Response};
    use rust_embed::RustEmbed;

    /// The Leptos app, put here by `cargo xtask ui`. Empty after a plain `cargo build`, which
    /// needs no wasm toolchain; the placeholder page below is then served instead.
    #[derive(RustEmbed)]
    #[folder = "ui/"]
    struct App;

    /// The placeholder page and the favicon: no build step, always there.
    #[derive(RustEmbed)]
    #[folder = "assets/"]
    struct Placeholder;

    const INDEX: &str = "index.html";

    pub async fn serve(uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { INDEX } else { path };
        // `/api/…` belongs to the API, whatever is or isn't there. Handing an API caller the
        // page with a 200 would let a typo look like success.
        if path == "api" || path.starts_with("api/") {
            return (StatusCode::NOT_FOUND, "no such endpoint\n").into_response();
        }
        // The app wins where both have a file, so a built UI replaces the placeholder page.
        let file = App::get(path)
            .or_else(|| Placeholder::get(path))
            .map(|file| (path, file))
            // A path with no file extension is one of the app's own pages (`/devices`), which
            // the app routes itself once it has loaded: it gets the page, not a 404. A missing
            // file (`/nope.css`) is still a 404, so a broken asset says so plainly.
            .or_else(|| {
                (!path.contains('.'))
                    .then(|| {
                        App::get(INDEX)
                            .or_else(|| Placeholder::get(INDEX))
                            .map(|index| (INDEX, index))
                    })
                    .flatten()
            });
        match file {
            Some((path, file)) => {
                let mime = mime_guess::from_path(path).first_or_octet_stream();
                ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
            }
            None => (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    }
}

#[cfg(not(feature = "ui"))]
mod ui {
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};

    pub async fn serve() -> Response {
        (
            StatusCode::NOT_FOUND,
            "The web UI is not compiled into this build (built without the `ui` feature).\n",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use super::*;

    fn core() -> Core {
        Core::new(Arc::new(irori_core::SystemClock))
    }

    /// A server on throwaway directories. Held for the length of a test, because the config
    /// directory has to outlive the requests that write to it.
    struct Server {
        dir: tempfile::TempDir,
        core: Core,
        config: Config,
        history: History,
        // The restart handle, held back so a test can check that POST /api/dev/restart woke the
        // shutdown and told it to restart, not just that it answered 202.
        restart: Arc<tokio::sync::Notify>,
        restarting: Arc<AtomicBool>,
    }

    impl Server {
        fn new(core: Core) -> anyhow::Result<Self> {
            let dir = tempfile::tempdir()?;
            // Asking before adding is the default (`irori.toml`, `[devices] new`), so without
            // this every test below would have to add the demo's devices before it could look at
            // one. The tests that are *about* asking turn it back on for themselves.
            let config_dir = dir.path().join("config");
            std::fs::create_dir_all(&config_dir)?;
            std::fs::write(config_dir.join("irori.toml"), "[devices]\nnew = \"add\"\n")?;
            let config = Config::open_dir(config_dir, &core);
            Ok(Self {
                dir,
                core,
                config,
                history: History::default(),
                restart: Arc::new(tokio::sync::Notify::new()),
                restarting: Arc::new(AtomicBool::new(false)),
            })
        }

        fn app(&self) -> anyhow::Result<Router> {
            let host = irori_core::ExtensionHost::start(
                &self.core,
                Vec::new(),
                irori_core::Timing::default(),
            )
            .map_err(anyhow::Error::msg)?;
            Ok(router(AppState::new(
                crate::db::open(self.dir.path())?,
                self.core.clone(),
                self.config.clone(),
                host,
                self.history.clone(),
                self.restart.clone(),
                self.restarting.clone(),
            )))
        }

        /// The config directory this server writes to, for checking what landed on disk.
        fn config_dir(&self) -> std::path::PathBuf {
            self.dir.path().join("config")
        }

        async fn send(&self, request: Request<Body>) -> anyhow::Result<(StatusCode, Vec<u8>)> {
            let res = self.app()?.oneshot(request).await?;
            let status = res.status();
            let body = res.into_body().collect().await?.to_bytes().to_vec();
            Ok((status, body))
        }

        async fn json(
            &self,
            method: &str,
            path: &str,
            body: serde_json::Value,
        ) -> anyhow::Result<(StatusCode, serde_json::Value)> {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?;
            let (status, bytes) = self.send(request).await?;
            Ok((
                status,
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
            ))
        }

        async fn read(&self, path: &str) -> anyhow::Result<serde_json::Value> {
            let (_, bytes) = self.send(Request::get(path).body(Body::empty())?).await?;
            Ok(serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
        }
    }

    async fn get(path: &str) -> anyhow::Result<(StatusCode, Option<String>, Vec<u8>)> {
        get_from(core(), path).await
    }

    async fn get_from(
        core: Core,
        path: &str,
    ) -> anyhow::Result<(StatusCode, Option<String>, Vec<u8>)> {
        let server = Server::new(core)?;
        let res = server
            .app()?
            .oneshot(Request::get(path).body(Body::empty())?)
            .await?;
        let status = res.status();
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = res.into_body().collect().await?.to_bytes().to_vec();
        Ok((status, content_type, body))
    }

    #[tokio::test]
    async fn health_reports_ok_and_wal() -> anyhow::Result<()> {
        let (status, _, body) = get("/api/health").await?;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["sqlite"]["journal_mode"], "wal");
        Ok(())
    }

    /// The System menu's endpoint answers with the machine the test is running on, and helps
    /// rather than guesses: the numbers read real, and the rows that the OS refused to say are
    /// left out rather than made up.
    #[tokio::test]
    async fn system_describes_the_machine_the_instance_runs_on() -> anyhow::Result<()> {
        let (status, _, body) = get("/api/dev/system").await?;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert!(
            json["os"].as_str().is_some_and(|os| !os.is_empty()),
            "the OS should say what it is: {json}"
        );
        assert!(
            json["arch"].as_str().is_some_and(|arch| !arch.is_empty()),
            "the architecture should say what build this is: {json}"
        );
        assert!(json["cpu_cores"].as_u64().unwrap_or(0) > 0, "{json}");
        assert!(json["memory_total"].as_u64().unwrap_or(0) > 0, "{json}");
        assert!(
            json["disk"]["total"].as_u64().unwrap_or(0) > 0,
            "the volume with the data should report its size: {json}"
        );
        Ok(())
    }

    /// The endpoint's own shape: what devices are actually found is host-specific and covered in
    /// `crate::serial`'s own tests.
    #[tokio::test]
    async fn serial_ports_answers_with_a_list() -> anyhow::Result<()> {
        let (status, _, body) = get("/api/dev/serial-ports").await?;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert!(json.is_array(), "{json}");
        Ok(())
    }

    /// A restart request is accepted first, because the restart happens after the answer: the
    /// page's POST has to come back before the server — and with it the connection — goes. It
    /// also does what it says: the shutdown is woken and told it's a restart, which the runner
    /// (serve) reads to re-exec the binary instead of exiting.
    #[tokio::test]
    async fn restart_is_accepted() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        // The waiter serve()'s own shutdown future would be. It is registered before the
        // request goes out so the handler's notify_one has a waiter to wake; a retained
        // notification (no waiter registered) is covered by the handler's semantics instead.
        let mut shutdown = Box::pin(server.restart.notified());
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(shutdown.as_mut().poll(&mut cx).is_pending());
        let (status, _) = server
            .send(
                Request::post("/api/dev/restart")
                    .header("x-irori-ui", "1")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(
            server.restarting.load(Ordering::SeqCst),
            "the restart was told it's a restart"
        );
        assert!(
            shutdown.as_mut().poll(&mut cx).is_ready(),
            "the shutdown was woken"
        );
        Ok(())
    }

    /// A restart without the page's header is refused: restart is a service interruption, so a
    /// bare POST — a cross-site form, say — must not be able to take the instance down. The
    /// header is what only same-origin JavaScript can set (a form carries none, and a fetch
    /// with a custom header is stopped by CORS preflight). The body says what the caller would
    /// need to know to ask again, per the API error contract.
    #[tokio::test]
    async fn restart_without_the_page_header_is_refused() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (status, body) = server
            .send(Request::post("/api/dev/restart").body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let body = String::from_utf8(body)?;
        assert!(
            body.contains("x-irori-ui: 1"),
            "the refusal should name the header and value it needs: {body}"
        );
        assert!(
            !server.restarting.load(Ordering::SeqCst),
            "a refused restart must not mark the boot as restarting"
        );
        Ok(())
    }

    /// A second restart inside the cooldown is refused: anyone who can reach this route at all
    /// can spoof the page header (there is no auth yet), so the endpoint must not let a caller
    /// stack outages. The page never hits this — its button is disabled while a restart is in
    /// flight — so the cooldown only costs an abuser.
    #[tokio::test]
    async fn restart_is_refused_again_within_the_cooldown() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        // One app, so the two requests share the cooldown clock (each fresh `send` would build
        // its own).
        let app = server.app()?;
        let ask = || {
            Request::post("/api/dev/restart")
                .header("x-irori-ui", "1")
                .body(Body::empty())
        };
        let first = app.clone().oneshot(ask()?).await?;
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let second = app.clone().oneshot(ask()?).await?;
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        Ok(())
    }

    /// A new boot is a new instance: the health of two servers — two process starts, in the
    /// restart's case — must not carry the same boot id, or the page couldn't tell the restart
    /// it asked for from the process that was already there.
    #[tokio::test]
    async fn every_boot_gets_its_own_id() -> anyhow::Result<()> {
        async fn boot_id(server: &Server) -> anyhow::Result<String> {
            let (_, body) = server
                .send(Request::get("/api/health").body(Body::empty())?)
                .await?;
            let json: serde_json::Value = serde_json::from_slice(&body)?;
            Ok(json["boot_id"].as_str().unwrap_or_default().to_owned())
        }
        let server = Server::new(core())?;
        let first = boot_id(&server).await?;
        let server = Server::new(core())?;
        let second = boot_id(&server).await?;
        assert!(!first.is_empty(), "a boot should name itself: {first:?}");
        assert_ne!(first, second, "two boots share an id: {first}");
        Ok(())
    }

    #[cfg(feature = "ui")]
    #[tokio::test]
    async fn serves_embedded_index() -> anyhow::Result<()> {
        let (status, content_type, body) = get("/").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/html"));
        assert!(String::from_utf8(body)?.contains("IroriOS"));
        Ok(())
    }

    #[cfg(feature = "ui")]
    #[tokio::test]
    async fn serves_svg_favicon() -> anyhow::Result<()> {
        let (status, content_type, body) = get("/favicon.svg").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("image/svg+xml"));
        assert!(String::from_utf8(body)?.starts_with("<svg"));
        Ok(())
    }

    /// A core with the demo extension running and its first state reports in.
    async fn demo() -> anyhow::Result<(Core, irori_core::ExtensionHost)> {
        let core = core();
        let host = irori_core::ExtensionHost::start(
            &core,
            vec![
                irori_protocol::builtin::<irori_protocol_demo::Demo>()
                    .map_err(anyhow::Error::msg)?,
            ],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            if core.states().iter().any(|s| s.state.is_some()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok((core, host))
    }

    async fn post(
        core: Core,
        path: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<(StatusCode, serde_json::Value)> {
        Server::new(core)?.json("POST", path, body).await
    }

    /// Everything the Devices page needs arrives in one response.
    #[tokio::test]
    async fn dev_home_describes_the_whole_home() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/dev/home").await?;
        assert_eq!(status, StatusCode::OK);
        let home: serde_json::Value = serde_json::from_slice(&body)?;

        let named = |key: &str, id: &str| {
            home[key]
                .as_array()
                .is_some_and(|all| all.iter().any(|item| item["name"] == id))
        };
        assert!(named("devices", "Demo lamp"), "no demo lamp in {home}");
        assert!(named("entities", "Demo lamp"), "no lamp entity in {home}");
        assert!(
            named("devices", "Demo hallway light"),
            "no hallway light in {home}"
        );
        assert!(
            named("devices", "Demo movement sensor"),
            "no movement sensor in {home}"
        );
        assert!(
            named("devices", "Demo luminosity sensor"),
            "no luminosity sensor in {home}"
        );
        assert!(
            named("devices", "Demo mmWave sensor"),
            "no mmWave sensor in {home}"
        );
        assert_eq!(home["extensions"]["demo"]["state"], "running");
        let lamp = home["states"]
            .as_array()
            .and_then(|all| all.iter().find(|s| s["entity_id"] == "light.demo_lamp"))
            .ok_or_else(|| anyhow::anyhow!("no demo lamp state in {home}"))?;
        assert_eq!(lamp["availability"], "available");

        host.shutdown().await;
        Ok(())
    }

    /// The page turns the lamp on and is told what it became, without waiting for a refresh.
    #[tokio::test]
    async fn a_command_answers_with_the_state_it_produced() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let lamp = EntityId::try_from("light.demo_lamp")?;

        for on in [true, false] {
            let (status, body) = post(
                core.clone(),
                "/api/dev/command",
                serde_json::json!({
                    "entity_id": lamp, "command": if on { "turn_on" } else { "turn_off" },
                }),
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["state"]["on"], on, "{body}");
            // The device is what changed, but it can be traced back to the click that asked it
            // to (`docs/specs/entities.md` §6).
            assert_eq!(body["context"]["origin"]["type"], "device");
            assert!(body["context"]["parent_id"].is_string(), "{body}");
            // The core's own view agrees: the answer isn't a hopeful echo of the request.
            let state = core
                .state(&lamp)
                .ok_or_else(|| anyhow::anyhow!("the lamp vanished"))?;
            assert!(
                matches!(&state.state, Some(irori_types::State::Light(light)) if light.on == on),
                "the core says {:?}, the answer said {on}",
                state.state
            );
        }

        host.shutdown().await;
        Ok(())
    }

    /// The per-entity history endpoint answers with the recorder's last day of changes, oldest
    /// first, and says plainly when the entity isn't one Irori knows.
    #[tokio::test]
    async fn history_reports_the_last_day_of_changes() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let entity = core
            .states()
            .first()
            .map(|state| state.entity_id.clone())
            .ok_or_else(|| anyhow::anyhow!("the demo reported nothing"))?;

        // What the recorder would have kept: feed it the endpoint's history by hand here, since
        // the server under test isn't the one subscribed to the core.
        let today = core
            .state(&entity)
            .ok_or_else(|| anyhow::anyhow!("the demo entity vanished"))?;
        server.history.record(entity.clone(), today.clone());
        let earlier = core
            .state(&entity)
            .ok_or_else(|| anyhow::anyhow!("the demo entity vanished"))?;
        server.history.record(entity.clone(), earlier);

        let (status, body) = server
            .send(Request::get(format!("/api/dev/history/{entity}")).body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::OK);
        let history: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(history["entity"], serde_json::json!(entity.to_string()));
        let states = history["states"].as_array().expect("a list of states");
        assert_eq!(states.len(), 2, "the two recorded changes: {history}");

        // An entity Irori has never heard of is refused, not answered with an empty table.
        let (status, _) = server
            .send(Request::get("/api/dev/history/sensor.never_heard_of").body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        host.shutdown().await;
        Ok(())
    }

    /// A refused command says why, with a status that matches the reason.
    #[tokio::test]
    async fn refusals_explain_themselves() -> anyhow::Result<()> {
        let (core, host) = demo().await?;

        let (status, body) = post(
            core.clone(),
            "/api/dev/command",
            serde_json::json!({"entity_id": "light.nowhere", "command": "toggle"}),
        )
        .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "there's no entity `light.nowhere`");

        // A sensor has nothing to turn on.
        let (status, body) = post(
            core.clone(),
            "/api/dev/command",
            serde_json::json!({"entity_id": "sensor.demo_hallway_sensor_temperature", "command": "toggle"}),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // Brightness belongs to `turn_on`, and nowhere else.
        let (status, body) = post(
            core.clone(),
            "/api/dev/command",
            serde_json::json!({
                "entity_id": "light.demo_lamp", "command": "turn_off", "data": {"brightness": 5},
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "`data` is only for `turn_on`");

        host.shutdown().await;
        Ok(())
    }

    /// The demo's virtual devices show up in the read-only view.
    #[tokio::test]
    async fn dev_view_lists_demo_devices_and_states() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/dev/states").await?;
        assert_eq!(status, StatusCode::OK);
        let states: serde_json::Value = serde_json::from_slice(&body)?;
        let lamp = states
            .as_array()
            .and_then(|all| all.iter().find(|s| s["entity_id"] == "light.demo_lamp"))
            .ok_or_else(|| anyhow::anyhow!("no demo lamp in {states}"))?;
        assert_eq!(lamp["state"]["kind"], "light");

        let (_, _, body) = get_from(core.clone(), "/api/dev/extensions").await?;
        let extensions: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(extensions["demo"]["state"], "running");

        host.shutdown().await;
        Ok(())
    }

    /// The catalog's `icon` field is whether `extension_icon` can serve bytes for it *right now*
    /// (running, with an icon in its manifest) — not a static claim from the catalog file — so
    /// the Extensions page never points an `<img>` at a file that isn't there yet, or ever.
    #[tokio::test]
    async fn the_catalog_reports_whether_an_icon_is_actually_servable() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/dev/catalog").await?;
        assert_eq!(status, StatusCode::OK);
        let catalog: serde_json::Value = serde_json::from_slice(&body)?;
        let entries = catalog.as_array().expect("a list");
        let demo = entries
            .iter()
            .find(|e| e["id"] == "demo")
            .expect("demo is official");
        assert_eq!(demo["icon"], true, "{catalog}");
        let mqtt = entries
            .iter()
            .find(|e| e["id"] == "mqtt")
            .expect("mqtt is official");
        assert_eq!(mqtt["icon"], false, "{catalog}");

        host.shutdown().await;
        Ok(())
    }

    // --- Rooms and names ------------------------------------------------------------------

    /// The whole round trip: make a room, put a device in it, and find both on disk in files a
    /// person could have written themselves.
    #[tokio::test]
    async fn a_room_and_a_device_in_it_are_written_where_a_person_can_read_them()
    -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let (status, area) = server
            .json(
                "POST",
                "/api/dev/areas",
                serde_json::json!({"name": "Study"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{area}");
        assert_eq!(area["id"], "study");

        let (status, device) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({
                    "name": "Reading lamp", "description": "On the desk", "area": "study",
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{device}");
        assert_eq!(device["name"], "Reading lamp");
        assert_eq!(device["description"], "On the desk");
        assert_eq!(device["id"], "demo_lamp", "renaming it didn't move its id");
        assert_eq!(device["area_id"], "study");

        let areas = std::fs::read_to_string(server.config_dir().join("areas.toml"))?;
        assert!(areas.contains("[areas.study]"), "{areas}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        // Keyed by the device's one id — the same id as its page address — so there is never a
        // second identifier for the same device to keep in step (ROADMAP D36).
        assert!(devices.contains("[devices.demo_lamp]"), "{devices}");
        assert!(
            devices.contains("description = \"On the desk\""),
            "{devices}"
        );
        assert!(devices.contains("name = \"Reading lamp\""), "{devices}");

        // And the page sees the same thing it would after a restart.
        let home = server.read("/api/dev/home").await?;
        assert_eq!(home["areas"][0]["name"], "Study");

        host.shutdown().await;
        Ok(())
    }

    /// Renaming a device renames the entities that were following its name, and taking the name
    /// away gives the protocol's name back rather than leaving the chosen one stuck.
    #[tokio::test]
    async fn a_rename_carries_the_entities_and_can_be_undone() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let lamp = EntityId::try_from("light.demo_lamp")?;
        let entity_named = |core: &Core| {
            core.entities()
                .into_iter()
                .find(|entity| entity.id == lamp)
                .map(|entity| entity.name.to_string())
        };
        assert_eq!(entity_named(&core).as_deref(), Some("Demo lamp"));

        let (status, _) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"name": "Reading lamp"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(entity_named(&core).as_deref(), Some("Reading lamp"));

        let (status, device) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"name": null}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{device}");
        assert_eq!(device["name"], "Demo lamp");
        assert_eq!(entity_named(&core).as_deref(), Some("Demo lamp"));

        host.shutdown().await;
        Ok(())
    }

    /// An entity can be named on its own, and then it stops following its device.
    #[tokio::test]
    async fn an_entity_can_have_a_name_of_its_own() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let (status, entity) = server
            .json(
                "PATCH",
                "/api/dev/entities/light.demo_lamp",
                serde_json::json!({"name": "Reading light"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{entity}");
        assert_eq!(entity["name"], "Reading light");

        server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"name": "Reading lamp"}),
            )
            .await?;
        let home = server.read("/api/dev/home").await?;
        let named = home["entities"]
            .as_array()
            .and_then(|all| all.iter().find(|e| e["id"] == "light.demo_lamp"))
            .map(|e| e["name"].clone());
        assert_eq!(named, Some(serde_json::json!("Reading light")));

        host.shutdown().await;
        Ok(())
    }

    /// Deleting a room doesn't delete what was said about the devices in it: the device is
    /// unplaced, and making the room again puts it back.
    #[tokio::test]
    async fn deleting_a_room_unplaces_its_devices_without_forgetting_them() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        server
            .json(
                "POST",
                "/api/dev/areas",
                serde_json::json!({"name": "Study"}),
            )
            .await?;
        server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"area": "study"}),
            )
            .await?;

        let (status, _) = server
            .json("DELETE", "/api/dev/areas/study", serde_json::json!(null))
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let placed = |core: &Core| {
            core.devices()
                .into_iter()
                .find(|device| device.id.as_str() == "demo_lamp")
                .and_then(|device| device.area_id)
        };
        assert_eq!(placed(&core), None);

        server
            .json(
                "POST",
                "/api/dev/areas",
                serde_json::json!({"name": "Study"}),
            )
            .await?;
        assert_eq!(
            placed(&core).map(|id| id.to_string()).as_deref(),
            Some("study"),
            "the device remembered where it belonged"
        );

        host.shutdown().await;
        Ok(())
    }

    /// "Not in a room" is an answer. The demo lamp's firmware asks for the Study; once that room
    /// exists the lamp is in it, and `area: false` must take it out and keep it out, while
    /// `area: null` hands the decision back to the device.
    #[tokio::test]
    async fn a_device_can_be_kept_out_of_the_room_it_asks_for() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let placed = |core: &Core| {
            core.devices()
                .into_iter()
                .find(|device| device.id.as_str() == "demo_lamp")
                .and_then(|device| device.area_id)
                .map(|id| id.to_string())
        };
        server
            .json(
                "POST",
                "/api/dev/areas",
                serde_json::json!({"name": "Study"}),
            )
            .await?;
        assert_eq!(
            placed(&core).as_deref(),
            Some("study"),
            "the suggestion stands in"
        );

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"area": false}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(placed(&core), None, "and a person can say no");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = false"), "{devices}");

        server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"area": null}),
            )
            .await?;
        assert_eq!(
            placed(&core).as_deref(),
            Some("study"),
            "or give it back to the device"
        );

        let (status, _) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"area": true}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "`true` names no room");

        host.shutdown().await;
        Ok(())
    }

    /// A plan drawn on the Floorplan page lands in `floorplan.toml` and comes back with the
    /// home, and one that couldn't be drawn is refused rather than written.
    #[tokio::test]
    async fn a_plan_is_saved_whole_and_a_door_has_to_fit_its_wall() -> anyhow::Result<()> {
        // A real device, because part of what saving a plan does is put devices in the rooms
        // they were drawn in.
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let home = server.read("/api/dev/home").await?;
        assert!(
            home.get("floorplan").is_none(),
            "a home nobody has drawn sends no plan: {home}"
        );

        let plan = serde_json::json!({
            "floors": {
                "ground": {
                    "walls": [{
                        "from": [0, 0],
                        "to": [400, 0],
                        "thickness": 20,
                        "openings": [{"kind": "door", "at": 200, "width": 80}],
                    }],
                    "areas": [{
                        "area": "kitchen",
                        "points": [[0, 0], [400, 0], [400, 300], [0, 300]],
                    }],
                    "devices": [{"device": "demo_lamp", "at": [120, 90]}],
                },
            },
        });
        let (status, body) = server
            .json("PUT", "/api/dev/floorplan", plan.clone())
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["placed"], 0,
            "there is no room called `kitchen` to put it in yet: {body}"
        );

        let written = std::fs::read_to_string(server.config_dir().join("floorplan.toml"))?;
        assert!(written.contains("from = [0, 0]"), "{written}");
        assert!(written.contains("kind = \"door\""), "{written}");
        assert!(written.contains("area = \"kitchen\""), "{written}");
        assert!(written.contains("device = \"demo_lamp\""), "{written}");

        let home = server.read("/api/dev/home").await?;
        assert_eq!(
            home["floorplan"]["floors"]["ground"]["walls"][0]["to"][0], 400,
            "{home}"
        );
        assert_eq!(server.read("/api/dev/floorplan").await?, plan);

        // Make the room the lamp was drawn standing in, and saving the same plan again puts the
        // lamp in it: dragging a device onto a floor is a person saying where it is.
        server
            .json(
                "POST",
                "/api/dev/areas",
                serde_json::json!({"name": "Kitchen"}),
            )
            .await?;
        let (_, body) = server
            .json("PUT", "/api/dev/floorplan", plan.clone())
            .await?;
        assert_eq!(body["placed"], 1, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = \"kitchen\""), "{devices}");

        // Saying so again moves nothing: it is where the plan says already.
        let (_, body) = server
            .json("PUT", "/api/dev/floorplan", plan.clone())
            .await?;
        assert_eq!(body["placed"], 0, "{body}");

        // A device deliberately in no room stays in none. That answer exists so a guess can't
        // overrule it, and a dot standing on a floor is a guess.
        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"area": false}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = server
            .json("PUT", "/api/dev/floorplan", plan.clone())
            .await?;
        assert_eq!(body["placed"], 0, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = false"), "{devices}");
        assert_eq!(
            core.devices()
                .iter()
                .find(|device| device.id.as_ref() == "demo_lamp")
                .and_then(|device| device.area_id.clone()),
            None,
            "and the core agrees"
        );

        // A plan naming a device the home has never heard of is kept and simply not drawn. It
        // must not become a `devices.toml` entry for a device nobody has.
        let (_, body) = server
            .json(
                "PUT",
                "/api/dev/floorplan",
                serde_json::json!({
                    "floors": {
                        "ground": {
                            "areas": [{
                                "area": "kitchen",
                                "points": [[0, 0], [400, 0], [400, 300], [0, 300]],
                            }],
                            "devices": [{"device": "a_device_nobody_has", "at": [120, 90]}],
                        },
                    },
                }),
            )
            .await?;
        assert_eq!(body["placed"], 0, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(!devices.contains("a_device_nobody_has"), "{devices}");
        // Put the drawn plan back, for what follows.
        server
            .json("PUT", "/api/dev/floorplan", plan.clone())
            .await?;

        // A door wider than the wall it's in would have to be drawn hanging off the end.
        let (status, why) = server
            .json(
                "PUT",
                "/api/dev/floorplan",
                serde_json::json!({
                    "floors": {
                        "ground": {
                            "walls": [{
                                "from": [0, 0],
                                "to": [100, 0],
                                "openings": [{"kind": "door", "at": 50, "width": 300}],
                            }],
                        },
                    },
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("hangs off the end")),
            "{why}"
        );
        // And the refused plan changed nothing: the drawn one is still there.
        assert_eq!(server.read("/api/dev/floorplan").await?, plan);

        host.shutdown().await;
        Ok(())
    }

    /// Floors are made, listed lowest first, and rooms go on them; removing a floor leaves its
    /// rooms where they are, on no floor.
    #[tokio::test]
    async fn floors_hold_rooms_and_removing_one_keeps_them() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (status, upstairs) = server
            .json(
                "POST",
                "/api/dev/floors",
                serde_json::json!({"name": "Upstairs", "level": 1}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{upstairs}");
        server
            .json(
                "POST",
                "/api/dev/floors",
                serde_json::json!({"name": "Ground floor"}),
            )
            .await?;
        let (status, room) = server
            .json(
                "POST",
                "/api/dev/areas",
                serde_json::json!({"name": "Bedroom", "floor": "upstairs"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{room}");
        assert_eq!(room["floor_id"], "upstairs");

        let home = server.read("/api/dev/home").await?;
        assert_eq!(
            home["floors"][0]["id"], "ground_floor",
            "lowest first: {home}"
        );
        let areas = std::fs::read_to_string(server.config_dir().join("areas.toml"))?;
        assert!(
            areas.contains("[floors.upstairs]") && areas.contains("floor = \"upstairs\""),
            "{areas}"
        );

        let (status, _) = server
            .json(
                "POST",
                "/api/dev/areas",
                serde_json::json!({"name": "Attic", "floor": "nowhere"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = server
            .json(
                "DELETE",
                "/api/dev/floors/upstairs",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let home = server.read("/api/dev/home").await?;
        assert_eq!(home["areas"][0]["name"], "Bedroom", "the room stayed");

        let (status, moved) = server
            .json(
                "PATCH",
                "/api/dev/areas/bedroom",
                serde_json::json!({"floor": "ground_floor"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{moved}");
        assert_eq!(moved["floor_id"], "ground_floor");
        assert_eq!(
            moved["name"], "Bedroom",
            "a floor change leaves the name alone"
        );
        Ok(())
    }

    /// Edits that can't be made say why, and change nothing.
    #[tokio::test]
    async fn refused_edits_explain_themselves() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"area": "nowhere"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "there's no room `nowhere`");

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/not_a_device",
                serde_json::json!({"name": "Nope"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/areas/nowhere",
                serde_json::json!({"name": "Nope"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // A name that isn't a name at all, rather than one that's merely wrong.
        let (status, body) = server
            .json("POST", "/api/dev/areas", serde_json::json!({"name": ""}))
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

        assert!(
            !server.config_dir().join("devices.toml").exists(),
            "nothing was written"
        );

        host.shutdown().await;
        Ok(())
    }

    // --- Secrets ----------------------------------------------------------------------------

    /// A protocol that won't do anything without a key, and says where the key goes.
    struct Safe;

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct SafeSettings {
        #[serde(default)]
        code: Option<String>,
        /// Never read by `run` below; only here so a test can exercise the generic settings
        /// endpoint's schema-driven secret routing without needing a real external extension.
        #[serde(default)]
        #[expect(dead_code)]
        key: Option<TestSecret>,
    }

    #[derive(Debug, Deserialize)]
    struct TestSecret(#[expect(dead_code)] String);

    impl schemars::JsonSchema for TestSecret {
        fn schema_name() -> std::borrow::Cow<'static, str> {
            "TestSecret".into()
        }

        fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
            schemars::json_schema!({ "type": "string", "writeOnly": true })
        }
    }

    impl irori_protocol::Protocol for Safe {
        type Config = SafeSettings;
        const MANIFEST: &'static str = r#"
            [extension]
            id = "safe"
            name = "Safe"
            version = "0.1.0"
            irori = ">=0.0.0"

            [[contributes.protocol]]
            iot_class = "local_push"
            entity_kinds = ["switch"]

            [[contributes.protocol.actions]]
            id = "open"
            label = "Open the safe"
        "#;
        async fn run(
            settings: SafeSettings,
            mut ctx: irori_protocol::ProtocolContext,
        ) -> Result<(), irori_protocol::ProtocolError> {
            if settings.code.is_none() {
                ctx.set_waiting(vec![irori_types::Waiting {
                    unique_id: "vault".parse()?,
                    name: "Vault".parse()?,
                    reason: "it wants a code".into(),
                    secret: Some(irori_types::SecretRequest {
                        path: vec!["code".into()],
                        label: "Code".into(),
                        hint: None,
                    }),
                }])
                .await;
            }
            // Only offered once it has a code — same idea as `zigbee`'s permit_join only
            // showing up once it's actually found a Z2M bridge.
            if settings.code.is_some() {
                ctx.set_available_actions(vec!["open".to_owned()]).await;
            }
            while let Some(incoming) = ctx.next_action().await {
                incoming.reply(Ok(()));
            }
            Ok(())
        }
    }

    async fn safe() -> anyhow::Result<(Core, irori_core::ExtensionHost)> {
        let core = core();
        let host = irori_core::ExtensionHost::start(
            &core,
            vec![irori_protocol::builtin::<Safe>().map_err(anyhow::Error::msg)?],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        waiting_for(&core, 1).await?;
        Ok((core, host))
    }

    async fn waiting_for(core: &Core, count: usize) -> anyhow::Result<()> {
        for _ in 0..500 {
            let waiting = core
                .extensions()
                .get(&ExtensionId::try_from("safe")?)
                .map_or(0, |overview| overview.waiting.len());
            if waiting == count {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("the safe never had {count} waiting")
    }

    async fn available_actions_for(core: &Core, count: usize) -> anyhow::Result<()> {
        for _ in 0..500 {
            let available = core
                .extensions()
                .get(&ExtensionId::try_from("safe")?)
                .map_or(0, |overview| overview.available_actions.len());
            if available == count {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("the safe never had {count} available actions")
    }

    /// The whole path of a secret: the page shows what's waiting, sends the secret to the place
    /// it was asked for, and the extension is restarted with it — and it's written somewhere only
    /// Irori's user can read, and never handed back.
    #[tokio::test]
    async fn a_secret_given_where_it_was_asked_for_unlocks_the_extension() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let home = server.read("/api/dev/home").await?;
        assert_eq!(home["extensions"]["safe"]["waiting"][0]["name"], "Vault");
        assert_eq!(
            home["extensions"]["safe"]["waiting"][0]["secret"]["path"],
            serde_json::json!(["code"])
        );

        let (status, body) = server
            .json(
                "PUT",
                "/api/dev/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "1234-5678"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(
            !body.to_string().contains("1234"),
            "the secret came back: {body}"
        );

        waiting_for(&core, 0).await?;
        let written = std::fs::read_to_string(server.config_dir().join("secrets.toml"))?;
        assert!(
            written.contains("[safe]") && written.contains("code = \"1234-5678\""),
            "{written}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(server.config_dir().join("secrets.toml"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        }
        let home = server.read("/api/dev/home").await?;
        assert!(
            !home.to_string().contains("1234"),
            "a secret leaked into the home view"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Without sign-in, this endpoint can only answer a question an extension is asking. It can't
    /// put anything anywhere else in anyone's settings.
    #[tokio::test]
    async fn a_secret_nobody_asked_for_is_refused() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, _) = server
            .json(
                "PUT",
                "/api/dev/extensions/safe/secrets",
                serde_json::json!({"path": ["something_else"], "value": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = server
            .json(
                "PUT",
                "/api/dev/extensions/nope/secrets",
                serde_json::json!({"path": ["code"], "value": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = server
            .json(
                "PUT",
                "/api/dev/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "   "}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        assert!(
            !server.config_dir().join("secrets.toml").exists(),
            "nothing was written"
        );
        host.shutdown().await;
        Ok(())
    }

    // --- Generic settings (the gear icon) ----------------------------------------------------

    /// A non-secret field goes to `extensions/<id>.toml` and restarts the extension with it —
    /// same outcome as giving a secret, through the generic form instead of the waiting-item one.
    #[tokio::test]
    async fn generic_settings_reach_the_extension_and_restart_it() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/settings",
                serde_json::json!({"code": "1234-5678"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        waiting_for(&core, 0).await?;
        let written = std::fs::read_to_string(server.config_dir().join("extensions/safe.toml"))?;
        assert!(written.contains("code = \"1234-5678\""), "{written}");
        assert!(
            !server.config_dir().join("secrets.toml").exists(),
            "a non-secret field must not land in secrets.toml"
        );

        host.shutdown().await;
        Ok(())
    }

    /// A schema-valid `null` for an `Option<T>` field means "leave this unset" — it must clear
    /// the field, not get handed to TOML (which has no `null`) and silently blank the whole file.
    #[tokio::test]
    async fn a_null_value_unsets_the_field_instead_of_erasing_the_file() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/settings",
                serde_json::json!({"code": "1234-5678"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/settings",
                serde_json::json!({"code": null}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let written = std::fs::read_to_string(server.config_dir().join("extensions/safe.toml"))?;
        assert!(
            !written.contains("1234-5678"),
            "the field should have been cleared: {written}"
        );

        host.shutdown().await;
        Ok(())
    }

    /// A `writeOnly` field — schemars' shape for an `Option<Secret>`-like type — is routed to
    /// `secrets.toml` instead, the same file a waiting-item secret goes to.
    #[tokio::test]
    async fn a_write_only_field_is_routed_to_secrets_toml() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/settings",
                serde_json::json!({"key": "shh"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let written = std::fs::read_to_string(server.config_dir().join("secrets.toml"))?;
        assert!(
            written.contains("[safe]") && written.contains("key = \"shh\""),
            "{written}"
        );
        assert!(
            !server.config_dir().join("extensions/safe.toml").exists()
                || !std::fs::read_to_string(server.config_dir().join("extensions/safe.toml"))?
                    .contains("shh"),
            "a secret field must not land in extensions/<id>.toml"
        );

        host.shutdown().await;
        Ok(())
    }

    /// A bad secret value is caught before the non-secret half of the same request is written —
    /// otherwise a request with both a valid `code` and an invalid `key` would save `code` and
    /// then fail on `key`, leaving the request half-applied.
    #[tokio::test]
    async fn an_invalid_secret_value_leaves_the_non_secret_half_unwritten() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/settings",
                serde_json::json!({"code": "1234-5678", "key": 42}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            !server.config_dir().join("extensions/safe.toml").exists(),
            "the non-secret field must not be written when the secret field is invalid"
        );
        assert!(
            !server.config_dir().join("secrets.toml").exists(),
            "nothing was written"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Scoped to exactly the fields the extension's own schema declares — not a way to write
    /// anything into anyone's settings (`docs/specs/config.md` §3.6).
    #[tokio::test]
    async fn unknown_settings_keys_are_refused() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/settings",
                serde_json::json!({"not_a_real_field": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            !server.config_dir().join("extensions/safe.toml").exists(),
            "nothing was written"
        );

        let (status, _) = server
            .json(
                "POST",
                "/api/dev/extensions/nope/settings",
                serde_json::json!({"code": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        host.shutdown().await;
        Ok(())
    }

    // --- Protocol actions (the "+ Add device" button, e.g. Zigbee's permit_join) ------------

    /// An action declared in the manifest, and said to be available right now, actually runs.
    #[tokio::test]
    async fn a_declared_and_available_action_can_be_triggered() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        // Give it its code, which is what makes `open` available (see `Safe::run`).
        server
            .json(
                "PUT",
                "/api/dev/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "1234-5678"}),
            )
            .await?;
        waiting_for(&core, 0).await?;
        available_actions_for(&core, 1).await?;

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/actions/open",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        host.shutdown().await;
        Ok(())
    }

    /// An action nobody declared, one that isn't available yet, and an unknown extension are
    /// all refused rather than reaching the extension.
    #[tokio::test]
    async fn an_undeclared_or_unavailable_action_is_refused() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, _) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/actions/not_a_real_action",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Declared in the manifest, but not yet available: no code has been given.
        let (status, _) = server
            .json(
                "POST",
                "/api/dev/extensions/safe/actions/open",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = server
            .json(
                "POST",
                "/api/dev/extensions/nope/actions/open",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        host.shutdown().await;
        Ok(())
    }

    /// Ignoring a device takes it out of everything the page shows, writes it down, and letting
    /// it back in restores it with its entities.
    #[tokio::test]
    async fn a_device_can_be_ignored_and_let_back_in() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"ignored": true}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let home = server.read("/api/dev/home").await?;
        let listed = |key: &str, id: &str| {
            home[key]
                .as_array()
                .is_some_and(|all| all.iter().any(|item| item["id"] == id))
        };
        assert!(!listed("devices", "demo_lamp"));
        assert!(!listed("entities", "light.demo_lamp"));
        assert!(listed("held", "demo_lamp"));
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("ignored = true"), "{devices}");

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"ignored": false}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let home = server.read("/api/dev/home").await?;
        assert!(
            home["entities"]
                .as_array()
                .is_some_and(|all| all.iter().any(|e| e["id"] == "light.demo_lamp"))
        );
        assert!(home.get("held").is_none(), "{home}");

        host.shutdown().await;
        Ok(())
    }

    /// "Let back in" restores the device to the home, even when Irori is asking before adding
    /// new ones. Clearing `ignored` alone would put a never-added device on the waiting list.
    #[tokio::test]
    async fn letting_a_device_back_in_adds_it_even_when_asking() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"ignored": true}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");

        let mut settings = core.settings();
        settings.ask_before_adding = true;
        core.apply_settings(settings);

        let (status, body) = server
            .json(
                "PATCH",
                "/api/dev/devices/demo_lamp",
                serde_json::json!({"ignored": false}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let home = server.read("/api/dev/home").await?;
        assert!(
            home["devices"]
                .as_array()
                .is_some_and(|all| all.iter().any(|item| item["id"] == "demo_lamp")),
            "{home}"
        );
        assert!(home.get("held").is_none(), "{home}");

        host.shutdown().await;
        Ok(())
    }

    /// The helpers extension on its own, started against a core with storage that outlives it,
    /// as the database does.
    async fn helpers(core: &Core) -> anyhow::Result<irori_core::ExtensionHost> {
        irori_core::ExtensionHost::start(
            core,
            vec![irori_protocol::builtin::<irori_helpers::Helpers>().map_err(anyhow::Error::msg)?],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)
    }

    async fn until(what: &str, mut check: impl FnMut() -> bool) -> anyhow::Result<()> {
        for _ in 0..500 {
            if check() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("timed out waiting for: {what}")
    }

    /// A toggle made from the page is a switch that remembers its value — through a new toggle
    /// being added, which restarts the extension, and through Irori starting again. Renaming it
    /// changes its one name, in the file that defines it, and removing it removes the entity.
    #[tokio::test]
    async fn a_toggle_keeps_its_value_and_has_one_name() -> anyhow::Result<()> {
        let storage: Arc<dyn irori_protocol::Storage> =
            Arc::new(irori_protocol::MemoryStorage::default());
        let core = core();
        core.use_storage(Arc::clone(&storage));
        let server = Server::new(core.clone())?;
        let host = helpers(&core).await?;

        let (status, made) = server
            .json(
                "POST",
                "/api/dev/helpers/toggles",
                serde_json::json!({"name": "Guests are over"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(made["entity_id"], "switch.guests_are_over");
        let guests = EntityId::try_from("switch.guests_are_over")?;
        until("the toggle exists", || {
            core.state(&guests).is_some_and(|s| s.state.is_some())
        })
        .await?;

        let (status, body) = server
            .json(
                "POST",
                "/api/dev/command",
                serde_json::json!({"entity_id": guests, "command": "turn_on"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");

        // Another toggle restarts the extension; the first keeps its value.
        server
            .json(
                "POST",
                "/api/dev/helpers/toggles",
                serde_json::json!({"name": "Holiday"}),
            )
            .await?;
        let holiday = EntityId::try_from("switch.holiday")?;
        until("the second toggle exists", || {
            core.state(&holiday).is_some_and(|s| s.state.is_some())
        })
        .await?;
        let on = |core: &Core, id: &EntityId| {
            matches!(
                core.state(id).and_then(|s| s.state),
                Some(irori_types::State::Switch(irori_types::SwitchState {
                    on: true
                }))
            )
        };
        until("still on", || on(&core, &guests)).await?;

        // One name, kept where the toggle is defined.
        let (status, _) = server
            .json(
                "PATCH",
                "/api/dev/entities/switch.guests_are_over",
                serde_json::json!({"name": "Visitors"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let file = std::fs::read_to_string(server.config_dir().join("extensions/helpers.toml"))?;
        assert!(file.contains("Visitors"), "{file}");
        assert!(
            !server.config_dir().join("entities.toml").exists(),
            "no second name anywhere"
        );
        until("renamed", || {
            core.entities()
                .iter()
                .any(|e| e.id == guests && e.name.as_str() == "Visitors")
        })
        .await?;
        host.shutdown().await;

        // Irori starting again, with the same storage: still on.
        let again = self::core();
        again.use_storage(storage);
        let _config = Config::open_dir(server.config_dir(), &again);
        let host = helpers(&again).await?;
        until("on after a restart", || on(&again, &guests)).await?;
        host.shutdown().await;

        let host = helpers(&core).await?;
        let (status, _) = server
            .json(
                "DELETE",
                "/api/dev/helpers/toggles/holiday",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        until("the removed toggle's entity is gone", || {
            core.state(&holiday).is_none()
        })
        .await?;
        host.shutdown().await;
        Ok(())
    }

    /// An icon arrives as an image with a policy that stops it doing anything but being one.
    #[tokio::test]
    async fn an_extensions_icon_is_served_as_a_locked_down_image() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let home = server.read("/api/dev/home").await?;
        assert_eq!(home["extensions"]["demo"]["has_icon"], true);

        let res = server
            .app()?
            .oneshot(Request::get("/api/dev/extensions/demo/icon.svg").body(Body::empty())?)
            .await?;
        assert_eq!(res.status(), StatusCode::OK);
        let headers = res.headers().clone();
        assert_eq!(headers["content-type"], "image/svg+xml");
        assert!(
            headers["content-security-policy"]
                .to_str()?
                .contains("default-src 'none'")
        );
        let body = res.into_body().collect().await?.to_bytes();
        assert!(
            String::from_utf8(body.to_vec())?
                .trim_start()
                .starts_with("<svg")
        );

        let (status, _) = server
            .send(Request::get("/api/dev/extensions/nope/icon.svg").body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        host.shutdown().await;
        Ok(())
    }

    /// A missing file says so, rather than quietly handing back the page.
    #[tokio::test]
    async fn a_missing_file_is_not_found() -> anyhow::Result<()> {
        let (status, _, _) = get("/does-not-exist.css").await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        Ok(())
    }

    /// An endpoint that doesn't exist says so, rather than answering with the page: a caller
    /// asking for JSON must not read a 200 as "that worked".
    #[tokio::test]
    async fn a_missing_endpoint_is_not_found() -> anyhow::Result<()> {
        for path in ["/api/dev/not-real", "/api/nope", "/api"] {
            let (status, _, _) = get(path).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
        Ok(())
    }

    /// The UI's own pages are its business: reloading on `/devices` has to reach the app, which
    /// then decides what to show. Holds whether or not the UI has been built into this binary.
    #[cfg(feature = "ui")]
    #[tokio::test]
    async fn the_apps_own_pages_reach_the_app() -> anyhow::Result<()> {
        let (status, content_type, body) = get("/devices").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/html"));
        assert!(String::from_utf8(body)?.contains("IroriOS"));
        Ok(())
    }
}
