//! What a marker opens onto while the plan is being read: everything a device can do, held
//! down for; what a camera sees, clicked on; and who is at the door while a doorbell rings.
//!
//! A tap on a marker does the one thing that device is for — switch the lamp, lock the door. The
//! rest of what it can do (a lamp's brightness, a thermostat's target) is a long press or a
//! right-click away, in [`More`], which draws each of the device's entities with the same control
//! its row has on the device's own page. So every kind of entity there is has its controls here
//! without the plan knowing anything about any of them.

use irori_types::{Availability, Capabilities, Entity, EntityCategory, EntityId};
use leptos::ev;
use leptos::prelude::*;

use crate::devices::{self, Controls};
use crate::icons::{Icon, icon};

/// How long a press has to last before it is a hold rather than a click.
pub(super) const HOLD_MS: u64 = 450;

/// How far a pointer may wander, in pixels, and still be holding the same marker.
pub(super) const HOLD_SLOP: f64 = 8.0;

/// The fastest a feed asks its camera for a new picture: a new one as soon as the last has
/// arrived, but not more often than this.
const FEED_MS: u64 = 350;

/// How much a feed may be zoomed in, and the step a button or a notch of the wheel takes.
const MOST_ZOOM: f64 = 6.0;
const ZOOM_STEP: f64 = 1.25;

/// The entities a hold shows: the ones that make the device what it is, then its settings.
/// Diagnostics (signal strength, uptime) are the device page's business, not the plan's.
pub(super) fn shown(entities: Vec<Entity>) -> Vec<Entity> {
    let mut entities: Vec<Entity> = entities
        .into_iter()
        .filter(|entity| entity.entity_category != Some(EntityCategory::Diagnostic))
        .collect();
    entities.sort_by_key(|entity| entity.entity_category.is_some());
    entities
}

/// Everything one device can do, next to its marker. `at` is the marker's place on screen.
#[component]
pub(super) fn More(
    name: String,
    entities: Vec<Entity>,
    at: (f64, f64),
    /// Where the device's own page is, for anything the plan has no room for.
    page: Option<String>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_watch: Callback<EntityId>,
) -> impl IntoView {
    let controls = expect_context::<Controls>();
    let rows = entities
        .into_iter()
        .map(|entity| view! { <Row entity=entity controls=controls on_watch=on_watch /> })
        .collect_view();
    let (x, y) = at;
    let said = format!("{name}: everything it can do");
    view! {
        <div
            class="more"
            role="dialog"
            aria-label=said
            style=format!("left:{x}px;top:{y}px")
            on:mousedown=|event: ev::MouseEvent| event.stop_propagation()
            on:click=|event: ev::MouseEvent| event.stop_propagation()
            on:wheel=|event: ev::WheelEvent| event.stop_propagation()
            on:keydown=move |event: ev::KeyboardEvent| {
                if event.key() == "Escape" {
                    on_close.run(());
                }
            }
        >
            <div class="more-head">
                <h2>{name}</h2>
                <button
                    type="button"
                    class="icon-button"
                    aria-label="Close"
                    autofocus
                    on:click=move |_| on_close.run(())
                >
                    {icon(Icon::Close)}
                </button>
            </div>
            <div class="more-rows">{rows}</div>
            {page.map(|page| view! { <a class="more-page quiet-button" href=page>"Open its page"</a> })}
        </div>
    }
}

/// One entity and its control, drawn again only when that entity says something new — the
/// device page does the same, and it is what keeps a slider still under the finger dragging it
/// while the rest of the home reports.
#[component]
fn Row(entity: Entity, controls: Controls, on_watch: Callback<EntityId>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let id = entity.id.clone();
    let state = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                home.states
                    .iter()
                    .find(|state| state.entity_id == id)
                    .cloned()
            })
        })
    };
    let offline = Memo::new(move |_| {
        state.with(|state| {
            state
                .as_ref()
                .is_some_and(|state| state.availability == Availability::Unavailable)
        })
    });
    let control = match devices::kept(&entity, state.into(), offline.into(), controls) {
        Some(kept) => kept,
        None => {
            let entity = entity.clone();
            (move || devices::control(&entity, state.get().as_ref(), offline.get(), controls))
                .into_any()
        }
    };
    let watch = matches!(entity.capabilities, Capabilities::Camera(_)).then(|| {
        let id = id.clone();
        view! {
            <button type="button" class="quiet-button" on:click=move |_| on_watch.run(id.clone())>
                "Watch"
            </button>
        }
    });
    let failure = move || {
        controls
            .failures
            .with(|failures| failures.get(&id).cloned())
    };
    view! {
        <div class="entity more-row" class:offline=move || offline.get()>
            {crate::icons::entity(&entity.capabilities)}
            <span class="name">{entity.name.to_string()}</span>
            {control}
            {watch}
            {move || failure().map(|why| view! { <p class="why">{why}</p> })}
        </div>
    }
}

