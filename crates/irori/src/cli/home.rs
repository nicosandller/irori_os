//! Devices, entities, rooms, floors, the plan, and the live socket.

use std::path::Path;

use irori_client::{Client, Home};
use irori_types::Entity;
use irori_types::{Availability, EntityState};
use serde_json::{Value, json};

use super::output::{self, explain};
use super::prompt;
use super::{
    args::{AreasCmd, DevicesCmd, EntitiesCmd, FloorplanCmd, FloorsCmd, Remote},
    connect,
};

fn err(error: irori_client::Error) -> String {
    explain(&error)
}

pub async fn devices(remote: &Remote, cmd: DevicesCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        DevicesCmd::List => {
            let home: Home = link.client.get("/api/home").await.map_err(err)?;
            output::show(link.json, &home_list(&home), || print_devices(&home))
        }
        DevicesCmd::Show { id } => {
            let home: Home = link.client.get("/api/home").await.map_err(err)?;
            let device = home
                .devices
                .iter()
                .find(|device| device.id.as_str() == id)
                .ok_or_else(|| format!("there's no device `{id}`"))?;
            output::show(link.json, device, || {
                println!(
                    "{}\t{}\t{}\t{}",
                    device.id,
                    device.name.as_str(),
                    device.protocol,
                    device.area_id.as_ref().map(|id| id.as_str()).unwrap_or("—")
                );
            })
        }
        DevicesCmd::Add { id } => {
            let body = json!({ "added": true });
            let value = link
                .client
                .patch::<Value>(&format!("/api/devices/{id}"), &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("added {id}"))
        }
        DevicesCmd::Rename { id, name } => {
            let body = json!({ "name": name });
            let value = link
                .client
                .patch::<Value>(&format!("/api/devices/{id}"), &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("renamed {id}"))
        }
        DevicesCmd::Area { id, area, none } => {
            if none == area.is_some() {
                return Err("pass a room's id, or --none, and not both".to_owned());
            }
            let body = if none {
                json!({ "area": false })
            } else {
                json!({ "area": area })
            };
            let value = link
                .client
                .patch::<Value>(&format!("/api/devices/{id}"), &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("moved {id}"))
        }
        DevicesCmd::Describe { id, text, clear } => {
            if clear == text.is_some() {
                return Err("pass the words, or --clear, and not both".to_owned());
            }
            let body = if clear {
                json!({ "description": null })
            } else {
                json!({ "description": text })
            };
            let value = link
                .client
                .patch::<Value>(&format!("/api/devices/{id}"), &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("described {id}"))
        }
        DevicesCmd::Remove { id, force } => {
            let home: Home = link.client.get("/api/home").await.map_err(err)?;
            let name = home
                .devices
                .iter()
                .find(|device| device.id.as_str() == id)
                .map(|device| device.name.as_str().to_owned())
                .unwrap_or_else(|| id.clone());
            let unpairs = home
                .extensions
                .get(&protocol_of(&home, &id))
                .and_then(|ext| ext.get("unpairs").and_then(Value::as_bool));
            let extra = if unpairs == Some(true) {
                " It is unpaired from its network; --force removes it even when that network doesn't answer."
            } else {
                ""
            };
            if !prompt::confirm(
                &format!("Remove {name} from the home?{extra}"),
                link.yes || force,
                link.json,
            )? {
                return output::done(link.json, "left it in the home");
            }
            let path = if force {
                format!("/api/devices/{id}?force=true")
            } else {
                format!("/api/devices/{id}")
            };
            link.client
                .empty(reqwest::Method::DELETE, &path, None::<&()>)
                .await
                .map_err(err)?;
            output::done(link.json, &format!("removed {id}"))
        }
    }
}

fn protocol_of(home: &Home, id: &str) -> String {
    home.devices
        .iter()
        .find(|device| device.id.as_str() == id)
        .map(|device| device.protocol.to_string())
        .unwrap_or_default()
}

fn home_list(home: &Home) -> Value {
    json!({
        "devices": home.devices,
        "held": home.held,
    })
}

fn print_devices(home: &Home) {
    if home.devices.is_empty() {
        println!("no devices in the home");
    }
    for device in &home.devices {
        println!(
            "{}\t{}\t{}\t{}",
            device.id,
            device.name.as_str(),
            device.protocol,
            device.area_id.as_ref().map(|id| id.as_str()).unwrap_or("—")
        );
    }
    if !home.held.is_empty() {
        println!("found:");
        for device in &home.held {
            println!(
                "{}\t{}\t{}",
                device.id,
                device.name.as_str(),
                device.protocol
            );
        }
    }
}

