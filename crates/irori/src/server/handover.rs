//! Handing a device over to another way of reaching the same hardware (`docs/specs/config.md`
//! §3.11, `docs/specs/api.md` §3): a device someone first tried through a bridge (Home
//! Assistant) moves to the extension that reaches it directly, keeping what the home knows of it.
//!
//! Both devices are already in the home and recognised as one (`duplicate_of`,
//! `docs/specs/protocols.md` §6.1). Handing over:
//!
//! - gives the device that takes over the name, description and room the other one had, and its
//!   spot on the floorplan;
//! - rewrites the automations (through the Automations extension, which owns them) and helpers
//!   that name the other one's entities to name the matching entities instead;
//! - takes the other one out of the home without deleting its history, and writes the handover
//!   down in `handovers.toml`, so it isn't offered under "+ Add device" and can be handed back.
//!
//! Nothing is said to the bridge or to what is behind it: Home Assistant is never written to.
//! Anything that names an entity with no counterpart stops the handover before it starts, so no
//! reference is ever left pointing at nothing. Handing back is the same, the other way round,
//! with the pairs that were written down.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use irori_core::{AppRequestError, Core};
use irori_protocol::engine::Caller;
use irori_types::{
    AreaId, Device, DeviceId, Entity, EntityId, EntityKind, ExtensionId, Handover, Name, Placement,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{AppState, edit_failed, refused, trail};
use crate::config::Refused;

/// The extension whose flows are the home's automations.
fn automations() -> ExtensionId {
    ExtensionId::try_from("automations").expect("a valid extension id")
}

/// The extension that keeps helpers, in `extensions/helpers.toml`.
fn helpers() -> ExtensionId {
    ExtensionId::try_from("helpers").expect("a valid extension id")
}

/// One of the two devices, as the page names it.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Side {
    id: DeviceId,
    name: Name,
    /// The extension that reaches it, by its own name where it has one.
    through: String,
}

/// An entity of the device that steps aside, and the one that stands for it from now on.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Pair {
    from: EntityId,
    to: EntityId,
    name: Name,
}

/// An entity of the device that steps aside with nothing to stand for it.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Left {
    id: EntityId,
    name: Name,
}

/// Something of the home's that names the device's entities: an automation, or a helper.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Uses {
    id: String,
    name: String,
    entities: Vec<EntityId>,
}

/// What handing a device over would do. Asking changes nothing: cancelling is not sending.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Plan {
    from: Side,
    to: Side,
    /// Handing back to a device that stepped aside before, rather than over to a new one.
    back: bool,
    /// The name and room the device keeps.
    name: Name,
    #[serde(skip_serializing_if = "Option::is_none")]
    area: Option<AreaId>,
    entities: Vec<Pair>,
    /// Entities with nothing to stand for them. Their history is kept with the device that
    /// steps aside; nothing may name them, or the handover is refused.
    left_behind: Vec<Left>,
    automations: Vec<Uses>,
    helpers: Vec<Uses>,
    /// Why it can't be done now, in words for the page. Absent when it can.
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HandOver {
    /// The device to hand over to, as the plan the person saw named it: if that has changed
    /// meanwhile, nothing is done.
    to: DeviceId,
}

/// What a page can be told is wrong.
type Refusal = (StatusCode, String);

/// `GET /api/devices/{id}/handover`: what handing this device over would do.
pub(super) async fn plan(State(state): State<AppState>, Path(id): Path<DeviceId>) -> Response {
    match plan_for(&state, &id).await {
        Ok(plan) => Json(plan).into_response(),
        Err((status, why)) => refused(status, why),
    }
}

