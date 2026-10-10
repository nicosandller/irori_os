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
use irori_ui_kit::json::pieces;
use leptos::prelude::*;
use leptos::task::spawn_local;
use web_sys::wasm_bindgen::JsCast;

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
    let painted = NodeRef::<leptos::html::Pre>::new();
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
                // A text box can't colour what is in it, so the colour is a second copy of the
                // text drawn underneath, in the same letters at the same place, and the box
                // itself is typed into with its own letters see-through. The two scroll as one.
                <div class="plan-code">
                    <pre aria-hidden="true" node_ref=painted>
                        {move || {
                            text.with(|text| pieces(text))
                                .into_iter()
                                .map(|(kind, piece)| view! { <span class=kind.class()>{piece}</span> })
                                .collect_view()
                        }}
                        // A last line with nothing on it still has to take up a line.
                        "\n"
                    </pre>
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
                        on:scroll=move |event| {
                            let Some(pre) = painted.get_untracked() else { return };
                            if let Some(box_) = event
                                .target()
                                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                            {
                                pre.set_scroll_top(box_.scroll_top());
                                pre.set_scroll_left(box_.scroll_left());
                            }
                        }
                    ></textarea>
                </div>
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
    irori_ui_kit::json::written(&serde_json::to_value(plan).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_ui_kit::json::Kind;

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
    fn the_text_is_coloured_by_what_each_piece_is() {
        let got = pieces(r#"{"kind": "door", "at": -20, "open": true}"#);
        let of = |kind: Kind| -> Vec<&str> {
            got.iter()
                .filter(|(each, _)| *each == kind)
                .map(|(_, piece)| piece.as_str())
                .collect()
        };
        assert_eq!(of(Kind::Name), [r#""kind""#, r#""at""#, r#""open""#]);
        assert_eq!(of(Kind::Text), [r#""door""#]);
        assert_eq!(of(Kind::Number), ["-20"]);
        assert_eq!(of(Kind::Word), ["true"]);
    }

    /// The colouring runs on every key, so it sees every kind of half-typed nonsense, and what
    /// it draws has to be the text that was typed, letter for letter, or the letters under
    /// the caret are not the ones on the screen.
    #[test]
    fn whatever_is_typed_comes_back_whole() {
        for text in [
            PLAN,
            "",
            "{\"floors\": {\"gro",
            "\"ends in an escape\\",
            "\"a \\\" inside\": 1",
            "café ☕ \"naïve\": nul",
            "\"unclosed\n\"next\": [1,2",
            "-",
            "tru fals nulll",
        ] {
            let back: String = pieces(text).into_iter().map(|(_, piece)| piece).collect();
            assert_eq!(back, text);
        }
        // A string left open stops at the end of its line.
        let open = pieces("\"unclosed\n\"next\": 1");
        assert_eq!(open[0], (Kind::Text, "\"unclosed\n".to_owned()));
        assert_eq!(open[1], (Kind::Name, "\"next\"".to_owned()));
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
