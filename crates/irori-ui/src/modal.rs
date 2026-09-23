//! A window over the page, for a job with a beginning and an end: an extension's settings, or
//! adding a device.
//!
//! These used to open in place, growing the card or the row they hung off — which pushed
//! everything below them down the page and left the form competing with the list around it for
//! attention. A job that has its own Save and Cancel gets its own window.
//!
//! Closing works three ways, because people reach for all three: the × button, the backdrop, and
//! Escape. Anything inside is the caller's; this only frames it.

use leptos::ev;
use leptos::prelude::*;

/// `on_close` fires for every way out. The caller decides what closing means — this component
/// draws nothing once its caller stops rendering it.
#[component]
pub fn Modal(
    /// Named in the heading, and what a screen reader announces the window as.
    title: String,
    #[prop(into)] on_close: Callback<()>,
    children: Children,
) -> impl IntoView {
    let label = title.clone();
    let keys = window_event_listener(ev::keydown, move |event: ev::KeyboardEvent| {
        if event.key() == "Escape" {
            on_close.run(());
        }
    });
    on_cleanup(move || keys.remove());

    view! {
        <div class="modal-backdrop" on:click=move |_| on_close.run(())>
            // Clicks inside the window stop here, so only the backdrop around it closes: typing
            // in the form, or dragging to select text in it, mustn't throw the form away.
            <div
                class="modal"
                role="dialog"
                aria-modal="true"
                aria-label=label
                on:click=|ev: ev::MouseEvent| ev.stop_propagation()
            >
                <div class="modal-head">
                    <h2>{title}</h2>
                    <button
                        type="button"
                        class="modal-close"
                        aria-label="Close"
                        on:click=move |_| on_close.run(())
                    >
                        "×"
                    </button>
                </div>
                <div class="modal-body">{children()}</div>
            </div>
        </div>
    }
}