/// A new picture as soon as the last one has arrived, no faster than `every`, for as long as
/// `open` holds and the page is in view. The returned signal is the address of the next one.
fn pictures(camera: EntityId, every: u64, open: Signal<bool>) -> (Signal<String>, Callback<()>) {
    let frame = RwSignal::new(0u64);
    let alive = RwSignal::new(true);
    on_cleanup(move || alive.set(false));
    // Arrived (or failed): wait out the rest of the interval, then ask again.
    let next = Callback::new(move |()| {
        leptos::task::spawn_local(async move {
            gloo_timers::future::sleep(std::time::Duration::from_millis(every)).await;
            loop {
                match alive.try_get_untracked() {
                    Some(true) => {}
                    _ => return,
                }
                let hidden = web_sys::window()
                    .and_then(|window| window.document())
                    .is_some_and(|document| document.hidden());
                if open.get_untracked() && !hidden {
                    frame.update(|n| *n += 1);
                    return;
                }
                gloo_timers::future::sleep(std::time::Duration::from_millis(1000)).await;
            }
        });
    });
    let src =
        Signal::derive(move || format!("/api/entities/{camera}/picture?frame={}", frame.get()));
    (src, next)
}

/// What a camera sees, big, with zoom: the window a click on a camera opens.
///
/// The camera is asked for a new picture as soon as the last one has arrived — a few a second
/// on a camera that answers quickly — which is how a camera that only sends stills (ESPHome's
/// do) is watched live. Zoom is the page's: the picture is scaled and moved, the camera isn't.
#[component]
pub(super) fn Feed(
    camera: EntityId,
    name: String,
    #[prop(into)] on_close: Callback<()>,
) -> impl IntoView {
    let (src, next) = pictures(camera, FEED_MS, Signal::stored(true));
    let failed = RwSignal::new(false);
    let zoom = RwSignal::new(1.0_f64);
    // How far the picture has been moved, in its own pixels at zoom 1.
    let pan = RwSignal::new((0.0_f64, 0.0_f64));
    let grab = StoredValue::new(None::<((f64, f64), (f64, f64))>);
    let zoom_to = move |to: f64| {
        let to = to.clamp(1.0, MOST_ZOOM);
        zoom.set(to);
        if to <= 1.0 {
            pan.set((0.0, 0.0));
        }
    };
    let transform = move || {
        let (x, y) = pan.get();
        format!("translate({x}px, {y}px) scale({})", zoom.get())
    };
    view! {
        <div
            class="feed-backdrop"
            on:click=move |event: ev::MouseEvent| {
                event.stop_propagation();
                on_close.run(());
            }
            on:mousedown=|event: ev::MouseEvent| event.stop_propagation()
            on:wheel=|event: ev::WheelEvent| event.stop_propagation()
            on:contextmenu=|event: ev::MouseEvent| event.stop_propagation()
        >
            <div
                class="feed"
                role="dialog"
                aria-label=format!("What {name} sees")
                on:click=|event: ev::MouseEvent| event.stop_propagation()
                on:keydown=move |event: ev::KeyboardEvent| match event.key().as_str() {
                    "Escape" => on_close.run(()),
                    "+" | "=" => zoom_to(zoom.get_untracked() * ZOOM_STEP),
                    "-" => zoom_to(zoom.get_untracked() / ZOOM_STEP),
                    "0" => zoom_to(1.0),
                    _ => {}
                }
            >
                <div class="feed-head">
                    <h2>{name.clone()}</h2>
                    <span class="feed-live" class:failed=move || failed.get()>
                        <span class="camera-dot" aria-hidden="true"></span>
                        {move || if failed.get() { "Not answering" } else { "Live" }}
                    </span>
                    <button
                        type="button"
                        class="icon-button"
                        aria-label="Close"
                        autofocus
                        on:click=move |_| on_close.run(())
                    >
                        {icon(Icon::Close)}
                    </button>
                </div>
                <div
                    class="feed-frame"
                    class:zoomed=move || { zoom.get() > 1.0 }
                    on:wheel=move |event: ev::WheelEvent| {
                        event.prevent_default();
                        let step = if event.delta_y() < 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
                        zoom_to(zoom.get_untracked() * step);
                    }
                    on:pointerdown=move |event: ev::PointerEvent| {
                        if zoom.get_untracked() <= 1.0 {
                            return;
                        }
                        event.prevent_default();
                        let at = (f64::from(event.client_x()), f64::from(event.client_y()));
                        grab.set_value(Some((at, pan.get_untracked())));
                    }
                    on:pointermove=move |event: ev::PointerEvent| {
                        let Some((from, was)) = grab.get_value() else { return };
                        if event.buttons() & 1 == 0 {
                            grab.set_value(None);
                            return;
                        }
                        let (x, y) = (f64::from(event.client_x()), f64::from(event.client_y()));
                        pan.set((was.0 + x - from.0, was.1 + y - from.1));
                    }
                    on:pointerup=move |_| grab.set_value(None)
                    on:dblclick=move |_| {
                        zoom_to(if zoom.get_untracked() > 1.0 { 1.0 } else { 2.0 })
                    }
                >
                    <img
                        src=move || src.get()
                        alt=format!("What {name} sees now")
                        draggable="false"
                        style:transform=transform
                        on:load=move |_| {
                            failed.set(false);
                            next.run(());
                        }
                        on:error=move |_| {
                            failed.set(true);
                            next.run(());
                        }
                    />
                </div>
                <div class="feed-zoom" role="group" aria-label="Zoom">
                    <button
                        type="button"
                        aria-label="Zoom out"
                        disabled=move || { zoom.get() <= 1.0 }
                        on:click=move |_| zoom_to(zoom.get_untracked() / ZOOM_STEP)
                    >
                        "−"
                    </button>
                    <button
                        type="button"
                        class="feed-level"
                        aria-label="Back to the whole picture"
                        on:click=move |_| zoom_to(1.0)
                    >
                        {move || format!("{:.0}%", zoom.get() * 100.0)}
                    </button>
                    <button
                        type="button"
                        aria-label="Zoom in"
                        disabled=move || { zoom.get() >= MOST_ZOOM }
                        on:click=move |_| zoom_to(zoom.get_untracked() * ZOOM_STEP)
                    >
                        "+"
                    </button>
                </div>
            </div>
        </div>
    }
}

