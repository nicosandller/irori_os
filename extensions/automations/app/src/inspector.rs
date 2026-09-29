//! The panel's Edit tab: the selected node's form, a wire, or — with nothing selected — the
//! flow's own settings and everything the check found.
//!
//! Forms edit the node's JSON and read it back through the same types the engine uses, so a
//! value the engine wouldn't take can't get into the draft; the form says why instead.

use irori_flow_types::api::Severity;
use irori_flow_types::{Flow, Node, NodeId};
use irori_types::{EntityKind, SensorValueType};
use leptos::prelude::*;
use serde_json::{Value, json};

use crate::canvas::remove_node;
use crate::editor::{Editing, Selected};
use crate::widgets::{Choice, Combo, Toggle};
use crate::{Home, model};

/// Rewrites the node's JSON with `f`, and keeps it if it's still a node. `Err` says why not.
fn change(ed: &Editing, id: &NodeId, f: impl FnOnce(&mut Value)) -> Result<(), String> {
    let Some(node) = ed
        .draft
        .with_untracked(|d| d.as_ref().and_then(|flow| flow.nodes.get(id).cloned()))
    else {
        return Err("that node is gone".into());
    };
    let mut value = serde_json::to_value(&node).map_err(|e| e.to_string())?;
    f(&mut value);
    let node: Node = serde_json::from_value(value).map_err(|e| e.to_string())?;
    ed.edit(|flow| {
        flow.nodes.insert(id.clone(), node);
    });
    Ok(())
}

fn get<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |v, key| &v[*key])
}

fn set(value: &mut Value, path: &[&str], new: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut here = value;
    for key in parents {
        if !here[*key].is_object() {
            here[*key] = json!({});
        }
        here = &mut here[*key];
    }
    if let Value::Object(map) = here {
        if new.is_null() {
            map.remove(*last);
        } else {
            map.insert((*last).to_owned(), new);
        }
    }
}

#[component]
pub fn Inspector() -> impl IntoView {
    let ed = expect_context::<Editing>();
    move || match ed.selected.get() {
        Selected::Node(id) => {
            let exists = ed
                .draft
                .with(|d| d.as_ref().is_some_and(|f| f.nodes.contains_key(&id)));
            if exists {
                view! { <NodeForm id=id /> }.into_any()
            } else {
                view! { <FlowForm /> }.into_any()
            }
        }
        Selected::Wire(index) => view! { <WireForm index=index /> }.into_any(),
        Selected::Nothing => view! { <FlowForm /> }.into_any(),
    }
}

#[component]
fn WireForm(index: usize) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let wire = ed
        .draft
        .with_untracked(|d| d.as_ref().and_then(|f| f.wires.get(index).cloned()));
    view! {
        <h2>"Wire"</h2>
        {wire.map(|wire| view! {
            <p>{format!("From {} ({}) to {}", wire.from.node, wire.from.port, wire.to)}</p>
            <button class="btn danger" on:click=move |_| {
                ed.edit(|flow| { if index < flow.wires.len() { flow.wires.remove(index); } });
                ed.selected.set(Selected::Nothing);
            }>"Remove wire"</button>
        })}
    }
}

