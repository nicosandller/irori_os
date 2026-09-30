//! A trigger's form: what's watched, and what it has to do — change to one of some values, or,
//! for a sensor with numbers, go below or above a level — and for how long. Irori itself is one
//! of the things to watch: its own entities, and "starts up".

use irori_types::{EntityId, SensorValueType};
use leptos::prelude::*;
use serde_json::{Value, json};

use crate::Home;
use crate::checks::TextValue;
use crate::inspector::{EntityPicker, ValueInput, WATCHABLE};
use crate::widgets::Choice;

type Edit = Box<dyn FnOnce(&mut Value)>;

/// What the picker says for the trigger that fires when Irori starts.
pub const STARTS_UP: &str = "irori:starts_up";

/// How a numeric trigger watches its sensor.
#[derive(Clone, Copy, PartialEq)]
enum Watch {
    Change,
    Below,
    Above,
    Between,
}

impl Watch {
    fn of(trigger: &Value) -> Self {
        match (
            trigger.get("above").is_some(),
            trigger.get("below").is_some(),
        ) {
            (true, true) => Self::Between,
            (false, true) => Self::Below,
            (true, false) => Self::Above,
            (false, false) => Self::Change,
        }
    }
}

/// The values in a trigger's `to`: none, one, or a list.
fn values_of(to: &Value) -> Vec<Value> {
    match to {
        Value::Null => Vec::new(),
        Value::Array(values) => values.clone(),
        one => vec![one.clone()],
    }
}

/// `to` as it's written: nothing, one value, or a list.
fn write_values(trigger: &mut Value, values: Vec<Value>) {
    let Some(object) = trigger.as_object_mut() else {
        return;
    };
    match values.len() {
        0 => {
            object.remove("to");
        }
        1 => {
            object.insert("to".into(), values.into_iter().next().unwrap_or_default());
        }
        _ => {
            object.insert("to".into(), Value::Array(values));
        }
    }
}

fn sensor_type(entity: &str, home: &Home) -> Option<SensorValueType> {
    home.entities.with_untracked(|entities| {
        entities
            .iter()
            .find(|e| e.id.as_str() == entity)
            .and_then(|e| match &e.capabilities {
                irori_types::Capabilities::Sensor(s) => Some(s.value_type),
                _ => None,
            })
    })
}

/// The trigger's form. `trigger` is the node's `trigger`; `edit` changes it.
#[component]
pub fn TriggerForm(
    trigger: Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    let home = expect_context::<Home>();
    let kind = trigger["type"].as_str().unwrap_or("state").to_owned();
    let entity = trigger["entity"].as_str().unwrap_or_default().to_owned();
    let picked = if kind == "startup" {
        STARTS_UP.to_owned()
    } else {
        entity.clone()
    };
    let edit_what = edit.clone();
    let what = view! {
        <label>"What"</label>
        <EntityPicker value=picked kinds=WATCHABLE.to_vec()
            extra=vec![Choice::new(STARTS_UP, "Irori starts up").detail("Irori")]
            pick=move |id: String| {
                let home = home;
                edit_what(Box::new(move |t: &mut Value| {
                    if id == STARTS_UP {
                        *t = json!({ "type": "startup" });
                        return;
                    }
                    let hold = t.get("for").cloned();
                    let fresh = match sensor_type(&id, &home) {
                        Some(SensorValueType::Number) => json!({ "type": "state", "entity": id }),
                        Some(SensorValueType::Text) => {
                            let now = id.parse::<EntityId>().ok()
                                .and_then(|id| home.texts(&id).into_iter().next());
                            match now {
                                Some(text) => json!({ "type": "state", "entity": id, "to": text }),
                                None => json!({ "type": "state", "entity": id }),
                            }
                        }
                        None => json!({ "type": "state", "entity": id, "to": true }),
                    };
                    *t = fresh;
                    if let Some(hold) = hold {
                        t["for"] = hold;
                    }
                }));
            } />
    };
    let body = match kind.as_str() {
        "startup" => view! {
            <p class="muted" style="font-size:.8rem">
                "Fires once each time Irori starts, when this flow is ready to run."
            </p>
        }
        .into_any(),
        "state" => state_form(&trigger, &entity, edit, home),
        _ => view! { <p class="muted">"This kind of trigger is edited as JSON below."</p> }
            .into_any(),
    };
    view! { {what} {body} }
}

fn state_form(
    trigger: &Value,
    entity: &str,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
    home: Home,
) -> AnyView {
    let hold = trigger["for"].as_str().unwrap_or_default().to_owned();
    let edit_hold = edit.clone();
    let middle = match sensor_type(entity, &home) {
        Some(SensorValueType::Number) => level_form(trigger, entity, edit),
        Some(SensorValueType::Text) => text_values_form(trigger, entity, edit),
        None => {
            let to = trigger["to"].clone();
            view! {
                <label>"Changes to"</label>
                <ValueInput entity=entity.to_owned() value=to allow_any=true
                    pick=move |value: Value| edit(Box::new(move |t: &mut Value| {
                        write_values(t, if value.is_null() { Vec::new() } else { vec![value] });
                    })) />
            }
            .into_any()
        }
    };
    view! {
        {middle}
        <label>"And stays that way for (optional, like 5s or 2m)"</label>
        <input type="text" placeholder="no need" prop:value=hold
            on:change=move |e| {
                let text = event_target_value(&e).trim().to_owned();
                edit_hold(Box::new(move |t: &mut Value| {
                    if text.is_empty() {
                        t.as_object_mut().map(|o| o.remove("for"));
                    } else {
                        t["for"] = json!(text);
                    }
                }));
            } />
    }
    .into_any()
}

