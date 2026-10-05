//! A row of choices where exactly one is chosen, with a highlight that slides to it.
//!
//! The same `.switcher` the page has always had; what's new is that the chosen one's background
//! is a single highlight that travels (`glide.rs`) instead of one button losing it as another
//! gains it. Used for how a list is read: which view, and what it's grouped by.

use leptos::prelude::*;

/// `chosen` is read whenever the buttons paint; `on_pick` is told which one was clicked.
pub fn segmented<T>(
    label: &'static str,
    options: Vec<(T, &'static str)>,
    chosen: Signal<T>,
    on_pick: impl Fn(T) + Clone + 'static,
) -> AnyView
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    let list = NodeRef::<leptos::html::Div>::new();
    crate::glide::across(list, "button.chosen", move || {
        chosen.track();
    });
    view! {
        <div class="switcher" role="group" aria-label=label node_ref=list>
            <span class="glide" aria-hidden="true"></span>
            {options
                .into_iter()
                .map(|(value, words)| {
                    let on_pick = on_pick.clone();
                    view! {
                        <button
                            type="button"
                            class:chosen=move || chosen.get() == value
                            aria-pressed=move || (chosen.get() == value).to_string()
                            on:click=move |_| on_pick(value)
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