/// The flow's own settings, and every problem the check found.
#[component]
fn FlowForm() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let flow = move || ed.draft.get();
    let mode_kind = move || {
        flow()
            .map(|f| serde_json::to_value(&f.mode).unwrap_or_default())
            .map(|v| match v {
                Value::String(s) => s,
                Value::Object(o) => o
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("single")
                    .to_owned(),
                _ => "single".into(),
            })
            .unwrap_or_else(|| "single".into())
    };
    let mode_max = move || {
        flow()
            .and_then(|f| serde_json::to_value(&f.mode).ok())
            .and_then(|v| v.get("max").and_then(Value::as_u64))
            .unwrap_or(4)
    };
    let set_mode = move |kind: String, max: u64| {
        let value = match kind.as_str() {
            "queued" | "parallel" => json!({ "type": kind, "max": max.clamp(1, 32) }),
            other => json!(other),
        };
        if let Ok(mode) = serde_json::from_value(value) {
            ed.edit(|flow| flow.mode = mode);
        }
    };
    view! {
        <h2>"This flow"</h2>
        <p class="muted" style="font-size:.85rem">
            "Select a node to edit it, or add one from the left. Wire a node's port to another node to say what happens next."
        </p>
        <label>"Description"</label>
        <input type="text"
            prop:value=move || flow().and_then(|f| f.description.map(|d| d.to_string())).unwrap_or_default()
            on:change=move |e| {
                let text = event_target_value(&e);
                let description = if text.trim().is_empty() { None } else { irori_types::Description::try_from(text.as_str()).ok() };
                ed.edit(|flow| flow.description = description);
            } />
        <label>"When it's triggered while already running"</label>
        <select on:change=move |e| set_mode(event_target_value(&e), mode_max())
            prop:value=mode_kind>
            <option value="single">"Ignore the new trigger (single)"</option>
            <option value="restart">"Start over (restart)"</option>
            <option value="queued">"Run it after (queued)"</option>
            <option value="parallel">"Run both (parallel)"</option>
        </select>
        {move || matches!(mode_kind().as_str(), "queued" | "parallel").then(|| view! {
            <label>"At most"</label>
            <input type="number" min="1" max="32" prop:value=move || mode_max().to_string()
                on:change=move |e| set_mode(mode_kind(), event_target_value(&e).parse().unwrap_or(4)) />
        })}
        <label>"Enabled"</label>
        <div class="row">
            <Toggle
                on=Signal::derive(move || flow().is_some_and(|f| f.enabled))
                set=Callback::new(move |on: bool| ed.edit(|flow| flow.enabled = on))
                label="Enabled"
            />
            <span class="muted" style="font-size:.85rem">"Off keeps the flow and its history; nothing runs."</span>
        </div>
        <h2 style="margin-top:1.2rem">"Checked against your home"</h2>
        {move || {
            let problems = ed.problems.get();
            if problems.is_empty() {
                view! { <p class="chip ok"><span class="dot"></span>"Nothing to fix"</p> }.into_any()
            } else {
                problems.into_iter().map(|problem| {
                    let class = if problem.severity == Severity::Error { "problem" } else { "problem warning" };
                    let node = problem.node.clone();
                    view! {
                        <div class=class on:click=move |_| {
                            if let Some(node) = node.clone() { ed.selected.set(Selected::Node(node)); }
                        }>
                            {problem.node.as_ref().map(|n| view! { <strong class="mono">{n.to_string()}": "</strong> })}
                            {problem.message}
                        </div>
                    }
                }).collect_view().into_any()
            }
        }}
        <DeleteFlow />
        <details style="margin-top:1rem">
            <summary class="muted">"The whole flow as JSON"</summary>
            <JsonEditor
                text=move || flow().and_then(|f| serde_json::to_string_pretty(&f).ok()).unwrap_or_default()
                apply=move |text: String| {
                    let flow: Flow = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    ed.edit(|draft| *draft = flow);
                    Ok(())
                }
            />
        </details>
    }
}

/// Deleting takes two clicks: the frame can't show a confirmation dialog, and a flow is gone
/// once deleted (its versions and runs stay in the extension's data).
#[component]
fn DeleteFlow() -> impl IntoView {
    let ed = expect_context::<Editing>();
    let asked = RwSignal::new(false);
    view! {
        {move || (!ed.is_new.get()).then(|| view! {
            <div class="row" style="margin-top:1rem">
                {move || if asked.get() {
                    view! {
                        <button class="btn danger" on:click=move |_| {
                            let id = ed.id();
                            leptos::task::spawn_local(async move {
                                match crate::api::delete(&id).await {
                                    Ok(_) => crate::go(crate::Route::List),
                                    Err(why) => ed.message.set(Some(why)),
                                }
                            });
                        }>"Yes, delete it"</button>
                        <button class="btn" on:click=move |_| asked.set(false)>"Keep it"</button>
                    }.into_any()
                } else {
                    view! { <button class="btn danger" on:click=move |_| asked.set(true)>"Delete this flow"</button> }.into_any()
                }}
            </div>
        })}
    }
}

