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
