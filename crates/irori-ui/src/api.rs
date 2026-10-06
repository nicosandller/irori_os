//! Talking to the core over HTTP.
//!
//! These are the temporary `/api/dev/*` endpoints (`crates/irori/src/server.rs`), polled every
//! few seconds. The real API is a WebSocket that pushes changes, with typed messages shared
//! through `irori-types` (M0.5, M1.5); this module is what goes away then.

use std::collections::BTreeMap;

use gloo_net::http::Request;
use irori_types::{
    Area, AreaId, Device, DeviceId, Entity, EntityId, EntityState, ExtensionId, FloorId, Floorplan,
    LightTurnOn, Name, Waiting,
};
use serde::{Deserialize, Serialize};

const HOME_URL: &str = "/api/dev/home";
const HEALTH_URL: &str = "/api/health";
const COMMAND_URL: &str = "/api/dev/command";
const AREAS_URL: &str = "/api/dev/areas";
const HISTORY_URL: &str = "/api/dev/history";
const SYSTEM_URL: &str = "/api/dev/system";
const RESTART_URL: &str = "/api/dev/restart";

/// Everything the page shows. Mirrors `HomeView` on the server; the two meet again in
/// `irori-types` when the real API lands.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Home {
    pub devices: Vec<Device>,
    pub entities: Vec<Entity>,
    pub states: Vec<EntityState>,
    pub extensions: BTreeMap<ExtensionId, Extension>,
    /// The areas of the home, from the config directory. Empty until somebody makes one.
    #[serde(default)]
    pub areas: Vec<Area>,
    /// Devices kept out of the home: ignored, or new and waiting to be added.
    #[serde(default)]
    pub held: Vec<HeldDevice>,
    /// The levels of the home, lowest first.
    #[serde(default)]
    pub floors: Vec<irori_types::Floor>,
    /// The home as it's drawn. Absent until somebody draws it, which is the same as blank.
    #[serde(default)]
    pub floorplan: Floorplan,
}

/// A device an extension has found that isn't in the home: enough to recognise it and decide
/// whether it belongs. What it is, never what it's reporting, so the list of these holds still.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HeldDevice {
    pub id: DeviceId,
    pub protocol: String,
    pub name: Name,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// How many entities of each kind it would bring — `{"light": 1, "sensor": 2}`.
    #[serde(default)]
    pub provides: std::collections::BTreeMap<String, usize>,
}

impl Home {
    pub fn area(&self, id: &AreaId) -> Option<&Area> {
        self.areas.iter().find(|area| &area.id == id)
    }

    /// What to call the area a device is in, for showing next to it.
    pub fn area_of(&self, device: &Device) -> Option<String> {
        let area = self.area(device.area_id.as_ref()?)?;
        Some(area.name.to_string())
    }

    /// Takes an entity's state unless what's already here is at least as new, and says whether
    /// anything changed.
    ///
    /// The page learns about a change twice: from the command that caused it, and from the next
    /// poll. Those arrive in either order — a poll that started before the change lands after it
    /// — so the older of the two must never win, or the page would go backwards for a couple of
    /// seconds. `last_updated` only moves forward for an entity, which makes it the tiebreaker.
    pub fn accept(&mut self, state: EntityState) -> bool {
        let Some(shown) = self
            .states
            .iter_mut()
            .find(|shown| shown.entity_id == state.entity_id)
        else {
            // An entity this snapshot doesn't have: whether it's new or gone is the registry's
            // business, and the next poll settles it.
            return false;
        };
        if state.last_updated <= shown.last_updated {
            return false;
        }
        *shown = state;
        true
    }
}

