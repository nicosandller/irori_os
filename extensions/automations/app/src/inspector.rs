//! The forms: a flow's own settings and everything the check found (the panel's Edit tab), and a
//! node's, shown in the node itself when it's opened on the canvas.
//!
//! Forms edit the node's JSON and read it back through the same types the engine uses, so a
//! value the engine wouldn't take can't get into the draft; the form says why instead.

use irori_flow_types::api::Severity;
use irori_flow_types::{Amount, Flow, Node, NodeId, RuleService};
use irori_types::{EntityKind, ValueShape};
use irori_types::{FieldShape, ServiceName};
use leptos::prelude::*;
use serde_json::{Value, json};

use crate::canvas::remove_node;
use crate::checks::{Checks, ChecksForm, TextValue};
use crate::editor::{Editing, Selected};
use crate::widgets::{Choice, Combo};
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
    // A node read on its own is only the right shape. Whether it says something a flow can
    // hold (a light told a colour and a warmth at once) is the flow's to check.
    let held = ed.draft.with_untracked(|draft| {
        draft.as_ref().map_or(Ok(()), |flow| {
            let mut flow = flow.clone();
            flow.nodes.insert(id.clone(), node.clone());
            flow.validate().map_err(|e| e.to_string())
        })
    });
    held?;
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

/// The flow's own settings, and every problem the check found.
#[component]
pub fn FlowForm() -> impl IntoView {
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
            "Click a node on the canvas to open it and edit it, or add one from the left. Wire a node's port to another node to say what happens next."
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
        <details class="json-fold" style="margin-top:1.2rem">
            <summary>"Edit this flow as JSON"</summary>
            <p class="muted" style="font-size:.8rem;margin:.4rem 0">
                "The same flow, as the file it's kept in. Copy it to share it or keep it; paste one in and press Apply to bring it onto the canvas. Nothing is saved until you press Save."
            </p>
            <JsonEditor
                label="The whole flow as JSON"
                text=move || flow().map(|f| written(&f)).unwrap_or_default()
                apply=move |text: String| {
                    let flow: Flow = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    ed.edit(|draft| *draft = flow);
                    Ok(())
                }
            />
        </details>
        <DeleteFlow />
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

/// How long the copy button says it copied.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1600);

/// A flow, or one node of it, as JSON to read, copy and change.
///
/// A text box can't colour what is in it, so the colour is a second copy of the text drawn
/// underneath, in the same letters at the same place, and the box itself is typed into with
/// its own letters see-through; the two scroll as one. Nothing typed reaches the flow until
/// Apply, which holds it to the same rules as everything else here.
#[component]
fn JsonEditor(
    text: impl Fn() -> String + Send + Sync + 'static,
    apply: impl Fn(String) -> Result<(), String> + Send + Sync + 'static,
    /// What a screen reader calls the box.
    label: &'static str,
) -> impl IntoView {
    let error = RwSignal::new(None::<String>);
    // What's been typed and not applied yet. Without any, the box shows the flow as it is.
    let draft = RwSignal::new(None::<String>);
    let copied = RwSignal::new(None::<bool>);
    let painted = NodeRef::<leptos::html::Pre>::new();
    let shown = Signal::derive(move || draft.get().unwrap_or_else(&text));
    let copy = move |_| {
        let text = shown.get_untracked();
        leptos::task::spawn_local(async move {
            copied.set(Some(irori_ui_kit::clipboard::copy(&text).await));
            set_timeout(
                move || {
                    let _ = copied.try_set(None);
                },
                COPIED_FOR,
            );
        });
    };
    let format = move |_| match serde_json::from_str::<Value>(&shown.get_untracked()) {
        Ok(value) => {
            error.set(None);
            draft.set(Some(irori_ui_kit::json::written(&value)));
        }
        Err(why) => error.set(Some(format!("That isn't JSON yet: {why}"))),
    };
    view! {
        <div class="json-box">
            <pre aria-hidden="true" node_ref=painted>
                {move || {
                    shown.with(|text| irori_ui_kit::json::pieces(text))
                        .into_iter()
                        .map(|(kind, piece)| view! { <span class=kind.class()>{piece}</span> })
                        .collect_view()
                }}
                // A last line with nothing on it still has to take up a line.
                "\n"
            </pre>
            <textarea spellcheck="false" autocomplete="off" aria-label=label
                prop:value=move || shown.get()
                on:input=move |e| {
                    draft.set(Some(event_target_value(&e)));
                    error.set(None);
                }
                on:scroll=move |e| {
                    use wasm_bindgen::JsCast as _;
                    let Some(pre) = painted.get_untracked() else { return };
                    if let Some(typed) = e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
                        pre.set_scroll_top(typed.scroll_top());
                        pre.set_scroll_left(typed.scroll_left());
                    }
                }></textarea>
        </div>
        {move || error.get().map(|e| view! { <p class="json-why" role="alert">{e}</p> })}
        <div class="json-actions">
            <button type="button" class="btn small json-copy"
                class:done=move || copied.get() == Some(true)
                class:failed=move || copied.get() == Some(false)
                on:click=copy>
                <svg class="copy-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none"
                    stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                    <g class="copy-clip">
                        <rect x="8" y="8" width="12" height="12" rx="2" />
                        <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
                    </g>
                    <path class="copy-check" d="M5 12.5l4.5 4.5L19 7.5" pathLength="1" />
                </svg>
                {move || match copied.get() {
                    Some(true) => "Copied",
                    Some(false) => "Couldn't copy",
                    None => "Copy",
                }}
            </button>
            <button type="button" class="btn small" on:click=format
                title="Set it out again, one thing to a line">"Format"</button>
            <span class="grow"></span>
            {move || draft.get().is_some().then(|| view! {
                <button type="button" class="btn small" on:click=move |_| {
                    draft.set(None);
                    error.set(None);
                }>"Undo typing"</button>
            })}
            <button type="button" class="btn small primary"
                disabled=move || draft.get().is_none()
                on:click=move |_| {
                    if let Some(text) = draft.get_untracked() {
                        match apply(text) {
                            Ok(()) => { error.set(None); draft.set(None); }
                            Err(why) => error.set(Some(why)),
                        }
                    }
                }>"Apply"</button>
        </div>
    }
}

