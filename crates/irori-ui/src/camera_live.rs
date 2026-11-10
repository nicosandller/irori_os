//! A camera's live video, for its drawer: the player is `camera_live.js`, which this mounts on a
//! `<video>` while the drawer is open and takes down when it closes.
//!
//! It starts muted, since a browser won't play sound nobody asked for, with a button to turn it
//! on when the stream has any. A camera with a small and a full-size stream gets a toggle
//! between them, starting small: quicker to start, and lighter on a Raspberry Pi. While someone
//! talks through the camera (`camera_talk.rs`) its sound is muted.

use irori_types::{CameraCapabilities, EntityId, LiveQuality};
use leptos::prelude::*;

use crate::icons::{Icon, icon};
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
/// Where a camera's SD or HD is remembered, in this browser: a phone on the go may want SD
/// while the computer at home wants HD.
fn quality_key(id: &EntityId) -> String {
    format!("irori.camera.quality.{id}")
}

/// The quality remembered for a camera, when it still has it.
fn remembered(stored: Option<&str>, qualities: &[LiveQuality]) -> Option<LiveQuality> {
    qualities
        .iter()
        .copied()
        .find(|quality| Some(quality.as_str()) == stored)
}

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
    /// While true, the camera's sound is muted, so its microphone doesn't send the person's own
    /// voice back to them.
    #[prop(optional, into)]
    talking: Option<Signal<bool>>,
) -> impl IntoView {
    let video = NodeRef::<leptos::html::Video>::new();
    let standing = RwSignal::new(Standing::Starting);
    let sound = RwSignal::new(false);
    let muted = RwSignal::new(true);
    let both = capabilities.qualities.len() == 2;
    let quality = RwSignal::new(
        remembered(
            crate::devices::stored(&quality_key(&id)).as_deref(),
            &capabilities.qualities,
        )
        .or_else(|| capabilities.qualities.first().copied())
        .unwrap_or(LiveQuality::Low),
    );
    let key = StoredValue::new(quality_key(&id));
    let choose = move |chosen: LiveQuality| {
        key.with_value(|key| crate::devices::remember(key, chosen.as_str()));
        quality.set(chosen);
    };
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

    if let Some(talking) = talking {
        Effect::new(move |_| {
            let hush = talking.get() || muted.get_untracked();
            if let Some(element) = video.get_untracked() {
                let _ = Reflect::set(
                    &element,
                    &JsValue::from_str("muted"),
                    &JsValue::from_bool(hush),
                );
            }
        });
    }

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

    // Full screen: the whole camera block (`.camera-stage`: this, its talk button, its arrows)
    // fills the screen, with the controls over the video.
    let full = RwSignal::new(false);
    let on_leave = StoredValue::new_local(None::<Closure<dyn Fn()>>);
    let toggle_full = move |_| {
        let Some(element) = video.get_untracked() else {
            return;
        };
        let element: &web_sys::Element = &element;
        let stage = element
            .closest(".camera-stage")
            .ok()
            .flatten()
            .or_else(|| element.parent_element());
        let Some(stage) = stage else {
            return;
        };
        let call = Reflect::get(&window(), &JsValue::from_str("iroriCameraLive"))
            .ok()
            .and_then(|live| Reflect::get(&live, &JsValue::from_str("full")).ok())
            .and_then(|call| call.dyn_into::<Function>().ok());
        let Some(call) = call else {
            return;
        };
        if on_leave.with_value(Option::is_none) {
            on_leave.set_value(Some(Closure::<dyn Fn()>::new(move || {
                let _ = full.try_set(false);
            })));
        }
        let now = !full.get_untracked();
        let leave = on_leave.with_value(|leave| {
            leave
                .as_ref()
                .map(|leave| leave.as_ref().clone())
                .unwrap_or(JsValue::NULL)
        });
        let arguments: Array = [JsValue::from(stage), JsValue::from_bool(now), leave]
            .into_iter()
            .collect();
        if call.apply(&JsValue::NULL, &arguments).is_ok() {
            full.set(now);
        }
    };
    // Leaving the page while full: the stage goes with it, so the page isn't left covered.
    on_cleanup(move || {
        if full.get_untracked()
            && let Some(root) = document().document_element()
        {
            let _ = root.class_list().remove_1("camera-full");
        }
    });

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
                            class="live-icon glyph"
                            class:alt=move || !muted.get()
                            aria-pressed=move || (!muted.get()).to_string()
                            aria-label=move || if muted.get() { "Turn the sound on" } else { "Turn the sound off" }
                            title=move || if muted.get() { "Sound on" } else { "Sound off" }
                            on:click=toggle_sound
                        >
                            <span class="glyph-first">{icon(Icon::Muted)}</span>
                            <span class="glyph-second">{icon(Icon::Volume)}</span>
                        </button>
                    })}
                    {both.then(|| view! {
                        <span class="live-segs" role="group" aria-label="Quality">
                            <button
                                type="button"
                                class="live-seg"
                                aria-pressed=move || (quality.get() == LiveQuality::Low).to_string()
                                title="The camera's small stream: quick, and light on the network"
                                on:click=move |_| choose(LiveQuality::Low)
                            >
                                "SD"
                            </button>
                            <button
                                type="button"
                                class="live-seg"
                                aria-pressed=move || (quality.get() == LiveQuality::High).to_string()
                                title="The camera's full size"
                                on:click=move |_| choose(LiveQuality::High)
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
                    <button
                        type="button"
                        class="live-icon glyph live-full"
                        class:alt=move || full.get()
                        aria-pressed=move || full.get().to_string()
                        aria-label=move || if full.get() { "Leave full screen" } else { "Full screen" }
                        title=move || if full.get() { "Leave full screen (Esc)" } else { "Full screen" }
                        on:click=toggle_full
                    >
                        <span class="glyph-first">{icon(Icon::Expand)}</span>
                        <span class="glyph-second">{icon(Icon::Shrink)}</span>
                    </button>
                </span>
            </figcaption>
        </figure>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_quality_counts_only_if_the_camera_still_has_it() {
        let both = [LiveQuality::Low, LiveQuality::High];
        assert_eq!(remembered(Some("high"), &both), Some(LiveQuality::High));
        assert_eq!(remembered(Some("high"), &[LiveQuality::Low]), None);
        assert_eq!(remembered(Some("4k"), &both), None);
        assert_eq!(remembered(None, &both), None);
    }

    #[test]
    fn the_live_address_names_the_quality() {
        let id = EntityId::try_from("camera.front_door").expect("an id");
        assert_eq!(
            live_path(&id, LiveQuality::High),
            "/api/entities/camera.front_door/live?quality=high"
        );
    }
}
