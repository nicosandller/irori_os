//! The panel's other tabs: runs (and a run played back on the canvas), tests and the backtest,
//! "why didn't it fire?", and versions.

use std::collections::BTreeMap;
use std::time::Duration;

use irori_flow_types::api::{Backtest, RunSummary, TestRequest, Timeline, VersionEntry};
use irori_flow_types::trace::{NearMiss, RunRecord, Step};
use irori_flow_types::{Flow, NodeId};
use irori_types::EntityId;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};

use crate::editor::{Editing, Selected, Tab, View};
use crate::widgets::{Choice, Combo};
use crate::{Home, api, model, time};

/// Shows `record` on the canvas, drawn on the definition it ran: `flow` if given (a test of a
/// draft), else its version's.
pub fn show_run(ed: Editing, record: RunRecord, flow: Option<Flow>) {
    spawn_local(async move {
        let flow = match flow {
            Some(flow) => flow,
            None => {
                let current = ed
                    .saved
                    .get_untracked()
                    .or_else(|| ed.draft.get_untracked());
                match current {
                    Some(current) if current.version() == record.version => current,
                    _ => match api::version(record.flow_id.as_str(), &record.version).await {
                        Ok(VersionEntry {
                            flow: Some(flow), ..
                        }) => flow,
                        _ => match current {
                            Some(current) => current,
                            None => return,
                        },
                    },
                }
            }
        };
        let upto = record.steps.len().saturating_sub(1);
        ed.view.set(View::Trace {
            record: Box::new(record),
            flow: Box::new(flow),
            upto,
        });
        ed.tab.set(Tab::Runs);
    });
}

fn outcome_chip(summary: &RunSummary) -> impl IntoView + use<> {
    let class = match summary.outcome.as_str() {
        "completed" | "stopped" => "chip ok",
        "running" => "chip live",
        "error" => "chip error",
        _ => "chip warn",
    };
    let test = summary
        .test
        .map(|t| format!(" · test ({t:?})").to_lowercase())
        .unwrap_or_default();
    view! { <span class=class>{format!("{}{test}", summary.outcome)}</span> }
}

#[component]
pub fn Runs() -> impl IntoView {
    let ed = expect_context::<Editing>();
    move || match ed.view.get() {
        View::Trace { record, upto, .. } => view! { <Trace record=*record upto=upto /> }.into_any(),
        _ => view! { <RunList /> }.into_any(),
    }
}

