//! A camera's live video, for its drawer: the player is `camera_live.js`, which this mounts on a
//! `<video>` while the drawer is open and takes down when it closes.
//!
//! It starts muted, since a browser won't play sound nobody asked for, with a button to turn it
//! on when the stream has any. A camera with a small and a full-size stream gets a toggle
//! between them, starting small: quicker to start, and lighter on a Raspberry Pi.

use irori_types::{CameraCapabilities, EntityId, LiveQuality};
use leptos::prelude::*;
use web_sys::js_sys::{Array, Function, Reflect};
use web_sys::wasm_bindgen::closure::Closure;
use web_sys::wasm_bindgen::{JsCast as _, JsValue};

/// Where the player stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Standing {
    Starting,
    Playing,
    /// The tab is hidden; it picks up again when it's back.
    Paused,
    Ended(String),
}

/// A mounted player, and what it calls back.
struct Mounted {
    handle: JsValue,
    _on_state: Closure<dyn Fn(String, String)>,
}

impl Mounted {
    fn close(&self) {
        if let Ok(close) = Reflect::get(&self.handle, &JsValue::from_str("close"))
            && let Ok(close) = close.dyn_into::<Function>()
        {
            let _ = close.call0(&self.handle);
        }
    }
}

/// The address of a camera's live video.
pub fn live_path(id: &EntityId, quality: LiveQuality) -> String {
    format!("/api/entities/{id}/live?quality={}", quality.as_str())
}

/// A camera's live video while `open` holds.
#[component]
pub fn CameraLive(
    id: EntityId,
    name: String,
    capabilities: CameraCapabilities,
    open: Signal<bool>,
) -> impl IntoView {
    let video = NodeRef::<leptos::html::Video>::new();
    let standing = RwSignal::new(Standing::Starting);
    let sound = RwSignal::new(false);
    let muted = RwSignal::new(true);
    let both = capabilities.qualities.len() == 2;
    let quality = RwSignal::new(
        capabilities
            .qualities
            .first()
            .copied()
            .unwrap_or(LiveQuality::Low),
    );
    // Bumped by "Try again", so the player is mounted afresh.
    let attempt = RwSignal::new(0u32);
    let mounted = StoredValue::new_local(None::<Mounted>);

    let take_down = move || {
        mounted.update_value(|mounted| {
            if let Some(player) = mounted.take() {
                player.close();
            }
        });
    };

    let path_id = id.clone();
    Effect::new(move |_| {
        let wanted = quality.get();
        let _ = attempt.get();
        let showing = open.get();
        take_down();
        let Some(element) = video.get() else {
            return;
        };
        if !showing {
            return;
        }
        let mount = Reflect::get(&window(), &JsValue::from_str("iroriCameraLive"))
            .ok()
            .and_then(|live| Reflect::get(&live, &JsValue::from_str("mount")).ok())
            .and_then(|mount| mount.dyn_into::<Function>().ok());
        let Some(mount) = mount else {
            standing.set(Standing::Ended(
                "the page's player didn't load; reload the page".into(),
            ));
            return;
        };
        standing.set(Standing::Starting);
        sound.set(false);
        muted.set(true);
        let on_state =
            Closure::<dyn Fn(String, String)>::new(move |state: String, reason: String| {
                let next = match state.as_str() {
                    "playing" => {
                        let _ = sound.try_set(reason == "sound");
                        Standing::Playing
                    }
                    "paused" => Standing::Paused,
                    "ended" => Standing::Ended(reason),
                    _ => Standing::Starting,
                };
                let _ = standing.try_set(next);
            });
        let element: &web_sys::Element = &element;
        let arguments: Array = [
            JsValue::from(element.clone()),
            JsValue::from_str(&live_path(&path_id, wanted)),
            on_state.as_ref().clone(),
        ]
        .into_iter()
        .collect();
        if let Ok(handle) = mount.apply(&JsValue::NULL, &arguments) {
            mounted.set_value(Some(Mounted {
                handle,
                _on_state: on_state,
            }));
        }
    });
    on_cleanup(take_down);

    let toggle_sound = move |_| {
        let Some(element) = video.get_untracked() else {
            return;
        };
        let now = !muted.get_untracked();
        let _ = Reflect::set(
            &element,
            &JsValue::from_str("muted"),
            &JsValue::from_bool(now),
        );
        muted.set(now);
    };

    let caption = move || match standing.get() {
        Standing::Starting => "Connecting to the camera…".to_owned(),
        Standing::Playing => "Live".to_owned(),
        Standing::Paused => "Paused while this tab is hidden".to_owned(),
        Standing::Ended(why) if why.is_empty() => "The video stopped".to_owned(),
        Standing::Ended(why) => format!("The video stopped: {why}"),
    };
    let ended = move || matches!(standing.get(), Standing::Ended(_));
    let playing = move || standing.get() == Standing::Playing;

    view! {
        <figure class="camera-view camera-live" class:failed=ended class:playing=playing>
            <video
                node_ref=video
                muted=true
                playsinline=true
                autoplay=true
                aria-label=format!("{name}, live")
            ></video>
            <figcaption class="camera-live-bar small">
                <span class="camera-live-state">
                    {move || playing().then(|| view! {
                        <span class="camera-dot" aria-hidden="true"></span>
                    })}
                    {caption}
                </span>
                <span class="camera-live-buttons">
                    {move || (playing() && sound.get()).then(|| view! {
                        <button
                            type="button"
                            class="live-button"
                            aria-pressed=move || (!muted.get()).to_string()
                            on:click=toggle_sound
                        >
                            {move || if muted.get() { "Sound on" } else { "Sound off" }}
                        </button>
                    })}
                    {both.then(|| view! {
                        <span class="live-segs" role="group" aria-label="Quality">
                            <button
                                type="button"
                                class="live-seg"
                                aria-pressed=move || (quality.get() == LiveQuality::Low).to_string()
                                title="The camera's small stream: quick, and light on the network"
                                on:click=move |_| quality.set(LiveQuality::Low)
                            >
                                "SD"
                            </button>
                            <button
                                type="button"
                                class="live-seg"
                                aria-pressed=move || (quality.get() == LiveQuality::High).to_string()
                                title="The camera's full size"
                                on:click=move |_| quality.set(LiveQuality::High)
                            >
                                "HD"
                            </button>
                        </span>
                    })}
                    {move || ended().then(|| view! {
                        <button
                            type="button"
                            class="live-button"
                            on:click=move |_| attempt.update(|n| *n += 1)
                        >
                            "Try again"
                        </button>
                    })}
                </span>
            </figcaption>
        </figure>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_address_names_the_quality() {
        let id = EntityId::try_from("camera.front_door").expect("an id");
        assert_eq!(
            live_path(&id, LiveQuality::High),
            "/api/entities/camera.front_door/live?quality=high"
        );
    }
}