/// A text sensor: any change, or a change to one of the values picked.
fn text_values_form(
    trigger: &Value,
    entity: &str,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> AnyView {
    let values = values_of(&trigger["to"]);
    let chips = values
        .iter()
        .enumerate()
        .map(|(i, value)| {
            let edit = edit.clone();
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            view! {
                <span class="value-chip">
                    {text}
                    <button type="button" title="Take this value out"
                        on:click=move |_| edit(Box::new(move |t: &mut Value| {
                            let mut values = values_of(&t["to"]);
                            if i < values.len() { values.remove(i); }
                            write_values(t, values);
                        }))>"×"</button>
                </span>
            }
        })
        .collect_view();
    let empty = values.is_empty();
    view! {
        <label>{if empty { "Changes to (any change, until you pick values)" } else { "Changes to any of" }}</label>
        <div class="value-chips">{chips}</div>
        <TextValue entity=entity.to_owned() value=String::new()
            pick=move |text: String| {
                if text.is_empty() { return; }
                edit(Box::new(move |t: &mut Value| {
                    let mut values = values_of(&t["to"]);
                    let text = json!(text);
                    if !values.contains(&text) { values.push(text); }
                    write_values(t, values);
                }));
            } />
    }
    .into_any()
}

/// A sensor with numbers: any new reading, or going below, above or between levels.
fn level_form(
    trigger: &Value,
    entity: &str,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> AnyView {
    let watch = Watch::of(trigger);
    let above = trigger["above"].as_f64();
    let below = trigger["below"].as_f64();
    let now = entity.parse::<EntityId>().ok().and_then(|id| {
        expect_context::<Home>().states.with_untracked(|states| {
            states.get(&id).and_then(|s| match &s.state {
                Some(irori_types::State::Sensor(sensor)) => match sensor.value {
                    irori_types::SensorValue::Number(n) => Some(n),
                    irori_types::SensorValue::Text(_) => None,
                },
                _ => None,
            })
        })
    });
    let start = now.map_or(30.0, f64::round);
    let edit_watch = edit.clone();
    let level = |field: &'static str, value: Option<f64>, edit: Box<dyn Fn(Edit) + Send + Sync>| {
        view! {
            <input type="number" step="any" prop:value=value.map(|v| v.to_string()).unwrap_or_default()
                on:change=move |e| {
                    let Ok(n) = event_target_value(&e).trim().parse::<f64>() else { return };
                    edit(Box::new(move |t: &mut Value| t[field] = json!(n)));
                } />
        }
    };
    let (e_above, e_below) = (edit.clone(), edit);
    view! {
        <label>"Fires when it"</label>
        <div class="row test">
            <select on:change=move |e| {
                let picked = event_target_value(&e);
                edit_watch(Box::new(move |t: &mut Value| {
                    let Some(o) = t.as_object_mut() else { return };
                    o.remove("to");
                    o.remove("from");
                    let (a, b) = (o.remove("above"), o.remove("below"));
                    let keep = |v: Option<Value>| v.unwrap_or_else(|| json!(start));
                    match picked.as_str() {
                        "below" => { o.insert("below".into(), keep(b.or(a))); }
                        "above" => { o.insert("above".into(), keep(a.or(b))); }
                        "between" => {
                            let low = a.and_then(|v| v.as_f64()).unwrap_or(start);
                            let high = b.and_then(|v| v.as_f64()).filter(|h| *h > low).unwrap_or(low + 10.0);
                            o.insert("above".into(), json!(low));
                            o.insert("below".into(), json!(high));
                        }
                        _ => {}
                    }
                }));
            }>
                <option value="change" selected=watch == Watch::Change>"has a new reading"</option>
                <option value="below" selected=watch == Watch::Below>"goes below"</option>
                <option value="above" selected=watch == Watch::Above>"goes above"</option>
                <option value="between" selected=watch == Watch::Between>"goes between"</option>
            </select>
            {matches!(watch, Watch::Above | Watch::Between)
                .then(|| level("above", above, Box::new(e_above)))}
            {(watch == Watch::Between).then(|| view! { <span class="muted">"and"</span> })}
            {matches!(watch, Watch::Below | Watch::Between)
                .then(|| level("below", below, Box::new(e_below)))}
        </div>
        {(watch != Watch::Change).then(|| view! {
            <p class="muted" style="font-size:.8rem">
                "Only when it crosses the level, not while it's already there."
                {now.map(|n| format!(" It reads {n} now."))}
            </p>
        })}
    }
    .into_any()
}
