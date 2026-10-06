//! Changing page.
//!
//! Every page change moves, the way the sidebar goes: a page further down the sidebar comes up
//! from below, one further up comes down from above, so the page and the sidebar's highlight
//! travel together. Two changes say more than a direction:
//!
//! - **The device list and a device**: the list slides aside, and the device's name travels from
//!   its row up into the page's heading, so it's plain which row was opened.
//! - **A Start tile**: the tile grows into the heading of the page it opens, and the page rises
//!   in behind it.
//!
//! The browser does the drawing (the View Transition API, started by the router); this says
//! which change it is, as `data-nav` on `<html>`, and gives the two ends of what travels the same
//! `view-transition-name`. A browser without view transitions just changes page.

use leptos::ev;
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

/// Which page change this is.
pub fn navigation(from: &str, to: &str, hint: Option<&str>) -> Option<&'static str> {
    if from == to {
        return None;
    }
    if from == "/devices" && is_device(to) {
        return Some("into-device");
    }
    if is_device(from) && to == "/devices" {
        return Some("to-list");
    }
    if from == "/" && hint == Some("tile") {
        return Some("tile");
    }
    Some(match (place(from), place(to)) {
        (Some(from), Some(to)) if to > from => "down",
        (Some(from), Some(to)) if to < from => "up",
        // Between two devices, or somewhere the sidebar doesn't list: no direction to go in.
        _ => "fade",
    })
}

/// Where a page sits in the sidebar, top to bottom. A device's page sits with the list.
fn place(path: &str) -> Option<u8> {
    match path {
        "/" => Some(0),
        "/floorplan" => Some(1),
        "/devices" => Some(2),
        path if is_device(path) => Some(2),
        "/extensions" => Some(3),
        "/assistant" => Some(4),
        path if path.starts_with("/assistant/") => Some(4),
        "/settings" => Some(5),
        _ => None,
    }
}

/// Makes `change` a view transition of `kind` — for changes that aren't a page change, like the
/// Floorplan changing floor. The browser pictures the page, `change` runs, and once the page has
/// been redrawn it animates between the two; `data-nav` says which change it was until it's done.
/// Without view transitions, `change` just happens.
pub fn around(kind: &'static str, change: impl FnOnce() + 'static) {
    use web_sys::js_sys::{Function, Promise, Reflect};
    use web_sys::wasm_bindgen::{JsValue, closure::Closure};

    let document = document();
    let start = Reflect::get(&document, &JsValue::from_str("startViewTransition"))
        .ok()
        .and_then(|start| start.dyn_into::<Function>().ok());
    let (Some(start), Some(root)) = (start, document.document_element()) else {
        change();
        return;
    };
    let _ = root.set_attribute("data-nav", kind);
    let update = Closure::once_into_js(move || {
        change();
        // Leptos redraws in its own time, a task or two later. Not "the next frame": the browser
        // holds frames back until this answers, so waiting for one would wait forever.
        Promise::new(&mut |resolve, _| {
            set_timeout(
                move || {
                    let _ = resolve.call0(&JsValue::NULL);
                },
                std::time::Duration::ZERO,
            );
        })
    });
    let Ok(transition) = start.call1(&document, &update) else {
        return;
    };
    let done = Closure::once_into_js(move |_: JsValue| {
        if root.get_attribute("data-nav").as_deref() == Some(kind) {
            let _ = root.remove_attribute("data-nav");
        }
    });
    if let Ok(finished) = Reflect::get(&transition, &JsValue::from_str("finished"))
        && let Ok(then) = Reflect::get(&finished, &JsValue::from_str("then"))
        && let Ok(then) = then.dyn_into::<Function>()
    {
        // Either way it ended: the browser skips a transition it can't draw (a page in a
        // background tab), and the change has still happened.
        let _ = then.call2(&finished, &done, &done);
    }
}

/// Which way a step inside the Add device window goes: into an extension's screen, or back out
/// to all of them. Not a page change — the address stays put — so it goes through [`around`].
pub fn step(forward: bool) -> &'static str {
    if forward { "step-in" } else { "step-out" }
}

