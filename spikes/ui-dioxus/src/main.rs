//! M0.8 spike, Dioxus version: list every entity's live state, filterable, refreshed every few
//! seconds. The Leptos version (`spikes/ui-leptos`) does exactly the same.

use std::time::Duration;

use dioxus::prelude::*;
use gloo_net::http::Request;
use irori_types::{Availability, EntityState};

const REFRESH: Duration = Duration::from_secs(5);
const STATES_URL: &str = "/api/dev/states";

fn main() {
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut states = use_signal(Vec::<EntityState>::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut filter = use_signal(String::new);

    use_future(move || async move {
        loop {
            match fetch_states().await {
                Ok(fetched) => {
                    states.set(fetched);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
            gloo_timers::future::sleep(REFRESH).await;
        }
    });

    let shown = use_memo(move || {
        let needle = filter().to_lowercase();
        states()
            .into_iter()
            .filter(|state| state.entity_id.as_str().to_lowercase().contains(&needle))
            .collect::<Vec<_>>()
    });

    rsx! {
        h1 { "Devices" }
        input {
            placeholder: "Filter",
            value: "{filter}",
            oninput: move |ev| filter.set(ev.value()),
        }
        if let Some(e) = error() {
            p { class: "muted", "{e}" }
        }
        ul {
            for state in shown() {
                li { key: "{state.entity_id}",
                    span { class: "id", "{state.entity_id}" }
                    span { "{describe(&state)}" }
                }
            }
        }
        p { class: "muted", "{shown().len()} entities" }
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