pub async fn entities(remote: &Remote, cmd: EntitiesCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        EntitiesCmd::List { device, area } => {
            let home: Home = link.client.get("/api/home").await.map_err(err)?;
            let rows: Vec<&Entity> = home
                .entities
                .iter()
                .filter(|entity| {
                    device.as_ref().is_none_or(|id| {
                        entity
                            .device_id
                            .as_ref()
                            .is_some_and(|device| device.as_str() == id)
                    }) && area
                        .as_ref()
                        .is_none_or(|id| entity_area(&home, entity) == Some(id.as_str()))
                })
                .collect();
            if link.json {
                let values: Vec<Value> = rows
                    .iter()
                    .map(|entity| {
                        json!({
                            "id": entity.id.as_str(),
                            "name": entity.name.as_str(),
                            "state": state_of(&home, entity.id.as_str()),
                        })
                    })
                    .collect();
                return output::json_out(&values);
            }
            if rows.is_empty() {
                println!("no entities");
            }
            for entity in rows {
                println!(
                    "{}\t{}\t{}",
                    entity.id,
                    entity.name.as_str(),
                    brief(state_of(&home, entity.id.as_str()))
                );
            }
            Ok(())
        }
        EntitiesCmd::Show { id } => {
            let home: Home = link.client.get("/api/home").await.map_err(err)?;
            let entity = home
                .entities
                .iter()
                .find(|entity| entity.id.as_str() == id)
                .ok_or_else(|| format!("there's no entity `{id}`"))?;
            let view = json!({
                "entity": entity,
                "state": state_of(&home, &id),
            });
            output::show(link.json, &view, || {
                println!("{}\t{}", entity.id, entity.name.as_str());
                println!("{}", brief(state_of(&home, &id)));
            })
        }
        EntitiesCmd::Rename { id, name } => {
            let value = link
                .client
                .patch::<Value>(&format!("/api/entities/{id}"), &json!({ "name": name }))
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("renamed {id}"))
        }
        EntitiesCmd::History { id, since } => {
            let history = link
                .client
                .get::<irori_client::History>(&format!("/api/history/{id}"))
                .await
                .map_err(err)?;
            let since = since
                .as_deref()
                .map(|text| text.parse::<irori_types::Timestamp>())
                .transpose()
                .map_err(|error| error.to_string())?;
            let states: Vec<&EntityState> = history
                .states
                .iter()
                .filter(|state| since.is_none_or(|since| state.last_changed >= since))
                .collect();
            if link.json {
                return output::json_out(&states);
            }
            for state in states {
                println!("{}\t{}", state.last_changed, brief(Some(state)));
            }
            Ok(())
        }
        EntitiesCmd::Summary { id, since } => {
            let summary = link
                .client
                .get::<irori_client::Summary>(&format!(
                    "/api/history/{id}/summary?since={}",
                    urlencoding(&since)
                ))
                .await
                .map_err(err)?;
            output::show(link.json, &summary, || {
                for point in &summary.points {
                    println!("{}\t{}", point.start, point.value);
                }
            })
        }
        EntitiesCmd::Call { id, command, data } => {
            let command = match command.as_str() {
                "on" => "turn_on",
                "off" => "turn_off",
                other => other,
            };
            let data = data
                .as_deref()
                .map(serde_json::from_str::<Value>)
                .transpose()
                .map_err(|error| format!("--data isn't JSON: {error}"))?;
            if let Some(Value::Object(_)) | None = &data {
            } else {
                return Err("--data is a JSON object".to_owned());
            }
            let mut body = json!({ "entity_id": id, "command": command });
            if let Some(data) = data {
                body["data"] = data;
            }
            let value = link
                .client
                .post::<Value>("/api/command", &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || {
                if let Ok(state) = serde_json::from_value::<EntityState>(value.clone()) {
                    println!("{}", brief(Some(&state)));
                } else {
                    println!("{}", serde_json::to_string(&value).unwrap_or_default());
                }
            })
        }
    }
}

fn entity_area<'a>(home: &'a Home, entity: &'a Entity) -> Option<&'a str> {
    entity.area_id.as_ref().map(|id| id.as_str()).or_else(|| {
        let device = entity.device_id.as_ref()?;
        home.devices
            .iter()
            .find(|item| &item.id == device)?
            .area_id
            .as_ref()
            .map(|id| id.as_str())
    })
}

fn state_of<'a>(home: &'a Home, id: &str) -> Option<&'a EntityState> {
    home.states
        .iter()
        .find(|state| state.entity_id.as_str() == id)
}

fn brief(state: Option<&EntityState>) -> String {
    let Some(state) = state else {
        return "unknown".to_owned();
    };
    if state.availability == Availability::Unavailable {
        return "unavailable".to_owned();
    }
    match &state.state {
        None => "unknown".to_owned(),
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "unknown".to_owned()),
    }
}