/// How an extension is faring. Deliberately loose: the server's `ExtensionStatus` gains variants
/// as Irori grows, and a status this page doesn't know yet should still show up as a word rather
/// than blank the whole page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Extension {
    /// Its own name, from its manifest.
    #[serde(default)]
    pub name: String,
    /// What it says it's for.
    #[serde(default)]
    pub description: Option<String>,
    /// Which kinds of entity its protocol can provide.
    #[serde(default)]
    pub entity_kinds: Vec<String>,
    /// Where its devices live and what they need: `local_push`, `cloud_polling`, and so on.
    #[serde(default)]
    pub iot_class: Option<String>,
    /// `running`, `starting`, `degraded`, `failed`, `disabled`.
    pub state: String,
    /// Why, when something is wrong.
    #[serde(default)]
    pub reason: Option<String>,
    /// State reports the core refused, e.g. a value that doesn't fit the entity.
    #[serde(default)]
    pub rejected_reports: u64,
    /// State reports dropped because the core couldn't keep up.
    #[serde(default)]
    pub dropped_reports: u64,
    /// What it found but can't use until someone helps, e.g. a device that needs its key.
    #[serde(default)]
    pub waiting: Vec<Waiting>,
    /// What it found that Irori has no entity kind for yet, listed on its devices.
    #[serde(default)]
    pub unmodeled: Vec<irori_types::Unmodeled>,
    /// Whether it has an icon, at `/api/dev/extensions/<id>/icon.svg`.
    #[serde(default)]
    pub has_icon: bool,
    /// Actions it declares (static, from its manifest) — the "+ Add device" button for a
    /// protocol that has a dedicated flow, e.g. Zigbee's permit-join.
    #[serde(default)]
    pub actions: Vec<ProtocolActionInfo>,
    /// Which of `actions` are usable right now, as the protocol itself says.
    #[serde(default)]
    pub available_actions: Vec<String>,
    /// Its timed actions that are open right now, each with how long was left when this was
    /// read (Zigbee's network accepting new devices).
    #[serde(default)]
    pub open_actions: BTreeMap<String, OpenAction>,
    /// Whether removing one of its devices also unpairs it from the protocol's own network,
    /// so it has to join again to come back (Zigbee).
    #[serde(default)]
    pub unpairs: bool,
    /// Its own page, with an entry in the sidebar (`docs/specs/automations.md` §B3).
    #[serde(default)]
    pub app: Option<AppInfo>,
    /// Whether it's an automation engine.
    #[serde(default)]
    pub engine: bool,
}

/// An extension's page, as its manifest describes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AppInfo {
    pub label: String,
    /// The API scopes it declared: what the bridge may hand its page.
    #[serde(default)]
    pub api: Vec<String>,
    /// The entity format its page reads; what it's handed is in that format
    /// (`irori_types::format`). 1 for a page built before formats existed.
    #[serde(default = "first_format")]
    pub entity_format: u32,
}

fn first_format() -> u32 {
    1
}

/// One entry of `/api/dev/apps`: a running extension's page and whether its files are there.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AppEntry {
    pub extension: String,
    pub built: bool,
}

pub async fn fetch_apps() -> Result<Vec<AppEntry>, String> {
    let response = Request::get("/api/dev/apps")
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return Err(format!("/api/dev/apps answered {}", response.status()));
    }
    response
        .json::<Vec<AppEntry>>()
        .await
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

/// Asks an extension's engine something for its page. Answers the engine's value, or its
/// refusal in words.
pub async fn app_rpc(
    extension: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("/api/dev/apps/{extension}/rpc");
    let response = Request::post(&url)
        .json(&serde_json::json!({ "method": method, "params": params }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        let status = response.status();
        return Err(match response.json::<Refused>().await {
            Ok(refused) => refused.error,
            Err(_) => format!("the extension didn't answer ({status})"),
        });
    }
    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        value: serde_json::Value,
    }
    response
        .json::<Answer>()
        .await
        .map(|answer| answer.value)
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

/// A timed action that's open, as of when the extension was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct OpenAction {
    /// Milliseconds until it closes on its own.
    pub closes_in_ms: u64,
}

/// One action an extension declares (`docs/specs/protocols.md` §5).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProtocolActionInfo {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub seconds: Option<u32>,
}

/// The browser's own words for a failed request ("TypeError: Failed to fetch") say nothing a
/// person can act on, so they go to the console and the page says what it means instead.
fn unreachable(error: gloo_net::Error) -> String {
    leptos::logging::error!("{error}");
    "Can't reach Irori. Is it still running?".to_owned()
}

/// What Irori says about itself. Changes rarely, so the page asks once.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Health {
    pub version: String,
    /// The commit this Irori was built from, and when. Development builds are all `0.0.0`, so
    /// this is how you tell a running Irori from the one you just built.
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub built_at: String,
    /// Which boot this is. It changes every time the process starts, which is how the page tells
    /// the restart it asked for from a build that just happens to have started recently.
    #[serde(default)]
    pub boot_id: String,
    pub uptime_ms: u128,
    pub features: Vec<String>,
    pub sqlite: Sqlite,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Sqlite {
    pub version: String,
    pub journal_mode: String,
}

