//! A window over the page, for a job with a beginning and an end: an extension's settings, or
//! adding a device.
//!
//! These used to open in place, growing the card or the row they hung off — which pushed
//! everything below them down the page and left the form competing with the list around it for
//! attention. A job that has its own Save and Cancel gets its own window.
//!
//! Closing works three ways, because people reach for all three: the × button, the backdrop, and
//! Escape. Anything inside is the caller's; this only frames it.
//!
//! The keyboard goes where the eye does: the window takes the focus as it opens, Tab goes round
//! what is in it and not on into the page behind, and closing hands the focus back to whatever
//! opened it. Where one window stands over another (a log over a chat), the keys are the top
//! one's alone.

use leptos::ev;
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

/// What Tab stops at inside a window: not what is switched off, nor what a folded drawer holds.
const STOPS: &str = ":is(a[href], button, input, select, textarea, [tabindex])\
                     :not([disabled], [tabindex=\"-1\"], [type=\"hidden\"], [inert] *)";

/// `on_close` fires for every way out. The caller decides what closing means — this component
/// draws nothing once its caller stops rendering it.
#[component]
pub fn Modal(
    /// Named in the heading, and what a screen reader announces the window as.
    title: String,
    #[prop(into)] on_close: Callback<()>,
    /// Wider than a form needs, for what's read across: a log.
    #[prop(optional)]
    wide: bool,
    children: Children,
) -> impl IntoView {
    let label = title.clone();
    let frame = NodeRef::<leptos::html::Div>::new();
    let keys = window_event_listener(ev::keydown, move |event: ev::KeyboardEvent| {
        let Some(frame) = frame.get_untracked() else {
            return;
        };
        if !on_top(&frame) {
            return;
        }
        match event.key().as_str() {
            // Something inside that took the Escape itself (a list closing) has had it.
            "Escape" if !event.default_prevented() => on_close.run(()),
            "Tab" => keep_inside(&frame, &event),
            _ => {}
        }
    });

    // Whatever had the focus gets it back when the window goes: the button that opened it.
    let opener = StoredValue::new_local(document().active_element());
    Effect::new(move |_| {
        let Some(frame) = frame.get() else {
            return;
        };
        // Unless something inside asked for the focus itself (a form's first field).
        let inside = document()
            .active_element()
            .is_some_and(|active| frame.contains(Some(&active)));
        if !inside {
            let _ = frame.focus();
        }
    });
    on_cleanup(move || {
        keys.remove();
        let opener = opener.try_get_value().flatten();
        if let Some(opener) =
            opener.and_then(|opener| opener.dyn_into::<web_sys::HtmlElement>().ok())
            && opener.is_connected()
        {
            let _ = opener.focus();
        }
    });

    view! {
        <div class="modal-backdrop" on:click=move |_| on_close.run(())>
            // Clicks inside the window stop here, so only the backdrop around it closes: typing
            // in the form, or dragging to select text in it, mustn't throw the form away.
            <div
                class="modal"
                class:wide=wide
                node_ref=frame
                // Focusable by the page, not a stop of its own.
                tabindex="-1"
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

/// Whether `frame` is the window in front: the last one on the page.
fn on_top(frame: &web_sys::Element) -> bool {
    let Ok(windows) = document().query_selector_all(".modal") else {
        return true;
    };
    windows
        .length()
        .checked_sub(1)
        .and_then(|last| windows.item(last))
        .is_none_or(|last| last.is_same_node(Some(frame)))
}

/// Tab, pressed: from the window's last stop it goes round to the first, and back the other
/// way with Shift, so the keyboard stays in the window that is open.
fn keep_inside(frame: &web_sys::Element, event: &ev::KeyboardEvent) {
    let stops: Vec<web_sys::HtmlElement> = frame
        .query_selector_all(STOPS)
        .map(|found| {
            (0..found.length())
                .filter_map(|i| found.item(i)?.dyn_into::<web_sys::HtmlElement>().ok())
                // Drawn, not merely in the page.
                .filter(|stop| {
                    let size = stop.get_bounding_client_rect();
                    size.width() > 0.0 || size.height() > 0.0
                })
                .collect()
        })
        .unwrap_or_default();
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        // Nothing to stop at: the focus stays on the window.
        event.prevent_default();
        return;
    };
    let active = document().active_element();
    let at = |stop: &web_sys::HtmlElement| {
        active
            .as_ref()
            .is_some_and(|active| active.is_same_node(Some(stop)))
    };
    // On the window itself, as it is when it has just opened: Tab goes on to the first stop on
    // its own, and only Shift+Tab needs sending round to the last.
    let on_frame = active
        .as_ref()
        .is_some_and(|active| active.is_same_node(Some(frame)));
    let beyond = active
        .as_ref()
        .is_none_or(|active| !frame.contains(Some(active)));
    let to = if event.shift_key() {
        (beyond || on_frame || at(first)).then_some(last)
    } else {
        (beyond || at(last)).then_some(first)
    };
    if let Some(to) = to {
        event.prevent_default();
        let _ = to.focus();
    }
}
