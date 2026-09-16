//! M0.8 spike, Leptos version: list every entity's live state, filterable, refreshed every few
//! seconds. The Dioxus version (`spikes/ui-dioxus`) does exactly the same.

use std::time::Duration;

use gloo_net::http::Request;
use irori_types::{Availability, EntityState};
use leptos::prelude::*;
use leptos::task::spawn_local;

const REFRESH: Duration = Duration::from_secs(5);
const STATES_URL: &str = "/api/dev/states";

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let (states, set_states) = signal(Vec::<EntityState>::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (filter, set_filter) = signal(String::new());

    spawn_local(async move {
        loop {
            match fetch_states().await {
                Ok(fetched) => {
                    set_states.set(fetched);
                    set_error.set(None);
                }
                Err(e) => set_error.set(Some(e)),
            }
            gloo_timers::future::sleep(REFRESH).await;
        }
    });

    let shown = move || {
        let needle = filter.get().to_lowercase();
        states
            .get()
            .into_iter()
            .filter(|state| state.entity_id.as_str().to_lowercase().contains(&needle))
            .collect::<Vec<_>>()
    };

    view! {
        <h1>"Devices"</h1>
        <input
            placeholder="Filter"
            prop:value=filter
            on:input:target=move |ev| set_filter.set(ev.target().value())
        />
        {move || error.get().map(|e| view! { <p class="muted">{e}</p> })}
        <ul>
            <For each=shown key=|state| state.entity_id.to_string() let:state>
                <li>
                    <span class="id">{state.entity_id.to_string()}</span>
                    <span>{describe(&state)}</span>
                </li>
            </For>
        </ul>
        <p class="muted">{move || format!("{} entities", shown().len())}</p>
    }
}

async fn fetch_states() -> Result<Vec<EntityState>, String> {
    Request::get(STATES_URL)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<EntityState>>()
        .await
        .map_err(|e| e.to_string())
}

/// The shared bit: both spikes render state the same way, using the core's own types.
fn describe(state: &EntityState) -> String {
    let value = state.state.as_ref().map_or_else(
        || "unknown".to_owned(),
        |state| serde_json::to_string(state).unwrap_or_default(),
    );
    match state.availability {
        Availability::Available => value,
        Availability::Unavailable => format!("{value} (offline)"),
    }
}
