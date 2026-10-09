//! The plan as text: the same document `/api/floorplan` holds, to read, copy, and change.
//!
//! Dragging is the quick way to draw and the slow way to make one wall exactly 3.37 m, move a
//! whole floor a metre to the left, or bring a plan across from another home. For those the
//! plan is its own text. It is JSON because that is what the API speaks and the page already
//! reads; `floorplan.toml` says the same thing in the config directory's own words.
//!
//! What is typed here is held to exactly the rules the server holds a saved plan to
//! ([`Floorplan::check`]), and lands in the editor's working copy as one step to undo — so a
//! paste that turns out wrong is a ⌘Z, and nothing reaches the file until Save.

use irori_types::Floorplan;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

/// How long the copy button says it copied.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1600);

/// The window. `apply` is there while the plan is being edited; without it the text can only
/// be read and copied.
#[component]
pub(super) fn Source(
    plan: Floorplan,
    #[prop(optional_no_strip)] apply: Option<Callback<Floorplan>>,
    #[prop(into)] on_close: Callback<()>,
) -> impl IntoView {
    let text = RwSignal::new(written(&plan));
    let trouble = RwSignal::new(None::<String>);
    let copied = RwSignal::new(None::<bool>);
    let copy = move |_| {
        let text = text.get_untracked();
        spawn_local(async move {
            copied.set(Some(crate::log_window::write_to_clipboard(&text).await));
            set_timeout(move || copied.set(None), COPIED_FOR);
        });
    };
    view! {
        <crate::modal::Modal title="The plan as JSON".to_owned() on_close=on_close wide=true>
            <div class="plan-source">
                <p class="muted small">
                    {if apply.is_some() {
                        "Every floor, in whole centimetres. Change it and press Apply: it lands \
                         in the plan you're editing as one step to undo, and nothing is saved \
                         until you press Save."
                    } else {
                        "Every floor, in whole centimetres. Press Edit on the plan to change it \
                         here."
                    }}
                </p>
                <textarea
                    spellcheck="false"
                    autocomplete="off"
                    aria-label="The plan as JSON"
                    readonly=apply.is_none()
                    prop:value=move || text.get()
                    on:input=move |event| {
                        text.set(event_target_value(&event));
                        trouble.set(None);
                    }
                ></textarea>
                {move || trouble.get().map(|why| view! { <p class="why" role="alert">{why}</p> })}
                <div class="plan-source-actions">
                    <button type="button" on:click=copy>
                        {move || match copied.get() {
                            Some(true) => "Copied",
                            Some(false) => "Couldn't copy",
                            None => "Copy",
                        }}
                    </button>
                    {apply.map(|apply| view! {
                        <button
                            type="button"
                            class="solid"
                            on:click=move |_| match read(&text.get_untracked()) {
                                Ok(plan) => apply.run(plan),
                                Err(why) => trouble.set(Some(why)),
                            }
                        >
                            "Apply"
                        </button>
                    })}
                </div>
            </div>
        </crate::modal::Modal>
    }
}

/// A plan out of what somebody typed, or why it isn't one: not JSON, not the shape of a plan,
/// or a plan that can't be drawn — the last by the same check the server makes on Save.
pub(super) fn read(text: &str) -> Result<Floorplan, String> {
    if text.trim().is_empty() {
        // Nothing at all is a plan with nothing on it, which is what clearing the box means.
        return Ok(Floorplan::default());
    }
    let plan: Floorplan = serde_json::from_str(text).map_err(|error| error.to_string())?;
    plan.check().map_err(|why| why.to_string())?;
    Ok(plan)
}

/// A plan as text to read: indented, with each point — and each run of points — on one line.
/// The usual pretty-printing gives every number a line of its own, and a room's outline then
/// runs off the bottom of the window before it has said anything.
pub(super) fn written(plan: &Floorplan) -> String {
    let value = serde_json::to_value(plan).unwrap_or(Value::Null);
    let mut text = String::new();
    write(&value, 0, &mut text);
    text
}

/// Whether a value is short enough to say on one line: a number, a point, or a run of points.
fn brief(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(|item| match item {
            Value::Number(_) => true,
            Value::Array(inner) => inner.iter().all(Value::is_number),
            _ => false,
        }),
        Value::Object(_) => false,
        _ => true,
    }
}

fn write(value: &Value, depth: usize, into: &mut String) {
    let indent = |depth: usize, into: &mut String| into.push_str(&"  ".repeat(depth));
    match value {
        Value::Array(items) if brief(value) => {
            into.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    into.push_str(", ");
                }
                write(item, depth, into);
            }
            into.push(']');
        }
        Value::Array(items) => {
            into.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                indent(depth + 1, into);
                write(item, depth + 1, into);
                into.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
            }
            indent(depth, into);
            into.push(']');
        }
        Value::Object(fields) if fields.is_empty() => into.push_str("{}"),
        Value::Object(fields) => {
            into.push_str("{\n");
            for (index, (name, field)) in fields.iter().enumerate() {
                indent(depth + 1, into);
                into.push_str(&Value::from(name.as_str()).to_string());
                into.push_str(": ");
                write(field, depth + 1, into);
                into.push_str(if index + 1 < fields.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            indent(depth, into);
            into.push('}');
        }
        other => into.push_str(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"{"floors":{"ground":{
        "walls":[{"from":[0,0],"to":[400,0],"thickness":20,
                  "openings":[{"kind":"door","at":200,"width":80}]}],
        "areas":[{"area":"kitchen","points":[[0,0],[400,0],[400,300],[0,300]],"tint":"sky"}],
        "devices":[{"device":"demo_lamp","at":[120,90]}]}}}"#;

    #[test]
    fn a_plan_reads_back_as_the_plan_it_was_written_from() {
        let plan = read(PLAN).expect("a plan");
        let text = written(&plan);
        assert_eq!(read(&text).expect("its own text"), plan);
        // A point is one line, and so is a room's outline.
        assert!(text.contains(r#""from": [0, 0],"#), "{text}");
        assert!(
            text.contains(r#""points": [[0, 0], [400, 0], [400, 300], [0, 300]],"#),
            "{text}"
        );
        assert_eq!(written(&Floorplan::default()), "{}");
    }

    #[test]
    fn what_is_typed_is_held_to_the_rules_a_saved_plan_is() {
        assert_eq!(read("  ").expect("nothing at all"), Floorplan::default());
        let not_json = read("{").expect_err("not JSON");
        assert!(not_json.contains("line 1"), "{not_json}");
        let not_a_plan = read(r#"{"storeys":{}}"#).expect_err("not the shape of a plan");
        assert!(not_a_plan.contains("storeys"), "{not_a_plan}");
        let no_length = read(r#"{"floors":{"ground":{"walls":[{"from":[0,0],"to":[0,0]}]}}}"#)
            .expect_err("a wall with no length");
        assert!(no_length.contains("ground"), "{no_length}");
    }
}