/// `POST /api/devices/{id}/handover`: hands this device over, as the plan says.
pub(super) async fn hand_over(
    State(state): State<AppState>,
    Extension(trail): Extension<trail::Trail>,
    Path(id): Path<DeviceId>,
    Json(request): Json<HandOver>,
) -> Response {
    trail.target(id.as_str());
    let plan = match plan_for(&state, &id).await {
        Ok(plan) => plan,
        Err((status, why)) => return refused(status, why),
    };
    if plan.to.id != request.to {
        return refused(
            StatusCode::CONFLICT,
            format!(
                "{} would now go to {}, not the device you were shown; look again",
                plan.from.name, plan.to.name
            ),
        );
    }
    if let Some(why) = plan.blocked {
        return refused(StatusCode::CONFLICT, why);
    }
    let pairs: BTreeMap<EntityId, EntityId> = plan
        .entities
        .iter()
        .map(|pair| (pair.from.clone(), pair.to.clone()))
        .collect();
    let core = &state.0.core;

    // The automations first, then helpers, then the home: each step leaves every reference
    // pointing at a device that is there (or, handing back, there a moment later), and one that
    // fails can be tried again, since what was already rewritten names nothing to rewrite.
    for uses in &plan.automations {
        if let Err(why) = rewrite_flow(core, &uses.id, &pairs).await {
            return refused(
                StatusCode::BAD_GATEWAY,
                format!(
                    "Automations didn't take the change to “{}”: {why}. Nothing else was \
                     changed; try again.",
                    uses.name
                ),
            );
        }
    }
    if !plan.helpers.is_empty() {
        let rewritten = state
            .0
            .config
            .edit_extension(core, &helpers(), |file| {
                for table in file.values_mut() {
                    rewrite(table, &pairs);
                }
                Ok(())
            })
            .await;
        if let Err(e) = rewritten {
            return edit_failed(e);
        }
    }

    let (from, to) = (plan.from.id.clone(), plan.to.id.clone());
    let (name, area) = (plan.name.clone(), plan.area.clone());
    let description = core
        .devices()
        .into_iter()
        .find(|device| device.id == from)
        .and_then(|device| device.description);
    // The entities' own rows, by what their protocols call them, while both are known.
    let names: Vec<(irori_types::SettingsKey, irori_types::SettingsKey)> = pairs
        .iter()
        .filter_map(|(was, now)| Some((core.entity_key(was)?, core.entity_key(now)?)))
        .collect();
    let edited = state
        .0
        .config
        .edit(core, |settings| {
            if settings
                .handovers
                .get(&to)
                .is_some_and(|handover| handover.to != from)
            {
                return Err(Refused(format!("`{to}` was handed over to another device")));
            }
            settings.handovers.remove(&to);
            let was = settings.devices.get(&from).cloned().unwrap_or_default();
            let device = settings.devices.entry(to.clone()).or_default();
            device.added = true;
            device.name = Some(name.clone());
            if description.is_some() {
                device.description = description.clone();
            }
            device.area = match (&area, &was.area) {
                (Some(area), _) => Placement::In(area.clone()),
                (None, Placement::Nowhere) => Placement::Nowhere,
                (None, _) => device.area.clone(),
            };
            for (was, now) in &names {
                if let Some(name) = settings.entities.get(was).and_then(|row| row.name.clone()) {
                    settings.entities.entry(now.clone()).or_default().name = Some(name);
                }
            }
            repoint_plan(&mut settings.floorplan, &from, &to);
            settings.devices.remove(&from);
            settings.handovers.insert(
                from.clone(),
                Handover {
                    to: to.clone(),
                    entities: pairs.clone(),
                },
            );
            Ok(())
        })
        .await;
    match edited {
        Ok(()) => {
            trail.detail(to.as_str());
            Json(plan).into_response()
        }
        Err(e) => edit_failed(e),
    }
}

