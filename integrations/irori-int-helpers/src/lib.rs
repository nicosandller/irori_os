//! Helpers: values Irori keeps itself rather than a device reporting them, for rules to use —
//! a switch for "guests are over", say.
//!
//! Defined in `extensions/helpers.toml` in the config directory:
//!
//! ```toml
//! [toggles.guests_over]
//! name = "Guests are over"
//! initial = false          # what it is before anyone has switched it; optional
//! ```
//!
//! Each toggle is a `switch.<id>` entity with no device. Its value is kept in the integration's
//! own storage (`docs/specs/integrations.md` §5), so it survives Irori restarting — and this
//! integration restarting, which is how a new or removed toggle arrives.
//!
//! Only toggles so far. Numbers, text and timers need entity kinds Irori doesn't have yet, and
//! they come with rules (ROADMAP M1.4), which are what helpers are for.

use std::collections::BTreeMap;

use irori_integration::types::{
    Capabilities, EntityDescription, Name, ObjectId, Service, State, StateReport,
    SwitchCapabilities, SwitchState, UniqueId,
};
use irori_integration::{Integration, IntegrationContext, IntegrationError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;

/// The helpers integration.
#[derive(Debug)]
pub struct Helpers;

/// `extensions/helpers.toml`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// On/off values, by the id their entity gets (`switch.<id>`).
    #[serde(default)]
    pub toggles: BTreeMap<ObjectId, Toggle>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Toggle {
    pub name: Name,
    /// What it is until someone switches it.
    #[serde(default)]
    pub initial: bool,
}

impl Integration for Helpers {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: IntegrationContext) -> Result<(), IntegrationError> {
        run(settings, ctx).await
    }
}

/// Where the list of entities this integration last described is kept, so one whose toggle has
/// been deleted from the file can be removed rather than left behind.
const DESCRIBED: &str = "described";

fn unique_id(toggle: &ObjectId) -> Result<UniqueId, IntegrationError> {
    Ok(UniqueId::try_from(format!("toggle-{toggle}"))?)
}

fn value_key(toggle: &ObjectId) -> String {
    format!("toggle.{toggle}")
}

async fn run(settings: Settings, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
    // What was here last time and isn't now goes.
    let before: Vec<String> = ctx
        .load(DESCRIBED)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let mut toggles: BTreeMap<UniqueId, (ObjectId, bool)> = BTreeMap::new();
    for (id, toggle) in &settings.toggles {
        toggles.insert(unique_id(id)?, (id.clone(), toggle.initial));
    }
    for gone in before
        .iter()
        .filter(|unique| !toggles.keys().any(|kept| kept.as_str() == unique.as_str()))
    {
        let gone = UniqueId::try_from(gone.as_str())?;
        // Removing one that's already gone (a person deleted it by hand) is nothing to fail over.
        let _ = ctx.remove_entity(gone.clone()).await;
        if let Some(id) = gone.as_str().strip_prefix("toggle-") {
            ctx.forget(&format!("toggle.{id}")).await?;
        }
    }

    for (id, toggle) in &settings.toggles {
        ctx.describe_entity(EntityDescription {
            unique_id: unique_id(id)?,
            name: Some(toggle.name.clone()),
            device_unique_id: None,
            // `switch.guests_over`: a helper's id is chosen by a person, so it reads well in rules.
            suggested_object_id: Some(id.clone()),
            capabilities: Capabilities::Switch(SwitchCapabilities { device_class: None }),
        })
        .await?;
        let on = ctx
            .load(&value_key(id))
            .await?
            .and_then(|value| value.as_bool())
            .unwrap_or(toggle.initial);
        ctx.report_state(report(unique_id(id)?, on, None));
    }
    let described: Vec<String> = toggles.keys().map(|id| id.as_str().to_owned()).collect();
    ctx.store(DESCRIBED, serde_json::json!(described)).await?;

    while let Some(incoming) = ctx.next_call().await {
        let unique = incoming.call.unique_id.clone();
        let Some((id, _)) = toggles.get(&unique) else {
            let why = format!("`{unique}` isn't a helper any more");
            incoming.reply(Err(ServiceError::failed(why)));
            continue;
        };
        let on = match incoming.call.service {
            Service::SwitchTurnOn => true,
            Service::SwitchTurnOff => false,
            _ => {
                incoming.reply(Err(ServiceError::failed("a toggle only turns on and off")));
                continue;
            }
        };
        // Kept first: a value that was reported but not kept would come back wrong after a
        // restart, which is exactly what a helper mustn't do.
        if let Err(e) = ctx.store(&value_key(id), serde_json::json!(on)).await {
            incoming.reply(Err(ServiceError::failed(format!(
                "couldn't keep the value: {e}"
            ))));
            continue;
        }
        let caused_by = Some(incoming.call.context.id.clone());
        incoming.reply(Ok(()));
        ctx.report_state(report(unique, on, caused_by));
    }
    Ok(())
}

fn report(
    unique_id: UniqueId,
    on: bool,
    caused_by: Option<irori_integration::types::ContextId>,
) -> StateReport {
    StateReport {
        unique_id,
        state: Some(State::Switch(SwitchState { on })),
        attributes: BTreeMap::new(),
        caused_by,
    }
}
