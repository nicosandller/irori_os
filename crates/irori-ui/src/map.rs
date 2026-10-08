//! A map to put a pin on: where the home is.
//!
//! The picture is OpenFreeMap's, in its Positron style on the light page and its Dark style on
//! the dark one, with the ground and the water moved onto Irori's own paper and ink. It is
//! drawn by MapLibre, which `map.js` loads only when a map is shown. What stands on it is the
//! page's: the pin is Irori's mark, put down like any marker on the Floorplan with a bounce,
//! and the zoom buttons are the page's buttons.
//!
//! All of that needs the internet, which nothing else here does, so the map is an extra and
//! never the only way: when it can't be loaded, it says so and the coordinates under it carry
//! on by themselves.

use leptos::ev;
use leptos::prelude::*;
use web_sys::js_sys::{Array, Function, Object, Reflect};
use web_sys::wasm_bindgen::closure::Closure;
use web_sys::wasm_bindgen::{JsCast as _, JsValue};

/// How far out and in the map goes: the whole world, down to a street.
pub const MIN_ZOOM: f64 = 1.0;
pub const MAX_ZOOM: f64 = 18.0;

/// How close the map comes when it is sent to a place somebody searched for.
pub const FOUND_ZOOM: f64 = 15.0;

/// How close it opens on a home that already has a place.
const HOME_ZOOM: f64 = 13.0;

/// The furthest north and south the map draws.
const MAX_LATITUDE: f64 = 85.051_128;

/// What the map is showing: what's in the middle, and how close.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub latitude: f64,
    pub longitude: f64,
    pub zoom: f64,
}

impl View {
    /// The whole world, roughly: what a home with no location opens on.
    pub const WORLD: View = View {
        latitude: 25.0,
        longitude: 10.0,
        zoom: 1.2,
    };

    pub fn over(latitude: f64, longitude: f64, zoom: f64) -> Self {
        Self {
            latitude: latitude.clamp(-MAX_LATITUDE, MAX_LATITUDE),
            longitude: wrap(longitude),
            zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM),
        }
    }
}

/// A longitude brought back into −180..180.
pub fn wrap(longitude: f64) -> f64 {
    (longitude + 180.0).rem_euclid(360.0) - 180.0
}

/// How the map stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    Loading,
    Ready,
    /// It couldn't be fetched: the browser is offline, or the map's hosts are out of reach.
    Offline,
}

/// The map as `map.js` hands it back, and the two things it calls. Kept together so they are
/// let go of together.
struct Mounted {
    handle: JsValue,
    _on_pin: Closure<dyn Fn(f64, f64)>,
    _on_state: Closure<dyn Fn(String)>,
}

impl Mounted {
    fn call(&self, method: &str, arguments: &[JsValue]) {
        let Ok(function) = Reflect::get(&self.handle, &JsValue::from_str(method)) else {
            return;
        };
        if let Some(function) = function.dyn_ref::<Function>() {
            let _ = function.apply(&self.handle, &arguments.iter().collect::<Array>());
        }
    }
}

fn set(object: &Object, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(object, &JsValue::from_str(key), &value.into());
}