/// Works out what handing `id` over would do.
async fn plan_for(state: &AppState, id: &DeviceId) -> Result<Plan, Refusal> {
    let core = &state.0.core;
    // whole-home: handing a device over is an owner's
    let devices = core.devices();
    let Some(from) = devices.iter().find(|device| &device.id == id).cloned() else {
        return Err((StatusCode::NOT_FOUND, format!("there's no device `{id}`")));
    };
    let handovers = core.handovers();
    // Handing back: a device that stepped aside for this one.
    // Only while its extension still has it: handing back to a device that isn't there would
    // leave the home with neither.
    let back = handovers
        .iter()
        .find(|(before, handover)| {
            handover.to == *id
                && !devices.iter().any(|d| &d.id == *before)
                && core.device_key(before).is_some()
        })
        .map(|(before, handover)| (before.clone(), handover.clone()));
    let (to_id, pairs) = match (&back, &from.duplicate_of) {
        (Some((before, handover)), _) => {
            let mine: BTreeSet<EntityId> = entities_of(core, id)
                .into_iter()
                .map(|entity| entity.id)
                .collect();
            let pairs: BTreeMap<EntityId, EntityId> = handover
                .entities
                .iter()
                .filter(|(_, now)| mine.contains(*now))
                .map(|(was, now)| (now.clone(), was.clone()))
                .collect();
            (before.clone(), pairs)
        }
        (None, Some(twin)) if devices.iter().any(|device| &device.id == twin) => {
            let pairs = pair_up(&entities_of(core, id), &entities_of(core, twin));
            (twin.clone(), pairs)
        }
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                format!(
                    "{} has no other way of being reached to hand it over to",
                    from.name
                ),
            ));
        }
    };
    if back.is_none() && !core.same_hardware(id, &to_id) {
        return Err((
            StatusCode::CONFLICT,
            format!("{} isn't recognised as the same device any more", from.name),
        ));
    }
    let to = side_of(core, &to_id, &devices, back.as_ref().map(|_| &from));
    let mine = entities_of(core, id);
    let names: BTreeMap<EntityId, Name> = mine
        .iter()
        .map(|entity| (entity.id.clone(), entity.name.clone()))
        .collect();
    let left_behind: Vec<Left> = mine
        .iter()
        .filter(|entity| !pairs.contains_key(&entity.id))
        .map(|entity| Left {
            id: entity.id.clone(),
            name: entity.name.clone(),
        })
        .collect();
    let entities: Vec<Pair> = pairs
        .iter()
        .map(|(was, now)| Pair {
            from: was.clone(),
            to: now.clone(),
            name: names.get(was).cloned().unwrap_or_else(|| from.name.clone()),
        })
        .collect();
    let named: BTreeSet<EntityId> = mine.iter().map(|entity| entity.id.clone()).collect();

    let mut blocked = None;
    let automations = match automations_using(core, &named).await {
        Ok(automations) => automations,
        Err(why) => {
            blocked = Some(why);
            Vec::new()
        }
    };
    let helpers = helpers_using(state, &named).await;
    let (names, pairs_ref) = (&names, &pairs);
    let stranded: Vec<String> = automations
        .iter()
        .chain(&helpers)
        .flat_map(|uses| {
            uses.entities
                .iter()
                .filter(|entity| !pairs_ref.contains_key(*entity))
                .map(move |entity| {
                    let name = names.get(entity).map_or(entity.as_str(), Name::as_str);
                    format!("“{}” uses {name}", uses.name)
                })
        })
        .collect();
    if blocked.is_none() && !stranded.is_empty() {
        blocked = Some(format!(
            "{} has nothing that stands for what these use, so they would stop working: {}. \
             Change them first.",
            to.name,
            stranded.join("; ")
        ));
    }
    Ok(Plan {
        to,
        back: back.is_some(),
        name: from.name.clone(),
        area: from.area_id.clone(),
        entities,
        left_behind,
        automations,
        helpers,
        blocked,
        from: Side {
            through: through(core, &from),
            id: from.id,
            name: from.name,
        },
    })
}

/// The device being handed over to, as the page names it: in the home, or (handing back)
/// stepped aside, when it goes by the name it's about to be given.
fn side_of(core: &Core, id: &DeviceId, devices: &[Device], giving: Option<&Device>) -> Side {
    if let Some(device) = devices.iter().find(|device| &device.id == id) {
        return Side {
            id: id.clone(),
            name: device.name.clone(),
            through: through(core, device),
        };
    }
    let protocol = core.device_key(id).map(|(protocol, _)| protocol);
    Side {
        id: id.clone(),
        name: giving
            .map(|device| device.name.clone())
            .unwrap_or_else(|| Name::try_from(id.as_str()).expect("an id is a name")),
        through: protocol
            .map(|protocol| extension_name(core, protocol.as_str()))
            .unwrap_or_default(),
    }
}

