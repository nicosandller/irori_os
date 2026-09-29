//! A flow's editor: the canvas in the middle, where a node opens up to be edited; what can be
//! added on the left; and a panel on the right for the flow's settings, its runs, tests, "why
//! didn't it fire?", and versions.

use std::time::Duration;

use irori_flow_types::api::{ActiveRun, Armed, Problem, Severity};
use irori_flow_types::trace::RunRecord;
use irori_flow_types::{Flow, NodeId};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::{Home, Route, api, canvas, go, inspector, model, panels};

/// What's selected on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub enum Selected {
    Nothing,
    Node(NodeId),
    Wire(usize),
}

/// What the canvas shows.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// The draft, to edit, with live runs on it.
    Edit,
    /// A run, drawn on the definition it ran; steps up to `upto` shown.
    Trace {
        record: Box<RunRecord>,
        flow: Box<Flow>,
        upto: usize,
    },
    /// The draft against another version.
    Diff { other: Box<Flow>, version: String },
}

/// The panel's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Flow,
    Runs,
    Test,
    Why,
    Versions,
}

/// Everything the editor's parts share.
#[derive(Debug, Clone, Copy)]
pub struct Editing {
    pub draft: RwSignal<Option<Flow>>,
    pub saved: RwSignal<Option<Flow>>,
    pub is_new: RwSignal<bool>,
    pub problems: RwSignal<Vec<Problem>>,
    pub selected: RwSignal<Selected>,
    pub view: RwSignal<View>,
    pub tab: RwSignal<Tab>,
    pub active: RwSignal<Vec<ActiveRun>>,
    pub armed: RwSignal<Armed>,
    pub message: RwSignal<Option<String>>,
    /// Ticks every second while something is running, for countdowns.
    pub tick: RwSignal<u64>,
}

impl Editing {
    /// Changes the draft. Only in edit view: a trace or a diff is looked at, not edited.
    pub fn edit(&self, f: impl FnOnce(&mut Flow)) {
        if !matches!(self.view.get_untracked(), View::Edit) {
            self.view.set(View::Edit);
        }
        self.draft.update(|draft| {
            if let Some(flow) = draft {
                f(flow);
            }
        });
    }

    pub fn id(&self) -> String {
        self.draft
            .with_untracked(|draft| draft.as_ref().map(|f| f.id.to_string()))
            .unwrap_or_default()
    }

    pub fn dirty(&self) -> bool {
        self.is_new.get() || self.draft.get() != self.saved.get()
    }

    /// Problems pinned to one node.
    pub fn problems_at(&self, node: &NodeId) -> Vec<Problem> {
        self.problems.with(|problems| {
            problems
                .iter()
                .filter(|p| p.node.as_ref() == Some(node))
                .cloned()
                .collect()
        })
    }
}

