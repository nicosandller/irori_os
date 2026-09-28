//! A highlight that glides to whatever is chosen in a list, instead of the choice jumping.
//!
//! The list draws its own `<span class="glide">` as a direct child, and CSS says how wide it is
//! and what it looks like; this only measures where the chosen item sits and moves the
//! highlight there. The first placement lands without travelling, so a page never opens with
//! the highlight sliding in from the top.
//!
//! Only the vertical position and height are measured: every list this is used on is a column
//! of items as wide as the list. Where one lays out in a row instead (the sidebar and the
//! Floorplan tools on a phone), CSS hides the highlight and the chosen item draws its own.

use leptos::ev;
use leptos::html::ElementType;
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

/// Keeps `container`'s `.glide` over its descendant matching `selector`, measured again after
/// every change `track` reads and whenever the window changes size.
pub fn glide<E>(container: NodeRef<E>, selector: &'static str, track: impl Fn() + 'static)
where
    E: ElementType,
    E::Output: JsCast + Clone + 'static,
{
    let measure = move || {
        // After the DOM has caught up with whatever `track` just read: the chosen item's class
        // or `aria-current` is set by the same change, and may not be there yet.
        request_animation_frame(move || {
            if let Some(element) = container.get_untracked() {
                place(element.unchecked_ref(), selector);
            }
        })
    };
    Effect::new(move |_| {
        track();
        measure();
    });
    let resize = window_event_listener(ev::resize, move |_| measure());
    on_cleanup(move || resize.remove());
}

fn place(container: &web_sys::Element, selector: &str) {
    let Ok(Some(highlight)) = container.query_selector(":scope > .glide") else {
        return;
    };
    let Ok(Some(chosen)) = container.query_selector(selector) else {
        // Nothing chosen here (Start has no entry in the sidebar): fade out, and forget where it
        // was, so the next choice is landed on rather than travelled to from somewhere stale.
        let _ = highlight.remove_attribute("data-shown");
        let _ = highlight.remove_attribute("data-placed");
        return;
    };
    let (outer, inner) = (
        container.get_bounding_client_rect(),
        chosen.get_bounding_client_rect(),
    );
    // Measured from the container's padding edge, which is where an absolutely placed child's
    // `top: 0` is, and through any scrolling the container has done.
    let top = inner.top() - outer.top() - f64::from(container.client_top())
        + f64::from(container.scroll_top());
    let _ = highlight.set_attribute(
        "style",
        &format!(
            "transform: translateY({top}px); height: {}px",
            inner.height()
        ),
    );
    let _ = highlight.set_attribute("data-shown", "");
    if highlight.get_attribute("data-placed").is_none() {
        // From the next frame on it travels; this frame it just lands.
        request_animation_frame(move || {
            let _ = highlight.set_attribute("data-placed", "");
        });
    }
}