#[component]
fn RunList() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let runs = RwSignal::new(None::<Result<Vec<RunSummary>, String>>);
    let misses = RwSignal::new(Vec::<NearMiss>::new());
    let alive = StoredValue::new(true);
    spawn_local(async move {
        loop {
            if !alive.try_get_value().unwrap_or(false) {
                return;
            }
            let id = ed.id();
            if ed.is_new.get_untracked() {
                runs.set(Some(Ok(Vec::new())));
            } else {
                let fresh = api::runs(&id).await;
                if runs.get_untracked() != Some(fresh.clone()) {
                    runs.set(Some(fresh));
                }
                if let Ok(fresh) = api::near_misses(&id).await
                    && fresh != misses.get_untracked()
                {
                    misses.set(fresh);
                }
            }
            gloo_timers::future::sleep(Duration::from_secs(3)).await;
        }
    });
    on_cleanup(move || alive.set_value(false));

    let open = move |run_id: String| {
        let id = ed.id();
        spawn_local(async move {
            if let Ok(record) = api::run(&id, &run_id).await {
                show_run(ed, record, None);
            }
        });
    };

    view! {
        {move || (!ed.active.with(Vec::is_empty)).then(|| view! {
            <h2>"Going now"</h2>
            {ed.active.get().into_iter().map(|run| {
                let run_id = run.record.run_id.to_string();
                let cancel_id = run_id.clone();
                let record = run.record.clone();
                let where_ = run.at.iter().map(|t| format!("{} ({})", t.node, t.doing)).collect::<Vec<_>>().join(", ");
                view! {
                    <div class="item on" on:click=move |_| show_run(ed, record.clone(), None)>
                        <div class="row"><span class="chip live"><span class="dot"></span>"running"</span>
                            <span class="when grow">{format!("since {}", time::clock(&run.record.started_at))}</span>
                            <button class="btn small danger" on:click=move |e| {
                                e.stop_propagation();
                                let id = cancel_id.clone();
                                spawn_local(async move { let _ = api::cancel(&id).await; });
                            }>"Cancel"</button>
                        </div>
                        <div>{format!("at {where_}")}</div>
                    </div>
                }
            }).collect_view()}
        })}
        <h2>"Runs"</h2>
        {move || match runs.get() {
            None => view! { <p class="muted">"Loading…"</p> }.into_any(),
            Some(Err(why)) => view! { <div class="problem">{why}</div> }.into_any(),
            Some(Ok(runs)) if runs.is_empty() => view! {
                <p class="muted">"No runs yet. Test it, or wait for its trigger."</p>
            }.into_any(),
            Some(Ok(runs)) => runs.into_iter().take(60).map(|run| {
                let run_id = run.run_id.to_string();
                let summary = run.summary.clone();
                view! {
                    <div class="item" on:click=move |_| open(run_id.clone())>
                        <div class="row">{outcome_chip(&run)}<span class="when grow">{time::when(&run.started_at)}</span>
                            <span class="mono muted">{run.trigger.to_string()}</span></div>
                        {(!summary.is_empty()).then(|| view! { <div>{summary}</div> })}
                    </div>
                }
            }).collect_view().into_any(),
        }}
        <h2 style="margin-top:1rem">"Near-misses"</h2>
        <p class="muted" style="font-size:.8rem">"Times it nearly ran: refused because a run was going, a hold that reset, a sensor that dropped off."</p>
        {move || {
            let misses = misses.get();
            if misses.is_empty() {
                view! { <p class="muted">"None."</p> }.into_any()
            } else {
                misses.into_iter().take(40).map(|miss| {
                    let node = miss.node.clone();
                    view! {
                        <div class="item" on:click=move |_| ed.selected.set(Selected::Node(node.clone()))>
                            <div class="row"><span class="chip warn">{format!("{:?}", miss.kind).to_lowercase()}</span>
                                <span class="when grow">{time::when(&miss.at)}</span></div>
                            <div>{miss.message}</div>
                        </div>
                    }
                }).collect_view().into_any()
            }
        }}
    }
}

fn step_line(step: &Step) -> String {
    let port = step.port.map(|p| format!(" → {p}")).unwrap_or_default();
    format!("{}{}", step.node, port)
}