#[component]
pub fn Editor(id: String, is_new: bool) -> impl IntoView {
    let home = expect_context::<Home>();
    let ed = Editing {
        draft: RwSignal::new(None),
        saved: RwSignal::new(None),
        is_new: RwSignal::new(is_new),
        problems: RwSignal::new(Vec::new()),
        selected: RwSignal::new(Selected::Nothing),
        view: RwSignal::new(View::Edit),
        tab: RwSignal::new(Tab::Flow),
        active: RwSignal::new(Vec::new()),
        armed: RwSignal::new(Armed::Disabled),
        message: RwSignal::new(None),
        tick: RwSignal::new(0),
    };
    provide_context(ed);
    let failed = RwSignal::new(None::<String>);

    if is_new {
        ed.draft.set(Some(model::blank(&home)));
    } else {
        let id = id.clone();
        spawn_local(async move {
            match api::get(&id).await {
                Ok(detail) => {
                    ed.saved.set(Some(detail.flow.clone()));
                    ed.draft.set(Some(detail.flow));
                    ed.problems.set(detail.problems);
                    ed.armed.set(detail.armed);
                }
                Err(why) => failed.set(Some(why)),
            }
        });
    }

    // Every edit is checked against the home a moment after it's made.
    let generation = StoredValue::new(0u64);
    Effect::new(move |_| {
        let Some(flow) = ed.draft.get() else {
            return;
        };
        let mine = generation.get_value() + 1;
        generation.set_value(mine);
        spawn_local(async move {
            gloo_timers::future::sleep(Duration::from_millis(350)).await;
            if generation.try_get_value() != Some(mine) {
                return;
            }
            if let Ok(problems) = api::validate(&flow).await
                && generation.try_get_value() == Some(mine)
            {
                ed.problems.set(problems);
            }
        });
    });

    // Runs in progress, every second, and a clock for their countdowns.
    let alive = StoredValue::new(true);
    spawn_local(async move {
        loop {
            if !alive.try_get_value().unwrap_or(false) {
                return;
            }
            let id = ed.id();
            // The editor may be closed while the answer is on its way.
            if !id.is_empty()
                && !ed.is_new.get_untracked()
                && let Ok(active) = api::active(&id).await
                && ed
                    .active
                    .try_get_untracked()
                    .is_some_and(|shown| shown != active)
            {
                let _ = ed.active.try_set(active);
            }
            if ed.active.try_with_untracked(|runs| !runs.is_empty()) == Some(true) {
                let _ = ed.tick.try_update(|t| *t += 1);
            }
            gloo_timers::future::sleep(Duration::from_secs(1)).await;
        }
    });
    on_cleanup(move || alive.set_value(false));

    let save = move || {
        let Some(mut flow) = ed.draft.get_untracked() else {
            return;
        };
        spawn_local(async move {
            if ed.is_new.get_untracked() {
                // A new flow's id comes from its name, and mustn't be one that's taken.
                let taken: Vec<String> = api::list()
                    .await
                    .map(|l| l.flows.iter().map(|f| f.id.to_string()).collect())
                    .unwrap_or_default();
                let base = model::slug(flow.name.as_str());
                let id = if taken.contains(&base) {
                    (2..)
                        .map(|n| format!("{base}_{n}"))
                        .find(|id| !taken.contains(id))
                        .unwrap_or(base)
                } else {
                    base
                };
                match id.parse() {
                    Ok(id) => flow.id = id,
                    Err(error) => {
                        ed.message.set(Some(format!("{error}")));
                        return;
                    }
                }
            }
            match api::save(&flow).await {
                Ok(saved) => {
                    let errors = saved.problems.iter().filter(|p| p.is_error()).count();
                    ed.problems.set(saved.problems);
                    ed.saved.set(Some(flow.clone()));
                    ed.draft.set(Some(flow.clone()));
                    ed.message.set(Some(if errors == 0 {
                        "Saved. It's running.".into()
                    } else {
                        format!(
                            "Saved, but it won't run until {errors} problem{} {} fixed.",
                            if errors == 1 { "" } else { "s" },
                            if errors == 1 { "is" } else { "are" }
                        )
                    }));
                    if ed.is_new.get_untracked() {
                        ed.is_new.set(false);
                        go(Route::Flow(flow.id.to_string()));
                    }
                    if let Ok(detail) = api::get(flow.id.as_str()).await {
                        ed.armed.set(detail.armed);
                    }
                }
                Err(why) => ed.message.set(Some(why)),
            }
        });
    };

    // Drawn once the flow is there, and not again with every edit: that would start the canvas
    // over, and drop a drag halfway through.
    let loaded = Memo::new(move |_| ed.draft.with(Option::is_some));

    let status = move || {
        let errors = ed
            .problems
            .with(|p| p.iter().filter(|p| p.severity == Severity::Error).count());
        let warnings = ed
            .problems
            .with(|p| p.iter().filter(|p| p.severity == Severity::Warning).count());
        let running = ed.active.with(Vec::len);
        view! {
            {(running > 0).then(|| view! {
                <span class="chip live"><span class="dot"></span>{format!("running ×{running}")}</span>
            })}
            {match ed.armed.get() {
                _ if ed.is_new.get() => view! { <span class="chip">"not saved yet"</span> }.into_any(),
                Armed::Armed => view! { <span class="chip ok"><span class="dot"></span>"on"</span> }.into_any(),
                Armed::Disabled => view! { <span class="chip">"off"</span> }.into_any(),
                Armed::Unarmed { .. } => view! { <span class="chip error">"can't run"</span> }.into_any(),
            }}
            {(errors > 0).then(|| view! {
                <button class="chip error" on:click=move |_| {
                    ed.selected.set(Selected::Nothing);
                    ed.tab.set(Tab::Flow);
                }>{format!("{errors} to fix")}</button>
            })}
            {(warnings > 0).then(|| view! {
                <button class="chip warn" on:click=move |_| {
                    ed.selected.set(Selected::Nothing);
                    ed.tab.set(Tab::Flow);
                }>{format!("{warnings} warning{}", if warnings == 1 { "" } else { "s" })}</button>
            })}
        }
    };

    view! {
        {move || failed.get().map(|why| view! {
            <section class="list">
                <div class="problems-box">{why}</div>
                <p><button class="link" on:click=move |_| go(Route::List)>"Back to all flows"</button></p>
            </section>
        })}
        {move || loaded.get().then(|| view! {
            <div class="editor">
                <header class="bar">
                    <button class="btn small" on:click=move |_| go(Route::List) title="All flows">"←"</button>
                    <input
                        class="title"
                        type="text"
                        aria-label="Name"
                        prop:value=move || ed.draft.with(|d| d.as_ref().map(|f| f.name.to_string()).unwrap_or_default())
                        on:change=move |event| {
                            let name = event_target_value(&event);
                            if let Ok(name) = irori_types::Name::try_from(name.as_str()) {
                                ed.edit(|flow| flow.name = name);
                            }
                        }
                    />
                    <HeaderToggle />
                    {status}
                    <span class="grow"></span>
                    {move || ed.message.get().map(|m| view! { <span class="muted" style="font-size:.85rem">{m}</span> })}
                    <button class="btn" on:click=move |_| ed.tab.set(Tab::Test)>"Test"</button>
                    <button class="btn primary" disabled=move || !ed.dirty() on:click=move |_| save()>
                        {move || if ed.dirty() { "Save" } else { "Saved" }}
                    </button>
                </header>
                <div class="body">
                    <Palette />
                    <canvas::Canvas />
                    <aside class="panel">
                        <nav class="tabs">
                            {[(Tab::Flow, "Edit"), (Tab::Runs, "Runs"), (Tab::Test, "Test"), (Tab::Why, "Why?"), (Tab::Versions, "Versions")]
                                .into_iter()
                                .map(|(tab, label)| view! {
                                    <button class:on=move || ed.tab.get() == tab on:click=move |_| ed.tab.set(tab)>{label}</button>
                                })
                                .collect_view()}
                        </nav>
                        <div class="panel-body">
                            {move || match ed.tab.get() {
                                Tab::Flow => view! { <div class="tab"><inspector::FlowForm /></div> }.into_any(),
                                Tab::Runs => view! { <div class="tab"><panels::Runs /></div> }.into_any(),
                                Tab::Test => view! { <div class="tab"><panels::Test /></div> }.into_any(),
                                Tab::Why => view! { <div class="tab"><panels::Why /></div> }.into_any(),
                                Tab::Versions => view! { <div class="tab"><panels::Versions /></div> }.into_any(),
                            }}
                        </div>
                    </aside>
                </div>
            </div>
        })}
    }
}