/// The map. `pin` is where the home is, and is set by clicking the map or dragging the pin.
/// `fly` is a place to travel to: set it, and the map goes there and clears it.
#[component]
pub fn Map(
    pin: RwSignal<Option<(f64, f64)>>,
    fly: RwSignal<Option<View>>,
    /// Whether the pin may be moved: an owner's map, not a visitor's.
    #[prop(default = true)]
    editable: bool,
) -> impl IntoView {
    let frame = NodeRef::<leptos::html::Div>::new();
    let standing = RwSignal::new(Standing::Loading);
    let mounted = StoredValue::new_local(None::<Mounted>);

    // Which of the two maps: the one for the theme the page is in, followed if it changes.
    let scheme = window()
        .match_media("(prefers-color-scheme: dark)")
        .ok()
        .flatten();
    let dark = RwSignal::new(
        scheme
            .as_ref()
            .is_some_and(web_sys::MediaQueryList::matches),
    );
    if let Some(scheme) = scheme {
        let asked = scheme.clone();
        let changed = Closure::<dyn Fn()>::new(move || {
            let _ = dark.try_set(asked.matches());
        });
        scheme.set_onchange(Some(changed.as_ref().unchecked_ref()));
        // Kept for as long as the page is: the query outlives this map, and so must what it
        // calls. It does nothing once the map is gone.
        changed.forget();
    }

    // Once the frame is on the page, the map is put in it.
    Effect::new(move |_| {
        let Some(frame) = frame.get() else {
            return;
        };
        if mounted.with_value(Option::is_some) {
            return;
        }
        let mount = Reflect::get(&window(), &JsValue::from_str("iroriMap"))
            .ok()
            .and_then(|maps| Reflect::get(&maps, &JsValue::from_str("mount")).ok())
            .and_then(|mount| mount.dyn_into::<Function>().ok());
        let Some(mount) = mount else {
            standing.set(Standing::Offline);
            return;
        };
        let here = pin.get_untracked();
        let view = match here {
            Some((latitude, longitude)) => View::over(latitude, longitude, HOME_ZOOM),
            None => View::WORLD,
        };
        let options = Object::new();
        set(&options, "latitude", view.latitude);
        set(&options, "longitude", view.longitude);
        set(&options, "zoom", view.zoom);
        set(&options, "dark", dark.get_untracked());
        set(&options, "editable", editable);
        match here {
            Some((latitude, longitude)) => set(
                &options,
                "pin",
                [latitude, longitude]
                    .into_iter()
                    .map(JsValue::from_f64)
                    .collect::<Array>(),
            ),
            None => set(&options, "pin", JsValue::NULL),
        }
        let on_pin = Closure::<dyn Fn(f64, f64)>::new(move |latitude: f64, longitude: f64| {
            let _ = pin.try_set(Some((latitude, wrap(longitude))));
        });
        let on_state = Closure::<dyn Fn(String)>::new(move |state: String| {
            let _ = standing.try_set(if state == "ready" {
                Standing::Ready
            } else {
                Standing::Offline
            });
        });
        let element: &web_sys::Element = &frame;
        let arguments: Array = [
            JsValue::from(element.clone()),
            options.into(),
            on_pin.as_ref().clone(),
            on_state.as_ref().clone(),
        ]
        .into_iter()
        .collect();
        if let Ok(handle) = mount.apply(&JsValue::NULL, &arguments) {
            mounted.set_value(Some(Mounted {
                handle,
                _on_pin: on_pin,
                _on_state: on_state,
            }));
        }
    });
    on_cleanup(move || {
        mounted.update_value(|mounted| {
            if let Some(map) = mounted.take() {
                map.call("destroy", &[]);
            }
        });
    });

    let ask = move |method: &'static str, arguments: Vec<JsValue>| {
        mounted.with_value(|mounted| {
            if let Some(map) = mounted {
                map.call(method, &arguments);
            }
        });
    };
    // Out of sight there are no frames to travel in, so it is simply there.
    let still = || crate::count::still() || document().hidden();

    // The pin moved from outside the map: a search result, typed coordinates, or taken off.
    Effect::new(move |_| {
        let arguments = match pin.get() {
            Some((latitude, longitude)) => vec![latitude.into(), longitude.into()],
            None => vec![JsValue::NULL, JsValue::NULL],
        };
        ask("setPin", arguments);
    });
    // Sent somewhere: travel there.
    Effect::new(move |_| {
        let Some(to) = fly.get() else {
            return;
        };
        fly.set(None);
        ask(
            "flyTo",
            vec![
                to.latitude.into(),
                to.longitude.into(),
                to.zoom.into(),
                still().into(),
            ],
        );
    });
    Effect::new(move |before: Option<bool>| {
        let now = dark.get();
        if before.is_some_and(|before| before != now) {
            ask("setDark", vec![now.into()]);
        }
        now
    });

    let zoom = move |steps: f64| ask("zoomBy", vec![steps.into(), still().into()]);

    view! {
        <div class="map" class:editable=editable class:ready=move || standing.get() == Standing::Ready>
            <div class="map-canvas" node_ref=frame></div>
            // Nothing came: say so where the map would be.
            {move || (standing.get() == Standing::Offline).then(|| view! {
                <p class="map-quiet">
                    "The map couldn't be loaded, so this browser may be offline. \
                     The coordinates below work without it."
                </p>
            })}
            <div class="map-zoom" on:pointerdown=|event: ev::PointerEvent| event.stop_propagation()>
                <button type="button" aria-label="Zoom in" on:click=move |_| zoom(1.0)>"+"</button>
                <button type="button" aria-label="Zoom out" on:click=move |_| zoom(-1.0)>"−"</button>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_stays_on_the_map() {
        let far = View::over(89.0, 190.0, 40.0);
        assert_eq!(far.latitude, MAX_LATITUDE);
        assert!((far.longitude - -170.0).abs() < 1e-9);
        assert_eq!(far.zoom, MAX_ZOOM);
        assert_eq!(View::over(0.0, 0.0, 0.0).zoom, MIN_ZOOM);
    }

    #[test]
    fn a_longitude_goes_round_the_world() {
        assert!((wrap(181.0) - -179.0).abs() < 1e-9);
        assert!((wrap(-181.0) - 179.0).abs() < 1e-9);
        assert!((wrap(4.35) - 4.35).abs() < 1e-9);
    }
}
