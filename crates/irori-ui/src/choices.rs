//! A short list of choices, drawn as buttons.
//!
//! A browser dropdown is the operating system's control, in the operating system's colours.
//! These are the page's: the same bordered button as everywhere else, with the one that is
//! chosen sunk and edged in ember. Hover and press are the button's own motion.

use leptos::prelude::*;

/// `chosen` is the value that should look pressed, read whenever the buttons paint, so a
/// choice made here shows up without waiting for the rest of the page to be built again.
/// `disabled` is the same for a command still in flight.
pub fn choices(
    label: impl Into<String>,
    options: Vec<(String, String)>,
    chosen: impl Fn() -> Option<String> + Send + Sync + 'static,
    disabled: impl Fn() -> bool + Send + Sync + 'static,
    on_pick: impl Fn(String) + Send + Sync + 'static,
) -> AnyView {
    // Shared across the buttons. `Arc` rather than `Rc`: a reactive attribute has to be `Send`.
    let chosen = std::sync::Arc::new(chosen);
    let disabled = std::sync::Arc::new(disabled);
    let on_pick = std::sync::Arc::new(on_pick);
    let label = label.into();
    view! {
        <div class="choices" role="group" aria-label=label>
            {options
                .into_iter()
                .map(|(value, words)| {
                    let chosen = chosen.clone();
                    let disabled = disabled.clone();
                    let on_pick = on_pick.clone();
                    let value_for_pressed = value.clone();
                    view! {
                        <button
                            type="button"
                            aria-pressed=move || {
                                (chosen().as_deref() == Some(value_for_pressed.as_str())).to_string()
                            }
                            disabled=move || disabled()
                            on:click=move |_| on_pick(value.clone())
                        >
                            {words}
                        </button>
                    }
                })
                .collect_view()}
        </div>
    }
    .into_any()
}
