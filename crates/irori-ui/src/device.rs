//! One device: everything Irori knows about it, and everything it provides.
//!
//! The list pages answer "what's going on"; this one answers "what is this thing" — which
//! integration brought it in, what it calls itself, what firmware it's running, and which
//! entities belong to it.

use irori_types::{Device, Entity, EntityState};
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::Home;
use crate::devices::{self, Controls};

#[component]
pub fn DevicePage() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();
    let params = use_params_map();

    move || {
        let home = live.home.get();
        let id = params.read().get("id").unwrap_or_default();
        let found = home
            .devices
            .iter()
            .find(|device| device.id.as_str() == id)
            .cloned();
        match found {
            None => missing(&id, &home).into_any(),
            Some(device) => {
                let entities = of_device(&home, &device);
                page(device, entities, controls).into_any()
            }
        }
    }
}

/// The device's entities, with their state, in the same order the list pages use.
fn of_device(home: &Home, device: &Device) -> Vec<(Entity, Option<EntityState>)> {
    devices::groups(home, "")
        .into_iter()
        .find(|group| group.device.as_ref().is_some_and(|d| d.id == device.id))
        .map(|group| group.entities)
        .unwrap_or_default()
}

fn page(
    device: Device,
    entities: Vec<(Entity, Option<EntityState>)>,
    controls: Controls,
) -> impl IntoView {
    let battery = devices::battery(&entities);
    let subtitle = [device.manufacturer.clone(), device.model.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    view! {
        <p class="crumb"><A href="/devices">"← All devices"</A></p>
        <div class="page-head">
            <h1>{device.name.to_string()}</h1>
        </div>
        <p class="lede">{subtitle}</p>

        <section class="card">
            <h2>"What it is"</h2>
            <dl>
                <dt>"Through"</dt>
                <dd>{device.integration.to_string()}</dd>
                <dt>"Known to it as"</dt>
                // For an ESPHome device this is its MAC address; every integration picks
                // something of its own that survives a rename.
                <dd>{device.unique_id.to_string()}</dd>
                <dt>"Irori's id"</dt>
                <dd>{device.id.to_string()}</dd>
                {device.manufacturer.clone().map(|make| view! {
                    <dt>"Make"</dt>
                    <dd>{make}</dd>
                })}
                {device.model.clone().map(|model| view! {
                    <dt>"Model"</dt>
                    <dd>{model}</dd>
                })}
                {device.sw_version.clone().map(|version| view! {
                    <dt>"Firmware"</dt>
                    <dd>{version}</dd>
                })}
                {device.hw_version.clone().map(|version| view! {
                    <dt>"Hardware"</dt>
                    <dd>{version}</dd>
                })}
                {battery.map(|level| view! {
                    <dt>"Battery"</dt>
                    <dd>{level}</dd>
                })}
                {device.via_device_id.clone().map(|via| view! {
                    <dt>"Reached through"</dt>
                    <dd><A href=format!("/devices/{via}")>{via.to_string()}</A></dd>
                })}
            </dl>
            {device.sw_version.as_ref().map(|_| view! {
                <p class="muted small">
                    "Irori can read the firmware version but can't install updates yet; see the "
                    "roadmap (M1.8)."
                </p>
            })}
        </section>

        <section class="card">
            <h2>
                {format!(
                    "{} entit{}",
                    entities.len(),
                    if entities.len() == 1 { "y" } else { "ies" },
                )}
            </h2>
            {if entities.is_empty() {
                view! {
                    <p class="muted">
                        "Nothing Irori can model yet. The device may provide kinds it doesn't "
                        "know about, which are left out rather than guessed at."
                    </p>
                }
                .into_any()
            } else {
                entities
                    .into_iter()
                    .map(|(entity, state)| devices::row(entity, state, controls))
                    .collect_view()
                    .into_any()
            }}
        </section>
    }
}

/// A device id that isn't here: either mistyped, or one that has gone away since the link was
/// made. Both are worth saying plainly.
fn missing(id: &str, home: &Home) -> impl IntoView {
    let known = !home.devices.is_empty();
    view! {
        <section class="card">
            <h1>"No such device"</h1>
            <p class="muted">
                {if known {
                    format!("Irori has no device called `{id}`. It may have been removed.")
                } else {
                    "Irori hasn't heard from any devices yet.".to_owned()
                }}
            </p>
            <p><A href="/devices">"All devices"</A></p>
        </section>
    }
}
