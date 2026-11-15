//! Turning a camera from its drawer: arrows to pan and tilt, buttons to zoom, and its presets.
//!
//! Holding an arrow sends `camera.move` again every [`AGAIN_MS`], each one lasting a little
//! longer than the gap, so the camera keeps turning smoothly and stops by itself soon after the
//! last one if the page goes away. Letting go sends `camera.stop`.

use std::time::Duration;

use irori_types::{CameraCapabilities, EntityId, State};
use leptos::prelude::*;

/// How often a held arrow asks again.
const AGAIN_MS: u64 = 400;
/// How long each ask keeps it moving: past the next one, so it doesn't stutter.
const LASTS_SECS: f64 = 0.6;
/// How fast it turns, from 0 to 1.
const SPEED: f64 = 0.5;

/// One way to turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Way {
    pub pan: f64,
    pub tilt: f64,
    pub zoom: f64,
}

/// The data of one `camera.move`.
pub fn move_data(way: Way) -> serde_json::Value {
    let mut data = serde_json::Map::new();
    for (key, value) in [("pan", way.pan), ("tilt", way.tilt), ("zoom", way.zoom)] {
        if value != 0.0 {
            data.insert(key.into(), value.into());
        }
    }
    data.insert("seconds".into(), LASTS_SECS.into());
    serde_json::Value::Object(data)
}

fn send(id: &EntityId, action: &'static str, data: Option<serde_json::Value>) {
    let id = id.clone();
    leptos::task::spawn_local(async move {
        let _ = crate::api::act(&id, action, data).await;
    });
}

/// The arrows, zoom buttons and presets a camera has.
#[component]
pub fn CameraTurn(id: EntityId, capabilities: CameraCapabilities) -> impl IntoView {
    let holding = StoredValue::new_local(None::<IntervalHandle>);
    let let_go = {
        let id = id.clone();
        move || {
            let was = holding.try_update_value(|held| held.take()).flatten();
            if let Some(handle) = was {
                handle.clear();
                send(&id, "stop", None);
            }
        }
    };
    on_cleanup({
        let let_go = let_go.clone();
        move || let_go()
    });

    let button = {
        let id = id.clone();
        let let_go = let_go.clone();
        move |label: &'static str, symbol: &'static str, way: Way| {
            let start = {
                let id = id.clone();
                let let_go = let_go.clone();
                move || {
                    let_go();
                    send(&id, "move", Some(move_data(way)));
                    let again = id.clone();
                    let handle = set_interval_with_handle(
                        move || send(&again, "move", Some(move_data(way))),
                        Duration::from_millis(AGAIN_MS),
                    )
                    .ok();
                    holding.set_value(handle);
                }
            };
            let (down, key_down) = (start.clone(), start);
            let (up, leave, cancel, key_up) = (
                let_go.clone(),
                let_go.clone(),
                let_go.clone(),
                let_go.clone(),
            );
            view! {
                <button
                    type="button"
                    class="turn-button"
                    aria-label=label
                    title=label
                    on:pointerdown=move |event: web_sys::PointerEvent| {
                        event.prevent_default();
                        down();
                    }
                    on:pointerup=move |_| up()
                    on:pointerleave=move |_| leave()
                    on:pointercancel=move |_| cancel()
                    on:keydown=move |event: web_sys::KeyboardEvent| {
                        if !event.repeat() && matches!(event.key().as_str(), "Enter" | " ") {
                            event.prevent_default();
                            key_down();
                        }
                    }
                    on:keyup=move |_| key_up()
                    on:contextmenu=|event: web_sys::MouseEvent| event.prevent_default()
                >
                    {symbol}
                </button>
            }
        }
    };
    let way = |pan: f64, tilt: f64, zoom: f64| Way { pan, tilt, zoom };

    let arrows = capabilities.pan_tilt.then(|| {
        view! {
            <span class="turn-pad" role="group" aria-label="Turn the camera">
                <span></span>
                {button("Up", "▲", way(0.0, SPEED, 0.0))}
                <span></span>
                {button("Left", "◀", way(-SPEED, 0.0, 0.0))}
                <span class="turn-middle" aria-hidden="true"></span>
                {button("Right", "▶", way(SPEED, 0.0, 0.0))}
                <span></span>
                {button("Down", "▼", way(0.0, -SPEED, 0.0))}
                <span></span>
            </span>
        }
    });
    let zoom = capabilities.zoom.then(|| {
        view! {
            <span class="turn-zoom" role="group" aria-label="Zoom">
                {button("Zoom in", "+", way(0.0, 0.0, SPEED))}
                {button("Zoom out", "−", way(0.0, 0.0, -SPEED))}
            </span>
        }
    });

    let presets = capabilities.presets.then(|| {
        let live = use_context::<crate::Live>();
        let watched = id.clone();
        let presets = Memo::new(move |_| {
            live.map(|live| {
                live.home.with(|home| {
                    home.states
                        .iter()
                        .find(|state| state.entity_id == watched)
                        .and_then(|state| match &state.state {
                            Some(State::Camera(camera)) => camera.presets.clone(),
                            _ => None,
                        })
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default()
        });
        let id = id.clone();
        move || {
            let list = presets.get();
            (!list.is_empty()).then(|| {
                let id = id.clone();
                view! {
                    <label class="turn-presets small">
                        "Go to "
                        <select on:change=move |event| {
                            let chosen = event_target_value(&event);
                            if !chosen.is_empty() {
                                send(
                                    &id,
                                    "go_to_preset",
                                    Some(serde_json::json!({ "preset": chosen })),
                                );
                            }
                        }>
                            <option value="" selected=true>"a preset…"</option>
                            {list
                                .into_iter()
                                .map(|preset| view! { <option value=preset.id>{preset.name}</option> })
                                .collect_view()}
                        </select>
                    </label>
                }
            })
        }
    });

    view! { <div class="camera-turn">{arrows}{zoom}{presets}</div> }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_names_only_what_turns_and_how_long() {
        assert_eq!(
            move_data(Way {
                pan: -0.5,
                tilt: 0.0,
                zoom: 0.0
            }),
            serde_json::json!({ "pan": -0.5, "seconds": 0.6 })
        );
        assert_eq!(
            move_data(Way {
                pan: 0.0,
                tilt: 0.0,
                zoom: 0.5
            }),
            serde_json::json!({ "zoom": 0.5, "seconds": 0.6 })
        );
    }

    #[test]
    fn each_move_outlasts_the_gap_before_the_next() {
        assert!(LASTS_SECS * 1000.0 > AGAIN_MS as f64);
    }

    #[test]
    fn a_move_is_a_valid_camera_move() {
        let parsed: irori_types::CameraMove = serde_json::from_value(move_data(Way {
            pan: 0.5,
            tilt: -0.5,
            zoom: 0.0,
        }))
        .expect("a move the core takes");
        assert_eq!(parsed.pan, Some(0.5));
        assert_eq!(parsed.seconds, Some(LASTS_SECS));
    }
}