/// Who is at the door, in a small window beside a doorbell that has just rung. A click on it
/// opens the whole feed.
#[component]
pub(super) fn Peek(
    camera: EntityId,
    name: String,
    at: (f64, f64),
    #[prop(into)] on_open: Callback<()>,
    #[prop(into)] on_close: Callback<()>,
) -> impl IntoView {
    let (src, next) = pictures(camera, 1000, Signal::stored(true));
    let (x, y) = at;
    view! {
        <div
            class="peek"
            style=format!("left:{x}px;top:{y}px")
            on:mousedown=|event: ev::MouseEvent| event.stop_propagation()
            on:click=|event: ev::MouseEvent| event.stop_propagation()
        >
            <button
                type="button"
                class="peek-picture"
                aria-label=format!("{name} is ringing — see who's there")
                on:click=move |_| on_open.run(())
            >
                <img
                    src=move || src.get()
                    alt=""
                    draggable="false"
                    on:load=move |_| next.run(())
                    on:error=move |_| next.run(())
                />
                <span class="peek-says">{format!("{name} rang")}</span>
            </button>
            <button
                type="button"
                class="peek-close icon-button"
                aria-label="Dismiss"
                on:click=move |_| on_close.run(())
            >
                {icon(Icon::Close)}
            </button>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{EntityCategory, SwitchCapabilities};

    use super::*;

    fn entity(id: &str, category: Option<EntityCategory>) -> Entity {
        Entity {
            id: id.parse().expect("a valid entity id"),
            protocol: "demo".parse().expect("a valid protocol id"),
            unique_id: id.replace('.', "-").parse().expect("a valid unique id"),
            name: "Thing".parse().expect("a valid name"),
            device_id: None,
            area_id: None,
            capabilities: Capabilities::Switch(SwitchCapabilities::default()),
            entity_category: category,
        }
    }

    /// The device's own things first, then its settings; never its diagnostics.
    #[test]
    fn a_hold_shows_what_the_device_is_then_its_settings() {
        let shown: Vec<String> = shown(vec![
            entity("switch.child_lock", Some(EntityCategory::Config)),
            entity("switch.signal", Some(EntityCategory::Diagnostic)),
            entity("switch.relay", None),
        ])
        .into_iter()
        .map(|entity| entity.id.to_string())
        .collect();
        assert_eq!(shown, ["switch.relay", "switch.child_lock"]);
    }
}
