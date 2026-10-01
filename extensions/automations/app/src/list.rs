//! Every flow, with whether it's running, how its last run went, and what it nearly did.

use std::time::Duration;

use irori_flow_types::api::{Armed, FlowSummary};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::time::ago;
use crate::widgets::Toggle;
use crate::{Route, api, go};

fn status(summary: &FlowSummary) -> impl IntoView + use<> {
    let (class, text) = match (&summary.armed, summary.active_runs) {
        (Armed::Armed, n) if n > 0 => ("chip live", format!("running ×{n}")),
        (Armed::Armed, _) => ("chip ok", "on".to_owned()),
        (Armed::Disabled, _) => ("chip", "off".to_owned()),
        (Armed::Unarmed { .. }, _) => ("chip error", "can't run".to_owned()),
    };
    view! { <span class=class><span class="dot"></span>{text}</span> }
}

#[component]
pub fn List() -> impl IntoView {
    let listing = RwSignal::new(None::<Result<api::Listing, String>>);
    let refresh = move || {
        spawn_local(async move {
            // The list may be gone by the time the answer comes.
            let _ = listing.try_set(Some(api::list().await));
        });
    };
    refresh();
    // A run finishing, or a flow file edited by hand, shows without reloading.
    let alive = StoredValue::new(true);
    spawn_local(async move {
        loop {
            gloo_timers::future::sleep(Duration::from_secs(3)).await;
            if !alive.try_get_value().unwrap_or(false) {
                return;
            }
            if let Ok(fresh) = api::list().await
                && let Some(shown) = listing.try_get_untracked()
                && shown.and_then(Result::ok).as_ref() != Some(&fresh)
            {
                let _ = listing.try_set(Some(Ok(fresh)));
            }
        }
    });
    on_cleanup(move || alive.set_value(false));

    view! {
        <section class="list">
            <div class="row">
                <h1 class="grow">"Automations"</h1>
                <button class="btn primary" on:click=move |_| go(Route::New)>"New flow"</button>
            </div>
            <p class="muted">
                "Flows start when something happens, check what they need to, and act. "
                "Open one to see its runs lit up on the canvas, and why it didn't run when it didn't."
            </p>
            {move || match listing.get() {
                None => view! { <p class="muted">"Loading…"</p> }.into_any(),
                Some(Err(why)) => view! { <div class="problems-box">{why}</div> }.into_any(),
                Some(Ok(found)) => {
                    let problems = found.file_problems.clone();
                    view! {
                        {(!problems.is_empty()).then(|| view! {
                            <div class="problems-box">
                                <strong>"Some flow files can't be read. "</strong>
                                "The last good version of each keeps running."
                                <ul>
                                    {problems.into_iter().map(|p| view! {
                                        <li><span class="mono">{p.file}</span>": "{p.reason}</li>
                                    }).collect_view()}
                                </ul>
                            </div>
                        })}
                        {if found.flows.is_empty() {
                            view! {
                                <div class="empty cards">
                                    <p><strong>"No flows yet."</strong></p>
                                    <p class="muted">
                                        "A flow is a picture of what should happen: a trigger, "
                                        "the checks it needs, and what to do."
                                    </p>
                                    <p><button class="btn primary" on:click=move |_| go(Route::New)>"Make the first one"</button></p>
                                </div>
                            }.into_any()
                        } else {
                            view! {
                                <div class="cards">
                                    {found.flows.into_iter().map(|flow| {
                                        let id = flow.id.to_string();
                                        let open = id.clone();
                                        let toggle = id.clone();
                                        let enabled = flow.enabled;
                                        let last = flow.last_run.as_ref().map(|run| {
                                            format!("last run {} · {}", ago(&run.started_at),
                                                if run.summary.is_empty() { run.outcome.clone() } else { run.summary.clone() })
                                        }).unwrap_or_else(|| "hasn't run yet".into());
                                        let misses = flow.near_misses;
                                        let problems = flow.problems;
                                        let reason = match &flow.armed {
                                            Armed::Unarmed { reason } => Some(reason.clone()),
                                            _ => None,
                                        };
                                        view! {
                                            <div class="flow-card" role="button" tabindex="0"
                                                on:click=move |_| go(Route::Flow(open.clone()))>
                                                <div class="row">
                                                    <span class="name">{flow.name.to_string()}</span>
                                                    {status(&flow)}
                                                    {(problems > 0).then(|| view! {
                                                        <span class="chip warn">{format!("{problems} to fix")}</span>
                                                    })}
                                                    {(misses > 0).then(|| view! {
                                                        <span class="chip">{format!("{misses} near-miss{}", if misses == 1 { "" } else { "es" })}</span>
                                                    })}
                                                </div>
                                                <Toggle
                                                    on=Signal::stored(enabled)
                                                    label=if enabled { "Turn off" } else { "Turn on" }
                                                    set=Callback::new(move |on: bool| {
                                                        let id = toggle.clone();
                                                        spawn_local(async move {
                                                            let _ = api::enable(&id, on).await;
                                                            let _ = listing.try_set(Some(api::list().await));
                                                        });
                                                    })
                                                />
                                                <div class="meta">
                                                    {format!("{} trigger{} · {} steps · {last}", flow.triggers,
                                                        if flow.triggers == 1 { "" } else { "s" }, flow.nodes)}
                                                    {reason.map(|r| view! { <div style="color: var(--error)">{r}</div> })}
                                                </div>
                                            </div>
                                        }
                                    }).collect_view()}
                                </div>
                            }.into_any()
                        }}
                    }.into_any()
                }
            }}
        </section>
    }
}
