//! Every flow, with whether it's running, how its last run went, and what it nearly did.

use std::collections::BTreeMap;
use std::time::Duration;

use irori_flow_types::api::{Armed, FlowSummary};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::time::ago;
use crate::widgets::{HandOverWarning, Toggle};
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
    // Kept here, not in the panel: the list below is drawn again whenever it changes (every few
    // seconds while flows run), and the panel would close and forget what it said.
    let home_assistant = HaPanel {
        open: RwSignal::new(false),
        found: RwSignal::new(None),
        results: RwSignal::new(BTreeMap::new()),
        note: RwSignal::new(None),
    };
    // A flow brought over from Home Assistant being turned on: the warning shown under it, and
    // what turning one on or off said, by flow id. Kept here for the same reason.
    let confirming = RwSignal::new(None::<String>);
    let handing = RwSignal::new(false);
    let said = RwSignal::new(BTreeMap::<String, Result<String, String>>::new());
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
                    let beside_home_assistant = found.home_assistant;
                    view! {
                        {beside_home_assistant.then(|| view! {
                            <FromHomeAssistant panel=home_assistant brought=Callback::new(move |()| {
                                // What turning a flow on said may not hold any more.
                                said.set(BTreeMap::new());
                                refresh();
                            }) />
                        })}
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
                                        let warned = id.clone();
                                        let told = id.clone();
                                        let enabled = flow.enabled;
                                        let twin = flow.home_assistant.clone();
                                        let twinned = twin.is_some();
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
                                                    {beside_home_assistant.then(|| view! {
                                                        <span class="chip" title="Irori runs this automation. Home Assistant's own automations are run by Home Assistant.">
                                                            "Run by Irori"
                                                        </span>
                                                    })}
                                                    {flow.home_assistant.as_ref().map(|twin| view! {
                                                        <span class="chip" title=format!(
                                                            "Brought over from Home Assistant's {twin}. While this flow is on, Irori keeps that one switched off in Home Assistant."
                                                        )>
                                                            "From Home Assistant"
                                                        </span>
                                                    })}
                                                    {flow.owner.as_ref().map(|owner| view! {
                                                        <span class="chip" title="Whose automation this is">
                                                            {format!("{}'s", owner.name)}
                                                        </span>
                                                    })}
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
                                                        // Turned on only once the owner read what that does in
                                                        // Home Assistant.
                                                        if on && twinned {
                                                            said.update(|all| { all.remove(&id); });
                                                            confirming.set(Some(id));
                                                            return;
                                                        }
                                                        spawn_local(async move {
                                                            let answer = api::enable(&id, on).await;
                                                            if twinned {
                                                                let note = answer.map(|done| api::still_waiting(&done).map_or_else(
                                                                    || "Turned off. Home Assistant runs its automation again.".to_owned(),
                                                                    |why| format!("Turned off. Home Assistant's automation will be switched back on as soon as it can be: {why}"),
                                                                ));
                                                                let _ = said.try_update(|all| all.insert(id, note));
                                                            }
                                                            let _ = listing.try_set(Some(api::list().await));
                                                        });
                                                    })
                                                />
                                                <div class="meta">
                                                    {format!("{} trigger{} · {} steps · {last}", flow.triggers,
                                                        if flow.triggers == 1 { "" } else { "s" }, flow.nodes)}
                                                    {reason.map(|r| view! { <div style="color: var(--error)">{r}</div> })}
                                                </div>
                                                {move || (confirming.get().as_deref() == Some(warned.as_str())).then(|| {
                                                    let id = warned.clone();
                                                    let automation = twin.clone().unwrap_or_default();
                                                    view! {
                                                        <HandOverWarning automation=automation busy=handing
                                                            confirm=Callback::new(move |()| {
                                                                let id = id.clone();
                                                                handing.set(true);
                                                                spawn_local(async move {
                                                                    let answer = api::enable_handing_over(&id).await.map(|done| {
                                                                        if done["already_off"] == true {
                                                                            "Turned on. Its automation was already off in Home Assistant, so it was left as it was.".to_owned()
                                                                        } else {
                                                                            "Turned on. Its automation is switched off in Home Assistant while this runs.".to_owned()
                                                                        }
                                                                    });
                                                                    let _ = handing.try_set(false);
                                                                    let _ = confirming.try_set(None);
                                                                    let _ = said.try_update(|all| all.insert(id, answer));
                                                                    let _ = listing.try_set(Some(api::list().await));
                                                                });
                                                            })
                                                            cancel=Callback::new(move |()| confirming.set(None)) />
                                                    }
                                                })}
                                                {move || said.with(|all| all.get(&told).cloned()).map(|note| match note {
                                                    Ok(note) => view! { <p class="meta" on:click=|e| e.stop_propagation()>{note}</p> }.into_any(),
                                                    Err(why) => view! { <div class="problems-box" on:click=|e| e.stop_propagation()>{why}</div> }.into_any(),
                                                })}
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

