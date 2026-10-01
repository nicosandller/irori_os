//! Things a finger can push around.
//!
//! - **A toggle** can be swiped as well as clicked: the knob follows the finger across, and
//!   letting go past halfway is on (or off) — then the knob springs home from where it was let
//!   go, the way something let go of does.
//! - **A history drawer** can be pulled: drag the reading down to open it, up to close it, with
//!   a little give as it goes.
//!
//! What a drag is in the middle of lives on the element itself (`data-*`), not in Rust: the row
//! it's on is redrawn when a reading arrives — in place, but with new handlers — and a drag has
//! to survive that. A drag is never also the click that follows it.

use leptos::ev;
use web_sys::Element;
use web_sys::wasm_bindgen::JsCast;

/// How far a finger has to move before it's dragging rather than clicking, in pixels.
const SLOP: f64 = 4.0;
/// How far a pull has to go to open or close the drawer.
const PULL: f64 = 24.0;

fn element(event: &ev::Event) -> Option<Element> {
    event.current_target()?.dyn_into::<Element>().ok()
}

fn number(element: &Element, name: &str) -> Option<f64> {
    element.get_attribute(name)?.parse().ok()
}

fn set(element: &Element, name: &str, value: f64) {
    let _ = element.set_attribute(name, &value.to_string());
}

/// Whether this click is the tail end of a drag, which already did what it meant; forgets it
/// either way.
pub fn swallow_click(event: &ev::MouseEvent) -> bool {
    let Some(element) = element(event) else {
        return false;
    };
    let swiped = element.has_attribute("data-swiped");
    let _ = element.remove_attribute("data-swiped");
    swiped
}

/// A finger down on a toggle: note where, and how far its knob can travel.
pub fn knob_down(event: &ev::PointerEvent) {
    let Some(toggle) = element(event) else { return };
    let Ok(Some(knob)) = toggle.query_selector(".knob") else {
        return;
    };
    let (outer, inner) = (
        toggle.get_bounding_client_rect(),
        knob.get_bounding_client_rect(),
    );
    // The knob sits one padding in from whichever end it's at; it can travel the rest.
    let padding = (inner.left() - outer.left()).min(outer.right() - inner.right());
    let travel = outer.width() - inner.width() - 2.0 * padding;
    let at = inner.left() - outer.left() - padding;
    set(&toggle, "data-x0", f64::from(event.client_x()));
    set(&toggle, "data-travel", travel);
    set(&toggle, "data-from", at);
    let _ = toggle.set_pointer_capture(event.pointer_id());
}

/// The finger moving: past the slop, the knob follows it, within the track.
pub fn knob_move(event: &ev::PointerEvent) {
    let Some(toggle) = element(event) else { return };
    let (Some(x0), Some(travel), Some(from)) = (
        number(&toggle, "data-x0"),
        number(&toggle, "data-travel"),
        number(&toggle, "data-from"),
    ) else {
        return;
    };
    let moved = f64::from(event.client_x()) - x0;
    if moved.abs() < SLOP && !toggle.has_attribute("data-dragging") {
        return;
    }
    let _ = toggle.set_attribute("data-dragging", "");
    let at = (from + moved).clamp(0.0, travel);
    set(&toggle, "data-at", at);
    if let Ok(Some(knob)) = toggle.query_selector(".knob") {
        let _ = knob.set_attribute("style", &format!("transform: translateX({at}px)"));
    }
}

/// The finger lifted. If it was dragging, which side the knob was let go on — `true` for on —
/// and the click that follows is swallowed. The knob is let go of: CSS springs it home.
pub fn knob_up(event: &ev::PointerEvent) -> Option<bool> {
    let toggle = element(event)?;
    let dragged = toggle.has_attribute("data-dragging");
    let side = number(&toggle, "data-at")
        .zip(number(&toggle, "data-travel"))
        .map(|(at, travel)| at > travel / 2.0);
    for name in [
        "data-x0",
        "data-travel",
        "data-from",
        "data-at",
        "data-dragging",
    ] {
        let _ = toggle.remove_attribute(name);
    }
    if let Ok(Some(knob)) = toggle.query_selector(".knob") {
        let _ = knob.remove_attribute("style");
    }
    if !dragged {
        return None;
    }
    let _ = toggle.set_attribute("data-swiped", "");
    side
}

/// A finger down on a drawer's handle: note where.
pub fn pull_down(event: &ev::PointerEvent) {
    let Some(handle) = element(event) else { return };
    set(&handle, "data-y0", f64::from(event.client_y()));
    let _ = handle.set_pointer_capture(event.pointer_id());
}

/// The finger moving on a drawer's handle. The handle gives a little as it's pulled; once the
/// pull is far enough, which way it went — `true` for down, open — once per pull.
pub fn pull_move(event: &ev::PointerEvent) -> Option<bool> {
    let handle = element(event)?;
    let y0 = number(&handle, "data-y0")?;
    let moved = f64::from(event.client_y()) - y0;
    if moved.abs() < SLOP {
        return None;
    }
    // Rubber-banded: it follows the finger a third as far, and never more than 8px.
    let give = (moved / 3.0).clamp(-8.0, 8.0);
    let _ = handle.set_attribute("style", &format!("translate: 0 {give}px"));
    if moved.abs() < PULL || handle.has_attribute("data-swiped") {
        return None;
    }
    let _ = handle.set_attribute("data-swiped", "");
    Some(moved > 0.0)
}

/// The finger lifted off a drawer's handle: it springs back into place.
pub fn pull_up(event: &ev::PointerEvent) {
    let Some(handle) = element(event) else { return };
    let _ = handle.remove_attribute("data-y0");
    let _ = handle.remove_attribute("style");
}
