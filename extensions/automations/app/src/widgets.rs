//! Controls the page is made of: a switch, and the combo box it shares with the shell — a big
//! text field that searches as you type, with the matches in a list below it.

use leptos::prelude::*;

pub use irori_ui_kit::combo::{Choice, Combo};

/// On/off, as a switch that slides.
#[component]
pub fn Toggle(
    #[prop(into)] on: Signal<bool>,
    #[prop(into)] set: Callback<bool>,
    #[prop(optional, into)] label: String,
) -> impl IntoView {
    view! {
        <button
            type="button"
            role="switch"
            class="switch"
            class:on=move || on.get()
            aria-checked=move || on.get().to_string()
            aria-label=label.clone()
            title=label
            on:click=move |event| {
                event.stop_propagation();
                set.run(!on.get_untracked());
            }
        >
            <span class="knob"></span>
        </button>
    }
}

/// The warning before a flow brought over from Home Assistant is turned on (issue #199): its
/// automation there is switched off so the two don't both run. The frame can't show a dialog,
/// so it's said in place, with the two answers.
#[component]
pub fn HandOverWarning(
    /// The Home Assistant automation that will be switched off, by its `entity_id`.
    #[prop(into)]
    automation: String,
    #[prop(into)] busy: Signal<bool>,
    confirm: Callback<()>,
    cancel: Callback<()>,
) -> impl IntoView {
    view! {
        <div class="hand-over" role="alertdialog" aria-label="Turn on in Irori"
            on:click=|event| event.stop_propagation()>
            <p>
                <strong>"Turning this on switches off "</strong>
                <span class="mono">{automation}</span>
                <strong>" in Home Assistant"</strong>
                ", so the two don't both run. Nothing else in Home Assistant changes."
            </p>
            <p class="muted">
                "Turning this flow off, deleting it, or removing Irori's Home Assistant extension "
                "switches it back on. If Irori is stopped while this flow is on, the automation "
                "stays off in Home Assistant until Irori is back."
            </p>
            <div class="row">
                <button class="btn primary" disabled=move || busy.get()
                    on:click=move |_| confirm.run(())>
                    {move || if busy.get() { "Switching it off…" } else { "Switch it off and turn this on" }}
                </button>
                <button class="btn" on:click=move |_| cancel.run(())>"Cancel"</button>
            </div>
        </div>
    }
}