/// What the System section of Settings shows about the machine running Irori, from
/// `/api/dev/system`. The server reads it from the OS each time it's asked, so an "Ask again"
/// sees the machine as it is — a disk that filled since the last ask is a disk that filled.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct System {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub os_version: String,
    #[serde(default)]
    pub kernel: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub cpu: String,
    #[serde(default)]
    pub cpu_cores: usize,
    #[serde(default)]
    pub memory_total: u64,
    #[serde(default)]
    pub memory_used: u64,
    /// Every core the OS schedules on, and how many processes wanted one over the last one,
    /// five and fifteen minutes.
    #[serde(default)]
    pub cpu_logical: usize,
    #[serde(default)]
    pub load_average: [f64; 3],
    /// What Irori's own process holds of the memory.
    #[serde(default)]
    pub process_memory: u64,
    #[serde(default)]
    pub swap_total: u64,
    #[serde(default)]
    pub swap_used: u64,
    /// Every volume mounted on the machine.
    #[serde(default)]
    pub disks: Vec<Disk>,
    #[serde(default)]
    pub data_dir: String,
    /// The database on disk, write-ahead log included.
    #[serde(default)]
    pub database_bytes: u64,
    /// How long the machine has been up, in the OS's seconds.
    #[serde(default)]
    pub uptime_secs: u64,
    /// The volume the instance's data is on.
    #[serde(default)]
    pub disk: Disk,
}

/// Enough about a volume to see whether it's getting full.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Disk {
    #[serde(default)]
    pub mount: String,
    pub total: u64,
    pub available: u64,
    pub used: u64,
}

