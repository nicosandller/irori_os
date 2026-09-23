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

/// A device kept out of the home: enough to recognise it and let it in.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HeldDevice {
    pub id: DeviceId,
    pub protocol: String,
    pub name: Name,
    /// `ignored`, or `new` while Irori asks before adding.
    pub why: String,
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
    command: &'static str,
    /// Brightness or color, for `turn_on` on a light that supports them.
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a LightTurnOn>,
}

#[derive(Debug, Deserialize)]
struct Refused {
    error: String,
}

/// Sends a command, and answers with the entity's state once the protocol confirms (or `None`
/// if it vanished meanwhile).
async fn command(
    entity_id: &EntityId,
    command: &'static str,
    data: Option<&LightTurnOn>,
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
    command(entity_id, "turn_on", Some(data)).await
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
    /// `Some(true)` keeps the device out of the home; `Some(false)` lets it back in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
    /// `Some(true)` adds a device that's waiting to be added.
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
    pub installed: bool,
    pub icon: bool,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
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

pub async fn install_extension(id: &str) -> Result<(), String> {
    let response = Request::post(&format!("/api/dev/extensions/{id}/install"))
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
}
