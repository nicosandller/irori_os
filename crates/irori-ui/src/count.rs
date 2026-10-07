//! Numbers that count to their new value.
//!
//! A reading that changes from 21.4 to 22.1 runs through the numbers in between, briefly, and
//! pulses the way it went — so a glance shows not just what it says but that it just moved, and
//! which way. Any number drawn inside an element with `data-n` (its value, as a number) takes
//! part; nothing else about the page needs to know.
//!
//! The count writes into the number's own text node — the node Leptos keeps and updates — and
//! always ends on the text Leptos wrote, so the page never disagrees with what it was told to
//! show. A number seen for the first time just appears; so does every change with motion off.

use std::cell::{Cell, RefCell};

use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

/// How long a count takes: long enough to see it run, over before it's read.
const DURATION_MS: f64 = 420.0;

struct Count {
    node: web_sys::Node,
    from: f64,
    to: f64,
    decimals: usize,
    /// What Leptos wrote, to land on exactly.
    last: String,
    start: f64,
}

thread_local! {
    static RUNNING: RefCell<Vec<Count>> = const { RefCell::new(Vec::new()) };
    static TICKING: Cell<bool> = const { Cell::new(false) };
}

/// Looks for numbers that changed whenever `changes` does — each new reading.
pub fn watch(changes: impl Fn() + 'static) {
    Effect::new(move |_| {
        changes();
        // After the page has been redrawn with the new values.
        request_animation_frame(sweep);
    });
}

fn sweep() {
    let Ok(numbers) = document().query_selector_all("[data-n]") else {
        return;
    };
    let calm = still();
    for i in 0..numbers.length() {
        let Some(element) = numbers
            .item(i)
            .and_then(|n| n.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let Some(to) = number(element.get_attribute("data-n")) else {
            continue;
        };
        let seen = number(element.get_attribute("data-seen"));
        let _ = element.set_attribute("data-seen", &to.to_string());
        let Some(from) = seen else { continue };
        if from == to || calm {
            continue;
        }
        // Which way it went, under one of two names so a second move the same way plays the
        // pulse again rather than finding it already applied.
        let way = if to > from { "up" } else { "down" };
        let again = element
            .get_attribute("data-moved")
            .is_some_and(|moved| moved.ends_with("-a"));
        let _ = element.set_attribute(
            "data-moved",
            &format!("{way}-{}", if again { "b" } else { "a" }),
        );
        let Some(node) = element.first_child() else {
            continue;
        };
        let last = node.text_content().unwrap_or_default();
        // Whichever of the two was written more finely — 21.4 counting to 22 runs in tenths.
        let decimals = decimals(&last).max(decimals(&from.to_string())).min(3);
        RUNNING.with_borrow_mut(|running| {
            running.retain(|count| count.node != node);
            running.push(Count {
                node,
                from,
                to,
                decimals,
                last,
                start: now(),
            });
        });
    }
    if !TICKING.get() && RUNNING.with_borrow(|running| !running.is_empty()) {
        TICKING.set(true);
        request_animation_frame(tick);
    }
}

fn tick() {
    let at = now();
    RUNNING.with_borrow_mut(|running| {
        running.retain(|count| {
            let t = ((at - count.start) / DURATION_MS).clamp(0.0, 1.0);
            if t >= 1.0 {
                count.node.set_node_value(Some(&count.last));
                return false;
            }
            count
                .node
                .set_node_value(Some(&between(count.from, count.to, t, count.decimals)));
            true
        });
    });
    if RUNNING.with_borrow(|running| running.is_empty()) {
        TICKING.set(false);
    } else {
        request_animation_frame(tick);
    }
}

/// The number shown `t` of the way through a count: fast at first, settling into its value.
pub fn between(from: f64, to: f64, t: f64, decimals: usize) -> String {
    let eased = 1.0 - (1.0 - t).powi(3);
    format!("{:.*}", decimals, from + (to - from) * eased)
}

/// How many decimal places a number was written with, so the count shows the same.
pub fn decimals(text: &str) -> usize {
    text.split_once('.').map_or(0, |(_, fraction)| {
        fraction.chars().take_while(char::is_ascii_digit).count()
    })
}

fn number(text: Option<String>) -> Option<f64> {
    text?.parse().ok().filter(|n: &f64| n.is_finite())
}

fn now() -> f64 {
    web_sys::js_sys::Date::now()
}

/// Motion off in Settings, or the system asking for less.
pub fn still() -> bool {
    let off = document()
        .document_element()
        .and_then(|root| root.get_attribute("data-motion"))
        .as_deref()
        == Some("off");
    off || window()
        .match_media("(prefers-reduced-motion: reduce)")
        .ok()
        .flatten()
        .is_some_and(|query| query.matches())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A count starts where the number was, ends where it's going, and is past halfway early.
    #[test]
    fn a_count_runs_from_the_old_number_to_the_new() {
        assert_eq!(between(20.0, 22.0, 0.0, 1), "20.0");
        assert_eq!(between(20.0, 22.0, 1.0, 1), "22.0");
        assert_eq!(between(10.0, 0.0, 1.0, 0), "0");
        let halfway: f64 = between(0.0, 100.0, 0.5, 0).parse().expect("a number");
        assert!(
            halfway > 80.0,
            "eased out: most of the way there by half time"
        );
    }

    #[test]
    fn a_count_keeps_the_numbers_decimal_places() {
        assert_eq!(decimals("21.45"), 2);
        assert_eq!(decimals("22"), 0);
        assert_eq!(decimals("-3.5"), 1);
    }
}
