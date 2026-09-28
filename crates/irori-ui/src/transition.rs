//! Moving between the device list and a device.
//!
//! Page changes are instant, except this one pair: the list slides aside for the device, and
//! the device's name travels from its row up into the page's heading, so it's plain which row
//! was opened. The browser does the drawing (the View Transition API, started by the router);
//! this says which way the page is going, as `data-nav` on `<html>`, and gives the two ends of
//! the name the same `view-transition-name`. A browser without view transitions just changes
//! page.

use leptos::prelude::*;

/// Which of the animated page changes this is, if either.
pub fn navigation(from: &str, to: &str) -> Option<&'static str> {
    if from == "/devices" && is_device(to) {
        Some("into-device")
    } else if is_device(from) && to == "/devices" {
        Some("to-list")
    } else {
        None
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
    let ident: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("view-transition-name: device-{ident}")
}

/// Keeps `data-nav` on `<html>` saying what kind of page change is under way. Set as the address
/// changes, which is before the router hands the new page to the browser to animate.
pub fn watch(pathname: Memo<String>) {
    Effect::new(move |from: Option<String>| {
        let to = pathname.get();
        if let Some(root) = document().document_element() {
            match from.as_deref().and_then(|from| navigation(from, &to)) {
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

    /// Only the list and a device animate, in either direction; everything else is instant.
    #[test]
    fn only_the_list_and_a_device_animate() {
        assert_eq!(
            navigation("/devices", "/devices/demo_lamp"),
            Some("into-device")
        );
        assert_eq!(
            navigation("/devices/demo_lamp", "/devices"),
            Some("to-list")
        );
        assert_eq!(navigation("/devices", "/extensions"), None);
        assert_eq!(navigation("/", "/devices/demo_lamp"), None);
        assert_eq!(navigation("/devices/a", "/devices/b"), None);
        assert_eq!(navigation("/devices", "/devices/"), None);
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