/// A flow as text to read: indented, with each wire and each place on the canvas on one line.
fn written<T: serde::Serialize>(what: &T) -> String {
    serde_json::to_value(what)
        .map(|value| irori_ui_kit::json::written(&value))
        .unwrap_or_default()
}

/// Entities to choose from, those of `kinds` (all when empty), searched as you type. The current
/// one is kept even if it's gone, so the form can say so.
#[component]
pub fn EntityPicker(
    value: String,
    kinds: Vec<EntityKind>,
    /// More to pick from than entities, listed first ("Irori starts up").
    #[prop(optional)]
    extra: Vec<Choice>,
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
                        .with(|states| states.get(&e.id).map(|s| model::state_words(s, &home)))
                        .map(|now| format!(" · {now}"))
                        .unwrap_or_default();
                    Choice::new(e.id.to_string(), e.name.to_string())
                        .detail(format!("{}{now}", e.id))
                        .group(home.device_before(e))
                        .icon(irori_ui_kit::entity_icon::drawing(&e.capabilities))
                })
                .collect()
        });
        // A device's entities together, in the order they're read.
        choices.sort_by_key(|choice| {
            let label = choice.label.to_lowercase();
            if choice.group.is_empty() {
                (label.clone(), label)
            } else {
                (choice.group.to_lowercase(), label)
            }
        });
        choices.splice(0..0, extra.iter().cloned());
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
pub fn ValueInput(
    entity: String,
    value: Value,
    allow_any: bool,
    pick: impl Fn(Value) + Send + Sync + Clone + 'static,
) -> impl IntoView {
    let home = expect_context::<Home>();
    let shape = home.value_shape(&entity);
    let now = entity.parse::<irori_types::EntityId>().ok().and_then(|id| {
        home.states
            .with_untracked(|states| states.get(&id).map(|s| model::state_words(s, &home)))
    });
    match shape {
        ValueShape::Text => {
            let shown = value.as_str().unwrap_or_default().to_owned();
            view! {
                <TextValue entity=entity value=shown allow_any=allow_any
                    pick=move |text: String| pick(if text.is_empty() { Value::Null } else { json!(text) }) />
            }
            .into_any()
        }
        ValueShape::Number => {
            let shown = match &value {
                Value::Null => String::new(),
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
                    placeholder="Type a number…"
                    pick=Callback::new(move |text: String| {
                        let text = text.trim().to_owned();
                        pick(text.parse::<f64>().map(|n| json!(n)).unwrap_or(Value::Null));
                    })
                />
            }
            .into_any()
        }
        ValueShape::Bool => {
            let shown = match value {
                Value::Bool(true) => "on",
                Value::Bool(false) => "off",
                _ => "any",
            };
            let (on, off) = entity
                .parse::<irori_types::EntityId>()
                .map_or(("on", "off"), |id| home.flag_words(&id));
            let mut choices = vec![Choice::new("on", on), Choice::new("off", off)];
            if let Some(now) = now {
                for choice in &mut choices {
                    if choice.label == now {
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
                    placeholder=format!("{on} or {off}")
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

/// What a trigger can watch: anything with a value, or that reports something happening. That
/// is every kind but a button, which is only ever pressed from here and has nothing to say.
pub fn watchable() -> Vec<EntityKind> {
    EntityKind::ALL
        .iter()
        .copied()
        .filter(|kind| *kind != EntityKind::Button)
        .collect()
}

/// What a check or a wait can ask about: something with a value that lasts. A remote's press
/// happens and is over, so it starts a flow and can't be asked what it "is".
pub fn lasting() -> Vec<EntityKind> {
    watchable()
        .into_iter()
        .filter(|kind| !kind.counts_every_report())
        .collect()
}

/// What a call can act on: every kind with something to ask of it.
fn callable() -> Vec<EntityKind> {
    EntityKind::ALL
        .iter()
        .copied()
        .filter(|kind| kind.has_services())
        .collect()
}

/// A service's action as a person says it: `set_position` is "Set position".
pub fn action_words(action: &str) -> String {
    let words = action.replace('_', " ");
    let mut letters = words.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => words,
    }
}

/// What a call starts with when its service is picked: the fields it can't do without, each
/// at something sensible. `None` when it needs none.
fn starting_data(service: RuleService, entity: Option<&irori_types::Entity>) -> Option<Value> {
    let RuleService::Named(name) = service else {
        return None;
    };
    let mut data = serde_json::Map::new();
    for field in name.fields().iter().filter(|field| field.required) {
        let value = match &field.shape {
            FieldShape::Number { min, max, .. } => match (min, max) {
                // Half way: a blind sent to "position" is most often being tried out.
                (Some(min), Some(max)) => json!(((min + max) / 2.0).round()),
                (Some(min), None) => json!(min),
                _ => json!(0),
            },
            FieldShape::Bool => json!(true),
            FieldShape::Choice(words) => json!(words.first().cloned().unwrap_or_default()),
            FieldShape::Text => json!(
                entity
                    .and_then(|entity| own_words(entity, name))
                    .and_then(|words| words.first().cloned())
                    .unwrap_or_default()
            ),
            FieldShape::Other => continue,
        };
        data.insert(field.name.clone(), value);
    }
    (!data.is_empty()).then_some(Value::Object(data))
}

/// The entity's own list for a service's text field, where it has one: a select's options.
fn own_words(entity: &irori_types::Entity, name: ServiceName) -> Option<Vec<String>> {
    match (&entity.capabilities, name) {
        (irori_types::Capabilities::Select(select), ServiceName::SelectSelectOption) => {
            Some(select.options.clone())
        }
        _ => None,
    }
}

/// Whether a call lets something or someone in, for the form to say so before it's wired to a
/// trigger anyone can set off.
fn opens_something(service: RuleService, entity: Option<&irori_types::Entity>) -> bool {
    let way_in = entity.is_some_and(|entity| match &entity.capabilities {
        irori_types::Capabilities::Cover(cover) => matches!(
            cover.device_class,
            Some(
                irori_types::CoverClass::Garage
                    | irori_types::CoverClass::Door
                    | irori_types::CoverClass::Gate
            )
        ),
        irori_types::Capabilities::Lock(_) => true,
        _ => false,
    });
    way_in
        && !matches!(
            service,
            RuleService::Named(
                ServiceName::LockLock | ServiceName::CoverClose | ServiceName::CoverStop
            )
        )
}

/// A change to a node, as its form hands it to the editor.
type Change = Box<dyn FnOnce(&mut Value)>;

/// The settings of a call, one input per field of its service: a blind's position, a
/// thermostat's temperature and mode, a fan's direction. Drawn from what the service says it
/// takes, so a kind Irori learns later has a form without this page knowing it.
///
/// A field the service can do without is left out of the call while its input is empty. A
/// number can be worked out when the call runs instead of written down.
#[component]
fn SettingFields(
    service: RuleService,
    entity: Option<irori_types::Entity>,
    /// The call's `data` as it's written, `null` when it has none.
    data: Value,
    edit: impl Fn(Change) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    let RuleService::Named(name) = service else {
        return ().into_any();
    };
    // A code is a secret, and a flow is a file people share: it isn't asked for here.
    let fields: Vec<_> = name
        .fields()
        .iter()
        .filter(|field| field.name != "code" && field.shape != FieldShape::Other)
        .cloned()
        .collect();
    if fields.is_empty() {
        return ().into_any();
    }
    // Writes one field, and drops `data` altogether once nothing is left in it.
    let write = move |edit: &dyn Fn(Change), field: String, value: Value| {
        edit(Box::new(move |v: &mut Value| {
            set(v, &["data", field.as_str()], value);
            if v["data"].as_object().is_some_and(serde_json::Map::is_empty) {
                v.as_object_mut().map(|o| o.remove("data"));
            }
        }));
    };
    fields
        .into_iter()
        .map(|field| {
            let now = data.get(&field.name).cloned().unwrap_or(Value::Null);
            let worked = now.get("expr").and_then(Value::as_str).map(str::to_owned);
            let label = if field.required {
                action_words(&field.name)
            } else {
                format!("{} (optional)", action_words(&field.name))
            };
            let hint = field.description.clone();
            let key = field.name.clone();
            let optional = !field.required;
            let input = match &field.shape {
                FieldShape::Number { min, max, integer } => {
                    let (edit_how, edit_number, edit_expr) = (edit.clone(), edit.clone(), edit.clone());
                    let (key_how, key_number, key_expr) = (key.clone(), key.clone(), key.clone());
                    let fixed = now.as_f64();
                    let start = fixed.or(*min).unwrap_or(0.0);
                    let range = match (min, max) {
                        (Some(min), Some(max)) => format!("{min} to {max}"),
                        (Some(min), None) => format!("{min} or more"),
                        (None, Some(max)) => format!("up to {max}"),
                        (None, None) => String::new(),
                    };
                    let step = if *integer { "1" } else { "0.5" };
                    view! {
                        <div class="row">
                            <select on:change=move |e| {
                                let value = if event_target_value(&e) == "expr" {
                                    json!({ "expr": start.to_string() })
                                } else {
                                    json!(start)
                                };
                                write(&edit_how, key_how.clone(), value);
                            }>
                                <option value="fixed" selected=worked.is_none()>"a fixed number"</option>
                                <option value="expr" selected=worked.is_some()>"worked out"</option>
                            </select>
                            {match worked.clone() {
                                None => view! {
                                    <input type="number" class="grow" step=step
                                        min=min.map(|n| n.to_string()) max=max.map(|n| n.to_string())
                                        placeholder=if optional { "leave as it is" } else { "" }
                                        prop:value=fixed.map(|n| n.to_string()).unwrap_or_default()
                                        on:change=move |e| {
                                            let text = event_target_value(&e);
                                            let value = text.trim().parse::<f64>().map_or(Value::Null, |n| json!(n));
                                            write(&edit_number, key_number.clone(), value);
                                        } />
                                }.into_any(),
                                Some(expr) => view! {
                                    <div class="grow">
                                        <ExprInput value=expr
                                            commit=move |text| write(&edit_expr, key_expr.clone(), json!({ "expr": text })) />
                                    </div>
                                }.into_any(),
                            }}
                        </div>
                        {(!range.is_empty()).then(|| view! {
                            <p class="muted" style="font-size:.8rem">
                                {if worked.is_some() {
                                    format!("Worked out when the call runs, and kept {range}.")
                                } else {
                                    format!("From {range}.")
                                }}
                            </p>
                        })}
                    }
                    .into_any()
                }
                FieldShape::Bool => {
                    let edit = edit.clone();
                    let chosen = now.as_bool();
                    view! {
                        <select on:change=move |e| {
                            let value = match event_target_value(&e).as_str() {
                                "yes" => json!(true),
                                "no" => json!(false),
                                _ => Value::Null,
                            };
                            write(&edit, key.clone(), value);
                        }>
                            {optional.then(|| view! { <option value="" selected=chosen.is_none()>"leave as it is"</option> })}
                            <option value="yes" selected=chosen == Some(true)>"yes"</option>
                            <option value="no" selected=chosen == Some(false)>"no"</option>
                        </select>
                    }
                    .into_any()
                }
                FieldShape::Choice(_) | FieldShape::Text => {
                    // A fixed list of the service's, or the entity's own (a select's options).
                    let words = match &field.shape {
                        FieldShape::Choice(words) => Some(words.clone()),
                        _ => entity.as_ref().and_then(|entity| own_words(entity, name)),
                    };
                    let chosen = now.as_str().unwrap_or_default().to_owned();
                    let edit = edit.clone();
                    match words {
                        Some(words) => view! {
                            <select on:change=move |e| {
                                let text = event_target_value(&e);
                                let value = if text.is_empty() { Value::Null } else { json!(text) };
                                write(&edit, key.clone(), value);
                            }>
                                {optional.then(|| view! { <option value="" selected=chosen.is_empty()>"leave as it is"</option> })}
                                {words.into_iter().map(|word| view! {
                                    <option value=word.clone() selected=word == chosen>{word.replace('_', " ")}</option>
                                }).collect_view()}
                            </select>
                        }
                        .into_any(),
                        None => view! {
                            <input type="text" prop:value=chosen
                                placeholder=if optional { "leave as it is" } else { "" }
                                on:change=move |e| {
                                    let text = event_target_value(&e).trim().to_owned();
                                    let value = if text.is_empty() { Value::Null } else { json!(text) };
                                    write(&edit, key.clone(), value);
                                } />
                        }
                        .into_any(),
                    }
                }
                FieldShape::Other => ().into_any(),
            };
            view! {
                <label title=hint>{label}</label>
                {input}
            }
        })
        .collect_view()
        .into_any()
}

/// Colours to pick with one press: the ones a room is most often lit in.
const SWATCHES: [(&str, [u8; 3]); 8] = [
    ("Ember", [255, 122, 61]),
    ("Amber", [255, 184, 77]),
    ("Rose", [255, 99, 132]),
    ("Violet", [168, 110, 255]),
    ("Blue", [72, 140, 255]),
    ("Aqua", [64, 212, 220]),
    ("Green", [96, 214, 120]),
    ("White", [255, 255, 255]),
];

/// What a light looks like when it's turned on: how warm its white is, or its colour. Only
/// what the light can do is offered, and a light is one or the other, never both.
#[component]
fn LightLook(
    light: irori_types::LightCapabilities,
    /// The call's `data` as it's written, `null` when it has none.
    data: Value,
    edit: impl Fn(Change) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    use irori_ui_kit::color::{hex_from_rgb, kelvin_rgb, kelvin_word, rgb_from_hex};

    let range = light.color_temp_kelvin;
    if range.is_none() && !light.rgb {
        return ().into_any();
    }
    let warmth = data.get("color_temp_kelvin").cloned();
    let colour: Option<[u8; 3]> = data
        .get("rgb")
        .and_then(|rgb| serde_json::from_value(rgb.clone()).ok());
    let worked = warmth
        .as_ref()
        .and_then(|w| w.get("expr"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    // Writes the look as a whole: setting one takes the other away, in the same change, since
    // a light told both is told nothing it can do.
    let put = move |edit: &dyn Fn(Change), kelvin: Value, rgb: Value| {
        edit(Box::new(move |v: &mut Value| {
            set(v, &["data", "color_temp_kelvin"], kelvin);
            set(v, &["data", "rgb"], rgb);
            if v["data"].as_object().is_some_and(serde_json::Map::is_empty) {
                v.as_object_mut().map(|o| o.remove("data"));
            }
        }));
    };
    let chosen = match (&warmth, &colour) {
        (Some(_), _) => "warmth",
        (None, Some(_)) => "colour",
        (None, None) => "none",
    };
    // Where a warmth starts: a living room's, kept inside what this light can do.
    let start = range.map_or(2700, |range| 2700.clamp(range.min, range.max));
    let (edit_none, edit_warm, edit_colour) = (edit.clone(), edit.clone(), edit.clone());
    let kept_colour = colour.unwrap_or([255, 184, 77]);
    let kept_warmth = warmth.clone().unwrap_or_else(|| json!(start));
    let body = match chosen {
        "warmth" => {
            let range = range.unwrap_or(irori_types::ColorTempRange {
                min: 1000,
                max: 20000,
            });
            match worked {
                Some(expr) => {
                    let edit = edit.clone();
                    view! {
                        <ExprInput value=expr
                            commit=move |text| put(&edit, json!({ "expr": text }), Value::Null) />
                        <p class="muted" style="font-size:.8rem">
                            "Worked out in kelvin when the light is turned on."
                        </p>
                    }
                    .into_any()
                }
                None => {
                    let now = warmth
                        .as_ref()
                        .and_then(Value::as_u64)
                        .and_then(|k| u16::try_from(k).ok())
                        .unwrap_or(start)
                        .clamp(range.min, range.max);
                    let live = RwSignal::new(now);
                    let edit = edit.clone();
                    view! {
                        <div class="look-row">
                            <span class="look-dot" style=move || {
                                format!("background:{}", hex_from_rgb(kelvin_rgb(live.get())))
                            }></span>
                            <input type="range" class="kelvin" step="50"
                                min=range.min.to_string() max=range.max.to_string()
                                prop:value=now.to_string()
                                aria-label="How warm or cool the light is"
                                // The label and the dot follow the thumb; letting go writes it.
                                on:input=move |e| {
                                    if let Ok(kelvin) = event_target_value(&e).parse::<u16>() {
                                        live.set(kelvin);
                                    }
                                }
                                on:change=move |e| {
                                    if let Ok(kelvin) = event_target_value(&e).parse::<u16>() {
                                        put(&edit, json!(kelvin), Value::Null);
                                    }
                                } />
                        </div>
                        <div class="look-ends muted">
                            <span>"warm"</span>
                            <strong>{move || format!("{} K · {}", live.get(), kelvin_word(live.get()))}</strong>
                            <span>"cool"</span>
                        </div>
                    }
                    .into_any()
                }
            }
        }
        "colour" => {
            let now = colour.unwrap_or(kept_colour);
            let edit_well = edit.clone();
            view! {
                <div class="look-row">
                    <input type="color" class="look-well" prop:value=hex_from_rgb(now)
                        aria-label="The light's colour"
                        on:change=move |e| {
                            if let Some(rgb) = rgb_from_hex(&event_target_value(&e)) {
                                put(&edit_well, Value::Null, json!(rgb));
                            }
                        } />
                    <div class="look-swatches" role="group" aria-label="Colours to pick from">
                        {SWATCHES.iter().map(|(name, rgb)| {
                            let (rgb, edit) = (*rgb, edit.clone());
                            view! {
                                <button type="button" class="look-pick" title=*name aria-label=*name
                                    aria-pressed=(rgb == now).to_string()
                                    style=format!("--c:{}", hex_from_rgb(rgb))
                                    on:click=move |_| put(&edit, Value::Null, json!(rgb))></button>
                            }
                        }).collect_view()}
                    </div>
                </div>
                <p class="muted" style="font-size:.8rem">
                    {format!("{} · how bright it is stays with the brightness above.", hex_from_rgb(now))}
                </p>
            }
            .into_any()
        }
        _ => ().into_any(),
    };
    view! {
        <label>"Colour"</label>
        <div class="looks" role="group" aria-label="What the light looks like">
            <button type="button" aria-pressed=(chosen == "none").to_string()
                on:click=move |_| put(&edit_none, Value::Null, Value::Null)>
                "As it was"
            </button>
            {range.is_some().then(|| view! {
                <button type="button" aria-pressed=(chosen == "warmth").to_string()
                    on:click=move |_| put(&edit_warm, kept_warmth.clone(), Value::Null)>
                    "Warm to cool white"
                </button>
            })}
            {light.rgb.then(|| view! {
                <button type="button" aria-pressed=(chosen == "colour").to_string()
                    on:click=move |_| put(&edit_colour, Value::Null, json!(kept_colour))>
                    "A colour"
                </button>
            })}
        </div>
        {body}
    }
    .into_any()
}

/// A node's form, in the node itself when it's open on the canvas.
#[component]
pub fn NodeForm(id: NodeId) -> impl IntoView {
    let ed = expect_context::<Editing>();
    let home = expect_context::<Home>();
    let error = RwSignal::new(None::<String>);
    // Conditions shown as the expression they are, rather than as a comparison to fill in.
    let raw = RwSignal::new(std::collections::BTreeSet::<String>::new());
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
    let json_id = id.clone();

    let body = move || {
        // Showing a condition as its expression changes the form, not the node.
        raw.track();
        let Some(node) = node.get() else {
            return ().into_any();
        };
        let v = value();
        let field = field.clone();
        let edit = edit.clone();
        match node {
            Node::Trigger { .. } => {
                let trigger = get(&v, &["trigger"]).clone();
                let edit = edit.clone();
                view! {
                    <crate::triggers::TriggerForm trigger=trigger
                        edit=move |f: Edit| edit(Box::new(move |v: &mut Value| {
                            let mut here = v["trigger"].clone();
                            f(&mut here);
                            v["trigger"] = here;
                        })) />
                }.into_any()
            }
            Node::Gate { .. } => view! {
                <ConditionForm path=&["condition"] value=get(&v, &["condition"]).clone() edit=edit.clone() raw=raw />
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
                                <IndexedCondition index=i value=case edit=edit raw=raw />
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
                let action = service.action();
                let found = home.entities.with_untracked(|es| es.iter().find(|e| e.id == entity).cloned());
                // What can be asked of it: its own kind's services, whatever kind that is.
                let offered = RuleService::all_of(entity.kind());
                let warn = opens_something(service, found.as_ref());
                let is_light_on = matches!(service, RuleService::Named(ServiceName::LightTurnOn));
                let entity_s = entity.to_string();
                let dimmable = home.entities.with_untracked(|es| es.iter().any(|e| e.id == entity
                    && matches!(&e.capabilities, irori_types::Capabilities::Light(l) if l.brightness)));
                let level = data.as_ref().and_then(|d| d.get("brightness_pct").cloned());
                let has_pct = level.is_some();
                let pct = match &level {
                    Some(Amount::Fixed(n)) => n.as_u64().unwrap_or(100),
                    _ => 100,
                };
                // The flow's calculations, for a brightness worked out by one of them.
                let calculations: Vec<String> = ed.draft.with_untracked(|d| {
                    d.as_ref().map(|f| f.nodes.values().filter_map(|n| match n {
                        Node::Set { name, .. } => Some(name.to_string()),
                        _ => None,
                    }).collect()).unwrap_or_default()
                });
                let worked = match &level {
                    Some(Amount::Worked(w)) => Some(w.expr.as_str().to_owned()),
                    _ => None,
                };
                let from_calculation = worked.as_deref().and_then(calculation_read);
                let edit_entity = edit.clone();
                let edit_action = edit.clone();
                let edit_pct = edit.clone();
                let edit_fields = edit.clone();
                let edit_look = edit.clone();
                let light = found.as_ref().and_then(|e| match &e.capabilities {
                    irori_types::Capabilities::Light(light) => Some(light.clone()),
                    _ => None,
                });
                let look_data = data.as_ref().and_then(|d| serde_json::to_value(d).ok()).unwrap_or(Value::Null);
                let action_now = action.to_owned();
                let (for_action, for_fields) = (found.clone(), found.clone());
                let settings = data.as_ref().and_then(|d| serde_json::to_value(d).ok()).unwrap_or(Value::Null);
                view! {
                    <label>"What"</label>
                    <EntityPicker value=entity_s kinds=callable()
                        pick=move |id| {
                            let action = action_now.clone();
                            let home = home;
                            edit_entity(Box::new(move |v: &mut Value| {
                                let Ok(picked) = id.parse::<irori_types::EntityId>() else { return };
                                let was = v["entity"].as_str().and_then(|e| e.split('.').next()).map(str::to_owned);
                                let kind = picked.kind();
                                // The same thing asked of the new one, where it has it; else
                                // the first thing it can be asked.
                                let service = RuleService::of(kind, &action)
                                    .or_else(|| RuleService::all_of(kind).into_iter().next());
                                let Some(service) = service else { return };
                                v["entity"] = json!(id);
                                v["service"] = json!(service.to_string());
                                // Settings are a kind's own: another kind's mean nothing here.
                                if was.as_deref() != Some(kind.domain()) || service.action() != action {
                                    let entity = home.entities.with_untracked(|es| es.iter().find(|e| e.id == picked).cloned());
                                    match starting_data(service, entity.as_ref()) {
                                        Some(data) => v["data"] = data,
                                        None => { v.as_object_mut().map(|o| o.remove("data")); }
                                    }
                                }
                            }));
                        } />
                    <label>"Do"</label>
                    <select on:change=move |e| {
                        let action = event_target_value(&e);
                        let entity = for_action.clone();
                        edit_action(Box::new(move |v: &mut Value| {
                            let kind = v["entity"].as_str().and_then(|e| e.split('.').next())
                                .and_then(EntityKind::from_domain);
                            let Some(service) = kind.and_then(|kind| RuleService::of(kind, &action)) else { return };
                            v["service"] = json!(service.to_string());
                            match starting_data(service, entity.as_ref()) {
                                Some(data) => v["data"] = data,
                                None => { v.as_object_mut().map(|o| o.remove("data")); }
                            }
                        }));
                    }>
                        {offered.iter().map(|one| view! {
                            <option value=one.action() selected=one.action() == action>
                                {action_words(one.action())}
                            </option>
                        }).collect_view()}
                    </select>
                    {warn.then(|| view! {
                        <p class="call-warning" role="note">
                            "This lets something or someone in. Whatever starts this flow does it too: "
                            "a button anyone can press, or a sensor anyone can set off."
                        </p>
                    })}
                    {(!is_light_on).then(|| view! {
                        <SettingFields service=service entity=for_fields data=settings edit=edit_fields />
                    })}
                    {(dimmable && is_light_on).then(move || {
                        let edit_check = edit_pct.clone();
                        view! {
                            <label class="check">
                                <input type="checkbox" prop:checked=has_pct
                                    on:change=move |e| {
                                        let on = event_target_checked(&e);
                                        edit_check(Box::new(move |v: &mut Value| {
                                            if on {
                                                set(v, &["data", "brightness_pct"], json!(pct));
                                            } else {
                                                set(v, &["data", "brightness_pct"], Value::Null);
                                                if v["data"].as_object().is_some_and(|d| d.is_empty()) {
                                                    v.as_object_mut().map(|o| o.remove("data"));
                                                }
                                            }
                                        }));
                                    } />
                                <span>"Set the brightness"</span>
                                <span class="muted">{match (has_pct, &from_calculation) {
                                    (false, _) => "otherwise it stays as it was".to_owned(),
                                    (true, Some(name)) => format!("from {name}"),
                                    (true, None) if worked.is_some() => "worked out".to_owned(),
                                    (true, None) => format!("{pct}%"),
                                }}</span>
                            </label>
                            {has_pct.then(move || {
                                let how = match (&worked, &from_calculation) {
                                    (None, _) => "fixed",
                                    (Some(_), Some(_)) => "calculation",
                                    (Some(_), None) => "expr",
                                };
                                let (edit_how, edit_fixed, edit_calc, edit_expr) =
                                    (edit_pct.clone(), edit_pct.clone(), edit_pct.clone(), edit_pct.clone());
                                let first_calculation = calculations.first().cloned();
                                let calculation_list = calculations.clone();
                                let chosen = from_calculation.clone().unwrap_or_default();
                                view! {
                                    <select on:change=move |e| {
                                        let how = event_target_value(&e);
                                        let first = first_calculation.clone();
                                        edit_how(Box::new(move |v: &mut Value| {
                                            let level = match (how.as_str(), first) {
                                                ("calculation", Some(name)) => json!({ "expr": format!("var('{name}')") }),
                                                ("expr", _) => json!({ "expr": "50" }),
                                                _ => json!(pct),
                                            };
                                            set(v, &["data", "brightness_pct"], level);
                                        }));
                                    }>
                                        <option value="fixed" selected=how == "fixed">"to a fixed level"</option>
                                        <option value="calculation" selected=how == "calculation"
                                            disabled=calculations.is_empty() && how != "calculation">
                                            {if calculations.is_empty() { "from a calculation (add one first)" } else { "from a calculation" }}
                                        </option>
                                        <option value="expr" selected=how == "expr">"from an expression"</option>
                                    </select>
                                    {match how {
                                        "fixed" => view! {
                                            <input type="range" min="1" max="100" prop:value=pct.to_string()
                                                on:change=move |e| {
                                                    let pct: u64 = event_target_value(&e).parse().unwrap_or(100);
                                                    edit_fixed(Box::new(move |v: &mut Value| set(v, &["data", "brightness_pct"], json!(pct))));
                                                } />
                                        }.into_any(),
                                        "calculation" => view! {
                                            <select on:change=move |e| {
                                                let name = event_target_value(&e);
                                                edit_calc(Box::new(move |v: &mut Value| {
                                                    set(v, &["data", "brightness_pct"], json!({ "expr": format!("var('{name}')") }));
                                                }));
                                            }>
                                                {calculation_list.iter().map(|name| view! {
                                                    <option value=name.clone() selected=*name == chosen>{name.clone()}</option>
                                                }).collect_view()}
                                            </select>
                                            <p class="muted" style="font-size:.8rem">"Rounded, and kept between 1% and 100%."</p>
                                        }.into_any(),
                                        _ => view! {
                                            <ExprInput value=worked.clone().unwrap_or_default()
                                                commit=move |text| edit_expr(Box::new(move |v: &mut Value| {
                                                    set(v, &["data", "brightness_pct"], json!({ "expr": text }));
                                                })) />
                                            <p class="muted" style="font-size:.8rem">"Worked out when the light is turned on; rounded, and kept between 1% and 100%."</p>
                                        }.into_any(),
                                    }}
                                }
                            })}
                        }
                    })}
                    {light.filter(|_| is_light_on).map(|light| view! {
                        <LightLook light=light data=look_data edit=edit_look />
                    })}
                    <p class="muted" style="font-size:.8rem">"If the call fails, the run goes out of “failed” — or ends with an error if nothing is wired there."</p>
                }.into_any()
            }
            Node::Set { .. } => {
                let f1 = field.clone();
                let f2 = field.clone();
                view! {
                    <label>"Call it"</label>
                    <input type="text" prop:value=get(&v, &["name"]).as_str().unwrap_or_default().to_owned()
                        on:change=move |e| f1(&["name"], json!(event_target_value(&e).trim())) />
                    <label>"Work it out"</label>
                    <ExprInput value=get(&v, &["expr"]).as_str().unwrap_or_default().to_owned()
                        commit=move |text| f2(&["expr"], json!(text)) />
                    <p class="muted" style="font-size:.8rem">
                        "A “Do” node further on can set a light's brightness from it; an expression reads it as var('name')."
                    </p>
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
                        <option value="state" selected=kind == "state">"a device is…"</option>
                        <option value="expr" selected=kind == "expr">"an expression holds"</option>
                    </select>
                    {if kind == "expr" {
                        view! {
                            <ExprInput value=get(&v, &["until", "expr"]).as_str().unwrap_or_default().to_owned()
                                commit=move |text| f1(&["until", "expr"], json!(text)) />
                        }.into_any()
                    } else {
                        view! {
                            <EntityPicker value=entity kinds=lasting()
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
        <details class="json-fold" style="margin-top:.8rem">
            <summary>"Edit as JSON"</summary>
            <JsonEditor
                label="This node as JSON"
                text=move || node.get().map(|n| written(&n)).unwrap_or_default()
                apply=move |text: String| {
                    let parsed: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    // Refused here, under the box, with what was typed still in it.
                    change(&ed, &json_id, move |v: &mut Value| *v = parsed)
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
    raw: RwSignal<std::collections::BTreeSet<String>>,
) -> impl IntoView {
    condition_form(
        value,
        move |f: Box<dyn FnOnce(&mut Value)>| {
            edit(Box::new(move |v: &mut Value| {
                let mut here = get(v, path).clone();
                f(&mut here);
                set(v, path, here);
            }))
        },
        raw,
        path.join("."),
    )
}

/// A switch case's form.
#[component]
fn IndexedCondition(
    index: usize,
    value: Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
    raw: RwSignal<std::collections::BTreeSet<String>>,
) -> impl IntoView {
    condition_form(
        value,
        move |f: Box<dyn FnOnce(&mut Value)>| {
            edit(Box::new(move |v: &mut Value| {
                if let Some(case) = v["cases"].get_mut(index) {
                    f(case);
                }
            }))
        },
        raw,
        format!("cases.{index}"),
    )
}

fn condition_form(
    value: Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
    raw: RwSignal<std::collections::BTreeSet<String>>,
    key: String,
) -> impl IntoView {
    let home = expect_context::<Home>();
    let checks = (!raw.with_untracked(|r| r.contains(&key)))
        .then(|| Checks::from_condition(&value))
        .flatten();
    let expr = value["expr"].as_str().map(str::to_owned);
    let shown = match (&checks, value["type"].as_str()) {
        (Some(_), _) => "checks",
        (None, Some("expr")) => "expr",
        _ => "other",
    };
    let edit_kind = edit.clone();
    let body = match shown {
        "checks" => {
            let checks = checks.unwrap_or_default();
            view! { <ChecksForm checks=checks edit=edit.clone() /> }.into_any()
        }
        "expr" => {
            let e1 = edit.clone();
            view! {
                <ExprInput value=expr.clone().unwrap_or_default()
                    commit=move |text| e1(Box::new(move |v: &mut Value| v["expr"] = json!(text))) />
            }
            .into_any()
        }
        _ => view! { <p class="muted">"This condition is edited as JSON below."</p> }.into_any(),
    };
    let key_for_kind = key.clone();
    view! {
        <label>"Holds when"</label>
        <select on:change=move |e| {
            let picked = event_target_value(&e);
            let key = key_for_kind.clone();
            raw.update(|r| {
                if picked == "expr" { r.insert(key); } else { r.remove(&key); }
            });
            let fresh = Checks::starter(&home).render();
            edit_kind(Box::new(move |v: &mut Value| {
                *v = match picked.as_str() {
                    // Shown as the expression it is, when it is one; checks are written out.
                    "expr" => match v["expr"].as_str() {
                        Some(expr) => json!({ "type": "expr", "expr": expr }),
                        None => json!({ "type": "expr", "expr": "true" }),
                    },
                    _ if Checks::from_condition(v).is_some() => v.clone(),
                    _ => fresh,
                };
            }));
        }>
            <option value="checks" selected=shown == "checks">"these checks hold"</option>
            <option value="expr" selected=shown == "expr">"an expression holds"</option>
            {(shown == "other").then(|| view! {
                <option value="other" selected=true>"something else (JSON)"</option>
            })}
        </select>
        {body}
    }
}

/// Words that are functions, not the start of a device's name.
const FUNCTION_WORDS: [&str; 9] = [
    "num",
    "on",
    "text",
    "available",
    "var",
    "min",
    "max",
    "round",
    "clamp",
];

/// An expression to type, with the home's entities offered as you type their name or id, and
/// the functions a click away.
#[component]
fn ExprInput(value: String, commit: impl Fn(String) + Send + Sync + 'static) -> impl IntoView {
    let home = expect_context::<Home>();
    let text = RwSignal::new(value.clone());
    let committed = StoredValue::new(value);
    // The word being typed: where it starts, and what it is so far.
    let word = RwSignal::new(None::<(usize, String)>);
    let active = RwSignal::new(0usize);
    let area = NodeRef::<leptos::html::Textarea>::new();
    let commit = std::sync::Arc::new(commit);

    let suggestions = move || -> Vec<Choice> {
        let Some((_, typed)) = word.get() else {
            return Vec::new();
        };
        let typed = typed.to_lowercase();
        home.entities.with(|entities| {
            entities
                .iter()
                .filter(|e| {
                    e.id.as_str().to_lowercase().contains(&typed)
                        || e.name.as_str().to_lowercase().contains(&typed)
                })
                .take(8)
                .map(|e| {
                    Choice::new(e.id.to_string(), e.name.to_string())
                        .detail(e.id.to_string())
                        .icon(irori_ui_kit::entity_icon::drawing(&e.capabilities))
                })
                .collect()
        })
    };
    let look = move || {
        let Some(el) = area.get_untracked() else {
            return;
        };
        let value = el.value();
        let cursor = el.selection_start().ok().flatten().unwrap_or(0) as usize;
        let before: Vec<char> = value.chars().take(cursor).collect();
        let start = before
            .iter()
            .rposition(|c| !(c.is_alphanumeric() || *c == '_' || *c == '.'))
            .map_or(0, |i| i + 1);
        let typed: String = before[start..].iter().collect();
        let is_function = FUNCTION_WORDS.contains(&typed.as_str());
        word.set((typed.chars().count() >= 2 && !is_function).then_some((start, typed)));
        active.set(0);
    };
    // Puts `id` where the word being typed is, quoted unless it already is.
    let insert = move |id: String| {
        let Some((start, typed)) = word.get_untracked() else {
            return;
        };
        let value: Vec<char> = text.get_untracked().chars().collect();
        let end = (start + typed.chars().count()).min(value.len());
        let quote = (start > 0)
            .then(|| value[start - 1])
            .filter(|c| matches!(c, '\'' | '"'));
        let mut piece = match quote {
            Some(_) => id,
            None => format!("'{id}'"),
        };
        let mut caret = start + piece.chars().count();
        // `num('` finished off as `num('sensor.x')`, unless it's closed already.
        if let Some(quote) = quote {
            if value.get(end) == Some(&quote) {
                caret += 1;
            } else {
                piece.push(quote);
                caret += 1;
                if start >= 2 && value[start - 2] == '(' && value.get(end) != Some(&')') {
                    piece.push(')');
                    caret += 1;
                }
            }
        }
        let next: String = value[..start].iter().collect::<String>()
            + &piece
            + &value[end..].iter().collect::<String>();
        text.set(next.clone());
        word.set(None);
        if let Some(el) = area.get_untracked() {
            el.set_value(&next);
            let _ = el.set_selection_range(caret as u32, caret as u32);
            let _ = el.focus();
        }
    };
    let put_function = move |template: &'static str| {
        let Some(el) = area.get_untracked() else {
            return;
        };
        let value: Vec<char> = text.get_untracked().chars().collect();
        let at = (el
            .selection_start()
            .ok()
            .flatten()
            .unwrap_or(value.len() as u32) as usize)
            .min(value.len());
        let next: String = value[..at].iter().collect::<String>()
            + template
            + &value[at..].iter().collect::<String>();
        // Inside the quotes, ready for a name; or inside the brackets, ready for numbers.
        let caret = at
            + template
                .find("''")
                .or_else(|| template.find('('))
                .map_or(template.len(), |i| i + 1);
        text.set(next.clone());
        el.set_value(&next);
        let _ = el.set_selection_range(caret as u32, caret as u32);
        let _ = el.focus();
    };
    let save = {
        let commit = commit.clone();
        move || {
            let now = text.get_untracked();
            if now != committed.get_value() {
                committed.set_value(now.clone());
                commit(now);
            }
        }
    };
    let save_on_blur = save.clone();
    view! {
        <div class="expr combo" class:open=move || !suggestions().is_empty()>
            <textarea rows="3" node_ref=area spellcheck="false"
                placeholder="Type a device's name…"
                prop:value=move || text.get()
                on:input=move |e| { text.set(event_target_value(&e)); look(); }
                on:click=move |_| look()
                on:blur=move |_| { word.set(None); save_on_blur(); }
                on:keydown=move |e: web_sys::KeyboardEvent| {
                    let found = suggestions();
                    if found.is_empty() { return; }
                    match e.key().as_str() {
                        "ArrowDown" => { e.prevent_default(); active.update(|a| *a = (*a + 1).min(found.len() - 1)); }
                        "ArrowUp" => { e.prevent_default(); active.update(|a| *a = a.saturating_sub(1)); }
                        "Enter" | "Tab" => {
                            e.prevent_default();
                            if let Some(choice) = found.get(active.get_untracked()) { insert(choice.value.clone()); }
                        }
                        "Escape" => { e.prevent_default(); word.set(None); }
                        _ => {}
                    }
                }></textarea>
            <ul class="combo-list" role="listbox">
                {move || suggestions().into_iter().enumerate().map(|(i, choice)| {
                    let value = choice.value.clone();
                    view! {
                        <li role="option" class="combo-option" class:active=move || active.get() == i
                            on:pointerdown=move |e| { e.prevent_default(); e.stop_propagation(); insert(value.clone()); }
                            on:pointerenter=move |_| active.set(i)>
                            <span class="combo-label">{choice.label.clone()}</span>
                            <span class="combo-option-detail">{choice.detail.clone()}</span>
                        </li>
                    }
                }).collect_view()}
            </ul>
        </div>
        <div class="functions">
            {[("num('')", "a number"), ("on('')", "is on"), ("text('')", "a text"), ("available('')", "is there"),
              (" && ", "and"), (" || ", "or"), ("!", "not"),
              ("min(, )", "smaller of"), ("max(, )", "larger of"), ("round()", "round"), ("clamp(, , )", "keep between")]
                .into_iter()
                .map(|(template, words)| view! {
                    <button type="button" class="chip" title=template
                        on:pointerdown=move |e| { e.prevent_default(); e.stop_propagation(); put_function(template); }>{words}</button>
                })
                .collect_view()}
        </div>
    }
}

/// The calculation a brightness is taken from, when it's just `var('name')`.
fn calculation_read(expr: &str) -> Option<String> {
    let inner = expr.trim().strip_prefix("var(")?.strip_suffix(')')?.trim();
    let quote = inner.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let name = inner[1..].strip_suffix(quote)?;
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        .then(|| name.to_owned())
}