/// Home Assistant's own automations, to bring one over as a flow, and those Irori switched off
/// because their flow is on. Shown only in a home Home Assistant's devices came into. A flow
/// brought over arrives switched off, and Home Assistant keeps running its own until the owner
/// turns the flow on.
/// What the Home Assistant panel shows: whether it's open, the automations it found, and what
/// bringing each one over said, by `entity_id`.
#[derive(Clone, Copy)]
struct HaPanel {
    open: RwSignal<bool>,
    found: RwSignal<Option<Result<api::HaAutomations, String>>>,
    results: RwSignal<BTreeMap<String, Result<api::BroughtOver, String>>>,
    /// What Restore or Forget said, kept after the list is read again without that one.
    note: RwSignal<Option<Result<String, String>>>,
}

#[component]
fn FromHomeAssistant(panel: HaPanel, brought: Callback<()>) -> impl IntoView {
    let HaPanel {
        open,
        found,
        results,
        note,
    } = panel;
    let load = move || {
        found.set(None);
        spawn_local(async move {
            let _ = found.try_set(Some(api::ha_automations().await));
        });
    };
    view! {
        <div class="row">
            <p class="muted grow">
                "These flows are run by Irori. Home Assistant goes on running its own automations."
            </p>
            <button class="btn" on:click=move |_| {
                let opening = !open.get_untracked();
                open.set(opening);
                if opening {
                    results.set(BTreeMap::new());
                    note.set(None);
                    load();
                }
            }>{move || if open.get() { "Close" } else { "Bring over from Home Assistant" }}</button>
        </div>
        {move || open.get().then(|| view! {
            <div class="cards ha-import">
                <p>
                    <strong>"Home Assistant's automations. "</strong>
                    <span class="muted">
                        "Bring one over to try it in Irori. It arrives switched off, and Home "
                        "Assistant keeps running its own. Only when you turn the flow on does Irori "
                        "switch that one off in Home Assistant, after asking you. If anything in it "
                        "can't be done by a flow yet, nothing is brought over and you're told why."
                    </span>
                </p>
                {move || match found.get() {
                    None => view! { <p class="muted">"Asking Home Assistant…"</p> }.into_any(),
                    Some(Err(why)) => view! { <div class="problems-box">{why}</div> }.into_any(),
                    Some(Ok(all)) => {
                        let off = all.switched_off;
                        view! {
                            {move || note.get().map(|said| match said {
                                Ok(said) => view! { <p class="meta">{said}</p> }.into_any(),
                                Err(why) => view! { <div class="problems-box">{why}</div> }.into_any(),
                            })}
                            {(!off.is_empty()).then(|| view! {
                                <SwitchedOffList off=off note=note changed=Callback::new(move |()| {
                                    brought.run(());
                                    load();
                                }) />
                            })}
                            {if all.automations.is_empty() {
                                view! { <p class="muted">"Home Assistant has no automations."</p> }.into_any()
                            } else {
                                all.automations.into_iter().map(|automation| view! {
                                    <HaRow automation=automation results=results brought=Callback::new(move |()| {
                                        brought.run(());
                                        load();
                                    }) />
                                }).collect_view().into_any()
                            }}
                        }.into_any()
                    }
                }}
            </div>
        })}
    }
}