fn through(core: &Core, device: &Device) -> String {
    extension_name(core, device.protocol.as_str())
}

fn extension_name(core: &Core, protocol: &str) -> String {
    core.extensions()
        .into_iter()
        .find(|(id, _)| id.as_str() == protocol)
        .and_then(|(_, overview)| overview.info.map(|info| info.name.to_string()))
        .unwrap_or_else(|| protocol.to_owned())
}

fn entities_of(core: &Core, device: &DeviceId) -> Vec<Entity> {
    // whole-home: handing a device over is an owner's
    core.entities()
        .into_iter()
        .filter(|entity| entity.device_id.as_ref() == Some(device))
        .collect()
}

/// What an entity is, for telling which of the other device's stands for it: its kind, what
/// sort of thing it measures or is, and whether it's a setting or diagnostic.
fn what_it_is(
    entity: &Entity,
) -> (
    EntityKind,
    Option<String>,
    Option<irori_types::EntityCategory>,
) {
    let class = serde_json::to_value(&entity.capabilities)
        .ok()
        .and_then(|capabilities| capabilities.get("device_class").cloned())
        .and_then(|class| class.as_str().map(str::to_owned));
    (entity.id.kind(), class, entity.entity_category)
}

/// Pairs each entity of the device stepping aside with the one of the other device that stands
/// for it, one to one: the same kind, the same class (a temperature with a temperature), the
/// same standing (a diagnostic with a diagnostic), else the same kind and class whatever their
/// standing. Where that leaves several alike on either side, the same name decides; where it
/// still doesn't, they're left unpaired rather than guessed.
pub(super) fn pair_up(from: &[Entity], to: &[Entity]) -> BTreeMap<EntityId, EntityId> {
    let mut pairs = BTreeMap::new();
    let mut taken: BTreeSet<EntityId> = BTreeSet::new();
    // Finest first: the whole description, then the kind and class. Never the kind alone: a
    // temperature left over on one side and a humidity on the other aren't one thing.
    let ways: [fn(&Entity) -> String; 2] = [
        |entity| format!("{:?}", what_it_is(entity)),
        |entity| {
            let (kind, class, _) = what_it_is(entity);
            format!("{kind:?}{class:?}")
        },
    ];
    for way in ways {
        let mut theirs: BTreeMap<String, Vec<&Entity>> = BTreeMap::new();
        for entity in to.iter().filter(|entity| !taken.contains(&entity.id)) {
            theirs.entry(way(entity)).or_default().push(entity);
        }
        let mut mine: BTreeMap<String, Vec<&Entity>> = BTreeMap::new();
        for entity in from.iter().filter(|entity| !pairs.contains_key(&entity.id)) {
            mine.entry(way(entity)).or_default().push(entity);
        }
        for (what, mine) in mine {
            let Some(theirs) = theirs.get(&what) else {
                continue;
            };
            if let ([one], [other]) = (mine.as_slice(), theirs.as_slice()) {
                pairs.insert(one.id.clone(), other.id.clone());
                taken.insert(other.id.clone());
                continue;
            }
            // Several: only those whose names say which is which.
            for one in &mine {
                let same: Vec<&&Entity> = theirs
                    .iter()
                    .filter(|other| same_name(&one.name, &other.name))
                    .collect();
                let named_alike = mine
                    .iter()
                    .filter(|another| same_name(&another.name, &one.name))
                    .count();
                if let ([other], 1) = (same.as_slice(), named_alike)
                    && !taken.contains(&other.id)
                {
                    pairs.insert(one.id.clone(), other.id.clone());
                    taken.insert(other.id.clone());
                }
            }
        }
    }
    pairs
}