/// A run, step by step, with a scrubber that plays it back on the canvas.
#[component]
fn Trace(record: RunRecord, upto: usize) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let summary = record.summary();
    let steps = record.steps.clone();
    let last = steps.len().saturating_sub(1);
    let chosen = RwSignal::new(upto.min(last));
    let set_upto = move |index: usize| {
        chosen.set(index);
        ed.view.update(|view| {
            if let View::Trace { upto, .. } = view {
                *upto = index;
            }
        });
    };
    let detail_steps = steps.clone();
    let detail = move || detail_steps.get(chosen.get()).cloned();
    view! {
        <div class="row"><h2 class="grow">"A run"</h2>
            <button class="btn small" on:click=move |_| ed.view.set(View::Edit)>"Close"</button></div>
        <div class="row">{outcome_chip(&summary)}<span class="when">{time::when(&record.started_at)}</span></div>
        {(!summary.summary.is_empty()).then(|| view! { <p>{summary.summary.clone()}</p> })}
        {(!record.assumptions.is_empty()).then(|| view! {
            <div class="problem warning">
                <strong>"This was a dry run. It assumed:"</strong>
                <ul>{record.assumptions.iter().map(|a| view! { <li>{a.clone()}</li> }).collect_view()}</ul>
            </div>
        })}
        <label>{move || format!("Step {} of {}", chosen.get() + 1, last + 1)}</label>
        <input class="scrubber" type="range" min="0" max=last.to_string()
            prop:value=move || chosen.get().to_string()
            on:input=move |e| set_upto(event_target_value(&e).parse().unwrap_or(0)) />
        <div class="row" style="margin:.3rem 0">
            <button class="btn small" on:click=move |_| set_upto(chosen.get_untracked().saturating_sub(1))>"‹ Back"</button>
            <button class="btn small" on:click=move |_| set_upto((chosen.get_untracked() + 1).min(last))>"Next ›"</button>
        </div>
        {steps.iter().enumerate().map(|(i, step)| {
            let line = step_line(step);
            let note = step.note.clone().unwrap_or_default();
            let at = time::clock(&step.at);
            view! {
                <div class="item" class:on=move || chosen.get() == i on:click=move |_| set_upto(i)>
                    <div class="row"><strong class="mono">{format!("{} ", step.seq)}</strong>
                        <span class="grow">{line}</span><span class="when">{at}</span></div>
                    {(!note.is_empty()).then(|| view! { <div class="muted">{note}</div> })}
                </div>
            }
        }).collect_view()}
        {move || detail().map(|step| view! {
            <h2 style="margin-top:1rem">{format!("Step {}: {}", step.seq, step.node)}</h2>
            <dl class="kv">
                <dt>"started"</dt><dd>{time::clock(&step.at)}</dd>
                {step.finished_at.map(|f| view! { <dt>"finished"</dt><dd>{time::clock(&f)}</dd> })}
                {step.port.map(|p| view! { <dt>"went"</dt><dd>{p.to_string()}</dd> })}
                {step.via.as_ref().map(|w| view! { <dt>"came from"</dt><dd class="mono">{w.from.to_string()}</dd> })}
            </dl>
            {(!step.reads.is_empty()).then(|| view! {
                <label>"What it read"</label>
                <dl class="kv">{step.reads.iter().map(|read| {
                    let value = match (&read.availability, &read.value) {
                        (None, _) => "not in the home".to_owned(),
                        (Some(irori_types::Availability::Unavailable), _) => "unavailable".to_owned(),
                        (_, serde_json::Value::Null) => "unknown".to_owned(),
                        (_, serde_json::Value::Bool(b)) => if *b { "on".into() } else { "off".into() },
                        (_, other) => other.to_string(),
                    };
                    view! { <dt>{home.name(&read.entity_id)}</dt><dd>{value}</dd> }
                }).collect_view()}</dl>
            })}
            {step.call.as_ref().map(|call| view! {
                <label>{if call.simulated { "The call (recorded, not sent)" } else { "The call" }}</label>
                <dl class="kv">
                    <dt>"asked"</dt><dd>{format!("{} {}", call.service, home.name(&call.entity_id))}</dd>
                    {call.data.as_ref().map(|d| view! { <dt>"with"</dt><dd class="mono">{d.to_string()}</dd> })}
                    <dt>"answer"</dt><dd>{match &call.result {
                        None => "waiting…".to_owned(),
                        Some(Ok(())) => "done".to_owned(),
                        Some(Err(e)) => e.clone(),
                    }}</dd>
                </dl>
            })}
        })}
    }
}

/// What a test does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TestMode {
    /// The draft, with nothing switched and time skipping ahead.
    #[default]
    Dry,
    /// The saved flow, fired now, for real.
    Trigger,
    /// The draft over the last day of history.
    Backtest,
}

/// A value to pretend an entity has, in a dry run.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct Pretend {
    entity: String,
    value: serde_json::Value,
}

/// How a flow is tested, kept by the engine so it runs the same way next time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct TestSettings {
    mode: TestMode,
    trigger: Option<NodeId>,
    pretend: Vec<Pretend>,
}

const MODES: [(TestMode, &str, &str); 3] = [
    (TestMode::Dry, "dry", "Dry run"),
    (TestMode::Trigger, "trigger", "Trigger now"),
    (TestMode::Backtest, "backtest", "Backtest"),
];

