//! Home Assistant automations Irori switched off (issue #199): the twins of flows brought over
//! from Home Assistant that the owner turned on in Irori, so the two don't both run.
//!
//! The one exception to "never changes Home Assistant's setup", by the owner's choice:
//!
//! - [`HAND_OVER`]: the Automations extension asks, once the owner confirmed turning the flow
//!   on. Only an automation that is on is switched off (`automation.turn_off`), and it is kept on
//!   the restore list *before* that is sent. One the owner had already switched off in Home
//!   Assistant is left alone and never put on the list, so it is never switched on by Irori.
//! - [`GIVE_BACK`]: the flow was turned off or deleted. Only an automation on the restore list
//!   is switched back on (`automation.turn_on`); it's marked [`Held::releasing`] first, and
//!   leaves the list once Home Assistant has done it. Until then it is tried again at every
//!   connection.
//! - Removing this extension switches every one on the list back on first (the core asks,
//!   `tidies_up` in the manifest); if one can't be, the extension stays installed.
//! - [`LIST`] and [`FORGET`]: what's on the list, for the owner's page; and taking one off it
//!   without touching Home Assistant, for one the owner switched back on there themselves.
//!
//! The list is kept in the extension's store, so it survives restarts. While Irori is stopped,
//! an automation it switched off stays off in Home Assistant until Irori is back.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::is_automation;

/// The extension that may ask: the same one that brings automations over.
pub const ASKER: &str = crate::automations::ASKER;
/// Switch the twin off: `{ "entity_id": "automation.…" }`.
pub const HAND_OVER: &str = "twin.hand_over";
/// Switch the twin back on, if Irori switched it off: `{ "entity_id": "automation.…" }`.
pub const GIVE_BACK: &str = "twin.give_back";
/// What Irori switched off.
pub const LIST: &str = "twins.list";
/// Take one off the list without touching Home Assistant: `{ "entity_id": "automation.…" }`.
pub const FORGET: &str = "twin.forget";
/// Where the list is kept.
pub const STORED: &str = "twins";

/// Whether `method` is one of these.
pub fn asks(method: &str) -> bool {
    matches!(method, HAND_OVER | GIVE_BACK | LIST | FORGET)
}

/// One automation Irori switched off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub entity_id: String,
    /// Its name in Home Assistant when it was switched off.
    pub name: String,
    /// Being switched back on: the flow is off, Home Assistant hasn't done it yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub releasing: bool,
}

/// The automation a question names.
pub fn asked_entity(params: &Value) -> Result<String, String> {
    let entity_id = params["entity_id"]
        .as_str()
        .ok_or_else(|| "say which automation: `entity_id`".to_owned())?;
    if is_automation(entity_id) {
        Ok(entity_id.to_owned())
    } else {
        Err(format!("`{entity_id}` isn't a Home Assistant automation"))
    }
}

/// What [`LIST`] answers.
pub fn listed(held: &[Held]) -> Value {
    json!({ "twins": held })
}

/// Reads the list back from what was stored.
pub fn read(stored: Option<Value>) -> Vec<Held> {
    stored
        .and_then(|value| serde_json::from_value::<Vec<Held>>(value).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|held| is_automation(&held.entity_id))
        .collect()
}

/// The automation's state and name in `get_states`'s answer: `None` when Home Assistant has no
/// such automation.
pub fn state_of(states: &Value, entity_id: &str) -> Option<(bool, String)> {
    let state = states
        .as_array()?
        .iter()
        .find(|state| state["entity_id"] == entity_id)?;
    let name = state["attributes"]["friendly_name"]
        .as_str()
        .unwrap_or(entity_id)
        .to_owned();
    Some((state["state"] == "on", name))
}

/// The names of `held`, for a message.
pub fn names(held: &[Held]) -> String {
    held.iter()
        .map(|held| format!("“{}”", held.name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_comes_back_as_it_was_kept_and_nothing_else() {
        let held = vec![
            Held {
                entity_id: "automation.hall".into(),
                name: "Hall".into(),
                releasing: false,
            },
            Held {
                entity_id: "automation.porch".into(),
                name: "Porch".into(),
                releasing: true,
            },
        ];
        let kept = serde_json::to_value(&held).expect("json");
        assert_eq!(read(Some(kept)), held);
        assert_eq!(read(None), Vec::new());
        assert_eq!(read(Some(json!("odd"))), Vec::new());
        // Only automations are ever switched back on from the list, whatever is in the store.
        let odd = json!([{ "entity_id": "light.hall", "name": "Hall" }]);
        assert_eq!(read(Some(odd)), Vec::new());
    }

    #[test]
    fn only_an_automation_is_asked_about() {
        assert_eq!(
            asked_entity(&json!({ "entity_id": "automation.hall" })).as_deref(),
            Ok("automation.hall")
        );
        assert!(asked_entity(&json!({ "entity_id": "light.hall" })).is_err());
        assert!(asked_entity(&json!({})).is_err());
    }

    #[test]
    fn an_automations_state_is_read() {
        let states = json!([
            { "entity_id": "automation.hall", "state": "on",
              "attributes": { "friendly_name": "Hall light" } },
            { "entity_id": "automation.porch", "state": "off", "attributes": {} },
        ]);
        assert_eq!(
            state_of(&states, "automation.hall"),
            Some((true, "Hall light".to_owned()))
        );
        assert_eq!(
            state_of(&states, "automation.porch"),
            Some((false, "automation.porch".to_owned()))
        );
        assert_eq!(state_of(&states, "automation.none"), None);
    }
}
