//! The zoom wheel: a long knurled strip over the zoom buttons, turned to zoom by a little.
//!
//! The buttons and the scroll wheel zoom in jumps, which is right for getting about and wrong
//! for the last bit — the moment a wall has to fill the screen just so. The strip is the fine
//! control: dragging it its whole width only doubles the zoom, and its ridges roll with the
//! zoom wherever the zoom came from, so it reads as a wheel the plan is geared to rather than
//! as a slider with nowhere to be.

use leptos::ev;
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

use super::{MAX_SCALE, MIN_SCALE};

/// How far the strip is dragged, in screen pixels, to double the zoom: its own width, near
/// enough, so a ridge stays under the pointer that is turning it.
const TURN: f64 = 144.0;

/// How much one turn of a scroll wheel over the canvas zooms, for each pixel it reports. A
/// mouse wheel reports about a hundred a notch, which comes out at the eighth a notch has
/// always zoomed by; a trackpad reports a few at a time, and gets a few at a time.
const SCROLL: f64 = 0.0012;

/// The same for a pinch, which a trackpad reports as a scroll with Control held and far
/// smaller numbers.
const PINCH: f64 = 0.01;

/// What a scroll over the strip itself is worth: a third of one over the canvas.
const FINE: f64 = 0.0004;

/// How many pixels a scroll reported in lines, or in pages, stands for.
const LINE: f64 = 33.0;
const PAGE: f64 = 300.0;

/// How much to zoom by for one wheel event: over one to zoom in, under one to zoom out, and in
/// proportion to how far the wheel says it turned, so a trackpad's stream of small turns is a
/// smooth zoom and a mouse's notch is still a notch.
pub(super) fn scrolled(delta: f64, mode: u32, pinch: bool) -> f64 {
    scrolled_by(delta, mode, if pinch { PINCH } else { SCROLL })
}

fn scrolled_by(delta: f64, mode: u32, gain: f64) -> f64 {
    let pixels = match mode {
        1 => delta * LINE,
        2 => delta * PAGE,
        _ => delta,
    };
    // One event is never worth more than a few notches, whatever a device claims.
    (-pixels.clamp(-PAGE, PAGE) * gain).exp()
}

/// How far the ridges have rolled at a zoom, in screen pixels: a turn for every doubling.
fn rolled(scale: f64) -> f64 {
    (scale / MIN_SCALE).max(f64::MIN_POSITIVE).log2() * TURN
}

/// Where a zoom falls between the least and the most, from 0 to 100, for a screen reader.
fn share(scale: f64) -> f64 {
    let whole = (MAX_SCALE / MIN_SCALE).ln();
    ((scale / MIN_SCALE).ln() / whole * 100.0).clamp(0.0, 100.0)
}

/// The strip. `scale` is the zoom as it stands, and `by` zooms by a factor about the middle of
/// the canvas.
#[component]
pub(super) fn ZoomWheel(scale: Signal<f64>, by: Callback<f64>) -> impl IntoView {
    // Where the pointer turning the strip last was. `None` when nothing is turning it.
    let held = RwSignal::new(None::<f64>);
    view! {
        <div
            class="zoom-wheel"
            class:turning=move || held.get().is_some()
            role="slider"
            tabindex="0"
            aria-label="Zoom"
            aria-orientation="horizontal"
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow=move || format!("{:.0}", share(scale.get()))
            aria-valuetext=move || format!("{:.0} pixels to the metre", scale.get() * 100.0)
            title="Drag or scroll to zoom by a little"
            style:--roll=move || format!("{:.2}px", rolled(scale.get()))
            on:pointerdown=move |event: ev::PointerEvent| {
                event.prevent_default();
                if let Some(strip) = event
                    .current_target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                {
                    // So the turn carries on when the pointer slides off the strip.
                    let _ = strip.set_pointer_capture(event.pointer_id());
                }
                held.set(Some(f64::from(event.client_x())));
            }
            on:pointermove=move |event: ev::PointerEvent| {
                let Some(last) = held.get_untracked() else { return };
                let now = f64::from(event.client_x());
                if now != last {
                    by.run(((now - last) / TURN).exp2());
                    held.set(Some(now));
                }
            }
            on:pointerup=move |_| held.set(None)
            on:pointercancel=move |_| held.set(None)
            on:wheel=move |event: ev::WheelEvent| {
                event.prevent_default();
                // Sideways counts as well: a strip that lies along the screen is one a
                // trackpad will be swiped along.
                let delta = event.delta_y() - event.delta_x();
                by.run(scrolled_by(delta, event.delta_mode(), FINE));
            }
            on:keydown=move |event: ev::KeyboardEvent| {
                let factor = match event.key().as_str() {
                    "ArrowRight" | "ArrowUp" => 1.05,
                    "ArrowLeft" | "ArrowDown" => 1.0 / 1.05,
                    "Home" => MIN_SCALE / scale.get_untracked(),
                    "End" => MAX_SCALE / scale.get_untracked(),
                    _ => return,
                };
                event.prevent_default();
                by.run(factor);
            }
        ></div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notch_of_a_mouse_wheel_zooms_by_about_an_eighth() {
        let notch = scrolled(-100.0, 0, false);
        assert!((notch - 1.127).abs() < 0.01, "{notch}");
        // Back the other way undoes it exactly, so scrolling in and out doesn't drift.
        assert!((notch * scrolled(100.0, 0, false) - 1.0).abs() < 1e-12);
        // A wheel that counts in lines is worth the same as one that counts in pixels.
        assert!((scrolled(-3.0, 1, false) - notch).abs() < 0.01);
    }

    #[test]
    fn a_trackpad_zooms_a_little_at_a_time() {
        let sliver = scrolled(-4.0, 0, false);
        assert!(sliver > 1.0 && sliver < 1.01, "{sliver}");
        // A pinch reports small numbers and means more by them.
        assert!(scrolled(-4.0, 0, true) > sliver);
        // And nothing a device reports is worth more than a few notches.
        assert_eq!(scrolled(-1e9, 0, false), scrolled(-PAGE, 0, false));
    }

    #[test]
    fn the_ridges_roll_a_turn_for_every_doubling() {
        let turn = rolled(0.6) - rolled(0.3);
        assert!((turn - TURN).abs() < 1e-9, "{turn}");
        assert_eq!(share(MIN_SCALE), 0.0);
        assert!((share(MAX_SCALE) - 100.0).abs() < 1e-9);
    }
}