#[component]
fn JsonEditor(
    text: impl Fn() -> String + Send + Sync + 'static,
    apply: impl Fn(String) -> Result<(), String> + Send + Sync + 'static,
) -> impl IntoView {
    let error = RwSignal::new(None::<String>);
    let draft = RwSignal::new(None::<String>);
    view! {
        <textarea rows="12"
            prop:value=move || draft.get().unwrap_or_else(&text)
            on:input=move |e| draft.set(Some(event_target_value(&e)))></textarea>
        <div class="row" style="margin-top:.3rem">
            <button class="btn small" on:click=move |_| {
                if let Some(text) = draft.get_untracked() {
                    match apply(text) {
                        Ok(()) => { error.set(None); draft.set(None); }
                        Err(why) => error.set(Some(why)),
                    }
                }
            }>"Apply"</button>
            {move || error.get().map(|e| view! { <span style="color:var(--error);font-size:.8rem">{e}</span> })}
        </div>
    }
}

/// Entities to choose from, those of `kinds` (all when empty), searched as you type. The current
/// one is kept even if it's gone, so the form can say so.
#[component]
fn EntityPicker(
    value: String,
    kinds: Vec<EntityKind>,
    pick: impl Fn(String) + Send + Sync + 'static,
) -> impl IntoView {
    let home = expect_context::<Home>();
    let current = value.clone();
    let choices = Signal::derive(move || {
        let mut choices: Vec<Choice> = home.entities.with(|entities| {
            entities
                .iter()
                .filter(|e| kinds.is_empty() || kinds.contains(&e.id.kind()))
                .map(|e| {
                    let now = home
                        .states
                        .with(|states| states.get(&e.id).map(model::state_words))
                        .map(|now| format!(" · {now}"))
                        .unwrap_or_default();
                    Choice::new(e.id.to_string(), e.name.to_string())
                        .detail(format!("{}{now}", e.id))
                })
                .collect()
        });
        choices.sort_by_key(|choice| choice.label.to_lowercase());
        if !current.is_empty() && !choices.iter().any(|c| c.value == current) {
            choices.insert(
                0,
                Choice::new(current.clone(), current.clone()).detail("not in your home"),
            );
        }
        choices
    });
    view! {
        <Combo
            choices=choices
            value=Signal::stored(value)
            pick=Callback::new(pick)
            placeholder="Search your devices…"
        />
    }
}