/// Gives the element an event was on the `view-transition-name` `name`, for the next change: the
/// thing clicked is what travels (a card growing into the heading of the step it opens).
pub fn name_target(event: &ev::MouseEvent, name: &str) {
    let Some(target) = event
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    // Two elements with one name in the same picture and the browser skips the change: the card
    // that last came back from a step still has it.
    if let Ok(others) = document().query_selector_all(&format!("[style*=\"{name}\"]")) {
        for i in 0..others.length() {
            if let Some(other) = others
                .item(i)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                let style = other.get_attribute("style").unwrap_or_default();
                let kept = style.replace(&format!("view-transition-name: {name}"), "");
                let _ = other.set_attribute("style", &kept);
            }
        }
    }
    let style = target.get_attribute("style").unwrap_or_default();
    let _ = target.set_attribute("style", &format!("{style};view-transition-name: {name}"));
}

/// A Start tile, clicked: it grows into the heading of the page it opens, so it takes the
/// heading's name for the change (`hero`), and says so for [`watch`] to read.
pub fn expand(event: ev::MouseEvent) {
    let Some(tile) = event
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let style = tile.get_attribute("style").unwrap_or_default();
    let _ = tile.set_attribute("style", &format!("{style};view-transition-name: hero"));
    if let Some(root) = document().document_element() {
        let _ = root.set_attribute("data-hint", "tile");
    }
}

fn is_device(path: &str) -> bool {
    path.strip_prefix("/devices/")
        .is_some_and(|rest| !rest.is_empty())
}

/// Which device's name travels on the next page change: the one being opened, or the one
/// being left. Only that one row of the list carries the name — if every row did, every name
/// would lift out of the list and fade on its own while the page slid away.
#[derive(Debug, Clone, Copy)]
pub struct Travelling(pub RwSignal<Option<String>>);

/// The list link's `style` for device `id`: the travelling name if it's the one travelling.
pub fn list_name(id: &str) -> impl Fn() -> Option<String> + use<> {
    let travelling = expect_context::<Travelling>().0;
    let id = id.to_owned();
    move || (travelling.get().as_deref() == Some(id.as_str())).then(|| device_name(&id))
}

/// The name a device's name goes by while it travels: the same on the list's link and the
/// page's heading. Ids are safe characters already; anything else becomes `-`, since this has
/// to be a CSS identifier.
pub fn device_name(id: &str) -> String {
    format!("view-transition-name: device-{}", ident(id))
}

/// `id` as part of a CSS identifier.
pub fn ident(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Keeps `data-nav` on `<html>` saying what kind of page change is under way. Set as the address
/// changes, which is before the router hands the new page to the browser to animate.
pub fn watch(pathname: Memo<String>) {
    Effect::new(move |from: Option<String>| {
        let to = pathname.get();
        if let Some(root) = document().document_element() {
            let hint = root.get_attribute("data-hint");
            let _ = root.remove_attribute("data-hint");
            match from
                .as_deref()
                .and_then(|from| navigation(from, &to, hint.as_deref()))
            {
                Some(kind) => {
                    let _ = root.set_attribute("data-nav", kind);
                }
                None => {
                    let _ = root.remove_attribute("data-nav");
                }
            }
        }
        to
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pages move the way the sidebar goes; the list and a device slide sideways; a tile grows.
    #[test]
    fn a_page_change_goes_the_way_the_sidebar_does() {
        let go = |from, to| navigation(from, to, None);
        assert_eq!(go("/devices", "/devices/demo_lamp"), Some("into-device"));
        assert_eq!(go("/devices/demo_lamp", "/devices"), Some("to-list"));
        assert_eq!(go("/floorplan", "/extensions"), Some("down"));
        assert_eq!(go("/settings", "/devices"), Some("up"));
        assert_eq!(go("/", "/floorplan"), Some("down"));
        assert_eq!(go("/devices/demo_lamp", "/settings"), Some("down"));
        assert_eq!(go("/devices/a", "/devices/b"), Some("fade"));
        assert_eq!(go("/nowhere", "/devices"), Some("fade"));
        assert_eq!(go("/devices", "/devices"), None);
        assert_eq!(navigation("/", "/devices", Some("tile")), Some("tile"));
        assert_eq!(
            navigation("/floorplan", "/devices", Some("tile")),
            Some("down")
        );
    }

    #[test]
    fn a_step_in_the_add_device_window_says_which_way_it_goes() {
        assert_eq!(step(true), "step-in");
        assert_eq!(step(false), "step-out");
    }

    #[test]
    fn a_device_name_is_always_a_css_identifier() {
        assert_eq!(
            device_name("demo_lamp"),
            "view-transition-name: device-demo_lamp"
        );
        assert_eq!(
            device_name("aa:bb.cc"),
            "view-transition-name: device-aa-bb-cc"
        );
    }
}
