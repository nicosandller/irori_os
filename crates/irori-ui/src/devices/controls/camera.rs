//! A camera: what it's doing, with a red dot while it records, and its privacy switch. Its
//! picture is in its drawer ([`camera_picture`]), asked for again every few seconds only while
//! the drawer is open and the page is in view: five cameras on a Raspberry Pi add up.

use irori_types::{CameraCapabilities, CameraStatus, Entity, EntityId, State};
use leptos::prelude::*;

use super::knob;
use crate::devices::Controls;

/// How often an open drawer asks for the picture again.
const EVERY_MS: u64 = 3000;

/// What a camera is doing, in a word.
pub(crate) fn camera_words(status: CameraStatus) -> &'static str {
    match status {
        CameraStatus::Off => "Off",
        CameraStatus::Idle => "Watching",
        CameraStatus::Recording => "Recording",
        CameraStatus::Streaming => "Live",
    }
}

pub(crate) fn camera_control(
    entity: &Entity,
    capabilities: &CameraCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let status = match value {
        Some(State::Camera(camera)) => Some(camera.state),
        _ => None,
    };
    let recording = status == Some(CameraStatus::Recording);
    let words = status.map_or("—", camera_words);
    let switch = capabilities.on_off.then(|| {
        knob(
            entity,
            status.map(|status| status != CameraStatus::Off),
            offline,
            controls,
        )
    });
    view! {
        <span class="camera-status" class:recording=recording>
            <span class="camera-dot" aria-hidden="true"></span>
            {words}
        </span>
        {switch}
    }
    .into_any()
}

/// The picture a camera sees, for its drawer: asked for when the drawer opens, and again every
/// few seconds while it stays open and the page is in view.
pub(crate) fn camera_picture(entity: &Entity, open: Signal<bool>) -> AnyView {
    let id = entity.id.clone();
    let name = entity.name.to_string();
    let frame = RwSignal::new(0u64);
    let failed = RwSignal::new(false);
    let alive = RwSignal::new(true);
    on_cleanup(move || alive.set(false));
    leptos::task::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(std::time::Duration::from_millis(EVERY_MS)).await;
            match alive.try_get_untracked() {
                Some(true) => {}
                _ => return,
            }
            let hidden = web_sys::window()
                .and_then(|window| window.document())
                .is_some_and(|document| document.hidden());
            if open.get_untracked() && !hidden {
                frame.update(|n| *n += 1);
            }
        }
    });
    let src = move || {
        // Nothing until the drawer has been opened once.
        open.get().then(|| picture_url(&id, frame.get()))
    };
    view! {
        <figure class="camera-view" class:failed=move || failed.get()>
            <img
                src=src
                alt=format!("What {name} sees now")
                decoding="async"
                on:load=move |_| failed.set(false)
                on:error=move |_| failed.set(true)
            />
            <figcaption class="muted small">
                {move || if failed.get() {
                    "It didn't send a picture. Trying again in a moment."
                } else {
                    "Live, a new picture every few seconds"
                }}
            </figcaption>
        </figure>
    }
    .into_any()
}

/// Where a camera's picture is, a different address each time so nothing in between keeps an
/// old one.
fn picture_url(id: &EntityId, frame: u64) -> String {
    format!("/api/entities/{id}/picture?frame={frame}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_frame_has_an_address_of_its_own() {
        let id = EntityId::try_from("camera.garden_camera_1").expect("an id");
        assert_eq!(
            picture_url(&id, 3),
            "/api/entities/camera.garden_camera_1/picture?frame=3"
        );
        assert_ne!(picture_url(&id, 3), picture_url(&id, 4));
        assert_eq!(camera_words(CameraStatus::Recording), "Recording");
    }
}