/// One of Home Assistant's automations, and what bringing it over did.
#[component]
fn HaRow(
    automation: api::HaAutomation,
    results: RwSignal<BTreeMap<String, Result<api::BroughtOver, String>>>,
    brought: Callback<()>,
) -> impl IntoView {
    let busy = RwSignal::new(false);
    let entity_id = automation.entity_id.clone();
    let result = {
        let entity_id = entity_id.clone();
        move || results.with(|all| all.get(&entity_id).cloned())
    };
    let bring = move |_| {
        let entity_id = entity_id.clone();
        busy.set(true);
        spawn_local(async move {
            let answer = api::bring_over(&entity_id).await;
            let made = matches!(&answer, Ok(done) if done.flow.is_some());
            let _ = busy.try_set(false);
            let _ = results.try_update(|all| all.insert(entity_id, answer));
            if made {
                brought.run(());
            }
        });
    };
    let state = if automation.on {
        "on in Home Assistant"
    } else {
        "off in Home Assistant"
    };
    let action = match (&automation.flow, automation.readable) {
        (Some(flow), _) => {
            let flow = flow.clone();
            view! {
                <span class="chip ok">"Brought over"</span>
                <button class="btn" on:click=move |_| go(Route::Flow(flow.clone()))>"Open"</button>
            }
            .into_any()
        }
        (None, false) => view! {
            <span class="muted" title="Home Assistant keeps no setup of an automation written in YAML without an id">
                "Can't be read: written in YAML without an id"
            </span>
        }
        .into_any(),
        (None, true) => view! {
            <button class="btn primary" disabled=move || busy.get() on:click=bring>
                {move || if busy.get() { "Bringing over…" } else { "Bring over" }}
            </button>
        }
        .into_any(),
    };
    view! {
        <div class="flow-card">
            <div class="row">
                <span class="name grow">{automation.name.clone()}</span>
                <span class="chip">{state}</span>
                {action}
            </div>
            <div class="meta mono">{automation.entity_id.clone()}</div>
            {move || result().map(|answer| match answer {
                Err(why) => view! { <div class="problems-box">{why}</div> }.into_any(),
                Ok(done) if !done.untranslatable.is_empty() => view! {
                    <div class="problems-box">
                        <strong>"It can't come over yet, so nothing was brought over. "</strong>
                        "Home Assistant keeps running it. What a flow can't do yet:"
                        <ul>
                            {done.untranslatable.into_iter().map(|why| view! { <li>{why}</li> }).collect_view()}
                        </ul>
                    </div>
                }.into_any(),
                Ok(done) => {
                    let flow = done.flow.unwrap_or_default();
                    let open = flow.clone();
                    view! {
                        <p>
                            {if done.already { "Brought over before, as " } else { "Brought over, switched off, as " }}
                            <a href="#" on:click=move |e| {
                                e.prevent_default();
                                go(Route::Flow(open.clone()));
                            }>{flow}</a>
                            ". Home Assistant keeps running its own; turning the flow on switches that one off there, after asking you."
                        </p>
                    }.into_any()
                }
            })}
        </div>
    }
}

/// What Irori switched off in Home Assistant (issue #199), each with a way back: Restore turns
/// its flow off and switches it back on there; Forget only takes it off the list, for one
/// switched back on in Home Assistant by hand.
#[component]
fn SwitchedOffList(
    off: Vec<api::SwitchedOff>,
    note: RwSignal<Option<Result<String, String>>>,
    changed: Callback<()>,
) -> impl IntoView {
    let busy = RwSignal::new(false);
    view! {
        <div class="hand-over">
            <p>
                <strong>"Switched off in Home Assistant by Irori. "</strong>
                <span class="muted">
                    "Each stays off while its flow runs in Irori, and while Irori is stopped. "
                    "Restore turns the flow off and switches the automation back on."
                </span>
            </p>
            {off.into_iter().map(|twin| {
                let restoring = twin.entity_id.clone();
                let forgetting = twin.entity_id.clone();
                let act = move |entity_id: String, restore: bool| {
                    busy.set(true);
                    spawn_local(async move {
                        let answer = if restore {
                            api::ha_restore(&entity_id).await.map(|done| api::still_waiting(&done).map_or_else(
                                || "Switched back on in Home Assistant.".to_owned(),
                                |why| format!("Its flow is off; Home Assistant will switch it back on as soon as it can: {why}"),
                            ))
                        } else {
                            api::ha_forget(&entity_id).await.map(|_| "Taken off the list.".to_owned())
                        };
                        let _ = busy.try_set(false);
                        let _ = note.try_set(Some(answer));
                        changed.run(());
                    });
                };
                let forget = act.clone();
                view! {
                    <div class="row">
                        <span class="name grow">{twin.name.clone()}" "<span class="meta mono">{twin.entity_id.clone()}</span></span>
                        {match (&twin.flow, twin.waiting) {
                            (Some(flow), _) => {
                                let flow = flow.clone();
                                let open = flow.clone();
                                view! {
                                    <a href="#" on:click=move |e| {
                                        e.prevent_default();
                                        go(Route::Flow(open.clone()));
                                    }>{format!("on in Irori as {flow}")}</a>
                                }.into_any()
                            }
                            (None, _) => view! { <span class="chip warn">"waiting to be switched back on"</span> }.into_any(),
                        }}
                        <button class="btn" disabled=move || busy.get()
                            on:click=move |_| act(restoring.clone(), true)>"Restore"</button>
                        <button class="btn" disabled=move || busy.get()
                            title="Only takes it off this list. For one you switched back on in Home Assistant yourself."
                            on:click=move |_| forget(forgetting.clone(), false)>"Forget"</button>
                    </div>
                }
            }).collect_view()}
        </div>
    }
}
