//! Talking through a camera from its drawer: a "Hold to talk" button, like a walkie-talkie. The
//! microphone and the socket are `camera_talk.js`; this is the button and what it says.
//!
//! Pressing it (a pointer, or Space or Enter while it has focus) starts, letting go stops. While
//! it's held `talking` is true, so the camera's live sound can be muted and the camera doesn't
//! hear itself.

use irori_types::EntityId;
use leptos::prelude::*;

use crate::icons::{Icon, icon};
use web_sys::js_sys::{Array, Function, Reflect};
use web_sys::wasm_bindgen::closure::Closure;
use web_sys::wasm_bindgen::{JsCast as _, JsValue};

/// Where the button stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Standing {
    Idle,
    Connecting,
    Talking,
    /// It stopped by itself, and why.
    Failed(String),
}

/// What a person is told when a talk stops by itself.
fn told(reason: &str) -> String {
    match reason {
        "insecure" => {
            "Talking needs Irori on https (start it with --tls), or this page opened on the \
             computer Irori runs on"
                .to_owned()
        }
        "blocked" => "The browser didn't allow the microphone".to_owned(),
        "no-microphone" => "There's no microphone to talk with".to_owned(),
        other => format!("Stopped: {other}"),
    }
}

/// The address a camera is talked through.
pub fn talk_path(id: &EntityId) -> String {
    format!("/api/entities/{id}/talk")
}

/// A talk under way, and what it calls back.
struct Started {
    handle: JsValue,
    _on_state: Closure<dyn Fn(String, String)>,
}

impl Started {
    fn stop(&self) {
        if let Ok(stop) = Reflect::get(&self.handle, &JsValue::from_str("stop"))
            && let Ok(stop) = stop.dyn_into::<Function>()
        {
            let _ = stop.call0(&self.handle);
        }
    }
}

/// The "Hold to talk" button for a camera with a speaker.
#[component]
pub fn CameraTalk(id: EntityId, name: String, talking: RwSignal<bool>) -> impl IntoView {
    let standing = RwSignal::new(Standing::Idle);
    let started = StoredValue::new_local(None::<Started>);
    let button = NodeRef::<leptos::html::Button>::new();

    let let_go = move || {
        if let Some(talk) = started.try_update_value(Option::take).flatten() {
            talk.stop();
        }
        let _ = talking.try_set(false);
    };
    on_cleanup(let_go);

    let path = talk_path(&id);
    let press = move || {
        let_go();
        let start = Reflect::get(&window(), &JsValue::from_str("iroriCameraTalk"))
            .ok()
            .and_then(|talk| Reflect::get(&talk, &JsValue::from_str("start")).ok())
            .and_then(|start| start.dyn_into::<Function>().ok());
        let Some(start) = start else {
            standing.set(Standing::Failed(
                "The page's microphone part didn't load; reload the page".into(),
            ));
            return;
        };
        standing.set(Standing::Connecting);
        talking.set(true);
        let on_state =
            Closure::<dyn Fn(String, String)>::new(move |state: String, reason: String| {
                let next = match state.as_str() {
                    "connecting" => Standing::Connecting,
                    "talking" => Standing::Talking,
                    _ if reason.is_empty() => Standing::Idle,
                    _ => Standing::Failed(told(&reason)),
                };
                if !matches!(next, Standing::Connecting | Standing::Talking) {
                    let _ = talking.try_set(false);
                }
                let _ = standing.try_set(next);
            });
        let level_on = button.get_untracked().map_or(JsValue::NULL, JsValue::from);
        let arguments: Array = [
            JsValue::from_str(&path),
            on_state.as_ref().clone(),
            level_on,
        ]
        .into_iter()
        .collect();
        if let Ok(handle) = start.apply(&JsValue::NULL, &arguments) {
            started.set_value(Some(Started {
                handle,
                _on_state: on_state,
            }));
        }
    };

    let label = move || match standing.get() {
        Standing::Connecting => "Connecting…",
        Standing::Talking => "Talking · let go to stop",
        Standing::Idle | Standing::Failed(_) => "Hold to talk",
    };
    // What the styles key on: a turning arc while it connects, ripples while it talks, a shake
    // when it stopped by itself.
    let state = move || match standing.get() {
        Standing::Idle => "idle",
        Standing::Connecting => "connecting",
        Standing::Talking => "talking",
        Standing::Failed(_) => "failed",
    };
    let held = move || matches!(standing.get(), Standing::Connecting | Standing::Talking);
    let live = move || standing.get() == Standing::Talking;
    let failure = move || match standing.get() {
        Standing::Failed(why) => Some(view! { <span class="camera-talk-why small">{why}</span> }),
        _ => None,
    };
    let (down, key_down) = (press.clone(), press);

    view! {
        <div class="camera-talk" data-state=state>
            <button
                node_ref=button
                type="button"
                class="talk-button"
                class:held=held
                class:live=live
                aria-pressed=move || held().to_string()
                title=format!("Hold to talk through {name}'s speaker")
                on:pointerdown=move |event: web_sys::PointerEvent| {
                    event.prevent_default();
                    // Keep getting the pointer's let-go even if it slides off the button.
                    if let Some(target) = event.target()
                        && let Ok(element) = target.dyn_into::<web_sys::Element>()
                    {
                        let _ = element.set_pointer_capture(event.pointer_id());
                    }
                    down();
                }
                on:pointerup=move |_| let_go()
                on:pointercancel=move |_| let_go()
                on:lostpointercapture=move |_| let_go()
                on:keydown=move |event: web_sys::KeyboardEvent| {
                    if !event.repeat() && matches!(event.key().as_str(), "Enter" | " ") {
                        event.prevent_default();
                        key_down();
                    }
                }
                on:keyup=move |event: web_sys::KeyboardEvent| {
                    if matches!(event.key().as_str(), "Enter" | " ") {
                        let_go();
                    }
                }
                on:blur=move |_| let_go()
                on:contextmenu=|event: web_sys::MouseEvent| event.prevent_default()
            >
                <span class="talk-disc" aria-hidden="true">
                    <span class="talk-wave"></span>
                    <span class="talk-wave talk-wave-late"></span>
                    <span class="talk-level"></span>
                    <span class="talk-arc"></span>
                    {icon(Icon::Mic)}
                </span>
                <span class="talk-label" aria-live="polite">{label}</span>
            </button>
            {failure}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_talk_address_is_the_cameras() {
        let id = EntityId::try_from("camera.front_door").expect("an id");
        assert_eq!(talk_path(&id), "/api/entities/camera.front_door/talk");
    }

    #[test]
    fn what_the_browser_refused_is_said_plainly() {
        assert!(told("insecure").contains("https"));
        assert!(told("blocked").contains("microphone"));
        assert_eq!(
            told("someone else is talking through this camera"),
            "Stopped: someone else is talking through this camera"
        );
    }
}
