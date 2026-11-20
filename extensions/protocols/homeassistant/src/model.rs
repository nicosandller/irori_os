//! Home Assistant's registries and states, as Irori's devices, entities and values; and Irori's
//! commands, as Home Assistant service calls.
//!
//! Irori's entity kinds already use Home Assistant's names (`docs/specs/entities.md`), so each
//! capability and value is written in that shared vocabulary and checked by Irori's own types:
//! what they refuse is left out (a value) or listed as unmodeled (an entity), never sent.
//!
//! Which entities come over ([`plan`]): by default, those on a Home Assistant device and Home
//! Assistant's own helpers (`input_*`), of a kind Irori models, that aren't disabled or hidden.
//! A device's settings and diagnostics come over too, marked as such, so a device reads as it does
//! on Home Assistant's device page. The settings widen that by integration or area, or narrow it.
//! At most [`MAX_ENTITIES`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use irori_protocol::ServiceError;
use irori_types::{
    Capabilities, Connection, ConnectionKind, DeviceDescription, EntityCategory, EntityDescription,
    EntityKind, Name, Network, ObjectId, SensorClass, Service, State, SuggestedArea,
    SuggestedFloor, SuggestedLayout, UniqueId, Unmodeled, Waiting,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::settings::Settings;

/// The most entities this extension carries (rule 6): well inside the 4,096 the core's report
/// queue holds, and the load `dev/ha-load` proves the core takes without falling behind.
pub const MAX_ENTITIES: usize = 2_000;
/// The most unmodeled entries listed: a big home has hundreds, and the list is for reading.
pub const MAX_UNMODELED: usize = 500;

/// The kinds this extension carries, as in the manifest's `entity_kinds`.
pub const BRIDGED: [&str; 36] = [
    "light",
    "switch",
    "sensor",
    "binary_sensor",
    "button",
    "number",
    "select",
    "cover",
    "lock",
    "fan",
    "event",
    "valve",
    "siren",
    "climate",
    "media_player",
    "camera",
    "device_tracker",
    "update",
    "alarm_control_panel",
    "vacuum",
    "water_heater",
    "humidifier",
    "lawn_mower",
    "scene",
    "text",
    "date",
    "time",
    "datetime",
    "image",
    "notify",
    "remote",
    "weather",
    "calendar",
    "todo",
    "timer",
    "schedule",
];

/// Kinds that stand on their own in Home Assistant, often on no device (a weather service, a
/// shopping list, a timer helper): brought over by default, like helpers.
pub const STANDALONE: [&str; 5] = ["weather", "calendar", "todo", "timer", "schedule"];

/// Home Assistant's helpers, each carried as the Irori kind it is: an `input_boolean` is a switch.
/// An `input_datetime` is a date, a time or both, by what it holds.
pub const HELPERS: [(&str, &str); 6] = [
    ("input_boolean", "switch"),
    ("input_number", "number"),
    ("input_select", "select"),
    ("input_text", "text"),
    ("input_button", "button"),
    ("input_datetime", "datetime"),
];

/// Whether this extension sends commands to entities of Home Assistant's `domain`.
fn carried(domain: &str) -> bool {
    BRIDGED.contains(&domain) || HELPERS.iter().any(|(helper, _)| *helper == domain)
}

/// The `Waiting` item's id when more entities qualify than [`MAX_ENTITIES`].
pub const OVER_LIMIT: &str = "homeassistant-entity-limit";

// --- Home Assistant's registries, as `config/*_registry/list` answers them ------------------

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HaFloor {
    pub floor_id: String,
    pub name: String,
    #[serde(default)]
    pub level: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HaArea {
    pub area_id: String,
    pub name: String,
    #[serde(default)]
    pub floor_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HaDevice {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub name_by_user: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub sw_version: Option<String>,
    #[serde(default)]
    pub hw_version: Option<String>,
    #[serde(default)]
    pub serial_number: Option<String>,
    #[serde(default)]
    pub area_id: Option<String>,
    #[serde(default)]
    pub via_device_id: Option<String>,
    /// `[["mac", "aa:bb:…"], ["zigbee", "00:11:…"]]`.
    #[serde(default)]
    pub connections: Vec<(String, String)>,
    #[serde(default)]
    pub disabled_by: Option<String>,
    /// `[["hue", "001788…"], ["hassio", "core"]]`: which integrations know it, and as what.
    #[serde(default)]
    pub identifiers: Vec<(String, String)>,
    /// `service` for an online service Home Assistant lists as a device (the weather, a calendar).
    #[serde(default)]
    pub entry_type: Option<String>,
}

/// The integrations whose devices are Home Assistant itself rather than anything in the home:
/// Core, the Supervisor and the operating system, its apps (add-ons), its backups, and what HACS
/// installed (cards, integrations and themes, listed as devices).
const HOME_ASSISTANT_ITSELF: [&str; 4] = ["hassio", "backup", "hacs", "marketplace"];

impl HaDevice {
    /// One of Home Assistant's own, as [`HOME_ASSISTANT_ITSELF`] lists them.
    pub fn is_home_assistant_itself(&self) -> bool {
        self.identifiers
            .iter()
            .any(|(integration, _)| HOME_ASSISTANT_ITSELF.contains(&integration.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HaEntity {
    /// The registry entry's own id: stable across renames of `entity_id`.
    pub id: String,
    pub entity_id: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub area_id: Option<String>,
    #[serde(default)]
    pub disabled_by: Option<String>,
    #[serde(default)]
    pub hidden_by: Option<String>,
    #[serde(default)]
    pub entity_category: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub original_name: Option<String>,
    #[serde(default)]
    pub has_entity_name: bool,
}

/// Everything read from Home Assistant in one go.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Registry {
    pub floors: Vec<HaFloor>,
    pub areas: Vec<HaArea>,
    pub devices: Vec<HaDevice>,
    pub entities: Vec<HaEntity>,
    /// Each entity's state and attributes, by `entity_id`.
    pub states: HashMap<String, HaState>,
}

/// One entity's state as Home Assistant keeps it: `s` and `a` in a compact state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HaState {
    pub state: String,
    pub attributes: Map<String, Value>,
}

/// A list from a registry, skipping entries this extension can't read rather than all of them.
pub fn list<T: serde::de::DeserializeOwned>(value: &Value) -> Vec<T> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match serde_json::from_value(item.clone()) {
                    Ok(item) => Some(item),
                    Err(why) => {
                        tracing::debug!(%why, "skipping a registry entry");
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `get_states`' answer, by `entity_id`.
pub fn states(value: &Value) -> HashMap<String, HaState> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some((
                item["entity_id"].as_str()?.to_owned(),
                HaState {
                    state: item["state"].as_str()?.to_owned(),
                    attributes: item["attributes"].as_object().cloned().unwrap_or_default(),
                },
            ))
        })
        .collect()
}

// --- What comes over ------------------------------------------------------------------------

/// One bridged entity: what Irori is told, and how to reach it in Home Assistant.
#[derive(Debug, Clone, PartialEq)]
pub struct Bridged {
    pub description: EntityDescription,
    pub entity_id: String,
}

impl Bridged {
    pub fn domain(&self) -> &str {
        domain(&self.entity_id)
    }
}

/// What this extension tells Irori about a Home Assistant.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// Each one before any device it reaches through (`via_device_unique_id`).
    pub devices: Vec<DeviceDescription>,
    /// Sorted by `unique_id`.
    pub entities: Vec<Bridged>,
    pub unmodeled: Vec<Unmodeled>,
    pub waiting: Vec<Waiting>,
    pub layout: SuggestedLayout,
    /// Entities kept from before although Home Assistant has no state for them right now (their
    /// integration is reloading): unavailable until it does.
    pub absent: Vec<UniqueId>,
    /// The map of the devices that come over.
    pub network: Network,
    /// Entities brought over in an earlier run (`before`) that Home Assistant still lists but
    /// has no state for yet, and their devices: not brought over now, but not gone either.
    pub unloaded_entities: BTreeSet<UniqueId>,
    pub unloaded_devices: BTreeSet<UniqueId>,
}

pub fn domain(entity_id: &str) -> &str {
    entity_id
        .split_once('.')
        .map_or(entity_id, |(domain, _)| domain)
}

/// A Home Assistant name as an Irori name: trimmed, at most 100 characters. `None` when nothing
/// is left, or Irori refuses it anyway.
pub fn name(text: &str) -> Option<Name> {
    let trimmed = text.trim();
    let cut: String = trimmed.chars().take(100).collect();
    Name::try_from(cut.trim_end()).ok()
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Works out what comes over.
pub fn plan(registry: &Registry, settings: &Settings) -> Plan {
    plan_keeping(registry, settings, &HashMap::new(), &BTreeSet::new())
}

/// [`plan`], keeping the entities in `known` (by `entity_id`, as last brought over) that Home
/// Assistant still lists but has no state for right now. An integration reloading drops its
/// states for a moment; read then, they'd seem gone, and be removed from the home with the room
/// and name they were given there.
///
/// `before` holds the `unique_id`s of the entities brought over before, in this run or an
/// earlier one. Past [`MAX_ENTITIES`] they're taken first, so an entity added in Home Assistant
/// never pushes one already in the home out of it.
pub fn plan_keeping(
    registry: &Registry,
    settings: &Settings,
    known: &HashMap<String, Bridged>,
    before: &BTreeSet<String>,
) -> Plan {
    let areas: HashMap<&str, &HaArea> = registry
        .areas
        .iter()
        .map(|area| (area.area_id.as_str(), area))
        .collect();
    let devices: HashMap<&str, &HaDevice> = registry
        .devices
        .iter()
        .map(|device| (device.id.as_str(), device))
        .collect();
    let area_name = |id: Option<&String>| id.and_then(|id| areas.get(id.as_str())).map(|a| &a.name);
    let listed = |list: &[String], value: &str| list.iter().any(|item| same(item, value));

    let mut taken: Vec<(&HaEntity, Option<&HaDevice>, Capabilities)> = Vec::new();
    let mut unmodeled: Vec<(Option<&HaDevice>, Unmodeled)> = Vec::new();
    let mut unloaded_entities = BTreeSet::new();
    let mut unloaded_devices = BTreeSet::new();
    for entity in &registry.entities {
        if entity.disabled_by.is_some() {
            continue;
        }
        let device = entity
            .device_id
            .as_deref()
            .and_then(|id| devices.get(id).copied())
            .filter(|device| device.disabled_by.is_none());
        if entity.device_id.is_some() && device.is_none() {
            // Its device is disabled, or gone.
            continue;
        }
        let area = area_name(
            entity
                .area_id
                .as_ref()
                .or(device.and_then(|d| d.area_id.as_ref())),
        );
        let in_area = |list: &[String]| area.is_some_and(|area| listed(list, area));
        if listed(&settings.exclude_integrations, &entity.platform)
            || in_area(&settings.exclude_areas)
        {
            continue;
        }
        let included = listed(&settings.include_integrations, &entity.platform)
            || in_area(&settings.include_areas);
        // Home Assistant's own parts come over only when asked for by name.
        if device.is_some_and(HaDevice::is_home_assistant_itself)
            && !listed(&settings.include_integrations, &entity.platform)
        {
            continue;
        }
        let helper = HELPERS
            .iter()
            .any(|(helper, _)| *helper == domain(&entity.entity_id))
            || STANDALONE.contains(&domain(&entity.entity_id));
        let by_default = (device.is_some() || helper) && entity.hidden_by.is_none();
        if !(by_default || included) {
            continue;
        }
        let Some(state) = registry.states.get(&entity.entity_id) else {
            // Not loaded in Home Assistant right now: nothing to tell its kind or abilities by,
            // unless it was brought over before.
            if let Some(earlier) = known.get(&entity.entity_id) {
                taken.push((entity, device, earlier.description.capabilities.clone()));
            } else if before.contains(&entity.id)
                && let Ok(unique_id) = UniqueId::try_from(entity.id.as_str())
            {
                // Brought over in an earlier run: it waits in the home for its state.
                unloaded_entities.insert(unique_id);
                unloaded_devices
                    .extend(device.and_then(|d| UniqueId::try_from(d.id.as_str()).ok()));
            }
            continue;
        };
        let domain = domain(&entity.entity_id);
        match capabilities(domain, &state.attributes) {
            Ok(capabilities) => taken.push((entity, device, capabilities)),
            Err(reason) => {
                let Ok(platform) = ObjectId::try_from(domain) else {
                    continue;
                };
                unmodeled.push((
                    device,
                    Unmodeled {
                        device_unique_id: None,
                        platform,
                        name: entity_label(entity, state),
                        reason,
                    },
                ));
            }
        }
    }

    // The same ones every time, whatever order Home Assistant lists them in: those already in
    // the home first, so a new one waits rather than taking an old one's place.
    taken.sort_by(|a, b| {
        let new = |id: &String| !before.contains(id);
        (new(&a.0.id), &a.0.id).cmp(&(new(&b.0.id), &b.0.id))
    });
    let over = taken.len().saturating_sub(MAX_ENTITIES);
    taken.truncate(MAX_ENTITIES);
    taken.sort_by(|a, b| a.0.id.cmp(&b.0.id));

    // Devices with something taken, and the devices they're reached through, parents first.
    let mut wanted: BTreeSet<&str> = taken
        .iter()
        .filter_map(|(_, device, _)| device.map(|d| d.id.as_str()))
        .collect();
    let mut described: Vec<DeviceDescription> = Vec::new();
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut parents: Vec<&str> = Vec::new();
    for id in &wanted {
        // A chain of `via_device_id`s, nearest first; at most a few long.
        let mut at = devices.get(id).and_then(|d| d.via_device_id.as_deref());
        while let Some(parent) = at {
            if parents.contains(&parent) || wanted.contains(parent) || parents.len() > 64 {
                break;
            }
            parents.push(parent);
            at = devices.get(parent).and_then(|d| d.via_device_id.as_deref());
        }
    }
    wanted.extend(
        parents
            .iter()
            .copied()
            .filter(|id| devices.contains_key(id)),
    );
    fn visit<'a>(
        id: &'a str,
        devices: &HashMap<&'a str, &'a HaDevice>,
        wanted: &BTreeSet<&'a str>,
        done: &mut BTreeSet<&'a str>,
        out: &mut Vec<&'a HaDevice>,
        depth: usize,
    ) {
        if done.contains(id) || depth > 64 {
            return;
        }
        let Some(device) = devices.get(id) else {
            return;
        };
        if let Some(parent) = device.via_device_id.as_deref()
            && wanted.contains(parent)
        {
            visit(parent, devices, wanted, done, out, depth + 1);
        }
        if done.insert(id) {
            out.push(device);
        }
    }
    let mut ordered = Vec::new();
    for id in &wanted {
        visit(id, &devices, &wanted, &mut done, &mut ordered, 0);
    }
    let mut device_ids: BTreeSet<&str> = BTreeSet::new();
    let mut drawn: Vec<&HaDevice> = Vec::new();
    for device in ordered {
        if let Some(description) = describe_device(device, &areas, &device_ids) {
            device_ids.insert(device.id.as_str());
            described.push(description);
            drawn.push(device);
        }
    }
    let network = crate::network::draw(&drawn);

    let entities: Vec<Bridged> = taken
        .into_iter()
        .filter_map(|(entity, device, capabilities)| {
            let device = device.filter(|d| device_ids.contains(d.id.as_str()));
            let mut bridged = describe_entity(entity, device, capabilities, &registry.states)?;
            // Without a state there's no `friendly_name`: it keeps the name it had.
            if !registry.states.contains_key(&entity.entity_id)
                && let Some(before) = known.get(&entity.entity_id)
            {
                bridged.description.name = before.description.name.clone();
            }
            Some(bridged)
        })
        .collect();

    let unmodeled = unmodeled
        .into_iter()
        .map(|(device, mut item)| {
            item.device_unique_id = device
                .filter(|d| device_ids.contains(d.id.as_str()))
                .and_then(|d| UniqueId::try_from(d.id.as_str()).ok());
            item
        })
        .take(MAX_UNMODELED)
        .collect();

    let mut waiting = Vec::new();
    if over > 0
        && let (Ok(unique_id), Some(home)) =
            (UniqueId::try_from(OVER_LIMIT), name("Home Assistant"))
    {
        waiting.push(Waiting {
            unique_id,
            name: home,
            reason: format!(
                "{over} more entities qualify than the {MAX_ENTITIES} this extension brings over. Leave some integrations or areas out in its settings."
            ),
            secret: None,
        });
    }

    let absent = entities
        .iter()
        .filter(|b| !registry.states.contains_key(&b.entity_id))
        .map(|b| b.description.unique_id.clone())
        .collect();
    Plan {
        network,
        devices: described,
        entities,
        unmodeled,
        waiting,
        layout: layout(&registry.floors, &registry.areas),
        absent,
        unloaded_entities,
        unloaded_devices,
    }
}

fn entity_label(entity: &HaEntity, state: &HaState) -> Option<Name> {
    entity
        .name
        .as_deref()
        .or(state
            .attributes
            .get("friendly_name")
            .and_then(Value::as_str))
        .or(entity.original_name.as_deref())
        .and_then(name)
}

fn describe_device(
    device: &HaDevice,
    areas: &HashMap<&str, &HaArea>,
    described: &BTreeSet<&str>,
) -> Option<DeviceDescription> {
    let unique_id = UniqueId::try_from(device.id.as_str()).ok()?;
    let label = device
        .name_by_user
        .as_deref()
        .and_then(name)
        .or_else(|| device.name.as_deref().and_then(name))
        .or_else(|| device.model.as_deref().and_then(name))
        .or_else(|| name("Home Assistant device"))?;
    let tidy = |text: &Option<String>| {
        text.as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let serial = tidy(&device.serial_number);
    let mut connections: Vec<Connection> = device
        .connections
        .iter()
        .filter_map(|(kind, value)| {
            let kind = match kind.as_str() {
                "mac" | "bluetooth" => ConnectionKind::Mac,
                "zigbee" => ConnectionKind::Ieee,
                _ => return None,
            };
            Connection::new(kind, value).ok()
        })
        .collect();
    if let Some(serial) = &serial
        && let Ok(connection) = Connection::new(ConnectionKind::Serial, serial)
    {
        connections.push(connection);
    }
    connections.dedup();
    let description = DeviceDescription {
        unique_id,
        name: label,
        manufacturer: tidy(&device.manufacturer),
        model: tidy(&device.model),
        sw_version: tidy(&device.sw_version),
        hw_version: tidy(&device.hw_version),
        serial_number: serial,
        suggested_area: device
            .area_id
            .as_deref()
            .and_then(|id| areas.get(id))
            .and_then(|area| name(&area.name)),
        via_device_unique_id: device
            .via_device_id
            .as_deref()
            .filter(|parent| described.contains(parent) && *parent != device.id)
            .and_then(|parent| UniqueId::try_from(parent).ok()),
        connections,
    };
    description.validate().ok()?;
    Some(description)
}

fn describe_entity(
    entity: &HaEntity,
    device: Option<&HaDevice>,
    capabilities: Capabilities,
    states: &HashMap<String, HaState>,
) -> Option<Bridged> {
    let unique_id = UniqueId::try_from(entity.id.as_str()).ok()?;
    let own = entity.name.as_deref().or(entity.original_name.as_deref());
    let label = match device {
        // A device's main feature has no name of its own, and goes by its device's.
        Some(_) if entity.has_entity_name && own.is_none() => None,
        Some(_) => own.and_then(name),
        None => own
            .or(states
                .get(&entity.entity_id)
                .and_then(|state| state.attributes.get("friendly_name"))
                .and_then(Value::as_str))
            .and_then(name)
            .or_else(|| name(&entity.entity_id)),
    };
    let device_unique_id = device.and_then(|d| UniqueId::try_from(d.id.as_str()).ok());
    if label.is_none() && device_unique_id.is_none() {
        return None;
    }
    let description = EntityDescription {
        unique_id,
        name: label,
        device_unique_id,
        suggested_object_id: entity
            .entity_id
            .split_once('.')
            .and_then(|(_, object)| ObjectId::try_from(object).ok()),
        capabilities,
        entity_category: match entity.entity_category.as_deref() {
            Some("config") => Some(EntityCategory::Config),
            Some("diagnostic") => Some(EntityCategory::Diagnostic),
            _ => None,
        },
    };
    description.validate().ok()?;
    Some(Bridged {
        description,
        entity_id: entity.entity_id.clone(),
    })
}

/// Home Assistant's floors and areas, by name, for the owner to bring over in one step.
pub fn layout(floors: &[HaFloor], areas: &[HaArea]) -> SuggestedLayout {
    let floor_names: HashMap<&str, Name> = floors
        .iter()
        .filter_map(|floor| Some((floor.floor_id.as_str(), name(&floor.name)?)))
        .collect();
    SuggestedLayout {
        floors: floors
            .iter()
            .filter_map(|floor| {
                Some(SuggestedFloor {
                    name: floor_names.get(floor.floor_id.as_str())?.clone(),
                    level: floor
                        .level
                        .map(|level| level.clamp(i64::from(i8::MIN), i64::from(i8::MAX)))
                        .and_then(|level| i8::try_from(level).ok()),
                })
            })
            .take(irori_types::MAX_LAYOUT_FLOORS)
            .collect(),
        areas: areas
            .iter()
            .filter_map(|area| {
                Some(SuggestedArea {
                    name: name(&area.name)?,
                    floor: area
                        .floor_id
                        .as_deref()
                        .and_then(|id| floor_names.get(id))
                        .cloned(),
                })
            })
            .take(irori_types::MAX_LAYOUT_AREAS)
            .collect(),
    }
}

// --- Capabilities ---------------------------------------------------------------------------

fn text_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        // A siren's tones can be a map of id to name.
        Some(Value::Object(items)) => items.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

fn features(attributes: &Map<String, Value>) -> u64 {
    attributes
        .get("supported_features")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn number(attributes: &Map<String, Value>, key: &str) -> Option<f64> {
    attributes
        .get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
}

/// A text attribute Irori keeps short: 1 to 255 characters, cut at a character's edge.
fn short<'a>(attributes: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    let text = attributes.get(key)?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(255) {
        Some((at, _)) => &text[..at],
        None => text,
    })
}

fn sensor_class(attributes: &Map<String, Value>) -> Option<SensorClass> {
    attributes
        .get("device_class")
        .and_then(Value::as_str)
        .and_then(SensorClass::from_ha)
}

fn device_class(attributes: &Map<String, Value>) -> Value {
    attributes
        .get("device_class")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Why something isn't brought over: `Some` for a kind Irori has that this extension doesn't
/// carry (yet), or one it couldn't read; `None` for a kind Irori doesn't have.
type NotBridged = Option<String>;

/// What an entity of `domain` can do, from its attributes, as Irori's types take it.
pub fn capabilities(domain: &str, a: &Map<String, Value>) -> Result<Capabilities, NotBridged> {
    let caps = match domain {
        "light" => {
            let modes = text_list(a.get("supported_color_modes"));
            let has = |mode: &str| modes.iter().any(|m| m == mode);
            let rgb = ["hs", "xy", "rgb", "rgbw", "rgbww"].iter().any(|m| has(m));
            let temp = has("color_temp").then(|| {
                let kelvin = |key: &str, fallback: f64| {
                    number(a, key).unwrap_or(fallback).clamp(1000.0, 20000.0).round()
                };
                json!({ "min": kelvin("min_color_temp_kelvin", 2000.0), "max": kelvin("max_color_temp_kelvin", 6500.0) })
            });
            let dims = modes.iter().any(|m| m != "onoff" && m != "unknown");
            json!({ "kind": "light", "brightness": dims, "rgb": rgb, "color_temp_kelvin": temp })
        }
        "switch" => json!({ "kind": "switch", "device_class": device_class(a) }),
        "binary_sensor" => json!({
            "kind": "binary_sensor",
            "device_class": a.get("device_class").and_then(Value::as_str)
                .and_then(irori_types::BinarySensorClass::from_ha),
        }),
        "sensor" => {
            let class = sensor_class(a);
            let options = text_list(a.get("options"));
            let unit = a.get("unit_of_measurement").and_then(Value::as_str);
            let state_class = a.get("state_class").and_then(Value::as_str);
            let enumerated = a.get("device_class").and_then(Value::as_str) == Some("enum");
            let text = enumerated || class.is_some_and(SensorClass::reports_text);
            let mut caps = json!({
                "kind": "sensor",
                "value_type": if text { "text" } else { "number" },
                "device_class": class,
                "unit": unit,
                "state_class": state_class,
            });
            if text && !options.is_empty() {
                caps["options"] = json!(options);
            }
            if !text && unit.is_none() && state_class.is_none() && class.is_none() {
                // Nothing says it's a measurement: a sensor reporting words ("charging") with no
                // class. Text, so its values aren't refused.
                caps["value_type"] = json!("text");
            }
            caps
        }
        "button" => json!({ "kind": "button", "device_class": device_class(a) }),
        "number" => json!({
            "kind": "number",
            "min": number(a, "min").unwrap_or(0.0),
            "max": number(a, "max").unwrap_or(100.0),
            "step": number(a, "step").unwrap_or(1.0),
            "unit": a.get("unit_of_measurement"),
            "device_class": sensor_class(a),
            "mode": a.get("mode").and_then(Value::as_str).unwrap_or("auto"),
        }),
        "select" => json!({ "kind": "select", "options": text_list(a.get("options")) }),
        "cover" => {
            let f = features(a);
            json!({
                "kind": "cover",
                "device_class": device_class(a),
                "position": f & 4 != 0,
                "stop": f & 8 != 0,
                "tilt": f & 128 != 0,
            })
        }
        "valve" => {
            let f = features(a);
            json!({
                "kind": "valve",
                "device_class": device_class(a),
                "position": f & 4 != 0,
                "stop": f & 8 != 0,
            })
        }
        "lock" => {
            let code = a.get("code_format").and_then(Value::as_str);
            json!({
                "kind": "lock",
                "open": features(a) & 1 != 0,
                "requires_code": code.is_some(),
                "code_format": code,
            })
        }
        "fan" => {
            let f = features(a);
            let speeds = if f & 1 != 0 {
                a.get("percentage_step")
                    .and_then(Value::as_f64)
                    .filter(|step| *step > 0.0)
                    .map_or(100, |step| (100.0 / step).round() as u64)
            } else {
                0
            };
            json!({
                "kind": "fan",
                "speed_count": speeds.min(100),
                "oscillate": f & 2 != 0,
                "direction": f & 4 != 0,
                "preset_modes": if f & 8 != 0 { text_list(a.get("preset_modes")) } else { Vec::new() },
            })
        }
        "event" => json!({
            "kind": "event",
            "event_types": text_list(a.get("event_types")),
            "device_class": device_class(a),
        }),
        "siren" => {
            let f = features(a);
            json!({
                "kind": "siren",
                "tones": if f & 4 != 0 { text_list(a.get("available_tones")) } else { Vec::new() },
                "volume": f & 8 != 0,
                "duration": f & 16 != 0,
            })
        }
        "climate" => {
            let f = features(a);
            let humidity = (f & 4 != 0).then(|| {
                json!({
                    "min": number(a, "min_humidity").unwrap_or(30.0),
                    "max": number(a, "max_humidity").unwrap_or(99.0),
                })
            });
            json!({
                "kind": "climate",
                "hvac_modes": text_list(a.get("hvac_modes")),
                "min_temp": number(a, "min_temp").unwrap_or(7.0),
                "max_temp": number(a, "max_temp").unwrap_or(35.0),
                "temp_step": number(a, "target_temp_step").unwrap_or(0.5),
                "target_temperature": f & 1 != 0,
                "target_temperature_range": f & 2 != 0,
                "target_humidity": humidity,
                "fan_modes": if f & 8 != 0 { text_list(a.get("fan_modes")) } else { Vec::new() },
                "preset_modes": if f & 16 != 0 { text_list(a.get("preset_modes")) } else { Vec::new() },
                "swing_modes": if f & 32 != 0 { text_list(a.get("swing_modes")) } else { Vec::new() },
            })
        }
        "media_player" => {
            let f = features(a);
            json!({
                "kind": "media_player",
                "device_class": a.get("device_class").and_then(Value::as_str)
                    .and_then(irori_types::MediaPlayerClass::from_ha),
                "volume": f & 4 != 0,
                "mute": f & 8 != 0,
                "seek": f & 2 != 0,
                "play_media": f & 512 != 0,
                "queue": f & (16 | 32) != 0,
                "turn_on": f & 128 != 0,
                "turn_off": f & 256 != 0,
            })
        }
        "camera" => {
            // A camera Home Assistant streams is watched through that stream, at its one size.
            let live = features(a) & 2 != 0;
            json!({
                "kind": "camera",
                "on_off": features(a) & 1 != 0,
                "motion_detection": a.get("motion_detection").is_some_and(Value::is_boolean),
                "live": live,
                "qualities": if live { vec!["high"] } else { Vec::new() },
            })
        }
        "device_tracker" => json!({
            "kind": "device_tracker",
            "source_type": a.get("source_type").and_then(Value::as_str)
                .filter(|s| ["gps", "router", "bluetooth", "bluetooth_le"].contains(s)),
        }),
        "update" => json!({
            "kind": "update",
            "device_class": a.get("device_class").and_then(Value::as_str)
                .filter(|class| *class == "firmware"),
            "install": features(a) & 1 != 0,
            // Checking again is `homeassistant.update_entity`: not the update's own service.
            "check": false,
        }),
        "alarm_control_panel" => {
            let f = features(a);
            let modes: Vec<&str> = [
                (1, "home"),
                (2, "away"),
                (4, "night"),
                (16, "custom_bypass"),
                (32, "vacation"),
            ]
            .into_iter()
            .filter(|(bit, _)| f & bit != 0)
            .map(|(_, mode)| mode)
            .collect();
            let code = a.get("code_format").and_then(Value::as_str);
            json!({
                "kind": "alarm_control_panel",
                "arm_modes": modes,
                "trigger": f & 8 != 0,
                "code_format": code,
                "code_arm_required": code.is_some()
                    && a.get("code_arm_required").and_then(Value::as_bool).unwrap_or(true),
                // Home Assistant asks for the code to disarm whenever there is one.
                "code_disarm_required": code.is_some(),
            })
        }
        "vacuum" => {
            let f = features(a);
            json!({
                "kind": "vacuum",
                "start": f & 8192 != 0,
                "pause": f & 4 != 0,
                "stop": f & 8 != 0,
                "return_home": f & 16 != 0,
                "locate": f & 512 != 0,
                "clean_spot": f & 1024 != 0,
                "fan_speeds": if f & 32 != 0 { text_list(a.get("fan_speed_list")) } else { Vec::new() },
            })
        }
        "water_heater" => {
            let f = features(a);
            let listed = text_list(a.get("operation_list"));
            let modes: Vec<&str> = listed
                .iter()
                .filter_map(|mode| irori_types::WaterHeaterMode::parse(mode).map(|m| m.as_str()))
                .collect();
            if modes.is_empty() {
                return Err(Some(if listed.is_empty() {
                    "it doesn't say which modes it has".to_owned()
                } else {
                    format!(
                        "its modes ({}) aren't ones Irori's water heaters have",
                        listed.join(", ")
                    )
                }));
            }
            json!({
                "kind": "water_heater",
                "operation_modes": modes,
                "min_temp": number(a, "min_temp").unwrap_or(30.0),
                "max_temp": number(a, "max_temp").unwrap_or(60.0),
                "temp_step": number(a, "target_temp_step").unwrap_or(0.5),
                "target_temperature": f & 1 != 0,
                "on_off": f & 8 != 0,
            })
        }
        "humidifier" => json!({
            "kind": "humidifier",
            "device_class": a.get("device_class").and_then(Value::as_str)
                .and_then(irori_types::HumidifierClass::from_ha),
            "humidity": {
                "min": number(a, "min_humidity").unwrap_or(0.0),
                "max": number(a, "max_humidity").unwrap_or(100.0),
            },
            "modes": if features(a) & 1 != 0 { text_list(a.get("available_modes")) } else { Vec::new() },
        }),
        "lawn_mower" => {
            let f = features(a);
            json!({
                "kind": "lawn_mower",
                "start_mowing": f & 1 != 0,
                "pause": f & 2 != 0,
                "dock": f & 4 != 0,
            })
        }
        "scene" => json!({ "kind": "scene" }),
        "text" | "input_text" => json!({
            "kind": "text",
            "min_length": a.get("min").and_then(Value::as_u64).unwrap_or(0),
            "max_length": a.get("max").and_then(Value::as_u64).unwrap_or(255),
            "pattern": a.get("pattern").and_then(Value::as_str).filter(|p| !p.is_empty()),
            "mode": a.get("mode").and_then(Value::as_str).filter(|m| *m == "password"),
        }),
        "date" | "time" | "datetime" => json!({ "kind": domain }),
        "input_datetime" => {
            let has = |key: &str| a.get(key).and_then(Value::as_bool).unwrap_or(false);
            match (has("has_date"), has("has_time")) {
                (true, true) => json!({ "kind": "datetime" }),
                (true, false) => json!({ "kind": "date" }),
                (false, true) => json!({ "kind": "time" }),
                (false, false) => {
                    return Err(Some("it holds neither a date nor a time".to_owned()));
                }
            }
        }
        "image" => json!({ "kind": "image" }),
        "notify" => json!({ "kind": "notify", "title": features(a) & 1 != 0 }),
        "remote" => json!({
            "kind": "remote",
            // ACTIVITY is 4.
            "activities": if features(a) & 4 != 0 { text_list(a.get("activity_list")) } else { Vec::new() },
        }),
        "weather" => json!({ "kind": "weather" }),
        "calendar" => json!({ "kind": "calendar" }),
        // CREATE_TODO_ITEM is 1.
        "todo" => json!({ "kind": "todo", "add": features(a) & 1 != 0 }),
        "timer" => json!({ "kind": "timer" }),
        "schedule" => json!({ "kind": "schedule" }),
        "input_boolean" => json!({ "kind": "switch" }),
        "input_number" => return capabilities("number", a),
        "input_select" => return capabilities("select", a),
        "input_button" => json!({ "kind": "button" }),
        other => {
            return Err(irori_has(other).then(|| {
                "Irori has this kind of entity, but the Home Assistant extension doesn't bring it over yet".to_owned()
            }));
        }
    };
    let mut caps = caps;
    if let Value::Object(fields) = &mut caps {
        fields.retain(|_, value| !value.is_null());
    }
    match serde_json::from_value::<Capabilities>(caps.clone()) {
        Ok(caps) => Ok(caps),
        Err(first) => {
            // A class Irori doesn't know, or one its rules don't allow with this unit; or a state
            // class newer than Irori's (`measurement_angle`): the entity is still useful without.
            for field in ["device_class", "state_class"] {
                if let Value::Object(fields) = &mut caps {
                    fields.remove(field);
                }
                if let Ok(caps) = serde_json::from_value(caps.clone()) {
                    return Ok(caps);
                }
            }
            Err(Some(format!("Irori couldn't read what it can do: {first}")))
        }
    }
}

/// Whether Irori has a kind by this name.
fn irori_has(domain: &str) -> bool {
    serde_json::from_value::<EntityKind>(json!(domain)).is_ok()
}

// --- Values ---------------------------------------------------------------------------------

/// What one of Home Assistant's states says, for Irori.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// The device isn't reachable: availability, not a value.
    Unavailable,
    /// Reachable, with this value (`None`: Home Assistant doesn't know it).
    Value(Option<State>),
    /// Nothing to report: a button, which has no value.
    Nothing,
}

/// Seconds in Home Assistant's `H:MM:SS` (a timer's `duration`, `remaining`), which may start
/// with days: `1 day, 2:00:00`.
fn duration_seconds(text: &str) -> Option<f64> {
    let (days, clock) = match text.split_once(", ") {
        Some((days, clock)) => (days.split_whitespace().next()?.parse::<f64>().ok()?, clock),
        None => (0.0, text),
    };
    let mut parts = clock.split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let (h, m, s): (f64, f64, f64) = (h.parse().ok()?, m.parse().ok()?, s.parse().ok()?);
    Some(days * 86_400.0 + h * 3600.0 + m * 60.0 + s)
}

/// A weather entity's state in Irori's units: Home Assistant says which units it uses.
fn weather(s: &str, a: &Map<String, Value>) -> Value {
    let unit = |key: &str| a.get(key).and_then(Value::as_str).unwrap_or("");
    let temperature = |key: &str| {
        number(a, key).and_then(|t| match unit("temperature_unit") {
            "°C" | "" => Some(t),
            "°F" => Some((t - 32.0) * 5.0 / 9.0),
            "K" => Some(t - 273.15),
            _ => None,
        })
    };
    let pressure = number(a, "pressure").and_then(|p| {
        Some(match unit("pressure_unit") {
            "hPa" | "mbar" | "" => p,
            "kPa" | "cbar" => p * 10.0,
            "Pa" => p / 100.0,
            "inHg" => p * 33.863_886,
            "mmHg" => p * 1.333_224,
            "psi" => p * 68.947_573,
            _ => return None,
        })
    });
    let speed = |key: &str| {
        number(a, key).and_then(|v| {
            Some(match unit("wind_speed_unit") {
                "km/h" | "" => v,
                "m/s" => v * 3.6,
                "mph" => v * 1.609_344,
                "kn" => v * 1.852,
                "ft/s" => v * 1.097_28,
                _ => return None,
            })
        })
    };
    let visibility = number(a, "visibility").and_then(|v| {
        Some(match unit("visibility_unit") {
            "km" | "" => v,
            "mi" => v * 1.609_344,
            "m" => v / 1000.0,
            _ => return None,
        })
    });
    let round = |v: Option<f64>| v.map(|v| (v * 10.0).round() / 10.0);
    json!({
        "kind": "weather",
        "condition": s,
        "temperature": round(temperature("temperature")),
        "apparent_temperature": round(temperature("apparent_temperature")),
        "dew_point": round(temperature("dew_point")),
        "humidity": number(a, "humidity"),
        "pressure": round(pressure),
        "wind_speed": round(speed("wind_speed")),
        "wind_gust_speed": round(speed("wind_gust_speed")),
        // Some services say it as a compass point (`NW`), which isn't a number.
        "wind_bearing": number(a, "wind_bearing"),
        "cloud_coverage": number(a, "cloud_coverage"),
        "uv_index": number(a, "uv_index"),
        "visibility": round(visibility),
    })
}

/// The event a calendar's state is about, in the home's own time: Home Assistant writes its
/// start and end as `2026-11-20 09:00:00`, local, and an all-day event's from midnight.
fn calendar_event(a: &Map<String, Value>) -> Value {
    let Some(summary) = short(a, "message") else {
        return Value::Null;
    };
    let all_day = a.get("all_day").and_then(Value::as_bool).unwrap_or(false);
    let day = |key: &str| a.get(key).and_then(Value::as_str).and_then(|t| t.get(..10));
    let time = |key: &str| {
        a.get(key)
            .and_then(Value::as_str)
            .and_then(|t| t.get(11..19))
            .filter(|_| !all_day)
    };
    let detail = |key: &str| {
        a.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| t.chars().take(4096).collect::<String>())
    };
    match (day("start_time"), day("end_time")) {
        (Some(start), Some(end)) => json!({
            "summary": summary,
            "start": start,
            "start_time": time("start_time"),
            "end": end,
            "end_time": time("end_time"),
            "location": detail("location"),
            "description": detail("description"),
        }),
        _ => Value::Null,
    }
}

/// Home Assistant's state `s` and attributes `a`, for an entity with `caps`.
pub fn reading(caps: &Capabilities, s: &str, a: &Map<String, Value>) -> Reading {
    if s == "unavailable" {
        return Reading::Unavailable;
    }
    if matches!(
        caps,
        Capabilities::Button(_)
            | Capabilities::Scene(_)
            | Capabilities::Image(_)
            | Capabilities::Notify(_)
    ) {
        return Reading::Nothing;
    }
    if s == "unknown" {
        return Reading::Value(None);
    }
    let int = |key: &str, max: u64| a.get(key).and_then(Value::as_u64).filter(|n| *n <= max);
    let text = |key: &str| a.get(key).and_then(Value::as_str);
    let value = match caps {
        Capabilities::Light(light) => {
            let on = s == "on";
            let mode = match text("color_mode") {
                Some("color_temp") if light.rgb => Some("color_temp"),
                Some("hs" | "xy" | "rgb" | "rgbw" | "rgbww")
                    if light.color_temp_kelvin.is_some() =>
                {
                    Some("rgb")
                }
                _ => None,
            };
            json!({
                "kind": "light",
                "on": on,
                "brightness": int("brightness", 255).filter(|b| *b >= 1 && light.brightness),
                "color_mode": mode,
                "color_temp_kelvin": int("color_temp_kelvin", 20000)
                    .filter(|k| *k >= 1000 && light.color_temp_kelvin.is_some()),
                "rgb": a.get("rgb_color").filter(|_| light.rgb),
            })
        }
        Capabilities::Switch(_) => json!({ "kind": "switch", "on": s == "on" }),
        Capabilities::BinarySensor(_) => json!({ "kind": "binary_sensor", "on": s == "on" }),
        Capabilities::Siren(_) => json!({ "kind": "siren", "on": s == "on" }),
        Capabilities::Sensor(sensor) => match sensor.value_type {
            irori_types::SensorValueType::Number => match s.parse::<f64>() {
                Ok(n) if n.is_finite() => json!({ "kind": "sensor", "value": n }),
                _ => return Reading::Value(None),
            },
            irori_types::SensorValueType::Text => {
                if !sensor.options.is_empty() && !sensor.options.iter().any(|o| o == s) {
                    return Reading::Value(None);
                }
                json!({ "kind": "sensor", "value": s })
            }
        },
        Capabilities::Number(_) => match s.parse::<f64>() {
            Ok(n) if n.is_finite() => json!({ "kind": "number", "value": n }),
            _ => return Reading::Value(None),
        },
        Capabilities::Select(select) => {
            if !select.options.iter().any(|o| o == s) {
                return Reading::Value(None);
            }
            json!({ "kind": "select", "option": s })
        }
        Capabilities::Cover(_) => json!({
            "kind": "cover",
            "state": s,
            "position": int("current_position", 100),
            "tilt": int("current_tilt_position", 100),
        }),
        Capabilities::Valve(_) => json!({
            "kind": "valve",
            "state": s,
            "position": int("current_position", 100),
        }),
        Capabilities::Lock(_) => json!({ "kind": "lock", "state": s }),
        Capabilities::Fan(fan) => json!({
            "kind": "fan",
            "on": s == "on",
            "percentage": int("percentage", 100).filter(|_| fan.speed_count > 0),
            "oscillating": a.get("oscillating").filter(|_| fan.oscillate),
            "direction": a.get("direction").filter(|_| fan.direction),
            "preset_mode": a.get("preset_mode"),
        }),
        Capabilities::Event(_) => match text("event_type") {
            Some(event_type) => json!({ "kind": "event", "event_type": event_type }),
            None => return Reading::Value(None),
        },
        Capabilities::Climate(_) => json!({
            "kind": "climate",
            "hvac_mode": s,
            "hvac_action": a.get("hvac_action"),
            "current_temperature": number(a, "current_temperature"),
            "target_temperature": number(a, "temperature"),
            "target_temp_low": number(a, "target_temp_low"),
            "target_temp_high": number(a, "target_temp_high"),
            "current_humidity": number(a, "current_humidity"),
            "target_humidity": number(a, "humidity"),
            "fan_mode": a.get("fan_mode"),
            "swing_mode": a.get("swing_mode"),
            "preset_mode": a.get("preset_mode"),
        }),
        Capabilities::MediaPlayer(player) => json!({
            "kind": "media_player",
            // `on` is a player that's on with nothing to say about playing.
            "state": if s == "on" { "idle" } else { s },
            // Only what can be told from Irori is said: a car's player reports a volume it can't
            // be set to.
            "volume": number(a, "volume_level")
                .filter(|v| player.volume && (0.0..=1.0).contains(v))
                .map(|v| (v * 100.0).round() as u8),
            "muted": a.get("is_volume_muted").filter(|m| player.mute && m.is_boolean()),
            "title": short(a, "media_title"),
            "artist": short(a, "media_artist"),
            "album": short(a, "media_album_name"),
            "app": short(a, "app_name"),
            "content_type": short(a, "media_content_type"),
            "duration": number(a, "media_duration").filter(|d| *d >= 0.0),
            "position": number(a, "media_position").filter(|p| player.seek && *p >= 0.0),
        }),
        Capabilities::Camera(camera) => json!({
            "kind": "camera",
            "state": s,
            "motion_detection": a.get("motion_detection")
                .filter(|m| camera.motion_detection && m.is_boolean()),
        }),
        Capabilities::DeviceTracker(_) => json!({
            "kind": "device_tracker",
            "home": s == "home",
            "latitude": number(a, "latitude").filter(|l| (-90.0..=90.0).contains(l)),
            "longitude": number(a, "longitude").filter(|l| (-180.0..=180.0).contains(l)),
            "accuracy": number(a, "gps_accuracy").filter(|g| *g >= 0.0),
        }),
        Capabilities::Update(_) => {
            // Older integrations say how far an install is with a number here.
            let in_progress = match a.get("in_progress") {
                Some(Value::Bool(on)) => *on,
                Some(Value::Number(_)) => true,
                _ => false,
            };
            let progress = number(a, "update_percentage")
                .or_else(|| number(a, "in_progress"))
                .filter(|p| in_progress && (0.0..=100.0).contains(p));
            json!({
                "kind": "update",
                "available": s == "on",
                "installed_version": short(a, "installed_version"),
                "latest_version": short(a, "latest_version"),
                "in_progress": in_progress,
                "progress": progress,
                "title": short(a, "title"),
                "release_summary": a.get("release_summary").and_then(Value::as_str).filter(|t| !t.is_empty()),
                "release_url": a.get("release_url").and_then(Value::as_str).filter(|t| !t.is_empty()),
                "skipped_version": short(a, "skipped_version"),
            })
        }
        Capabilities::AlarmControlPanel(_) => json!({ "kind": "alarm_control_panel", "state": s }),
        Capabilities::Vacuum(vacuum) => json!({
            "kind": "vacuum",
            "activity": s,
            "fan_speed": text("fan_speed").filter(|speed| vacuum.fan_speeds.iter().any(|f| f == speed)),
        }),
        Capabilities::WaterHeater(_) => json!({
            "kind": "water_heater",
            "operation_mode": s,
            "current_temperature": number(a, "current_temperature"),
            "target_temperature": number(a, "temperature"),
        }),
        Capabilities::Humidifier(humidifier) => json!({
            "kind": "humidifier",
            "on": s == "on",
            "target_humidity": number(a, "humidity").filter(|h| (0.0..=100.0).contains(h)),
            "current_humidity": number(a, "current_humidity").filter(|h| (0.0..=100.0).contains(h)),
            "mode": text("mode").filter(|mode| humidifier.modes.iter().any(|m| m == mode)),
            "action": text("action").and_then(irori_types::HumidifierAction::parse),
        }),
        Capabilities::LawnMower(_) => json!({ "kind": "lawn_mower", "activity": s }),
        Capabilities::Text(_) => json!({ "kind": "text", "value": s }),
        Capabilities::Date(_) => json!({ "kind": "date", "value": s }),
        Capabilities::Time(_) => json!({ "kind": "time", "value": s }),
        Capabilities::Datetime(_) => {
            // A helper's is the home's local time with no offset; its `timestamp` is the moment.
            let moment = if s.contains('T') {
                Some(s.to_owned())
            } else {
                number(a, "timestamp")
                    .and_then(|t| irori_types::Timestamp::from_second(t as i64))
                    .map(|t| t.to_string())
            };
            match moment {
                Some(moment) => json!({ "kind": "datetime", "value": moment }),
                None => return Reading::Value(None),
            }
        }
        Capabilities::Remote(remote) => json!({
            "kind": "remote",
            "on": s == "on",
            "activity": text("current_activity")
                .filter(|activity| s == "on" && remote.activities.iter().any(|a| a == activity)),
        }),
        Capabilities::Weather(_) => weather(s, a),
        Capabilities::Calendar(_) => json!({
            "kind": "calendar",
            "on": s == "on",
            "event": calendar_event(a),
        }),
        Capabilities::Todo(_) => match s.parse::<u32>() {
            Ok(open) => json!({ "kind": "todo", "open": open }),
            Err(_) => return Reading::Value(None),
        },
        Capabilities::Timer(_) => {
            let seconds = |key: &str| text(key).and_then(duration_seconds);
            json!({
                "kind": "timer",
                "state": s,
                "duration": seconds("duration").unwrap_or(0.0),
                "finishes_at": text("finishes_at").filter(|_| s == "active"),
                "remaining": seconds("remaining").filter(|_| s == "paused"),
            })
        }
        Capabilities::Schedule(_) => json!({
            "kind": "schedule",
            "on": s == "on",
            "next_change": text("next_event"),
        }),
        _ => return Reading::Value(None),
    };
    let mut value = value;
    if let Value::Object(fields) = &mut value {
        fields.retain(|_, value| !value.is_null());
    }
    match serde_json::from_value::<State>(value) {
        Ok(state) => Reading::Value(Some(state)),
        Err(why) => {
            tracing::debug!(%why, state = s, "a value Irori doesn't take");
            Reading::Value(None)
        }
    }
}

// --- Commands -------------------------------------------------------------------------------

/// The Home Assistant service call for one of Irori's commands to `bridged`: its domain, service
/// and data. Only a service of the entity's own kind, and only for the kinds this extension
/// carries: never anything else.
pub fn command(
    bridged: &Bridged,
    service: &Service,
) -> Result<(String, String, Map<String, Value>), ServiceError> {
    let full = service.name().as_str();
    let (kind, action) = full
        .split_once('.')
        .ok_or_else(|| ServiceError::failed(format!("`{full}` isn't a service")))?;
    // The service is of the kind Irori knows the entity as; Home Assistant's own domain is the
    // entity's (a helper's `input_boolean` for a switch's `turn_on`).
    let domain = bridged.domain();
    let is_kind = serde_json::to_value(bridged.description.capabilities.kind())
        .is_ok_and(|named| named == kind);
    if !is_kind || !carried(domain) {
        return Err(ServiceError::failed(format!(
            "`{full}` isn't for {}",
            bridged.entity_id
        )));
    }
    let mut data = match serde_json::to_value(service) {
        Ok(Value::Object(data)) => data,
        Ok(_) => Map::new(),
        Err(why) => return Err(ServiceError::failed(why.to_string())),
    };
    data.retain(|_, value| !value.is_null());
    let rename = |data: &mut Map<String, Value>, from: &str, to: &str| {
        if let Some(value) = data.remove(from) {
            data.insert(to.to_owned(), value);
        }
    };
    let action = match (domain, action) {
        ("media_player", "volume_set") => {
            if let Some(volume) = data.remove("volume").and_then(|v| v.as_f64()) {
                data.insert("volume_level".into(), json!(volume / 100.0));
            }
            "volume_set"
        }
        ("media_player", "volume_mute") => {
            rename(&mut data, "mute", "is_volume_muted");
            "volume_mute"
        }
        ("media_player", "media_seek") => {
            rename(&mut data, "position", "seek_position");
            "media_seek"
        }
        ("media_player", "play_media") => {
            rename(&mut data, "content_type", "media_content_type");
            rename(&mut data, "content_id", "media_content_id");
            // What it is goes along for a page to show; Home Assistant plays it by its id.
            data.retain(|key, _| key.starts_with("media_content_"));
            "play_media"
        }
        ("input_datetime", "set_value") => "set_datetime",
        ("remote", "send_command") => {
            rename(&mut data, "repeats", "num_repeats");
            "send_command"
        }
        // Home Assistant's takes the seconds to add, or take away, as its `duration`.
        ("timer", "change") => {
            rename(&mut data, "by", "duration");
            "change"
        }
        ("light", "turn_on") => {
            rename(&mut data, "rgb", "rgb_color");
            "turn_on"
        }
        ("cover", "open") => "open_cover",
        ("cover", "close") => "close_cover",
        ("cover", "stop") => "stop_cover",
        ("cover", "set_position") => "set_cover_position",
        ("cover", "set_tilt") => {
            rename(&mut data, "tilt", "tilt_position");
            "set_cover_tilt_position"
        }
        ("valve", "open") => "open_valve",
        ("valve", "close") => "close_valve",
        ("valve", "stop") => "stop_valve",
        ("valve", "set_position") => "set_valve_position",
        // The core answers these itself, with a `set_value` (`docs/specs/protocols.md` §7.1).
        ("number", "increment" | "decrement") => {
            return Err(ServiceError::failed(format!(
                "`{full}` isn't sent to a protocol"
            )));
        }
        (_, action) => action,
    };
    Ok((domain.to_owned(), action.to_owned(), data))
}

/// The context id in a compact state's `c`: a string, or `{ "id": … }`.
pub fn context_id(value: Option<&Value>) -> Option<&str> {
    match value? {
        Value::String(id) => Some(id),
        Value::Object(context) => context.get("id").and_then(Value::as_str),
        _ => None,
    }
}

/// Entities by `entity_id`, for following Home Assistant's state changes.
pub fn by_entity_id(entities: &[Bridged]) -> BTreeMap<String, usize> {
    entities
        .iter()
        .enumerate()
        .map(|(index, bridged)| (bridged.entity_id.clone(), index))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{CoverState, LightState, LightTurnOn, OpenState, SetPosition, SetTilt};

    fn attrs(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    fn settings() -> Settings {
        serde_json::from_value(json!({ "url": "http://127.0.0.1:8123", "token": "t" }))
            .expect("settings")
    }

    fn device(id: &str, area: Option<&str>) -> HaDevice {
        serde_json::from_value(json!({
            "id": id, "name": format!("Device {id}"), "area_id": area,
            "connections": [["mac", "AA:BB:CC:DD:EE:FF"], ["zigbee", "00:11:22:33:44:55:66:77"], ["upnp", "uuid"]],
            "serial_number": " SN-1 ",
        }))
        .expect("device")
    }

    fn entity(id: &str, entity_id: &str, device: Option<&str>) -> HaEntity {
        serde_json::from_value(json!({
            "id": id, "entity_id": entity_id, "platform": "hue", "device_id": device,
            "has_entity_name": true, "original_name": null,
        }))
        .expect("entity")
    }

    fn registry() -> Registry {
        let mut registry = Registry {
            floors: list(
                &json!([{ "floor_id": "up", "name": " Upstairs ", "level": 1 }, { "floor_id": "deep", "name": "Bunker", "level": -300 }]),
            ),
            areas: list(&json!([
                { "area_id": "kitchen", "name": "Kitchen", "floor_id": "up" },
                { "area_id": "garage", "name": "Garage" },
            ])),
            devices: vec![device("d1", Some("kitchen")), device("d2", Some("garage"))],
            entities: vec![
                entity("e1", "light.kitchen", Some("d1")),
                entity("e2", "sensor.kitchen_temperature", Some("d1")),
                entity("e3", "light.garage", Some("d2")),
                entity("e4", "tts.kitchen", Some("d1")),
                entity("e5", "weather.home", Some("d1")),
                entity("e6", "light.loose", None),
            ],
            states: HashMap::new(),
        };
        registry.entities[1].original_name = Some("Temperature".into());
        for (entity_id, state, a) in [
            (
                "light.kitchen",
                "on",
                json!({ "supported_color_modes": ["color_temp", "hs"], "brightness": 200, "color_mode": "color_temp", "color_temp_kelvin": 3000, "min_color_temp_kelvin": 2202, "max_color_temp_kelvin": 6535 }),
            ),
            (
                "sensor.kitchen_temperature",
                "21.5",
                json!({ "device_class": "temperature", "unit_of_measurement": "°C", "state_class": "measurement" }),
            ),
            (
                "light.garage",
                "off",
                json!({ "supported_color_modes": ["onoff"] }),
            ),
            ("tts.kitchen", "unknown", json!({})),
            ("weather.home", "sunny", json!({})),
            (
                "light.loose",
                "off",
                json!({ "supported_color_modes": ["onoff"], "friendly_name": "Loose light" }),
            ),
        ] {
            registry.states.insert(
                entity_id.into(),
                HaState {
                    state: state.into(),
                    attributes: attrs(a),
                },
            );
        }
        registry
    }

    #[test]
    fn devices_carry_their_area_connections_and_serial() {
        let plan = plan(&registry(), &settings());
        let d1 = &plan.devices[0];
        assert_eq!(d1.unique_id.as_str(), "d1");
        assert_eq!(
            d1.suggested_area.as_ref().map(Name::as_str),
            Some("Kitchen")
        );
        assert_eq!(d1.serial_number.as_deref(), Some("SN-1"));
        let kinds: Vec<_> = d1
            .connections
            .iter()
            .map(|c| (c.kind, c.value().to_owned()))
            .collect();
        assert_eq!(
            kinds,
            [
                (ConnectionKind::Mac, "aabbccddeeff".to_owned()),
                (ConnectionKind::Ieee, "0011223344556677".to_owned()),
                (ConnectionKind::Serial, "SN-1".to_owned()),
            ]
        );
    }

    #[test]
    fn by_default_only_modeled_entities_on_devices_come_over() {
        let plan = plan(&registry(), &settings());
        let ids: Vec<_> = plan.entities.iter().map(|b| b.entity_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "light.kitchen",
                "sensor.kitchen_temperature",
                "light.garage",
                "weather.home"
            ]
        );
        // The main light goes by its device's name; the sensor has its own.
        assert_eq!(plan.entities[0].description.name, None);
        assert_eq!(
            plan.entities[1].description.name.as_ref().map(Name::as_str),
            Some("Temperature")
        );
        assert_eq!(
            plan.entities[0]
                .description
                .suggested_object_id
                .as_ref()
                .map(ObjectId::as_str),
            Some("kitchen")
        );
        // Every kind Irori has comes over; one Irori lacks is listed, with no reason to give.
        let unmodeled: Vec<_> = plan
            .unmodeled
            .iter()
            .map(|u| {
                (
                    u.platform.as_str(),
                    u.reason.is_some(),
                    u.device_unique_id.is_some(),
                )
            })
            .collect();
        assert_eq!(unmodeled, [("tts", false, true)]);
    }

    #[test]
    fn disabled_and_hidden_entities_stay_out_unless_included_and_diagnostics_come_marked() {
        let mut registry = registry();
        registry.entities[0].hidden_by = Some("user".into());
        registry.entities[1].entity_category = Some("diagnostic".into());
        registry.entities[2].disabled_by = Some("user".into());
        let ids = |plan: &Plan| {
            plan.entities
                .iter()
                .map(|b| b.entity_id.clone())
                .collect::<Vec<_>>()
        };
        // The diagnostic sensor comes over as one; hidden and disabled stay out.
        let plain = plan(&registry, &settings());
        assert_eq!(ids(&plain), ["sensor.kitchen_temperature", "weather.home"]);
        assert_eq!(
            plain.entities[0].description.entity_category,
            Some(EntityCategory::Diagnostic)
        );

        let mut wide = settings();
        wide.include_integrations = vec!["HUE".into()];
        let widened = plan(&registry, &wide);
        // Disabled stays out even then; the device-less light comes over under its own name.
        assert_eq!(
            ids(&widened),
            [
                "light.kitchen",
                "sensor.kitchen_temperature",
                "weather.home",
                "light.loose"
            ]
        );
        assert_eq!(
            widened.entities[1].description.entity_category,
            Some(EntityCategory::Diagnostic)
        );
        let loose = &widened.entities[3].description;
        assert_eq!(loose.device_unique_id, None);
        assert_eq!(loose.name.as_ref().map(Name::as_str), Some("Loose light"));
    }

    #[test]
    fn areas_and_integrations_can_be_left_out() {
        let mut narrow = settings();
        narrow.exclude_areas = vec!["garage".into()];
        let plan = plan(&registry(), &narrow);
        assert!(plan.entities.iter().all(|b| b.entity_id != "light.garage"));
        assert!(plan.devices.iter().all(|d| d.unique_id.as_str() != "d2"));

        let mut none = settings();
        none.exclude_integrations = vec!["hue".into()];
        assert!(super::plan(&registry(), &none).entities.is_empty());

        let mut garage_only = settings();
        garage_only.include_areas = vec!["Garage".into()];
        garage_only.exclude_areas = vec!["Kitchen".into()];
        let ids: Vec<_> = super::plan(&registry(), &garage_only)
            .entities
            .into_iter()
            .map(|b| b.entity_id)
            .collect();
        assert_eq!(ids, ["light.garage"]);
    }

    #[test]
    fn home_assistant_s_own_parts_stay_behind_unless_asked_for() {
        let mut registry = registry();
        for (id, integration) in [
            ("core", "hassio"),
            ("card", "marketplace"),
            ("backups", "backup"),
        ] {
            let mut own = device(id, None);
            own.identifiers = vec![(integration.into(), id.into())];
            registry.devices.push(own);
            let mut part = entity(&format!("e-{id}"), &format!("light.{id}"), Some(id));
            part.platform = integration.into();
            registry.entities.push(part);
            registry.states.insert(
                format!("light.{id}"),
                HaState {
                    state: "on".into(),
                    attributes: attrs(json!({ "supported_color_modes": ["onoff"] })),
                },
            );
        }
        let plan = plan(&registry, &settings());
        let ids: Vec<_> = plan.devices.iter().map(|d| d.unique_id.as_str()).collect();
        assert_eq!(ids, ["d1", "d2"]);
        assert!(
            plan.entities
                .iter()
                .all(|b| !b.entity_id.starts_with("light.c"))
        );

        let mut asked = settings();
        asked.include_integrations = vec!["hassio".into()];
        let plan = super::plan(&registry, &asked);
        assert!(plan.entities.iter().any(|b| b.entity_id == "light.core"));
        assert!(plan.entities.iter().all(|b| b.entity_id != "light.card"));
    }

    #[test]
    fn past_the_limit_the_rest_wait_and_the_same_ones_always_come_over() {
        let mut registry = registry();
        registry.entities.clear();
        for n in (0..MAX_ENTITIES + 5).rev() {
            let entity_id = format!("switch.s{n}");
            registry
                .entities
                .push(entity(&format!("id{n:05}"), &entity_id, Some("d1")));
            registry.states.insert(
                entity_id,
                HaState {
                    state: "on".into(),
                    attributes: Map::new(),
                },
            );
        }
        let plan = plan(&registry, &settings());
        assert_eq!(plan.entities.len(), MAX_ENTITIES);
        assert_eq!(plan.entities[0].description.unique_id.as_str(), "id00000");
        assert_eq!(plan.waiting.len(), 1);
        assert!(
            plan.waiting[0].reason.starts_with("5 more"),
            "{}",
            plan.waiting[0].reason
        );
    }

    #[test]
    fn past_the_limit_a_new_entity_waits_behind_those_already_in_the_home() {
        let mut registry = registry();
        registry.entities.clear();
        let add = |registry: &mut Registry, id: &str| {
            let entity_id = format!("switch.{id}");
            registry.entities.push(entity(id, &entity_id, Some("d1")));
            registry.states.insert(
                entity_id,
                HaState {
                    state: "on".into(),
                    attributes: Map::new(),
                },
            );
        };
        for n in 1..=MAX_ENTITIES {
            add(&mut registry, &format!("id{n:05}"));
        }
        let first = plan(&registry, &settings());
        let before: BTreeSet<String> = first
            .entities
            .iter()
            .map(|b| b.description.unique_id.as_str().to_owned())
            .collect();
        // Added in Home Assistant, with an id that sorts ahead of all of them.
        add(&mut registry, "id00000");
        let next = plan_keeping(&registry, &settings(), &HashMap::new(), &before);
        assert_eq!(next.entities, first.entities);
        assert!(next.waiting[0].reason.starts_with("1 more"));
    }

    #[test]
    fn a_device_reached_through_another_comes_after_it() {
        let mut registry = registry();
        let mut hub = device("a-hub", None);
        hub.connections.clear();
        hub.serial_number = None;
        registry.devices.push(hub);
        registry.devices[0].via_device_id = Some("a-hub".into());
        registry.devices[1].via_device_id = Some("missing".into());
        let plan = plan(&registry, &settings());
        let order: Vec<_> = plan.devices.iter().map(|d| d.unique_id.as_str()).collect();
        assert_eq!(order, ["a-hub", "d1", "d2"]);
        assert_eq!(
            plan.devices[1]
                .via_device_unique_id
                .as_ref()
                .map(UniqueId::as_str),
            Some("a-hub")
        );
        assert_eq!(plan.devices[2].via_device_unique_id, None);
    }

    #[test]
    fn floors_and_areas_are_proposed_by_name() {
        let plan = plan(&registry(), &settings());
        let floors: Vec<_> = plan
            .layout
            .floors
            .iter()
            .map(|f| (f.name.as_str(), f.level))
            .collect();
        assert_eq!(floors, [("Upstairs", Some(1)), ("Bunker", Some(-128))]);
        let areas: Vec<_> = plan
            .layout
            .areas
            .iter()
            .map(|a| (a.name.as_str(), a.floor.as_ref().map(Name::as_str)))
            .collect();
        assert_eq!(areas, [("Kitchen", Some("Upstairs")), ("Garage", None)]);
    }

    #[test]
    fn a_light_reads_as_irori_takes_it() {
        let registry = registry();
        let plan = plan(&registry, &settings());
        let light = &plan.entities[0];
        let Capabilities::Light(caps) = &light.description.capabilities else {
            panic!("a light");
        };
        assert!(caps.brightness && caps.rgb);
        assert_eq!(
            caps.color_temp_kelvin.map(|r| (r.min, r.max)),
            Some((2202, 6535))
        );
        let state = &registry.states["light.kitchen"];
        assert_eq!(
            reading(
                &light.description.capabilities,
                &state.state,
                &state.attributes
            ),
            Reading::Value(Some(State::Light(LightState {
                on: true,
                brightness: Some(200),
                color_mode: Some(irori_types::ColorMode::ColorTemp),
                color_temp_kelvin: Some(3000),
                rgb: None,
            })))
        );
        // Brightness 0 is Irori's "off"; it's left out rather than refused.
        let off = attrs(json!({ "brightness": 0 }));
        assert!(matches!(
            reading(&light.description.capabilities, "off", &off),
            Reading::Value(Some(State::Light(LightState {
                on: false,
                brightness: None,
                ..
            })))
        ));
        assert_eq!(
            reading(&light.description.capabilities, "unavailable", &off),
            Reading::Unavailable
        );
        assert_eq!(
            reading(&light.description.capabilities, "unknown", &off),
            Reading::Value(None)
        );
    }

    #[test]
    fn values_irori_refuses_are_unknown_not_sent() {
        let sensor =
            capabilities("sensor", &attrs(json!({ "unit_of_measurement": "W" }))).expect("sensor");
        assert_eq!(reading(&sensor, "abc", &Map::new()), Reading::Value(None));
        let select =
            capabilities("select", &attrs(json!({ "options": ["a", "b"] }))).expect("select");
        assert_eq!(reading(&select, "c", &Map::new()), Reading::Value(None));
        let words = capabilities("sensor", &Map::new()).expect("a sensor with no unit");
        assert!(matches!(
            reading(&words, "charging", &Map::new()),
            Reading::Value(Some(_))
        ));
    }

    #[test]
    fn a_class_irori_doesnt_know_is_dropped_not_the_entity() {
        let caps = capabilities(
            "binary_sensor",
            &attrs(json!({ "device_class": "something_new" })),
        )
        .expect("still a binary sensor");
        assert_eq!(caps.kind(), EntityKind::BinarySensor);
    }

    #[test]
    fn a_cover_reads_its_position() {
        let caps = capabilities(
            "cover",
            &attrs(json!({ "supported_features": 15, "device_class": "blind" })),
        )
        .expect("cover");
        assert_eq!(
            reading(&caps, "opening", &attrs(json!({ "current_position": 40 }))),
            Reading::Value(Some(State::Cover(CoverState {
                state: OpenState::Opening,
                position: Some(40),
                tilt: None,
            })))
        );
    }

    fn bridged(entity_id: &str, caps: Capabilities) -> Bridged {
        Bridged {
            description: EntityDescription {
                unique_id: UniqueId::try_from("x").expect("id"),
                name: name("X"),
                device_unique_id: None,
                suggested_object_id: None,
                capabilities: caps,
                entity_category: None,
            },
            entity_id: entity_id.into(),
        }
    }

    #[test]
    fn commands_become_home_assistants_own_services() {
        let light = bridged(
            "light.kitchen",
            capabilities("light", &attrs(json!({ "supported_color_modes": ["hs"] })))
                .expect("light"),
        );
        let (domain, service, data) = command(
            &light,
            &Service::LightTurnOn(LightTurnOn {
                brightness: Some(9),
                color_temp_kelvin: None,
                rgb: Some([1, 2, 3]),
            }),
        )
        .expect("a call");
        assert_eq!((domain.as_str(), service.as_str()), ("light", "turn_on"));
        assert_eq!(
            Value::Object(data),
            json!({ "brightness": 9, "rgb_color": [1, 2, 3] })
        );
        assert_eq!(
            command(&light, &Service::LightTurnOff).expect("off").1,
            "turn_off"
        );

        let cover = bridged(
            "cover.blind",
            capabilities("cover", &attrs(json!({ "supported_features": 255 }))).expect("cover"),
        );
        for (service, expected, data) in [
            (Service::CoverOpen, "open_cover", json!({})),
            (Service::CoverClose, "close_cover", json!({})),
            (Service::CoverStop, "stop_cover", json!({})),
            (
                Service::CoverSetPosition(SetPosition { position: 30 }),
                "set_cover_position",
                json!({ "position": 30 }),
            ),
            (
                Service::CoverSetTilt(SetTilt { tilt: 70 }),
                "set_cover_tilt_position",
                json!({ "tilt_position": 70 }),
            ),
        ] {
            let (_, name, sent) = command(&cover, &service).expect("a call");
            assert_eq!(name, expected);
            assert_eq!(Value::Object(sent), data);
        }
        let valve = bridged(
            "valve.main",
            capabilities("valve", &attrs(json!({ "supported_features": 15 }))).expect("valve"),
        );
        assert_eq!(
            command(&valve, &Service::ValveOpen).expect("open").1,
            "open_valve"
        );
        assert_eq!(
            command(
                &valve,
                &Service::ValveSetPosition(SetPosition { position: 5 })
            )
            .expect("set")
            .1,
            "set_valve_position"
        );
        // Never a service of another kind, whatever the entity.
        assert!(command(&light, &Service::SwitchTurnOn).is_err());
        assert!(command(&cover, &Service::LightTurnOff).is_err());
    }

    /// Each kind Irori has, as Home Assistant reports it in a real home: it comes over, and its
    /// value reads.
    #[test]
    fn every_kind_irori_has_reads_from_home_assistant() {
        for (entity_id, state, a) in [
            (
                "media_player.tv",
                "playing",
                json!({ "device_class": "tv", "supported_features": 153529, "volume_level": 0.25, "is_volume_muted": false, "media_title": "News", "app_name": "YouTube", "media_duration": 300, "media_position": 12.5 }),
            ),
            (
                "media_player.cast",
                "on",
                json!({ "supported_features": 21437 }),
            ),
            // A car's player: a volume and a position it reports but can't be told.
            (
                "media_player.car",
                "playing",
                json!({ "supported_features": 0, "volume_level": 0.5, "is_volume_muted": false, "media_position": 30 }),
            ),
            (
                "sensor.wind_direction",
                "270",
                json!({ "device_class": "wind_direction", "unit_of_measurement": "°", "state_class": "measurement_angle" }),
            ),
            (
                "camera.front",
                "idle",
                json!({ "supported_features": 2, "access_token": "secret", "entity_picture": "/api/camera_proxy/camera.front?token=secret" }),
            ),
            (
                "camera.porch",
                "recording",
                json!({ "supported_features": 3, "motion_detection": true }),
            ),
            (
                "device_tracker.phone",
                "home",
                json!({ "source_type": "gps", "latitude": 36.5, "longitude": 137.8, "gps_accuracy": 14.1 }),
            ),
            (
                "device_tracker.deco",
                "not_home",
                json!({ "tracking_type": "connection" }),
            ),
            (
                "update.core",
                "off",
                json!({ "supported_features": 15, "installed_version": "2026.11.2", "latest_version": "2026.11.2", "in_progress": false, "title": "Home Assistant Core", "release_url": "https://www.home-assistant.io/" }),
            ),
            (
                "update.bulb",
                "on",
                json!({ "device_class": "firmware", "supported_features": 5, "installed_version": "1.0", "latest_version": "1.1", "in_progress": true, "update_percentage": 40 }),
            ),
            (
                "alarm_control_panel.garden",
                "armed_home",
                json!({ "supported_features": 63, "code_format": null, "code_arm_required": false }),
            ),
            (
                "vacuum.dora",
                "docked",
                json!({ "supported_features": 14204, "fan_speed": "light", "fan_speed_list": ["light", "normal", "deep"] }),
            ),
            (
                "water_heater.tank",
                "eco",
                json!({ "supported_features": 11, "operation_list": ["eco", "electric", "off"], "min_temp": 40, "max_temp": 75, "temperature": 55, "current_temperature": 52 }),
            ),
            (
                "humidifier.closet",
                "on",
                json!({ "device_class": "dehumidifier", "supported_features": 1, "available_modes": ["auto", "dry"], "mode": "auto", "humidity": 50, "current_humidity": 61, "min_humidity": 30, "max_humidity": 80, "action": "drying" }),
            ),
            (
                "lawn_mower.yard",
                "mowing",
                json!({ "supported_features": 7 }),
            ),
            (
                "text.effect_color",
                "ff0000",
                json!({ "min": 0, "max": 255, "mode": "text", "pattern": null }),
            ),
            ("date.setting", "2026-11-19", json!({})),
            ("time.setting", "12:29:00", json!({})),
            ("datetime.alarm", "2026-11-19T07:00:00+00:00", json!({})),
            (
                "input_boolean.guest_mode",
                "on",
                json!({ "editable": true }),
            ),
            (
                "input_number.energy_price",
                "38.8",
                json!({ "min": 0.0, "max": 100.0, "step": 1.0, "mode": "box", "unit_of_measurement": "JPY/kWh" }),
            ),
            (
                "input_select.security_level",
                "normal",
                json!({ "options": ["off", "normal", "high"] }),
            ),
            (
                "input_text.note",
                "hello",
                json!({ "min": 0, "max": 100, "mode": "text" }),
            ),
            (
                "input_datetime.filter_changed",
                "2026-11-08",
                json!({ "has_date": true, "has_time": false, "timestamp": 1794063600.0 }),
            ),
            (
                "input_datetime.wake",
                "07:30:00",
                json!({ "has_date": false, "has_time": true, "timestamp": 27000 }),
            ),
            (
                "input_datetime.trip",
                "2026-12-01 09:00:00",
                json!({ "has_date": true, "has_time": true, "timestamp": 1796083200.0 }),
            ),
            (
                "remote.living_room_tv",
                "on",
                json!({ "supported_features": 4, "activity_list": ["Watch TV", "Play"], "current_activity": "Watch TV" }),
            ),
            (
                "weather.forecast_home",
                "partlycloudy",
                json!({ "temperature": 14.5, "humidity": 71, "pressure": 1012.3, "wind_speed": 12.2, "wind_bearing": 250.4, "temperature_unit": "°C", "pressure_unit": "hPa", "wind_speed_unit": "km/h", "visibility_unit": "km" }),
            ),
            (
                "calendar.family",
                "off",
                json!({ "message": "Dentist", "all_day": false, "start_time": "2026-11-20 09:00:00", "end_time": "2026-11-20 10:00:00", "location": "", "description": "" }),
            ),
            (
                "todo.shopping_list",
                "3",
                json!({ "supported_features": 15 }),
            ),
            (
                "timer.laundry",
                "active",
                json!({ "duration": "0:45:00", "finishes_at": "2026-11-19T10:00:00+00:00", "editable": true }),
            ),
            (
                "timer.oven",
                "paused",
                json!({ "duration": "1 day, 2:00:00", "remaining": "0:12:30" }),
            ),
            (
                "schedule.heating",
                "on",
                json!({ "next_event": "2026-11-19T22:00:00+09:00", "editable": true }),
            ),
        ] {
            let a = attrs(a);
            let caps = capabilities(domain(entity_id), &a)
                .unwrap_or_else(|why| panic!("{entity_id}: {why:?}"));
            match reading(&caps, state, &a) {
                Reading::Value(Some(_)) => {}
                other => panic!("{entity_id} = {state}: {other:?}"),
            }
        }
        // A scene, a helper button, an image and a notify target have nothing to report.
        for (entity_id, a) in [
            ("scene.evening", json!({})),
            ("input_button.ring", json!({})),
            (
                "image.doorbell_last_visitor",
                json!({ "access_token": "secret" }),
            ),
            ("notify.phone", json!({ "supported_features": 1 })),
        ] {
            let a = attrs(a);
            let caps = capabilities(domain(entity_id), &a).expect(entity_id);
            assert_eq!(reading(&caps, "unknown", &a), Reading::Nothing);
        }
    }

    #[test]
    fn weather_calendars_and_timers_come_in_irori_s_units_and_the_home_s_time() {
        let read = |entity_id: &str, s: &str, a: Value| {
            let a = attrs(a);
            let caps = capabilities(domain(entity_id), &a).expect(entity_id);
            match reading(&caps, s, &a) {
                Reading::Value(Some(state)) => serde_json::to_value(state).expect("json"),
                other => panic!("{entity_id}: {other:?}"),
            }
        };
        assert_eq!(
            read(
                "weather.home",
                "rainy",
                json!({ "temperature": 50, "temperature_unit": "°F", "pressure": 29.92, "pressure_unit": "inHg", "wind_speed": 10, "wind_speed_unit": "mph", "wind_bearing": "NW", "visibility": 6.2, "visibility_unit": "mi" })
            ),
            json!({ "kind": "weather", "condition": "rainy", "temperature": 10.0, "pressure": 1013.2, "wind_speed": 16.1, "visibility": 10.0 })
        );
        // All day: days only, the end the day after.
        assert_eq!(
            read(
                "calendar.bins",
                "on",
                json!({ "message": "Recycling", "all_day": true, "start_time": "2026-11-19 00:00:00", "end_time": "2026-11-20 00:00:00" })
            ),
            json!({ "kind": "calendar", "on": true, "event": { "summary": "Recycling", "start": "2026-11-19", "end": "2026-11-20" } })
        );
        assert_eq!(
            read("calendar.empty", "off", json!({})),
            json!({ "kind": "calendar", "on": false })
        );
        assert_eq!(
            read(
                "timer.oven",
                "paused",
                json!({ "duration": "1 day, 2:00:00", "remaining": "0:12:30" })
            ),
            json!({ "kind": "timer", "state": "paused", "duration": 93600.0, "remaining": 750.0 })
        );
        // An activity it doesn't list isn't one it says; nor any once it's off.
        assert_eq!(
            read(
                "remote.hub",
                "off",
                json!({ "supported_features": 4, "activity_list": ["Watch TV"], "current_activity": "Watch TV" })
            ),
            json!({ "kind": "remote", "on": false })
        );
    }

    #[test]
    fn what_irori_cant_hold_is_said_not_guessed() {
        // Modes Irori's water heaters don't have: listed with the reason, not brought over wrong.
        let why = capabilities(
            "water_heater",
            &attrs(
                json!({ "supported_features": 2, "operation_list": ["auto", "manual_heating"] }),
            ),
        )
        .expect_err("no modes Irori has");
        assert!(why.is_some_and(|why| why.contains("auto, manual_heating")));
        // A camera's access token stays in Home Assistant: it isn't part of what's sent.
        let a = attrs(json!({ "supported_features": 2, "access_token": "secret" }));
        let caps = capabilities("camera", &a).expect("camera");
        let Reading::Value(Some(state)) = reading(&caps, "idle", &a) else {
            panic!("a camera's state");
        };
        let sent = format!("{caps:?}{state:?}");
        assert!(!sent.contains("secret"), "{sent}");
        // A player says only what it can be told: no volume, mute or position it can't set.
        let a = attrs(
            json!({ "supported_features": 0, "volume_level": 0.5, "is_volume_muted": true, "media_position": 30 }),
        );
        let caps = capabilities("media_player", &a).expect("player");
        let Reading::Value(Some(state)) = reading(&caps, "playing", &a) else {
            panic!("a player's state");
        };
        assert_eq!(
            serde_json::to_value(&state).expect("json"),
            json!({ "kind": "media_player", "state": "playing" })
        );
        // A player's long title is cut, not refused.
        let long = "x".repeat(400);
        let a = attrs(json!({ "media_title": long }));
        let caps = capabilities("media_player", &a).expect("player");
        assert!(matches!(
            reading(&caps, "playing", &a),
            Reading::Value(Some(_))
        ));
    }

    #[test]
    fn helpers_and_new_kinds_are_told_in_home_assistants_words() {
        let call = |entity_id: &str, a: Value, service: Value| {
            let a = attrs(a);
            let caps = capabilities(domain(entity_id), &a).expect(entity_id);
            let mut service = service;
            service["unique_id"] = json!("x");
            service["context"] = json!({ "id": "01K5B2Q9A1B2C3D4E5F6G7H8J9", "origin": { "type": "user", "user_id": "nico" } });
            let service = serde_json::from_value::<irori_types::ServiceCall>(service)
                .expect("a service")
                .service;
            let (domain, name, data) = command(&bridged(entity_id, caps), &service)?;
            Ok::<_, ServiceError>((format!("{domain}.{name}"), Value::Object(data)))
        };
        let player = json!({ "supported_features": 153529 });
        assert_eq!(
            call(
                "media_player.tv",
                player.clone(),
                json!({ "service": "media_player.volume_set", "data": { "volume": 40 } })
            )
            .expect("volume"),
            (
                "media_player.volume_set".to_owned(),
                json!({ "volume_level": 0.4 })
            )
        );
        assert_eq!(
            call(
                "media_player.tv",
                player.clone(),
                json!({ "service": "media_player.volume_mute", "data": { "mute": true } })
            )
            .expect("mute"),
            (
                "media_player.volume_mute".to_owned(),
                json!({ "is_volume_muted": true })
            )
        );
        assert_eq!(
            call("media_player.tv", player, json!({ "service": "media_player.play_media", "data": { "content_type": "music", "content_id": "radio", "title": "Radio" } })).expect("play"),
            ("media_player.play_media".to_owned(), json!({ "media_content_type": "music", "media_content_id": "radio" }))
        );
        assert_eq!(
            call(
                "input_boolean.guest",
                json!({}),
                json!({ "service": "switch.turn_on" })
            )
            .expect("on"),
            ("input_boolean.turn_on".to_owned(), json!({}))
        );
        assert_eq!(
            call(
                "input_datetime.filter",
                json!({ "has_date": true, "has_time": false }),
                json!({ "service": "date.set_value", "data": { "date": "2026-11-19" } })
            )
            .expect("date"),
            (
                "input_datetime.set_datetime".to_owned(),
                json!({ "date": "2026-11-19" })
            )
        );
        assert_eq!(
            call(
                "alarm_control_panel.garden",
                json!({ "supported_features": 63 }),
                json!({ "service": "alarm_control_panel.alarm_disarm", "data": {} })
            )
            .expect("disarm"),
            ("alarm_control_panel.alarm_disarm".to_owned(), json!({}))
        );
        assert_eq!(
            call(
                "remote.living_room_tv",
                json!({ "supported_features": 4, "activity_list": ["Watch TV"] }),
                json!({ "service": "remote.send_command", "data": { "command": ["volume_up"], "repeats": 3 } })
            )
            .expect("send"),
            (
                "remote.send_command".to_owned(),
                json!({ "command": ["volume_up"], "num_repeats": 3 })
            )
        );
        assert_eq!(
            call(
                "notify.phone",
                json!({ "supported_features": 1 }),
                json!({ "service": "notify.send_message", "data": { "message": "Leak in the kitchen", "title": "Leak" } })
            )
            .expect("notify"),
            (
                "notify.send_message".to_owned(),
                json!({ "message": "Leak in the kitchen", "title": "Leak" })
            )
        );
        assert_eq!(
            call(
                "todo.shopping_list",
                json!({ "supported_features": 15 }),
                json!({ "service": "todo.add_item", "data": { "item": "Milk" } })
            )
            .expect("add"),
            ("todo.add_item".to_owned(), json!({ "item": "Milk" }))
        );
        assert_eq!(
            call(
                "timer.laundry",
                json!({ "duration": "0:45:00" }),
                json!({ "service": "timer.change", "data": { "by": -60.0 } })
            )
            .expect("change"),
            ("timer.change".to_owned(), json!({ "duration": -60.0 }))
        );
        // A helper is still only told what its kind takes.
        assert!(
            call(
                "input_boolean.guest",
                json!({}),
                json!({ "service": "light.turn_on", "data": {} })
            )
            .is_err()
        );
    }

    #[test]
    fn a_context_comes_as_text_or_an_object() {
        assert_eq!(context_id(Some(&json!("abc"))), Some("abc"));
        assert_eq!(
            context_id(Some(&json!({ "id": "def", "user_id": null }))),
            Some("def")
        );
        assert_eq!(context_id(None), None);
    }
}