fn same_name(one: &Name, other: &Name) -> bool {
    one.as_str().trim().to_lowercase() == other.as_str().trim().to_lowercase()
}

/// Every automation that names any of `entities`, with which. Empty when there's no
/// Automations extension; an error, in words, when there is one that can't be asked.
async fn automations_using(
    core: &Core,
    entities: &BTreeSet<EntityId>,
) -> Result<Vec<Uses>, String> {
    if !core.extensions().contains_key(&automations()) {
        return Ok(Vec::new());
    }
    let cannot = |error: AppRequestError| match error {
        AppRequestError::NotRunning(_) => "Automations isn't running, so the automations that \
                                           might use this device can't be checked. Turn it on, \
                                           then try again."
            .to_owned(),
        other => format!("the automations that might use this device can't be checked: {other}"),
    };
    let listed = core
        .app_request(
            &automations(),
            "flows.list".to_owned(),
            json!({}),
            Caller::owner(),
        )
        .await
        .map_err(cannot)?;
    let mut using = Vec::new();
    for summary in listed["flows"].as_array().into_iter().flatten() {
        let Some(id) = summary["id"].as_str() else {
            continue;
        };
        let detail = core
            .app_request(
                &automations(),
                "flows.get".to_owned(),
                json!({ "id": id }),
                Caller::owner(),
            )
            .await
            .map_err(cannot)?;
        let named = named_in(&detail["flow"], entities);
        if !named.is_empty() {
            using.push(Uses {
                id: id.to_owned(),
                name: summary["name"].as_str().unwrap_or(id).to_owned(),
                entities: named.into_iter().collect(),
            });
        }
    }
    Ok(using)
}

/// Every helper that names any of `entities`.
async fn helpers_using(state: &AppState, entities: &BTreeSet<EntityId>) -> Vec<Uses> {
    let file = state.0.config.extension_settings(&helpers()).await;
    let mut using = Vec::new();
    for (table, helpers) in &file {
        let Some(helpers) = helpers.as_object() else {
            continue;
        };
        for (id, helper) in helpers {
            let named = named_in(helper, entities);
            if !named.is_empty() {
                using.push(Uses {
                    id: format!("{table}.{id}"),
                    name: helper["name"].as_str().unwrap_or(id).to_owned(),
                    entities: named.into_iter().collect(),
                });
            }
        }
    }
    using
}

/// Saves an automation with its entities renamed, through the extension that keeps it.
async fn rewrite_flow(
    core: &Core,
    id: &str,
    pairs: &BTreeMap<EntityId, EntityId>,
) -> Result<(), AppRequestError> {
    let detail = core
        .app_request(
            &automations(),
            "flows.get".to_owned(),
            json!({ "id": id }),
            Caller::owner(),
        )
        .await?;
    let mut flow = detail["flow"].clone();
    if !rewrite(&mut flow, pairs) {
        return Ok(());
    }
    core.app_request(
        &automations(),
        "flows.save".to_owned(),
        json!({ "flow": flow }),
        Caller::owner(),
    )
    .await?;
    Ok(())
}

/// Moves the device's markers on the floorplan to the device taking over. Where the one stepping
/// aside had a marker of its own, that is where the device is from now on, and any the other had
/// become second markers of it.
fn repoint_plan(plan: &mut irori_types::Floorplan, from: &DeviceId, to: &DeviceId) {
    fn original(marker: &irori_types::PlacedDevice, device: &DeviceId) -> bool {
        marker.entity.is_none() && marker.device.as_ref() == Some(device) && !marker.copy
    }
    let had_one = plan
        .floors
        .values()
        .flat_map(|level| &level.devices)
        .any(|marker| original(marker, from));
    for level in plan.floors.values_mut() {
        for marker in &mut level.devices {
            if had_one && original(marker, to) {
                marker.copy = true;
            }
        }
        for marker in &mut level.devices {
            if marker.entity.is_none() && marker.device.as_ref() == Some(from) {
                marker.device = Some(to.clone());
            }
        }
    }
    plan.keep_originals();
}