fn urlencoding(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub async fn areas(remote: &Remote, cmd: AreasCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        AreasCmd::List => {
            let areas = link
                .client
                .get::<Vec<irori_types::Area>>("/api/areas")
                .await
                .map_err(err)?;
            output::show(link.json, &areas, || {
                for area in &areas {
                    println!(
                        "{}\t{}\t{}",
                        area.id,
                        area.name.as_str(),
                        area.floor_id.as_ref().map(|id| id.as_str()).unwrap_or("—")
                    );
                }
            })
        }
        AreasCmd::Add { name, floor } => {
            let mut body = json!({ "name": name });
            if let Some(floor) = floor {
                body["floor"] = json!(floor);
            }
            let value = link
                .client
                .post::<Value>("/api/areas", &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("added a room"))
        }
        AreasCmd::Rename { id, name } => {
            let value = link
                .client
                .patch::<Value>(&format!("/api/areas/{id}"), &json!({ "name": name }))
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("renamed {id}"))
        }
        AreasCmd::Move { id, floor, none } => {
            if none == floor.is_some() {
                return Err("pass a floor's id, or --none, and not both".to_owned());
            }
            let body = if none {
                json!({ "floor": null })
            } else {
                json!({ "floor": floor })
            };
            let value = link
                .client
                .patch::<Value>(&format!("/api/areas/{id}"), &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("moved {id}"))
        }
        AreasCmd::Remove { id } => {
            if !prompt::confirm(&format!("Remove the room {id}?"), link.yes, link.json)? {
                return output::done(link.json, "left the room");
            }
            link.client
                .empty(
                    reqwest::Method::DELETE,
                    &format!("/api/areas/{id}"),
                    None::<&()>,
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("removed {id}"))
        }
    }
}

pub async fn floors(remote: &Remote, cmd: FloorsCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        FloorsCmd::List => {
            let floors = link
                .client
                .get::<Vec<irori_types::Floor>>("/api/floors")
                .await
                .map_err(err)?;
            output::show(link.json, &floors, || {
                for floor in &floors {
                    println!("{}\t{}\t{}", floor.id, floor.name.as_str(), floor.level);
                }
            })
        }
        FloorsCmd::Add { name, level } => {
            let value = link
                .client
                .post::<Value>("/api/floors", &json!({ "name": name, "level": level }))
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("added a floor"))
        }
        FloorsCmd::Edit { id, name, level } => {
            if name.is_none() && level.is_none() {
                return Err("pass --name or --level".to_owned());
            }
            let mut body = json!({});
            if let Some(name) = name {
                body["name"] = json!(name);
            }
            if let Some(level) = level {
                body["level"] = json!(level);
            }
            let value = link
                .client
                .patch::<Value>(&format!("/api/floors/{id}"), &body)
                .await
                .map_err(err)?;
            output::show(link.json, &value, || println!("edited {id}"))
        }
        FloorsCmd::Remove { id } => {
            if !prompt::confirm(&format!("Remove the floor {id}?"), link.yes, link.json)? {
                return output::done(link.json, "left the floor");
            }
            link.client
                .empty(
                    reqwest::Method::DELETE,
                    &format!("/api/floors/{id}"),
                    None::<&()>,
                )
                .await
                .map_err(err)?;
            output::done(link.json, &format!("removed {id}"))
        }
    }
}

pub async fn floorplan(remote: &Remote, cmd: FloorplanCmd) -> Result<(), String> {
    let link = connect::connect(remote)?;
    match cmd {
        FloorplanCmd::Get => {
            let plan = link
                .client
                .get::<irori_types::Floorplan>("/api/floorplan")
                .await
                .map_err(err)?;
            output::json_out(&plan)
        }
        FloorplanCmd::Put { file } => put_plan(&link.client, &file, link.json, link.yes).await,
    }
}

async fn put_plan(client: &Client, file: &Path, json: bool, yes: bool) -> Result<(), String> {
    if !prompt::confirm("Replace the floor plan with this file?", yes, json)? {
        return output::done(json, "left the plan");
    }
    let text = std::fs::read_to_string(file)
        .map_err(|error| format!("couldn't read {}: {error}", file.display()))?;
    let plan: Value =
        serde_json::from_str(&text).map_err(|error| format!("that file isn't JSON: {error}"))?;
    let saved = client
        .put::<Value>("/api/floorplan", &plan)
        .await
        .map_err(err)?;
    output::show(json, &saved, || {
        let placed = saved.get("placed").and_then(Value::as_u64).unwrap_or(0);
        println!("saved the plan; {placed} devices took the room they were drawn in");
    })
}

pub async fn watch(remote: &Remote) -> Result<(), String> {
    let link = connect::connect(remote)?;
    super::watch::run(&link.client, link.json).await
}