/// A value an entity can have — on/off for flags, a number or text for sensors — searched or
/// typed in the same kind of field.
#[component]
fn ValueInput(
    entity: String,
    value: Value,
    allow_any: bool,
    pick: impl Fn(Value) + Send + Sync + Clone + 'static,
) -> impl IntoView {
    let home = expect_context::<Home>();
    let sensor_type = home.entities.with_untracked(|entities| {
        entities
            .iter()
            .find(|e| e.id.as_str() == entity)
            .and_then(|e| match &e.capabilities {
                irori_types::Capabilities::Sensor(s) => Some(s.value_type),
                _ => None,
            })
    });
    let now = entity.parse::<irori_types::EntityId>().ok().and_then(|id| {
        home.states
            .with_untracked(|states| states.get(&id).map(model::state_words))
    });
    match sensor_type {
        Some(kind) => {
            let shown = match &value {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let mut choices = Vec::new();
            if let Some(now) = now.filter(|n| n != "unknown" && n != "unavailable") {
                choices.push(Choice::new(now.clone(), now).detail("its value now"));
            }
            if allow_any {
                choices.insert(0, Choice::new("", "any change"));
            }
            view! {
                <Combo
                    choices=Signal::stored(choices)
                    value=Signal::stored(shown)
                    custom=true
                    placeholder=if kind == SensorValueType::Number { "Type a number…" } else { "Type a value…" }
                    pick=Callback::new(move |text: String| {
                        let text = text.trim().to_owned();
                        pick(if text.is_empty() {
                            Value::Null
                        } else if kind == SensorValueType::Number {
                            text.parse::<f64>().map(|n| json!(n)).unwrap_or(Value::Null)
                        } else {
                            json!(text)
                        });
                    })
                />
            }
            .into_any()
        }
        None => {
            let shown = match value {
                Value::Bool(true) => "on",
                Value::Bool(false) => "off",
                _ => "any",
            };
            let mut choices = vec![Choice::new("on", "on"), Choice::new("off", "off")];
            if let Some(now) = now {
                for choice in &mut choices {
                    if choice.value == now {
                        choice.detail = "now".into();
                    }
                }
            }
            if allow_any {
                choices.insert(0, Choice::new("any", "any change"));
            }
            view! {
                <Combo
                    choices=Signal::stored(choices)
                    value=Signal::stored(shown.to_owned())
                    placeholder="on or off"
                    pick=Callback::new(move |text: String| pick(match text.as_str() {
                        "on" => json!(true),
                        "off" => json!(false),
                        _ => Value::Null,
                    }))
                />
            }
            .into_any()
        }
    }
}

const WATCHABLE: [EntityKind; 4] = [
    EntityKind::BinarySensor,
    EntityKind::Switch,
    EntityKind::Light,
    EntityKind::Sensor,
];

#[component]
fn NodeForm(id: NodeId) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let error = RwSignal::new(None::<String>);
    let node = {
        let id = id.clone();
        Memo::new(move |_| {
            ed.draft
                .with(|d| d.as_ref().and_then(|f| f.nodes.get(&id).cloned()))
        })
    };
    let value = move || {
        node.get()
            .and_then(|n| serde_json::to_value(n).ok())
            .unwrap_or_default()
    };
    let edit = {
        let id = id.clone();
        move |f: Box<dyn FnOnce(&mut Value)>| match change(&ed, &id, f) {
            Ok(()) => error.set(None),
            Err(why) => error.set(Some(why)),
        }
    };
    // Sets one field at `path`, from a form.
    let field = {
        let edit = edit.clone();
        move |path: &'static [&'static str], new: Value| {
            let edit = edit.clone();
            edit(Box::new(move |v: &mut Value| set(v, path, new)))
        }
    };
    let problems = {
        let id = id.clone();
        move || ed.problems_at(&id)
    };
    let rename_id = id.clone();
    let delete_id = id.clone();
    let json_edit = edit.clone();

    let body = move || {
        let Some(node) = node.get() else {
            return ().into_any();
        };
        let v = value();
        let field = field.clone();
        let edit = edit.clone();
        match node {
            Node::Trigger { .. } => {
                let kind = get(&v, &["trigger", "type"]).as_str().unwrap_or("state").to_owned();
                let entity = get(&v, &["trigger", "entity"]).as_str().unwrap_or_default().to_owned();
                let to = get(&v, &["trigger", "to"]).clone();
                let hold = get(&v, &["trigger", "for"]).as_str().unwrap_or_default().to_owned();
                let edit2 = edit.clone();
                let f1 = field.clone();
                let f2 = field.clone();
                let f3 = field.clone();
                let home2 = home;
                view! {
                    <label>"Starts"</label>
                    <select on:change=move |e| {
                        let kind = event_target_value(&e);
                        let entity = model::TEMPLATES[0];
                        let made = (entity.make)(&home2);
                        edit2(Box::new(move |v: &mut Value| {
                            v["trigger"] = if kind == "startup" { json!({ "type": "startup" }) } else { made["trigger"].clone() };
                        }));
                    }>
                        <option value="state" selected=kind == "state">"when something changes"</option>
                        <option value="startup" selected=kind == "startup">"when Irori starts"</option>
                    </select>
                    {(kind == "state").then(move || {
                        let entity_for_value = entity.clone();
                        view! {
                            <label>"What"</label>
                            <EntityPicker value=entity kinds=WATCHABLE.to_vec()
                                pick=move |id| f1(&["trigger", "entity"], json!(id)) />
                            <label>"Changes to"</label>
                            <ValueInput entity=entity_for_value value=to allow_any=true
                                pick=move |value| f2(&["trigger", "to"], value) />
                            <label>"And stays that way for (optional, like 5s or 2m)"</label>
                            <input type="text" placeholder="no need" prop:value=hold
                                on:change=move |e| {
                                    let text = event_target_value(&e);
                                    f3(&["trigger", "for"], if text.trim().is_empty() { Value::Null } else { json!(text.trim()) });
                                } />
                        }
                    })}
                    {(kind != "state" && kind != "startup").then(|| view! {
                        <p class="muted">"This kind of trigger is edited as JSON below."</p>
                    })}
                }.into_any()
            }
            Node::Gate { .. } => view! {
                <ConditionForm path=&["condition"] value=get(&v, &["condition"]).clone() edit=edit.clone() />
            }.into_any(),
            Node::Switch { cases } => {
                let count = cases.len();
                let edit_add = edit.clone();
                view! {
                    <p class="muted" style="font-size:.85rem">"The first case that holds decides the way out; none holding goes to “none”."</p>
                    {(0..count).map(|i| {
                        let edit = edit.clone();
                        let edit_remove = edit.clone();
                        let case = v["cases"][i].clone();
                        view! {
                            <div class="item" style="cursor:default">
                                <div class="row"><strong class="grow">{format!("Case {}", i + 1)}</strong>
                                    {(count > 1).then(|| view! {
                                        <button class="btn small danger" on:click=move |_| edit_remove(Box::new(move |v: &mut Value| {
                                            if let Some(cases) = v["cases"].as_array_mut() { cases.remove(i); }
                                        }))>"Remove"</button>
                                    })}
                                </div>
                                <IndexedCondition index=i value=case edit=edit />
                            </div>
                        }
                    }).collect_view()}
                    <button class="btn small" on:click=move |_| edit_add(Box::new(|v: &mut Value| {
                        if let Some(cases) = v["cases"].as_array_mut() {
                            cases.push(json!({ "type": "expr", "expr": "true" }));
                        }
                    }))>"Add a case"</button>
                }.into_any()
            }
            Node::Call { service, entity, data } => {
                let action = match service.to_string().split('.').nth(1).unwrap_or("turn_on") {
                    "turn_off" => "turn_off",
                    "toggle" => "toggle",
                    _ => "turn_on",
                };
                let entity_s = entity.to_string();
                let dimmable = home.entities.with_untracked(|es| es.iter().any(|e| e.id == entity
                    && matches!(&e.capabilities, irori_types::Capabilities::Light(l) if l.brightness)));
                let pct = data.as_ref().and_then(|irori_flow_types::CallData::Light(l)| l.brightness_pct).unwrap_or(100);
                let has_pct = data.as_ref().is_some_and(|irori_flow_types::CallData::Light(l)| l.brightness_pct.is_some());
                let edit_entity = edit.clone();
                let edit_action = edit.clone();
                let edit_pct = edit.clone();
                let action_now = action.to_owned();
                view! {
                    <label>"Do"</label>
                    <select on:change=move |e| {
                        let action = event_target_value(&e);
                        edit_action(Box::new(move |v: &mut Value| {
                            let domain = v["entity"].as_str().and_then(|e| e.split('.').next()).unwrap_or("light").to_owned();
                            v["service"] = json!(format!("{domain}.{action}"));
                            if action != "turn_on" { v.as_object_mut().map(|o| o.remove("data")); }
                        }));
                    }>
                        <option value="turn_on" selected=action == "turn_on">"Turn on"</option>
                        <option value="turn_off" selected=action == "turn_off">"Turn off"</option>
                        <option value="toggle" selected=action == "toggle">"Toggle"</option>
                    </select>
                    <label>"What"</label>
                    <EntityPicker value=entity_s kinds=vec![EntityKind::Light, EntityKind::Switch]
                        pick=move |id| {
                            let action = action_now.clone();
                            edit_entity(Box::new(move |v: &mut Value| {
                                let domain = id.split('.').next().unwrap_or("light").to_owned();
                                v["entity"] = json!(id);
                                v["service"] = json!(format!("{domain}.{action}"));
                                if domain != "light" { v.as_object_mut().map(|o| o.remove("data")); }
                            }));
                        } />
                    {(dimmable && action == "turn_on").then(move || view! {
                        <label>{format!("Brightness: {}", if has_pct { format!("{pct}%") } else { "as it was".into() })}</label>
                        <input type="range" min="1" max="100" prop:value=pct.to_string()
                            on:change=move |e| {
                                let pct: u64 = event_target_value(&e).parse().unwrap_or(100);
                                edit_pct(Box::new(move |v: &mut Value| set(v, &["data", "brightness_pct"], json!(pct))));
                            } />
                    })}
                    <p class="muted" style="font-size:.8rem">"If the call fails, the run goes out of “failed” — or ends with an error if nothing is wired there."</p>
                }.into_any()
            }
            Node::Set { .. } => {
                let f1 = field.clone();
                let f2 = field.clone();
                view! {
                    <label>"Name"</label>
                    <input type="text" prop:value=get(&v, &["name"]).as_str().unwrap_or_default().to_owned()
                        on:change=move |e| f1(&["name"], json!(event_target_value(&e))) />
                    <label>"Value (an expression)"</label>
                    <textarea rows="3" prop:value=get(&v, &["expr"]).as_str().unwrap_or_default().to_owned()
                        on:change=move |e| f2(&["expr"], json!(event_target_value(&e)))></textarea>
                    <p class="muted" style="font-size:.8rem">"Read it later with var('name')."</p>
                }.into_any()
            }
            Node::Delay { .. } => {
                let f1 = field.clone();
                view! {
                    <label>"For (like 30s, 5m, 1h30m)"</label>
                    <input type="text" prop:value=get(&v, &["for"]).as_str().unwrap_or_default().to_owned()
                        on:change=move |e| f1(&["for"], json!(event_target_value(&e).trim())) />
                }.into_any()
            }
            Node::Wait { .. } => {
                let kind = get(&v, &["until", "type"]).as_str().unwrap_or("state").to_owned();
                let entity = get(&v, &["until", "entity"]).as_str().unwrap_or_default().to_owned();
                let entity_for_value = entity.clone();
                let is = get(&v, &["until", "is"]).clone();
                let hold = get(&v, &["until", "for"]).as_str().unwrap_or_default().to_owned();
                let (f1, f2, f3, f4, f5) = (field.clone(), field.clone(), field.clone(), field.clone(), field.clone());
                let edit_kind = edit.clone();
                view! {
                    <label>"Until"</label>
                    <select on:change=move |e| {
                        let kind = event_target_value(&e);
                        edit_kind(Box::new(move |v: &mut Value| {
                            let hold = v["until"]["for"].clone();
                            v["until"] = if kind == "expr" {
                                json!({ "type": "expr", "expr": "true" })
                            } else {
                                json!({ "type": "state", "entity": "binary_sensor.choose_one", "is": false })
                            };
                            if !hold.is_null() { v["until"]["for"] = hold; }
                        }));
                    }>
                        <option value="state" selected=kind == "state">"something is…"</option>
                        <option value="expr" selected=kind == "expr">"an expression holds"</option>
                    </select>
                    {if kind == "expr" {
                        view! {
                            <textarea rows="3" prop:value=get(&v, &["until", "expr"]).as_str().unwrap_or_default().to_owned()
                                on:change=move |e| f1(&["until", "expr"], json!(event_target_value(&e)))></textarea>
                        }.into_any()
                    } else {
                        view! {
                            <EntityPicker value=entity kinds=WATCHABLE.to_vec()
                                pick=move |id| f2(&["until", "entity"], json!(id)) />
                            <label>"Is"</label>
                            <ValueInput entity=entity_for_value value=is allow_any=false
                                pick=move |value| f3(&["until", "is"], value) />
                        }.into_any()
                    }}
                    <label>"For (optional: it has to hold this long)"</label>
                    <input type="text" placeholder="no need" prop:value=hold
                        on:change=move |e| {
                            let text = event_target_value(&e);
                            f4(&["until", "for"], if text.trim().is_empty() { Value::Null } else { json!(text.trim()) });
                        } />
                    <label>"Give up after"</label>
                    <input type="text" prop:value=get(&v, &["timeout"]).as_str().unwrap_or_default().to_owned()
                        on:change=move |e| f5(&["timeout"], json!(event_target_value(&e).trim())) />
                }.into_any()
            }
            Node::Join { .. } => {
                let mode = get(&v, &["mode"]).as_str().unwrap_or("all").to_owned();
                let (f1, f2) = (field.clone(), field.clone());
                view! {
                    <label>"Goes on when"</label>
                    <select on:change=move |e| {
                        let mode = event_target_value(&e);
                        f1(&["mode"], json!(mode));
                    }>
                        <option value="all" selected=mode == "all">"every path has arrived"</option>
                        <option value="first" selected=mode == "first">"the first path arrives"</option>
                    </select>
                    <label>"Give up after (needed when waiting for all)"</label>
                    <input type="text" prop:value=get(&v, &["timeout"]).as_str().unwrap_or_default().to_owned()
                        on:change=move |e| {
                            let text = event_target_value(&e);
                            f2(&["timeout"], if text.trim().is_empty() { Value::Null } else { json!(text.trim()) });
                        } />
                }.into_any()
            }
            Node::Stop { .. } => {
                let f1 = field.clone();
                view! {
                    <label>"Why (shown on the run)"</label>
                    <input type="text" prop:value=get(&v, &["reason"]).as_str().unwrap_or_default().to_owned()
                        on:change=move |e| {
                            let text = event_target_value(&e);
                            f1(&["reason"], if text.trim().is_empty() { Value::Null } else { json!(text.trim()) });
                        } />
                }.into_any()
            }
        }
    };

    view! {
        <div class="row">
            <h2 class="grow">{move || node.get().map(|n| format!("{} · {}", model::label(&n), model::sentence(&n, &home)))}</h2>
        </div>
        {move || problems().into_iter().map(|p| view! {
            <div class=if p.severity == Severity::Error { "problem" } else { "problem warning" }>{p.message}</div>
        }).collect_view()}
        {body}
        {move || error.get().map(|e| view! { <div class="problem">{e}</div> })}
        <label>"Name on the canvas (used by traces)"</label>
        <input type="text" prop:value=rename_id.to_string() on:change=move |e| {
            let Ok(new) = event_target_value(&e).parse::<NodeId>() else {
                error.set(Some("a name is lowercase letters, digits and _".into()));
                return;
            };
            let old = rename_id.clone();
            if new == old { return; }
            let taken = ed.draft.with_untracked(|d| d.as_ref().is_some_and(|f| f.nodes.contains_key(&new)));
            if taken {
                error.set(Some(format!("`{new}` is already taken")));
                return;
            }
            ed.edit(|flow| {
                if let Some(node) = flow.nodes.remove(&old) { flow.nodes.insert(new.clone(), node); }
                if let Some(at) = flow.layout.remove(&old) { flow.layout.insert(new.clone(), at); }
                for wire in &mut flow.wires {
                    if wire.from.node == old { wire.from.node = new.clone(); }
                    if wire.to == old { wire.to = new.clone(); }
                }
            });
            ed.selected.set(Selected::Node(new));
        } />
        <details style="margin-top:.8rem">
            <summary class="muted">"Edit as JSON"</summary>
            <JsonEditor
                text=move || node.get().and_then(|n| serde_json::to_string_pretty(&n).ok()).unwrap_or_default()
                apply=move |text: String| {
                    let parsed: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    serde_json::from_value::<Node>(parsed.clone()).map_err(|e| e.to_string())?;
                    json_edit(Box::new(move |v: &mut Value| *v = parsed));
                    Ok(())
                }
            />
        </details>
        <div class="row" style="margin-top:1rem">
            <button class="btn danger" on:click=move |_| remove_node(&ed, &delete_id)>"Delete node"</button>
        </div>
    }
}