/// The entities of `entities` that `value` names, anywhere in its text: a field that is one
/// (`"entity": "light.hall"`) or an expression that mentions one (`num('sensor.hall') < 30`).
pub(super) fn named_in(value: &Value, entities: &BTreeSet<EntityId>) -> BTreeSet<EntityId> {
    let mut found = BTreeSet::new();
    let mut look = |text: &str| {
        for entity in entities {
            if !occurrences(text, entity.as_str()).is_empty() {
                found.insert(entity.clone());
            }
        }
    };
    walk(value, &mut look);
    found
}

fn walk(value: &Value, look: &mut impl FnMut(&str)) {
    match value {
        Value::String(text) => look(text),
        Value::Array(values) => values.iter().for_each(|value| walk(value, look)),
        Value::Object(fields) => fields.values().for_each(|value| walk(value, look)),
        _ => {}
    }
}

/// Renames every whole mention of an entity in `pairs`, anywhere in `value`'s text. Keys are
/// left alone: they're the names of nodes and helpers, not entities. Says whether anything
/// changed.
pub(super) fn rewrite(value: &mut Value, pairs: &BTreeMap<EntityId, EntityId>) -> bool {
    match value {
        Value::String(text) => {
            let mut changed = false;
            for (was, now) in pairs {
                let at = occurrences(text, was.as_str());
                if at.is_empty() {
                    continue;
                }
                // From the end, so the places found first still point where they did.
                for start in at.into_iter().rev() {
                    text.replace_range(start..start + was.as_str().len(), now.as_str());
                }
                changed = true;
            }
            changed
        }
        Value::Array(values) => values
            .iter_mut()
            .fold(false, |changed, value| rewrite(value, pairs) | changed),
        Value::Object(fields) => fields
            .values_mut()
            .fold(false, |changed, value| rewrite(value, pairs) | changed),
        _ => false,
    }
}

/// Where `id` is mentioned whole in `text`: not as the start of a longer id
/// (`sensor.hall` in `sensor.hall_2`) or the end of one.
fn occurrences(text: &str, id: &str) -> Vec<usize> {
    let part_of_id = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
    text.match_indices(id)
        .filter(|(start, _)| {
            let before = text[..*start].chars().next_back();
            let after = text[start + id.len()..].chars().next();
            !before.is_some_and(|c| part_of_id(c) || c == '.') && !after.is_some_and(part_of_id)
        })
        .map(|(start, _)| start)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> BTreeSet<EntityId> {
        list.iter().map(|id| id.parse().expect("valid")).collect()
    }

    #[test]
    fn only_whole_mentions_are_renamed() {
        let pairs: BTreeMap<EntityId, EntityId> = [(
            "sensor.ha_temp".parse().expect("valid"),
            "sensor.esphome_temp".parse().expect("valid"),
        )]
        .into();
        let mut flow = json!({
            "nodes": {
                "sensor.ha_temp": { "entity": "sensor.ha_temp" },
                "warm": { "expr": "num('sensor.ha_temp') > 20 && num('sensor.ha_temp_2') > 0" },
                "other": { "expr": "num('binary_sensor.ha_temp') > 0 || x.sensor.ha_temp" },
            }
        });
        assert!(rewrite(&mut flow, &pairs));
        assert_eq!(
            flow,
            json!({
                "nodes": {
                    "sensor.ha_temp": { "entity": "sensor.esphome_temp" },
                    "warm": { "expr": "num('sensor.esphome_temp') > 20 && num('sensor.ha_temp_2') > 0" },
                    "other": { "expr": "num('binary_sensor.ha_temp') > 0 || x.sensor.ha_temp" },
                }
            })
        );
        assert!(!rewrite(&mut flow, &pairs), "nothing left to rename");
        assert!(named_in(&flow, &ids(&["sensor.ha_temp"])).is_empty());
        assert_eq!(
            named_in(&flow, &ids(&["sensor.esphome_temp", "light.x"])),
            ids(&["sensor.esphome_temp"])
        );
    }
}