#[component]
pub fn Test() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let triggers = move || {
        ed.draft.with(|d| {
            d.as_ref()
                .map(|f| {
                    f.nodes
                        .iter()
                        .filter(|(_, n)| n.is_trigger())
                        .map(|(id, n)| (id.clone(), model::sentence(n, &home)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    let settings = RwSignal::new(TestSettings::default());
    let result = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let backtest = RwSignal::new(None::<Result<Backtest, String>>);

    // What was set up last time, then every change kept for next time.
    let loaded = RwSignal::new(ed.is_new.get_untracked());
    if !ed.is_new.get_untracked() {
        let id = ed.id();
        spawn_local(async move {
            if let Ok(value) = api::test_settings(&id).await
                && let Ok(kept) = serde_json::from_value::<TestSettings>(value)
                // Changed while this was on its way: what they just set wins.
                && settings.try_with_untracked(|s| *s == TestSettings::default()) == Some(true)
            {
                let _ = settings.try_set(kept);
            }
            let _ = loaded.try_set(true);
        });
    }
    Effect::new(move |before: Option<TestSettings>| {
        let now = settings.get();
        if loaded.get()
            && !ed.is_new.get_untracked()
            && before.as_ref().is_some_and(|b| b != &now)
            && let Ok(value) = serde_json::to_value(&now)
        {
            let id = ed.id();
            spawn_local(async move {
                let _ = api::save_test_settings(&id, &value).await;
            });
        }
        now
    });
    let mode = move || settings.with(|s| s.mode);

    let run = move || {
        let Some(flow) = ed.draft.get_untracked() else {
            return;
        };
        let kept = settings.get_untracked();
        let Some(trigger) = kept
            .trigger
            .filter(|t| flow.nodes.contains_key(t))
            .or_else(|| triggers().into_iter().next().map(|(id, _)| id))
        else {
            result.set(Some("add a trigger first".into()));
            return;
        };
        let dry = kept.mode == TestMode::Dry;
        if !dry && ed.dirty() {
            result.set(Some("save first: triggering runs the saved flow".into()));
            return;
        }
        let pretend: BTreeMap<EntityId, serde_json::Value> = if dry {
            kept.pretend
                .iter()
                .filter(|p| !p.value.is_null())
                .filter_map(|p| Some((p.entity.parse().ok()?, p.value.clone())))
                .collect()
        } else {
            BTreeMap::new()
        };
        let request = TestRequest {
            flow: dry.then(|| flow.clone()),
            id: flow.id.clone(),
            trigger,
            dry,
            overrides: pretend,
        };
        busy.set(true);
        spawn_local(async move {
            match api::test(&request).await {
                Ok(record) => {
                    let _ = result.try_set(None);
                    show_run(ed, record, dry.then_some(flow));
                }
                Err(why) => {
                    let _ = result.try_set(Some(why));
                }
            }
            let _ = busy.try_set(false);
        });
    };

    let run_backtest = move || {
        let draft = ed.draft.get_untracked();
        let id = ed.id();
        let dirty = ed.dirty();
        backtest.set(None);
        busy.set(true);
        spawn_local(async move {
            let answer = api::backtest(&id, if dirty { draft.as_ref() } else { None }).await;
            let _ = backtest.try_set(Some(answer));
            let _ = busy.try_set(false);
        });
    };

    let picker = move || {
        let all = triggers();
        let chosen = settings.with(|s| s.trigger.clone());
        match all.len() {
            0 => view! { <p class="problem">"Add a trigger first."</p> }.into_any(),
            1 => view! {
                <label>"Starts from"</label>
                <p class="starts">{all[0].1.clone()}</p>
            }
            .into_any(),
            _ => view! {
                <label>"Starts from"</label>
                <select on:change=move |e| {
                    let picked = event_target_value(&e).parse().ok();
                    settings.update(|s| s.trigger = picked);
                }>
                    {all.into_iter().map(|(id, words)| {
                        let selected = chosen.as_ref() == Some(&id);
                        view! { <option value=id.to_string() selected=selected>{words}</option> }
                    }).collect_view()}
                </select>
            }
            .into_any(),
        }
    };

    let body = move || {
        match mode() {
        TestMode::Dry => view! {
            <div class="mode-body">
            <p class="muted mode-note">"Runs your draft with nothing switched, and waits skip ahead. Pretend values first to try a situation."</p>
            {picker}
            <PretendList settings=settings />
            <div class="row" style="margin-top:.9rem">
                <button class="btn primary" disabled=move || busy.get() on:click=move |_| run()>"Dry run"</button>
            </div>
            </div>
        }
        .into_any(),
        TestMode::Trigger => view! {
            <div class="mode-body">
            <p class="muted mode-note">"Fires the saved flow now, for real, as if its trigger had happened: devices switch."</p>
            {picker}
            {move || ed.dirty().then(|| view! { <p class="problem warning">"Save your changes first: this runs the saved flow."</p> })}
            <div class="row" style="margin-top:.9rem">
                <button class="btn primary" disabled=move || busy.get() || ed.dirty() on:click=move |_| run()>"Trigger now"</button>
            </div>
            </div>
        }
        .into_any(),
        TestMode::Backtest => view! {
            <div class="mode-body">
            <p class="muted mode-note">"What this version would have done over the last day of history, next to what the saved flow actually did."</p>
            <button class="btn primary" disabled=move || busy.get() || ed.is_new.get() on:click=move |_| run_backtest()>"Backtest the last 24 hours"</button>
            {move || backtest.get().map(|answer| match answer {
                Err(why) => view! { <div class="problem">{why}</div> }.into_any(),
                Ok(result) => view! { <BacktestResult result=result /> }.into_any(),
            })}
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <h2>"Try it"</h2>
        <div
            class="modes"
            role="radiogroup"
            aria-label="How to test"
            style=move || format!("--at: {}", MODES.iter().position(|(m, _, _)| *m == mode()).unwrap_or(0))
        >
            <span class="modes-thumb" data-mode=move || MODES.iter().find(|(m, _, _)| *m == mode()).map_or("dry", |(_, key, _)| *key)></span>
            {MODES.into_iter().map(|(which, key, words)| view! {
                <button
                    type="button"
                    role="radio"
                    data-mode=key
                    class:on=move || mode() == which
                    aria-checked=move || (mode() == which).to_string()
                    on:click=move |_| {
                        result.set(None);
                        settings.update(|s| s.mode = which);
                    }
                >{words}</button>
            }).collect_view()}
        </div>
        {body}
        {move || result.get().map(|r| view! { <div class="problem">{r}</div> })}
    }
}

/// The values a dry run pretends, each on an entity this flow uses.
#[component]
fn PretendList(settings: RwSignal<TestSettings>) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let choices = Signal::derive(move || {
        let used = ed
            .draft
            .with(|d| d.as_ref().map(model::entities_used).unwrap_or_default());
        home.entities.with(|entities| {
            entities
                .iter()
                .filter(|e| used.contains(&e.id))
                .map(|e| {
                    let now = home
                        .states
                        .with(|states| states.get(&e.id).map(|s| model::state_words(s, &home)))
                        .map(|now| format!(" · {now}"))
                        .unwrap_or_default();
                    Choice::new(e.id.to_string(), e.name.to_string())
                        .detail(format!("{}{now}", e.id))
                })
                .collect::<Vec<_>>()
        })
    });
    view! {
        <label>"Pretend"</label>
        {move || {
            let rows = settings.with(|s| s.pretend.clone());
            if rows.is_empty() {
                return view! { <p class="muted" style="font-size:.85rem;margin:.2rem 0">"Everything as it is now."</p> }.into_any();
            }
            rows.into_iter().enumerate().map(|(i, row)| {
                let entity = row.entity.clone();
                view! {
                    <div class="pretend">
                        <div class="row">
                            <div class="grow">
                                <Combo
                                    choices=choices
                                    value=Signal::stored(row.entity.clone())
                                    placeholder="One of this flow's devices…"
                                    pick=Callback::new(move |id: String| settings.update(|s| {
                                        if let Some(row) = s.pretend.get_mut(i) {
                                            row.entity = id;
                                            row.value = serde_json::Value::Null;
                                        }
                                    }))
                                />
                            </div>
                            <button class="btn small" title="Don't pretend this" on:click=move |_| settings.update(|s| {
                                if i < s.pretend.len() { s.pretend.remove(i); }
                            })>"×"</button>
                        </div>
                        {(!entity.is_empty()).then(|| view! {
                            <div class="pretend-value">
                                <span class="muted">"is"</span>
                                <div class="grow">
                                    <crate::inspector::ValueInput entity=entity value=row.value.clone() allow_any=false
                                        pick=move |value| settings.update(|s| {
                                            if let Some(row) = s.pretend.get_mut(i) { row.value = value; }
                                        }) />
                                </div>
                            </div>
                        })}
                    </div>
                }
            }).collect_view().into_any()
        }}
        <button class="btn small" on:click=move |_| settings.update(|s| s.pretend.push(Pretend::default()))>"Pretend a value…"</button>
    }
}

#[component]
fn BacktestResult(result: Backtest) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let from = time::millis(result.from);
    let span = (time::millis(result.to) - from).max(1) as f64;
    let place = move |at: &irori_types::Timestamp| {
        format!(
            "left: {:.2}%",
            (time::millis(*at) - from) as f64 / span * 100.0
        )
    };
    let would = result.would.clone();
    let covered = format!("{} → {}", time::when(&result.from), time::when(&result.to));
    view! {
        <div class="legend">{format!("{} changes replayed · covers {covered}", result.changes)}</div>
        <div class="timeline">
            <div class="lane would">{result.would.iter().map(|r| view! { <span class="mark" style=place(&r.started_at) title=time::when(&r.started_at)></span> }).collect_view()}</div>
            <div class="lane did">{result.did.iter().map(|r| view! { <span class="mark" style=place(&r.started_at) title=time::when(&r.started_at)></span> }).collect_view()}</div>
        </div>
        <div class="legend">{format!("green: would have run ({}) · blue: did run ({})", result.would.len(), result.did.len())}</div>
        {would.into_iter().rev().take(30).map(|record| {
            let summary = record.summary();
            let text = if summary.summary.is_empty() { summary.outcome.clone() } else { summary.summary.clone() };
            let flow = ed.draft.get_untracked();
            view! {
                <div class="item" on:click=move |_| show_run(ed, record.clone(), flow.clone())>
                    <div class="row">{outcome_chip(&summary)}<span class="when">{time::when(&summary.started_at)}</span></div>
                    <div>{text}</div>
                </div>
            }
        }).collect_view()}
    }
}

/// "Why didn't it fire?": everything around a moment — what the flow watches changing, its
/// near-misses, and its runs, in one timeline.
#[component]
pub fn Why() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let now = time::now();
    let from = RwSignal::new(time::local_input(&time::from_millis(
        time::millis(now) - 3_600_000,
    )));
    let to = RwSignal::new(time::local_input(&time::from_millis(
        time::millis(now) + 60_000,
    )));
    let answer = RwSignal::new(None::<Result<Timeline, String>>);
    let look = move || {
        let (Some(from), Some(to)) = (
            time::parse_local(&from.get_untracked()),
            time::parse_local(&to.get_untracked()),
        ) else {
            return;
        };
        let id = ed.id();
        spawn_local(async move {
            answer.set(Some(api::timeline(&id, from, to).await));
        });
    };
    look();

    #[derive(Clone)]
    enum Row {
        Change(irori_types::EntityState),
        Miss(NearMiss),
        Run(RunSummary),
    }

    view! {
        <h2>"Why didn't it fire?"</h2>
        <p class="muted" style="font-size:.85rem">
            "Pick when it should have. Here's everything this flow watches changing then, the times it nearly ran, and the runs."
        </p>
        <label>"From"</label>
        <input type="datetime-local" prop:value=move || from.get() on:change=move |e| from.set(event_target_value(&e)) />
        <label>"To"</label>
        <input type="datetime-local" prop:value=move || to.get() on:change=move |e| to.set(event_target_value(&e)) />
        <div class="row" style="margin-top:.5rem"><button class="btn" on:click=move |_| look()>"Look"</button></div>
        {move || answer.get().map(|answer| match answer {
            Err(why) => view! { <div class="problem">{why}</div> }.into_any(),
            Ok(timeline) => {
                let mut rows: Vec<(irori_types::Timestamp, Row)> = Vec::new();
                rows.extend(timeline.changes.into_iter().map(|c| (c.last_updated, Row::Change(c))));
                rows.extend(timeline.near_misses.into_iter().map(|m| (m.at, Row::Miss(m))));
                rows.extend(timeline.runs.into_iter().map(|r| (r.started_at, Row::Run(r))));
                rows.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
                if rows.is_empty() {
                    return view! {
                        <p class="muted">"Nothing it watches changed then, and it didn't run or nearly run. If it should have, its trigger never saw the change it waits for."</p>
                    }.into_any();
                }
                rows.into_iter().take(200).map(|(at, row)| match row {
                    Row::Change(state) => view! {
                        <div class="item" style="cursor:default">
                            <span class="when">{time::clock(&at)}</span>" "
                            {format!("{} → {}", home.name(&state.entity_id), model::state_words(&state, &home))}
                        </div>
                    }.into_any(),
                    Row::Miss(miss) => view! {
                        <div class="item" style="border-color:var(--warn)">
                            <span class="when">{time::clock(&at)}</span>" "<strong>"near-miss: "</strong>{miss.message}
                        </div>
                    }.into_any(),
                    Row::Run(run) => {
                        let run_id = run.run_id.to_string();
                        let summary = run.summary.clone();
                        view! {
                            <div class="item" style="border-color:var(--ok)" on:click=move |_| {
                                let id = ed.id();
                                let run_id = run_id.clone();
                                spawn_local(async move {
                                    if let Ok(record) = api::run(&id, &run_id).await { show_run(ed, record, None); }
                                });
                            }>
                                <span class="when">{time::clock(&at)}</span>" "<strong>"ran: "</strong>{summary}
                            </div>
                        }.into_any()
                    }
                }).collect_view().into_any()
            }
        })}
    }
}

#[component]
pub fn Versions() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let versions = RwSignal::new(None::<Result<Vec<VersionEntry>, String>>);
    let load = move || {
        let id = ed.id();
        spawn_local(async move {
            versions.set(Some(api::versions(&id).await));
        });
    };
    if !ed.is_new.get_untracked() {
        load();
    }
    let current = move || ed.saved.get().map(|f| f.version()).unwrap_or_default();
    view! {
        <h2>"Versions"</h2>
        <p class="muted" style="font-size:.85rem">"Every saved definition is kept. Moving nodes around isn't a new version."</p>
        {move || match versions.get() {
            None => view! { <p class="muted">"Save it first."</p> }.into_any(),
            Some(Err(why)) => view! { <div class="problem">{why}</div> }.into_any(),
            Some(Ok(list)) => list.into_iter().map(|entry| {
                let version = entry.version.clone();
                let compare = version.clone();
                let restore = version.clone();
                let is_current = version == current();
                view! {
                    <div class="item" style="cursor:default">
                        <div class="row"><span class="mono">{version[..10.min(version.len())].to_owned()}</span>
                            <span class="when grow">{time::when(&entry.saved_at)}</span>
                            {is_current.then(|| view! { <span class="chip ok">"current"</span> })}</div>
                        {(!is_current).then(move || view! {
                            <div class="row" style="margin-top:.3rem">
                                <button class="btn small" on:click=move |_| {
                                    let id = ed.id();
                                    let version = compare.clone();
                                    spawn_local(async move {
                                        if let Ok(VersionEntry { flow: Some(flow), .. }) = api::version(&id, &version).await {
                                            ed.view.set(View::Diff { other: Box::new(flow), version });
                                        }
                                    });
                                }>"Compare"</button>
                                <button class="btn small" on:click=move |_| {
                                    let id = ed.id();
                                    let version = restore.clone();
                                    spawn_local(async move {
                                        if api::restore(&id, &version).await.is_ok()
                                            && let Ok(detail) = api::get(&id).await
                                        {
                                            ed.saved.set(Some(detail.flow.clone()));
                                            ed.draft.set(Some(detail.flow));
                                            ed.view.set(View::Edit);
                                            ed.message.set(Some("Restored.".into()));
                                            load();
                                        }
                                    });
                                }>"Restore"</button>
                            </div>
                        })}
                    </div>
                }
            }).collect_view().into_any(),
        }}
    }
}
