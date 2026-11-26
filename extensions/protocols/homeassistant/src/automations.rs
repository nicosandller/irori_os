//! Home Assistant's own automations, read for the Automations extension so the owner can bring
//! one over as a flow (issue #198). Only the Automations extension may ask (this extension's
//! manifest, `answers`), and only two things, both reads:
//!
//! - [`LIST`]: the automations, from `get_states` (each one's name, whether it's on, and the id
//!   its setup is kept under), with the Home Assistant entities this extension brought over, so
//!   the asker can tell which Irori entity each one an automation names is.
//! - [`SETUP`]: one automation's setup, as Home Assistant keeps it, from its REST API
//!   (`GET /api/config/automation/config/<id>`; see [`crate::http`]).
//!
//! Nothing here changes Home Assistant: an automation stays as it is, running or not.

use serde_json::{Map, Value, json};

use crate::model::Bridged;

/// The extension that may ask.
pub const ASKER: &str = "automations";
/// The automations, and what was brought over.
pub const LIST: &str = "automations.list";
/// One automation's setup, by the `id` [`LIST`] gave.
pub const SETUP: &str = "automation.setup";
/// The most an automation's setup may be.
pub const SETUP_MOST: usize = 1_000_000;

/// What [`LIST`] answers: `automations` (each `entity_id`, `name`, `on`, and `id` when Home
/// Assistant keeps its setup where it can be read), and `entities`, each Home Assistant
/// `entity_id` brought over with its `unique_id` in Irori.
pub fn list(states: &Value, brought: &[Bridged]) -> Value {
    let mut automations: Vec<Value> = states
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|state| {
            let entity_id = state["entity_id"].as_str()?;
            if !entity_id.starts_with("automation.") {
                return None;
            }
            let attributes = state["attributes"].as_object();
            let name = attributes
                .and_then(|a| a.get("friendly_name"))
                .and_then(Value::as_str)
                .unwrap_or(entity_id);
            let mut automation = json!({
                "entity_id": entity_id,
                "name": name,
                "on": state["state"] == "on",
            });
            // Automations written in YAML without an `id` have no setup to read.
            if let Some(id) = attributes
                .and_then(|a| a.get("id"))
                .and_then(|id| match id {
                    Value::String(id) => Some(id.clone()),
                    Value::Number(id) => Some(id.to_string()),
                    _ => None,
                })
                .filter(|id| setup_path(id).is_ok())
            {
                automation["id"] = json!(id);
            }
            Some(automation)
        })
        .collect();
    automations.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
    });
    let entities: Map<String, Value> = brought
        .iter()
        .map(|b| {
            (
                b.entity_id.clone(),
                json!(b.description.unique_id.as_str()),
            )
        })
        .collect();
    json!({ "automations": automations, "entities": entities })
}

/// Where an automation's setup is read from, for its `id`. Home Assistant's ids are numbers or
/// slugs; anything else is refused, so the address can't name something else.
pub fn setup_path(id: &str) -> Result<String, String> {
    let fine = !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if fine {
        Ok(format!("/api/config/automation/config/{id}"))
    } else {
        Err(format!("`{id}` isn't an automation id Home Assistant gives"))
    }
}

/// The `id` a [`SETUP`] question names.
pub fn asked_id(params: &Value) -> Result<String, String> {
    params["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "say which automation: `id`".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automations_are_listed_with_where_their_setup_is() {
        let states = json!([
            { "entity_id": "automation.hall_light", "state": "on",
              "attributes": { "friendly_name": "Hall light", "id": "1700000000001" } },
            { "entity_id": "automation.yaml_one", "state": "off",
              "attributes": { "friendly_name": "A YAML one" } },
            { "entity_id": "automation.odd", "state": "on",
              "attributes": { "friendly_name": "Odd id", "id": "../services" } },
            { "entity_id": "light.hall", "state": "on", "attributes": {} },
        ]);
        let answer = list(&states, &[]);
        assert_eq!(
            answer["automations"],
            json!([
                { "entity_id": "automation.yaml_one", "name": "A YAML one", "on": false },
                { "entity_id": "automation.hall_light", "name": "Hall light", "on": true,
                  "id": "1700000000001" },
                { "entity_id": "automation.odd", "name": "Odd id", "on": true },
            ])
        );
        assert_eq!(answer["entities"], json!({}));
    }

    #[test]
    fn only_ids_home_assistant_gives_make_an_address() {
        assert_eq!(
            setup_path("1700000000001").as_deref(),
            Ok("/api/config/automation/config/1700000000001")
        );
        assert!(setup_path("hall_light-2").is_ok());
        for bad in ["", ".", "..", "a/b", "a?b", "a b", "a%2Fb", "a\r\nb"] {
            assert!(setup_path(bad).is_err(), "{bad:?}");
        }
        for good in ["1700000000001", "hall_light-2"] {
            assert!(
                crate::http::readable(&setup_path(good).expect("a path")),
                "{good}"
            );
        }
    }
}