/// What is using the machine, from `/api/dev/system/usage`: the heaviest processes, and what
/// Irori's data directory is made of. Slower to gather than [`System`], so it's asked for only
/// while a meter is open.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub processes: Vec<Process>,
    #[serde(default)]
    pub storage: Vec<Stored>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Process {
    pub name: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub memory: u64,
    /// Its share of the whole machine's processor, 0–100.
    #[serde(default)]
    pub cpu: f32,
    /// Whether it is Irori itself.
    #[serde(default)]
    pub own: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Stored {
    pub name: String,
    pub bytes: u64,
}

pub async fn fetch_usage() -> Result<Usage, String> {
    let url = "/api/dev/system/usage";
    let response = Request::get(url).send().await.map_err(unreachable)?;
    if !response.ok() {
        return Err(format!("{url} answered {}", response.status()));
    }
    response
        .json::<Usage>()
        .await
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

pub async fn fetch_system() -> Result<System, String> {
    let response = Request::get(SYSTEM_URL).send().await.map_err(unreachable)?;
    if !response.ok() {
        return Err(format!("{SYSTEM_URL} answered {}", response.status()));
    }
    response
        .json::<System>()
        .await
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

/// Asks Irori to restart itself. It answers before it goes; the page finds out it's back from
/// its own polling, which shows a fresh boot once the new instance is up. The outcome matters:
/// a refusal from the living server is a "nothing happened", while going quiet is not — a
/// restart may well have been accepted and the old server drained as the answer was on its way.
#[derive(Debug)]
pub enum RestartSent {
    /// The server answered 202: the restart is on its way, and the page verifies it by boot.
    Accepted,
    /// The server answered that it wouldn't — 403, 429, ... — so nothing is restarting.
    Refused(String),
    /// No answer came back at all. The server may already have been shutting down (a restart
    /// that was accepted) or be down for another reason; it is not a refusal, and the page must
    /// not treat it as one.
    Lost,
}

/// Asks Irori to restart itself.
pub async fn restart() -> RestartSent {
    // The server only restarts for the page's own fetch: this header is what says it is one,
    // and a cross-site website can't set it (a <form> POST has no header, and a fetch with a
    // custom header is stopped by CORS preflight).
    let response = match Request::post(RESTART_URL)
        .header("x-irori-ui", "1")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            // The browser's own words for a failed request ("TypeError: Failed to fetch") say
            // nothing a person can act on, so they go to the console.
            leptos::logging::error!("{error}");
            return RestartSent::Lost;
        }
    };
    match checked(response).await {
        Ok(()) => RestartSent::Accepted,
        Err(why) => RestartSent::Refused(why),
    }
}

pub async fn fetch_health() -> Result<Health, String> {
    let response = Request::get(HEALTH_URL).send().await.map_err(unreachable)?;
    if !response.ok() {
        return Err(format!("{HEALTH_URL} answered {}", response.status()));
    }
    response
        .json::<Health>()
        .await
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

pub async fn fetch_home() -> Result<Home, String> {
    let response = Request::get(HOME_URL).send().await.map_err(unreachable)?;
    if !response.ok() {
        return Err(format!("{HOME_URL} answered {}", response.status()));
    }
    response
        .json::<Home>()
        .await
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

#[derive(Debug, Serialize)]
struct CommandRequest<'a> {
    entity_id: &'a EntityId,
    command: &'a str,
    /// The action's data: a light's brightness or color for `turn_on`, a number's value.
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Refused {
    error: String,
}

/// Sends a command, and answers with the entity's state once the protocol confirms (or `None`
/// if it vanished meanwhile).
async fn command(
    entity_id: &EntityId,
    command: &str,
    data: Option<serde_json::Value>,
) -> Result<Option<EntityState>, String> {
    let body = CommandRequest {
        entity_id,
        command,
        data,
    };
    let response = Request::post(COMMAND_URL)
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        // The core explains refusals in a sentence; fall back to the status code if it didn't.
        let status = response.status();
        return Err(match response.json::<Refused>().await {
            Ok(refused) => refused.error,
            Err(_) => format!("the command was refused ({status})"),
        });
    }
    response
        .json::<Option<EntityState>>()
        .await
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

/// Turns an entity on or off.
pub async fn set_on(entity_id: &EntityId, on: bool) -> Result<Option<EntityState>, String> {
    command(entity_id, if on { "turn_on" } else { "turn_off" }, None).await
}

/// Turns a light on with its brightness, color temperature or color. Sending `turn_on` with the
/// level is what dimming *is*: the light comes on (or stays on) at the level it asked for.
pub async fn set_light(
    entity_id: &EntityId,
    data: &LightTurnOn,
) -> Result<Option<EntityState>, String> {
    command(entity_id, "turn_on", serde_json::to_value(data).ok()).await
}

/// Asks an entity for one of its kind's actions, with that action's data: a button's `press`, a
/// number's `set_value {value}`, a cover's `set_position {position}`.
pub async fn act(
    entity_id: &EntityId,
    action: &str,
    data: Option<serde_json::Value>,
) -> Result<Option<EntityState>, String> {
    command(entity_id, action, data).await
}

// --- Areas, names, and where things live ------------------------------------------------
//
// These write files in the config directory (`docs/specs/config.md`). Each answers with what the
// thing became, but the page refetches anyway: a rename can change more than the thing renamed,
// because entities without a name of their own follow their device.

/// Where to put a device: in an area, in none at all, or back to having said nothing — which
/// lets whatever the device suggests for itself stand in again.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum WhereTo {
    In(AreaId),
    /// Serialized as `false`: deliberately no area, suggestion and all.
    Nowhere(bool),
}

impl WhereTo {
    pub fn nowhere() -> Self {
        WhereTo::Nowhere(false)
    }
}

/// What a change to a device should do to one of its fields: leave it alone, set it, or clear it
/// so whatever the protocol reports comes back.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceEdit {
    /// `None` leaves the name alone; `Some(None)` clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<Option<Name>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<irori_types::Description>>,
    /// `None` leaves the area alone; `Some(None)` un-says it, letting the device suggest again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<Option<WhereTo>>,
    /// `Some(true)` adds a device an extension has found ("+ Add device").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<bool>,
}

/// The body of a refusal, which the core writes as a sentence.
async fn checked(response: gloo_net::http::Response) -> Result<(), String> {
    if response.ok() {
        return Ok(());
    }
    let status = response.status();
    Err(match response.json::<Refused>().await {
        Ok(refused) => refused.error,
        Err(_) => format!("Irori refused that ({status})"),
    })
}

pub async fn edit_device(device_id: &DeviceId, edit: &DeviceEdit) -> Result<(), String> {
    let response = Request::patch(&format!("/api/dev/devices/{device_id}"))
        .json(edit)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// Why a device wasn't removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveFailed {
    /// Its network couldn't let it go, usually because the device is asleep and didn't
    /// answer. Nothing was changed; removing it again with `force` drops it anyway.
    Unpair(String),
    Other(String),
}

/// Removes a device from the home: everything Irori keeps of it goes, and it's listed as found
/// again, to be added back from "+ Add device" if wanted. One whose protocol keeps a network
/// of its own (Zigbee) is unpaired from it too, and is gone until it joins again; `force` is
/// for one that didn't answer the first time.
pub async fn remove_device(device_id: &DeviceId, force: bool) -> Result<(), RemoveFailed> {
    #[derive(Deserialize)]
    struct Failed {
        error: String,
        #[serde(default)]
        code: Option<String>,
    }
    let query = if force { "?force=true" } else { "" };
    let response = Request::delete(&format!("/api/dev/devices/{device_id}{query}"))
        .send()
        .await
        .map_err(|e| RemoveFailed::Other(unreachable(e)))?;
    if response.ok() {
        return Ok(());
    }
    let status = response.status();
    Err(match response.json::<Failed>().await {
        Ok(failed) if failed.code.as_deref() == Some("unpair_failed") => {
            RemoveFailed::Unpair(failed.error)
        }
        Ok(failed) => RemoveFailed::Other(failed.error),
        Err(_) => RemoveFailed::Other(format!("Irori refused that ({status})")),
    })
}

#[derive(Debug, Serialize)]
struct EntityEdit {
    name: Option<Name>,
}

pub async fn rename_entity(entity_id: &EntityId, name: Option<Name>) -> Result<(), String> {
    let response = Request::patch(&format!("/api/dev/entities/{entity_id}"))
        .json(&EntityEdit { name })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

#[derive(Debug, Serialize)]
struct AreaRequest {
    name: Name,
    /// The floor the area sits on. The page makes areas straight onto a floor, so an area
    /// without one only exists when a floor it was on has gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    floor: Option<FloorId>,
}

pub async fn add_area(name: Name, floor: Option<&FloorId>) -> Result<(), String> {
    let response = Request::post(AREAS_URL)
        .json(&AreaRequest {
            name,
            floor: floor.cloned(),
        })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

pub async fn rename_area(id: &AreaId, name: Name) -> Result<(), String> {
    let response = Request::patch(&format!("{AREAS_URL}/{id}"))
        .json(&AreaRequest { name, floor: None })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// Where an area is going: a floor, or `null` for none.
#[derive(Debug, Serialize)]
struct AreaMove {
    floor: Option<FloorId>,
}

/// Moves an area to another floor, or off every floor. Its devices come with it.
pub async fn move_area(id: &AreaId, floor: Option<&FloorId>) -> Result<(), String> {
    let response = Request::patch(&format!("{AREAS_URL}/{id}"))
        .json(&AreaMove {
            floor: floor.cloned(),
        })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

#[derive(Debug, Serialize)]
struct FloorRequest {
    name: Name,
    level: i8,
}

pub async fn add_floor(name: Name, level: i8) -> Result<(), String> {
    let response = Request::post("/api/dev/floors")
        .json(&FloorRequest { name, level })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// A change to a floor: a new name, a new level, or both.
#[derive(Debug, Serialize)]
struct FloorEdit {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<Name>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<i8>,
}

/// Renames a floor or moves it to another level. Areas on it stay on it.
pub async fn edit_floor(
    id: &irori_types::FloorId,
    name: Option<Name>,
    level: Option<i8>,
) -> Result<(), String> {
    let response = Request::patch(&format!("/api/dev/floors/{id}"))
        .json(&FloorEdit { name, level })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

pub async fn remove_floor(id: &irori_types::FloorId) -> Result<(), String> {
    let response = Request::delete(&format!("/api/dev/floors/{id}"))
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// What saving a plan did beyond writing it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct PlanSaved {
    /// Devices the plan moved into the room they were drawn standing in.
    #[serde(default)]
    pub placed: usize,
}

/// Saves the plan of the home, whole. The editor keeps a working copy while somebody draws, so
/// this is only ever sent by Save — and Cancel is simply never sending it.
pub async fn save_floorplan(plan: &Floorplan) -> Result<PlanSaved, String> {
    let response = Request::put("/api/dev/floorplan")
        .json(plan)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(why) => Err(why),
            Ok(()) => Err("Irori refused that without a reason".into()),
        };
    }
    response
        .json()
        .await
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

pub async fn remove_area(id: &AreaId) -> Result<(), String> {
    let response = Request::delete(&format!("{AREAS_URL}/{id}"))
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// Makes a toggle helper: a switch Irori keeps itself. Its entity appears a moment later, once
/// the helpers extension has restarted with it.
pub async fn add_toggle(name: Name) -> Result<(), String> {
    let response = Request::post("/api/dev/helpers/toggles")
        .json(&AreaRequest { name, floor: None })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// Removes a toggle helper by the id it was made with: the object id of its `switch.` entity.
pub async fn remove_toggle(id: &str) -> Result<(), String> {
    let response = Request::delete(&format!("/api/dev/helpers/toggles/{id}"))
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

#[derive(Serialize)]
struct SecretGiven<'a> {
    path: &'a [String],
    value: &'a str,
}

/// Hands an extension a secret it asked for. Irori writes it to `secrets.toml`, restarts the
/// extension with it, and never sends it back.
/// An official extension, as the Extensions page lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub version: String,
    pub official: bool,
    /// The extension can do anything on the machine. The card says so, and Install asks
    /// before it proceeds.
    #[serde(default)]
    pub full_access: bool,
    pub installed: bool,
    /// The version that's running, when it's installed. `version` is the one Install fetches.
    #[serde(default)]
    pub installed_version: Option<String>,
    pub icon: bool,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    /// What it's configured with now, secrets excluded — the settings form opens showing this,
    /// so changing one field doesn't mean retyping the others.
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

pub async fn fetch_catalog() -> Result<Vec<CatalogEntry>, String> {
    let response = Request::get("/api/dev/catalog")
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

/// The tail of an extension's own output, oldest line first — what the log window shows, and
/// where a failure's real reason is written out in full rather than summarised onto the card.
pub async fn fetch_extension_log(id: &str) -> Result<Vec<String>, String> {
    fetch_log(&format!("/api/dev/extensions/{id}/log")).await
}

/// Irori's own log, oldest line first: what this process has said since it started, which
/// includes the lines an extension's output arrived as. The Settings page's log window shows it.
pub async fn fetch_system_log() -> Result<Vec<String>, String> {
    fetch_log("/api/dev/system/log").await
}

/// What the Ollama Irori installed has said lately.
pub async fn fetch_model_log() -> Result<Vec<String>, String> {
    fetch_log("/api/dev/assistant/log").await
}

/// Either log. The same `{"lines": [...]}` shape by design, so one window reads both, and always
/// a 200, so a log with nothing in it is an empty list rather than an error.
async fn fetch_log(path: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Log {
        #[serde(default)]
        lines: Vec<String>,
    }
    let response = Request::get(path).send().await.map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    let log: Log = response.json().await.map_err(unreachable)?;
    Ok(log.lines)
}

/// Serial devices plugged into the machine running Irori right now — suggestions for a
/// `"format": "serial-port"` settings field, alongside the plain text box it always was.
pub async fn fetch_serial_ports() -> Result<Vec<String>, String> {
    let response = Request::get("/api/dev/serial-ports")
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

/// Installs an extension, or with `update` replaces the version that's installed, keeping its
/// devices, settings and data.
pub async fn install_extension(
    id: &str,
    approve_full_access: bool,
    update: bool,
) -> Result<(), String> {
    let response = Request::post(&format!("/api/dev/extensions/{id}/install"))
        .json(&serde_json::json!({
            "approve_full_access": approve_full_access,
            "update": update,
        }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

pub async fn uninstall_extension(id: &str) -> Result<(), String> {
    let response = Request::delete(&format!("/api/dev/extensions/{id}"))
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// One extension's settings, as a JSON object matching its own `config_schema` — the generic
/// form behind the gear icon. A `writeOnly` field lands in `secrets.toml`; everything else in
/// `extensions/<id>.toml`.
pub async fn set_extension_settings(
    id: &str,
    settings: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let response = Request::post(&format!("/api/dev/extensions/{id}/settings"))
        .json(settings)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

pub async fn give_secret(
    extension: &ExtensionId,
    path: &[String],
    value: &str,
) -> Result<(), String> {
    let response = Request::put(&format!("/api/dev/extensions/{extension}/secrets"))
        .json(&SecretGiven { path, value })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    checked(response).await
}

/// Triggers one of an extension's declared, currently-available actions — Zigbee's
/// `permit_join`, say — from the "+ Add device" flow.
pub async fn trigger_action(extension: &ExtensionId, action_id: &str) -> Result<(), String> {
    let response = Request::post(&format!(
        "/api/dev/extensions/{extension}/actions/{action_id}"
    ))
    .send()
    .await
    .map_err(unreachable)?;
    checked(response).await
}

/// Ends an action that's open before its time is up: closes Zigbee's network to new devices.
pub async fn stop_action(extension: &ExtensionId, action_id: &str) -> Result<(), String> {
    let response = Request::delete(&format!(
        "/api/dev/extensions/{extension}/actions/{action_id}"
    ))
    .send()
    .await
    .map_err(unreachable)?;
    checked(response).await
}

/// One entity's last day, as the server's history endpoint answers it: the changes in order,
/// oldest first.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EntityHistory {
    pub entity: EntityId,
    pub states: Vec<EntityState>,
}

/// The last day of an entity's changes, for the page's expandable table. What the table shows:
/// the most recent day of changes the server has seen while it's been running. The real
/// recorder (M1.3) keeps the long, surviving view; this is the honest "while it's up" slice.
pub async fn entity_history(entity_id: &EntityId) -> Result<Vec<EntityState>, String> {
    let response = Request::get(&format!("{HISTORY_URL}/{entity_id}"))
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        // The core explains refusals in a sentence; fall back to the status code if it didn't.
        let status = response.status();
        return Err(match response.json::<Refused>().await {
            Ok(refused) => refused.error,
            Err(_) => format!("Irori refused that ({status})"),
        });
    }
    response
        .json::<EntityHistory>()
        .await
        .map(|history| history.states)
        .map_err(|e| format!("Irori sent something this page can't read: {e}"))
}

/// Whether a model can answer, and the fields the Settings card edits. The key itself is absent.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantStatus {
    pub ready: bool,
    pub mode: String,
    pub model: Option<String>,
    pub detail: String,
    pub credential: String,
    pub local_tag: String,
    pub library: String,
    pub library_index: String,
    pub ollama: String,
    /// Whether the Ollama here is the one Irori installed, and so can remove.
    #[serde(default)]
    pub managed: bool,
    #[serde(default)]
    pub pulled: Vec<PulledModel>,
    pub preset: String,
    pub base_url: String,
    pub cloud_model: String,
    pub cloud: bool,
    /// Memory a model could be loaded into right now, and how much the machine has.
    #[serde(default)]
    pub memory_free: u64,
    #[serde(default)]
    pub memory_total: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PulledModel {
    pub name: String,
    pub size: u64,
    pub active: bool,
    /// Whether it is in memory now.
    #[serde(default)]
    pub loaded: bool,
    /// About how much memory it takes once loaded.
    #[serde(default)]
    pub needs: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantMessage {
    pub role: String,
    pub body: String,
}

pub async fn fetch_assistant() -> Result<AssistantStatus, String> {
    let response = Request::get("/api/dev/assistant")
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

pub async fn save_assistant(body: &serde_json::Value) -> Result<AssistantStatus, String> {
    let response = Request::put("/api/dev/assistant")
        .json(body)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

pub async fn assistant_transcript(scope: &str) -> Result<Vec<AssistantMessage>, String> {
    let response = Request::get(&format!(
        "/api/dev/assistant/transcript/{}",
        encode_scope(scope)
    ))
    .send()
    .await
    .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

pub async fn assistant_clear(scope: &str) -> Result<(), String> {
    let response = Request::delete(&format!(
        "/api/dev/assistant/transcript/{}",
        encode_scope(scope)
    ))
    .send()
    .await
    .map_err(unreachable)?;
    checked(response).await
}

/// One thing a streamed answer said on its way.
#[derive(Debug, Clone, PartialEq)]
pub enum Streamed {
    /// More of the answer.
    Delta(String),
    /// The model has gone to look something up. The tool's name.
    Step(String),
    Failed(String),
    Done,
}

/// Where a download has got to. `total` is 0 while it isn't known.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress {
    pub status: String,
    pub completed: u64,
    pub total: u64,
}

/// Asks, and hands each piece of the answer to `on` as it arrives.
pub async fn assistant_ask(
    scope: &str,
    message: &str,
    on: impl FnMut(Streamed),
) -> Result<(), String> {
    let body = serde_json::json!({ "scope": scope, "message": message });
    stream("/api/dev/assistant/turns", &body, on).await
}

/// Installs Ollama if it is missing, then pulls a tag. `on` hears how far along it is.
pub async fn assistant_pull(tag: &str, mut on: impl FnMut(Progress)) -> Result<(), String> {
    let body = serde_json::json!({ "tag": tag });
    stream("/api/dev/assistant/pull", &body, |event| {
        if let Streamed::Delta(line) = event
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
        {
            on(Progress {
                status: value["status"].as_str().unwrap_or_default().to_owned(),
                completed: value["completed"].as_u64().unwrap_or(0),
                total: value["total"].as_u64().unwrap_or(0),
            });
        }
    })
    .await
}

/// Installs and starts Irori's own Ollama, with no model. `on` hears how far along it is.
pub async fn assistant_install(mut on: impl FnMut(Progress)) -> Result<(), String> {
    stream(
        "/api/dev/assistant/install",
        &serde_json::json!({}),
        |event| {
            if let Streamed::Delta(line) = event
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
            {
                on(Progress {
                    status: value["status"].as_str().unwrap_or_default().to_owned(),
                    completed: value["completed"].as_u64().unwrap_or(0),
                    total: value["total"].as_u64().unwrap_or(0),
                });
            }
        },
    )
    .await
}

pub async fn assistant_forget(tag: &str) -> Result<AssistantStatus, String> {
    let response = Request::post("/api/dev/assistant/forget")
        .json(&serde_json::json!({ "tag": tag }))
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

/// Loads a downloaded model into memory, or lets it go.
pub async fn assistant_hold(tag: &str, load: bool) -> Result<AssistantStatus, String> {
    let url = if load {
        "/api/dev/assistant/load"
    } else {
        "/api/dev/assistant/unload"
    };
    let response = Request::post(url)
        .json(&serde_json::json!({ "tag": tag }))
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

pub async fn assistant_uninstall() -> Result<AssistantStatus, String> {
    let response = Request::post("/api/dev/assistant/uninstall")
        .send()
        .await
        .map_err(unreachable)?;
    if !response.ok() {
        return match checked(response).await {
            Err(reason) => Err(reason),
            Ok(()) => Err("the server refused without a reason".into()),
        };
    }
    response.json().await.map_err(unreachable)
}

fn encode_scope(scope: &str) -> String {
    scope.replace(':', "%3A")
}

/// Posts `body` and reads the reply as it arrives, not once it has all arrived: that is what
/// lets the page show an answer being written.
async fn stream(
    url: &str,
    body: &serde_json::Value,
    mut on: impl FnMut(Streamed),
) -> Result<(), String> {
    use web_sys::js_sys::{Reflect, Uint8Array};
    use web_sys::wasm_bindgen::{JsCast as _, JsValue};

    let response = Request::post(url)
        .json(body)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(unreachable)?;
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(format!("Irori refused that ({status})"));
    }
    let Some(body) = response.body() else {
        return Err("Irori answered with nothing.".to_owned());
    };
    let reader: web_sys::ReadableStreamDefaultReader = body.get_reader().unchecked_into();
    let lost = |_| "The answer stopped early.".to_owned();
    let mut events = Events::default();
    loop {
        let chunk = wasm_bindgen_futures::JsFuture::from(reader.read())
            .await
            .map_err(lost)?;
        let done = Reflect::get(&chunk, &JsValue::from_str("done"))
            .ok()
            .and_then(|done| done.as_bool())
            .unwrap_or(true);
        if done {
            return Err("The answer stopped early.".to_owned());
        }
        let Ok(value) = Reflect::get(&chunk, &JsValue::from_str("value")) else {
            continue;
        };
        for event in events.push(&Uint8Array::new(&value).to_vec()) {
            match event {
                Streamed::Failed(why) => return Err(why),
                Streamed::Done => return Ok(()),
                event => on(event),
            }
        }
    }
}

/// The server's events, out of bytes in whatever size they came. The tail of an unfinished
/// line is kept as bytes, so a character cut by a chunk boundary is whole when it is read.
#[derive(Debug, Default)]
struct Events {
    pending: Vec<u8>,
}

impl Events {
    fn push(&mut self, chunk: &[u8]) -> Vec<Streamed> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(at) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=at).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim_end().strip_prefix("data:") else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
                continue;
            };
            if let Some(error) = value["error"].as_str() {
                events.push(Streamed::Failed(error.to_owned()));
            } else if let Some(delta) = value["delta"].as_str() {
                events.push(Streamed::Delta(delta.to_owned()));
            } else if let Some(step) = value["step"].as_str() {
                events.push(Streamed::Step(step.to_owned()));
            } else if value["done"] == true {
                events.push(Streamed::Done);
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{Availability, Context, Origin, State, SwitchState, Timestamp};

    use super::*;

    fn state(at: &str, on: bool) -> EntityState {
        let at: Timestamp = at.parse().expect("a valid timestamp");
        EntityState {
            entity_id: "switch.plug".parse().expect("a valid entity id"),
            availability: Availability::Available,
            state: Some(State::Switch(SwitchState { on })),
            attributes: Default::default(),
            last_changed: at,
            last_updated: at,
            last_reported: at,
            context: Context {
                id: "01K5B2Q9A1B2C3D4E5F6G7H8J9"
                    .parse()
                    .expect("a valid context id"),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    #[test]
    fn a_state_never_goes_backwards_on_the_page() {
        let mut home = Home {
            states: vec![state("2026-09-16T10:00:00Z", false)],
            ..Home::default()
        };

        assert!(home.accept(state("2026-09-16T10:00:01Z", true)), "newer");
        assert!(!home.accept(state("2026-09-16T10:00:00Z", false)), "older");
        assert!(
            !home.accept(state("2026-09-16T10:00:01Z", false)),
            "the same instant is the same change, however it reached the page"
        );
        assert_eq!(
            home.states[0].state,
            Some(State::Switch(SwitchState { on: true }))
        );
    }

    #[test]
    fn events_are_read_whole_however_the_bytes_were_cut() {
        let text = "data: {\"delta\":\"café\"}\n\ndata: {\"step\":\"list_devices\"}\n\n\
                    data: {\"delta\":\" 🙂\"}\n\ndata: {\"done\":true}\n\n"
            .as_bytes();
        for cut in 1..text.len() {
            let mut events = Events::default();
            let mut got = events.push(&text[..cut]);
            got.extend(events.push(&text[cut..]));
            assert_eq!(
                got,
                vec![
                    Streamed::Delta("café".into()),
                    Streamed::Step("list_devices".into()),
                    Streamed::Delta(" 🙂".into()),
                    Streamed::Done,
                ],
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn a_failure_is_an_event_of_its_own() {
        let mut events = Events::default();
        assert_eq!(
            events.push(b"data: {\"error\":\"no model\"}\n"),
            vec![Streamed::Failed("no model".into())]
        );
    }
}