/// Turns the flow on or off. A saved flow switches at once; a new one when it's saved.
#[component]
fn HeaderToggle() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let on = Signal::derive(move || ed.draft.with(|d| d.as_ref().is_some_and(|f| f.enabled)));
    let set = Callback::new(move |on: bool| {
        if ed.is_new.get_untracked() || ed.dirty() {
            ed.edit(|flow| flow.enabled = on);
            return;
        }
        let id = ed.id();
        spawn_local(async move {
            match api::enable(&id, on).await {
                Ok(_) => {
                    for signal in [ed.draft, ed.saved] {
                        signal.update(|f| {
                            if let Some(f) = f {
                                f.enabled = on;
                            }
                        });
                    }
                    if let Ok(detail) = api::get(&id).await {
                        ed.armed.set(detail.armed);
                    }
                }
                Err(why) => ed.message.set(Some(why)),
            }
        });
    });
    view! { <crate::widgets::Toggle on=on set=set label="On or off" /> }
}

/// What can be added: click one and it lands on the canvas, open.
#[component]
fn Palette() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let mut groups: Vec<&'static str> = Vec::new();
    for template in model::TEMPLATES {
        if !groups.contains(&template.group) {
            groups.push(template.group);
        }
    }
    view! {
        <nav class="palette" aria-label="Add a step">
            {groups.into_iter().map(|group| view! {
                <h3>{group}</h3>
                {model::TEMPLATES.iter().filter(|t| t.group == group).map(|template| {
                    let template = *template;
                    let colour = serde_json::from_value::<irori_flow_types::Node>((template.make)(&home))
                        .map(|node| model::family(&node))
                        .unwrap_or("var(--muted)");
                    view! {
                        <button on:click=move |_| {
                            let Ok(node) = serde_json::from_value::<irori_flow_types::Node>((template.make)(&home)) else {
                                return;
                            };
                            let mut added = None;
                            ed.edit(|flow| {
                                let id = model::fresh_id(flow, template.base);
                                let at = canvas::free_spot(flow);
                                flow.layout.insert(id.clone(), at);
                                flow.nodes.insert(id.clone(), node);
                                added = Some(id);
                            });
                            // It opens where it lands, ready to fill in.
                            if let Some(id) = added {
                                ed.selected.set(Selected::Node(id));
                            }
                        }>
                            <span class="swatch" style=format!("background:{colour}")></span>
                            {template.label}
                        </button>
                    }
                }).collect_view()}
            }).collect_view()}
        </nav>
    }
}
