//! Talking to the core over HTTP.
//!
//! These are the temporary `/api/dev/*` endpoints (`crates/irori/src/server.rs`), polled every
//! few seconds. The real API is a WebSocket that pushes changes, with typed messages shared
//! through `irori-types` (M0.5, M1.5); this module is what goes away then.

use std::collections::BTreeMap;

use gloo_net::http::Request;
use irori_types::{Device, Entity, EntityId, EntityState, ExtensionId};
use serde::{Deserialize, Serialize};

const HOME_URL: &str = "/api/dev/home";
const COMMAND_URL: &str = "/api/dev/command";

/// Everything the page shows. Mirrors `HomeView` on the server; the two meet again in
/// `irori-types` when the real API lands.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Home {
    pub devices: Vec<Device>,
    pub entities: Vec<Entity>,
    pub states: Vec<EntityState>,
    pub extensions: BTreeMap<ExtensionId, Extension>,
}

/// How an extension is faring. Deliberately loose: the server's `ExtensionStatus` gains variants
/// as Irori grows, and a status this page doesn't know yet should still show up as a word rather
/// than blank the whole page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Extension {
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
}

/// The browser's own words for a failed request ("TypeError: Failed to fetch") say nothing a
/// person can act on, so they go to the console and the page says what it means instead.
fn unreachable(error: gloo_net::Error) -> String {
    leptos::logging::error!("{error}");
    "Can't reach Irori. Is it still running?".to_owned()
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
}

#[derive(Debug, Deserialize)]
struct Refused {
    error: String,
}

/// Turns an entity on or off, and answers with its state once the integration confirms (or
/// `None` if it vanished meanwhile). Brightness and color follow when the page can set them.
pub async fn set_on(entity_id: &EntityId, on: bool) -> Result<Option<EntityState>, String> {
    let body = CommandRequest {
        entity_id,
        command: if on { "turn_on" } else { "turn_off" },
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