type Edit = Box<dyn FnOnce(&mut Value)>;

/// A condition's form at `path` inside the node.
#[component]
fn ConditionForm(
    path: &'static [&'static str],
    value: Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    condition_form(value, move |f: Box<dyn FnOnce(&mut Value)>| {
        edit(Box::new(move |v: &mut Value| {
            let mut here = get(v, path).clone();
            f(&mut here);
            set(v, path, here);
        }))
    })
}

/// A switch case's form.
#[component]
fn IndexedCondition(
    index: usize,
    value: Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    condition_form(value, move |f: Box<dyn FnOnce(&mut Value)>| {
        edit(Box::new(move |v: &mut Value| {
            if let Some(case) = v["cases"].get_mut(index) {
                f(case);
            }
        }))
    })
}

fn condition_form(
    value: Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    let kind = value["type"].as_str().unwrap_or("state").to_owned();
    let edit_kind = edit.clone();
    let body = match kind.as_str() {
        "state" => {
            let entity = value["entity"].as_str().unwrap_or_default().to_owned();
            let entity_for_value = entity.clone();
            let is = value["is"].clone();
            let (e1, e2) = (edit.clone(), edit.clone());
            view! {
                <EntityPicker value=entity kinds=WATCHABLE.to_vec()
                    pick=move |id| e1(Box::new(move |v: &mut Value| v["entity"] = json!(id))) />
                <label>"Is"</label>
                <ValueInput entity=entity_for_value value=is allow_any=false
                    pick=move |value| e2(Box::new(move |v: &mut Value| v["is"] = value)) />
            }
            .into_any()
        }
        "expr" => {
            let e1 = edit.clone();
            view! {
                <textarea rows="3" prop:value=value["expr"].as_str().unwrap_or_default().to_owned()
                    on:change=move |e| {
                        let text = event_target_value(&e);
                        e1(Box::new(move |v: &mut Value| v["expr"] = json!(text)));
                    }></textarea>
                <p class="muted" style="font-size:.78rem">
                    "num('sensor.x') < 30 · on('switch.x') · text('sensor.x') == 'rinse' · available('sensor.x') · var('name') — combine with && || !"
                </p>
            }.into_any()
        }
        _ => view! { <p class="muted">"This condition is edited as JSON below."</p> }.into_any(),
    };
    view! {
        <label>"Check"</label>
        <select on:change=move |e| {
            let kind = event_target_value(&e);
            edit_kind(Box::new(move |v: &mut Value| {
                *v = if kind == "expr" {
                    json!({ "type": "expr", "expr": "true" })
                } else {
                    json!({ "type": "state", "entity": "switch.choose_one", "is": true })
                };
            }));
        }>
            <option value="state" selected=kind == "state">"something is…"</option>
            <option value="expr" selected=kind == "expr">"an expression"</option>
            {(kind != "state" && kind != "expr").then(|| view! {
                <option value="other" selected=true>{kind.clone()}</option>
            })}
        </select>
        {body}
    }
}
