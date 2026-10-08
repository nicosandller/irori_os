//! A map to put a pin on: where the home is.
//!
//! Drawn by Irori rather than by a map library, for the same reasons the page draws its own
//! sliders: nothing is fetched from a script host, it wears the page's colours in both themes,
//! and it moves the way the rest of the page does. The pin is Irori's own mark, and it is put
//! down like any marker on the Floorplan, with a bounce.
//!
//! The tiles are OpenStreetMap's, fetched by the browser. That needs the internet, which
//! nothing else here does, so the map is an extra and never the only way: when the tiles don't
//! come, the map says so and the coordinates under it carry on by themselves.
//!
//! The arithmetic (Web Mercator, which tiles cover a view) is plain functions at the top, so it
//! is tested on the host with no browser.

use leptos::ev;
use leptos::prelude::*;

/// The side of a map tile, in pixels.
const TILE: f64 = 256.0;

/// How far out and in the map goes: the whole world, down to a street.
pub const MIN_ZOOM: f64 = 1.0;
pub const MAX_ZOOM: f64 = 18.0;

/// How close the map comes when it is sent to a place somebody searched for.
pub const FOUND_ZOOM: f64 = 15.0;

/// The furthest north and south Web Mercator draws.
const MAX_LATITUDE: f64 = 85.051_128;

/// How long the map takes to travel to a place it's sent to, in milliseconds. Longer than a
/// page change: the eye has a continent to cross, and has to see which way it went.
const FLIGHT_MS: f64 = 620.0;

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
        zoom: 1.6,
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

/// How many pixels wide the world is at a zoom.
fn world(zoom: f64) -> f64 {
    TILE * 2f64.powf(zoom)
}

/// A place as a point on the world drawn `size` pixels wide: x rightwards, y downwards.
pub fn project(latitude: f64, longitude: f64, size: f64) -> (f64, f64) {
    let latitude = latitude.clamp(-MAX_LATITUDE, MAX_LATITUDE).to_radians();
    let x = (longitude + 180.0) / 360.0 * size;
    let y =
        (1.0 - (latitude.tan() + 1.0 / latitude.cos()).ln() / std::f64::consts::PI) / 2.0 * size;
    (x, y)
}

/// The place a point on the world drawn `size` pixels wide stands for.
pub fn unproject(x: f64, y: f64, size: f64) -> (f64, f64) {
    let longitude = x / size * 360.0 - 180.0;
    let n = std::f64::consts::PI * (1.0 - 2.0 * y / size);
    (n.sinh().atan().to_degrees(), longitude)
}

/// Where a place is drawn in a frame `width` by `height`, in pixels from its top left.
pub fn on_screen(
    view: View,
    (width, height): (f64, f64),
    latitude: f64,
    longitude: f64,
) -> (f64, f64) {
    let size = world(view.zoom);
    let (cx, cy) = project(view.latitude, view.longitude, size);
    let (x, y) = project(latitude, longitude, size);
    // The short way round the world, so a pin near the date line isn't a world away.
    let dx = (x - cx + size / 2.0).rem_euclid(size) - size / 2.0;
    (width / 2.0 + dx, height / 2.0 + (y - cy))
}

/// The place under a point of the frame.
pub fn at_screen(view: View, (width, height): (f64, f64), x: f64, y: f64) -> (f64, f64) {
    let size = world(view.zoom);
    let (cx, cy) = project(view.latitude, view.longitude, size);
    let (latitude, longitude) = unproject(cx + x - width / 2.0, cy + y - height / 2.0, size);
    (latitude, wrap(longitude))
}

/// The view after dragging the map by `(dx, dy)` pixels.
pub fn panned(view: View, dx: f64, dy: f64) -> View {
    let size = world(view.zoom);
    let (cx, cy) = project(view.latitude, view.longitude, size);
    let (latitude, longitude) = unproject(cx - dx, (cy - dy).clamp(0.0, size), size);
    View::over(latitude, longitude, view.zoom)
}

/// The view after zooming to `zoom` with the place under `(x, y)` staying under it: what a
/// wheel over a street corner should do.
pub fn zoomed(view: View, frame: (f64, f64), x: f64, y: f64, zoom: f64) -> View {
    let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let (latitude, longitude) = at_screen(view, frame, x, y);
    let closer = View { zoom, ..view };
    let (now_x, now_y) = on_screen(closer, frame, latitude, longitude);
    panned(closer, x - now_x, y - now_y)
}

/// One tile of a view: which it is, and where it's drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub z: u32,
    pub x: u32,
    pub y: u32,
    /// Which column of the drawing it is, counted without going round the world: zoomed far
    /// out, the same tile is drawn more than once, and each drawing is its own.
    pub column: i64,
    /// From the frame's top left, in pixels.
    pub left: f64,
    pub top: f64,
    /// Its drawn side: 256 at a whole zoom, up to twice that between two.
    pub side: f64,
}

impl Tile {
    pub fn url(&self) -> String {
        format!(
            "https://tile.openstreetmap.org/{}/{}/{}.png",
            self.z, self.x, self.y
        )
    }
}

/// The tiles that cover a frame, with one more ring around it so a drag doesn't show an edge.
pub fn tiles(view: View, (width, height): (f64, f64)) -> Vec<Tile> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let z = view.zoom.floor().clamp(0.0, MAX_ZOOM) as u32;
    let side = TILE * 2f64.powf(view.zoom - f64::from(z));
    let count = 2i64.pow(z);
    #[allow(clippy::cast_precision_loss)]
    let size = side * count as f64;
    let (cx, cy) = project(view.latitude, view.longitude, size);
    let (left, top) = (cx - width / 2.0, cy - height / 2.0);
    #[allow(clippy::cast_possible_truncation)]
    let span = |from: f64, length: f64| {
        ((from / side).floor() as i64 - 1)..=(((from + length) / side).floor() as i64 + 1)
    };
    let mut found = Vec::new();
    for ty in span(top, height) {
        if ty < 0 || ty >= count {
            continue;
        }
        for tx in span(left, width) {
            #[allow(clippy::cast_precision_loss)]
            let (px, py) = (tx as f64 * side - left, ty as f64 * side - top);
            found.push(Tile {
                z,
                // The world goes round: past its east edge is its west.
                x: u32::try_from(tx.rem_euclid(count)).unwrap_or(0),
                y: u32::try_from(ty).unwrap_or(0),
                column: tx,
                left: px,
                top: py,
                side,
            });
        }
    }
    found
}

/// Somewhere between two views, `t` of the way (0 to 1), eased: quick away, long to settle.
/// The middle moves in a straight line on the drawn world, so it never swings off the map.
pub fn between(from: View, to: View, t: f64) -> View {
    let t = t.clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    // A long trip pulls back on the way, so the land in between is seen going by.
    let (ax, ay) = project(from.latitude, from.longitude, 1.0);
    let (bx, by) = project(to.latitude, to.longitude, 1.0);
    let bx = ax + ((bx - ax + 0.5).rem_euclid(1.0) - 0.5);
    let far = ((bx - ax).hypot(by - ay) * world(from.zoom.min(to.zoom)) / 600.0).min(3.0);
    let lift = far * (std::f64::consts::PI * t).sin();
    let (latitude, longitude) = unproject(ax + (bx - ax) * eased, ay + (by - ay) * eased, 1.0);
    View::over(
        latitude,
        longitude,
        from.zoom + (to.zoom - from.zoom) * eased - lift,
    )
}

/// What a drag is moving.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Held {
    /// The map, from this point of the frame; `moved` once it has gone further than a click.
    Map {
        x: f64,
        y: f64,
        moved: bool,
    },
    Pin,
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
    let size = RwSignal::new((640.0, 360.0));
    let view = RwSignal::new(match pin.get_untracked() {
        Some((latitude, longitude)) => View::over(latitude, longitude, 13.0),
        None => View::WORLD,
    });
    let held = RwSignal::new(None::<Held>);
    // Flips each time the pin is put down, which is what plays the landing once.
    let landed = RwSignal::new(false);
    let (loaded, failed) = (RwSignal::new(false), RwSignal::new(false));

    let measure = move || {
        if let Some(frame) = frame.get_untracked() {
            let rect = frame.get_bounding_client_rect();
            if rect.width() > 0.0 && rect.height() > 0.0 {
                size.set((rect.width(), rect.height()));
            }
        }
    };
    // Once it is on the page, and whenever the window changes shape. A Settings row opens
    // with a drawer that rolls down, so the frame is measured again as a pointer arrives too.
    Effect::new(move |_| {
        frame.track();
        request_animation_frame(measure);
    });
    let resized = window_event_listener(ev::resize, move |_| measure());
    on_cleanup(move || resized.remove());

    // Sent somewhere: travel there, unless the page has been asked to keep still.
    Effect::new(move |_| {
        let Some(to) = fly.get() else {
            return;
        };
        fly.set(None);
        let from = view.get_untracked();
        // Out of sight there are no frames to travel in, and it would hang half way.
        if crate::count::still() || document().hidden() || from == to {
            view.set(to);
            return;
        }
        let started = web_sys::js_sys::Date::now();
        fn frame_of(view: RwSignal<View>, from: View, to: View, started: f64) {
            let t = (web_sys::js_sys::Date::now() - started) / FLIGHT_MS;
            // The page may have gone while it was travelling.
            if view.try_set(between(from, to, t)).is_some() {
                return;
            }
            if t < 1.0 {
                request_animation_frame(move || frame_of(view, from, to, started));
            }
        }
        request_animation_frame(move || frame_of(view, from, to, started));
    });

    let point = move |event: &ev::PointerEvent| -> (f64, f64) {
        let rect = frame
            .get_untracked()
            .map(|frame| frame.get_bounding_client_rect());
        match rect {
            Some(rect) => (
                f64::from(event.client_x()) - rect.left(),
                f64::from(event.client_y()) - rect.top(),
            ),
            None => (0.0, 0.0),
        }
    };
    let capture = move |event: &ev::PointerEvent| {
        if let Some(frame) = frame.get_untracked() {
            let _ = frame.set_pointer_capture(event.pointer_id());
        }
    };

    let down = move |event: ev::PointerEvent| {
        measure();
        let (x, y) = point(&event);
        capture(&event);
        held.set(Some(Held::Map { x, y, moved: false }));
    };
    let pin_down = move |event: ev::PointerEvent| {
        if !editable {
            return;
        }
        event.stop_propagation();
        measure();
        capture(&event);
        held.set(Some(Held::Pin));
    };
    let moved = move |event: ev::PointerEvent| {
        let (x, y) = point(&event);
        match held.get_untracked() {
            Some(Held::Map {
                x: from_x,
                y: from_y,
                moved,
            }) => {
                let (dx, dy) = (x - from_x, y - from_y);
                // A hand isn't steady: a few pixels is still a click.
                if !moved && dx.hypot(dy) < 4.0 {
                    return;
                }
                view.update(|view| *view = panned(*view, dx, dy));
                held.set(Some(Held::Map { x, y, moved: true }));
            }
            Some(Held::Pin) => {
                pin.set(Some(at_screen(
                    view.get_untracked(),
                    size.get_untracked(),
                    x,
                    y,
                )));
            }
            None => {}
        }
    };
    let up = move |event: ev::PointerEvent| {
        let (x, y) = point(&event);
        match held.get_untracked() {
            // A click on the map, not a drag of it: the pin goes there.
            Some(Held::Map { moved: false, .. }) if editable => {
                pin.set(Some(at_screen(
                    view.get_untracked(),
                    size.get_untracked(),
                    x,
                    y,
                )));
                landed.update(|landed| *landed = !*landed);
            }
            Some(Held::Pin) => landed.update(|landed| *landed = !*landed),
            _ => {}
        }
        held.set(None);
    };
    let wheel = move |event: ev::WheelEvent| {
        event.prevent_default();
        measure();
        let rect = frame
            .get_untracked()
            .map(|frame| frame.get_bounding_client_rect());
        let Some(rect) = rect else {
            return;
        };
        let (x, y) = (
            f64::from(event.client_x()) - rect.left(),
            f64::from(event.client_y()) - rect.top(),
        );
        // A notch of a wheel is about a hundred; a trackpad sends many small ones.
        let step = (-event.delta_y() / 240.0).clamp(-0.6, 0.6);
        view.update(|view| *view = zoomed(*view, size.get_untracked(), x, y, view.zoom + step));
    };
    let nudge = move |by: f64| {
        let here = view.get_untracked();
        // To the next whole zoom, where the tiles are drawn at their own size and are sharp.
        fly.set(Some(View::over(
            here.latitude,
            here.longitude,
            (here.zoom + by).round(),
        )));
    };

    let shown = Memo::new(move |_| {
        let (view, size) = (view.get(), size.get());
        tiles(view, size)
            .into_iter()
            .map(|tile| (tile.z, tile.column, tile.y, tile.url()))
            .collect::<Vec<_>>()
    });
    // Where a tile is drawn follows the view; the tile itself is kept while it's in sight, so
    // dragging moves pictures the browser already has.
    let place_of = move |z: u32, column: i64, y: u32| {
        move || {
            let (view, size) = (view.get(), size.get());
            tiles(view, size)
                .into_iter()
                .find(|tile| (tile.z, tile.column, tile.y) == (z, column, y))
                .map(|tile| {
                    format!(
                        "left:{:.2}px;top:{:.2}px;width:{:.2}px;height:{:.2}px",
                        tile.left, tile.top, tile.side, tile.side
                    )
                })
                .unwrap_or_else(|| "display:none".to_owned())
        }
    };
    let pin_at = move || {
        pin.get().map(|(latitude, longitude)| {
            let (x, y) = on_screen(view.get(), size.get(), latitude, longitude);
            format!("left:{x:.2}px;top:{y:.2}px")
        })
    };

    view! {
        <div
            class="map"
            class:holding=move || matches!(held.get(), Some(Held::Map { moved: true, .. }))
            class:editable=editable
            node_ref=frame
            on:pointerdown=down
            on:pointermove=moved
            on:pointerup=up
            on:pointercancel=move |_| held.set(None)
            on:wheel=wheel
        >
            <div class="map-tiles" aria-hidden="true">
                <For each=move || shown.get() key=|tile| (tile.0, tile.1, tile.2) let:tile>
                    <img
                        class="map-tile"
                        src=tile.3.clone()
                        alt=""
                        draggable="false"
                        style=place_of(tile.0, tile.1, tile.2)
                        on:load=move |_| loaded.set(true)
                        on:error=move |_| failed.set(true)
                    />
                </For>
            </div>
            // Nothing came: say so where the map would be. One tile arriving takes it back.
            {move || (failed.get() && !loaded.get()).then(|| view! {
                <p class="map-quiet">
                    "The map couldn't be loaded, so this browser may be offline. \
                     The coordinates below work without it."
                </p>
            })}
            {move || pin_at().map(|style| view! {
                <div
                    class="map-pin"
                    class:held=move || held.get() == Some(Held::Pin)
                    // Two classes that mean the same, taken in turn, so each time it is put
                    // down is a new animation and not a second look at the old one.
                    class:landed-a=move || landed.get()
                    class:landed-b=move || !landed.get()
                    style=style
                    on:pointerdown=pin_down
                >
                    <span class="map-pin-ring" aria-hidden="true"></span>
                    // Irori's mark: the frame around the hearth, and the ember in it.
                    <svg viewBox="0 0 48 60" aria-hidden="true">
                        <path class="map-pin-stem" d="M24 58 14 44h20Z" />
                        <rect class="map-pin-frame" x="4" y="4" width="40" height="40" rx="4" />
                        <rect class="map-pin-ember" x="17" y="17" width="14" height="14" rx="1.5" />
                    </svg>
                </div>
            })}
            <div class="map-zoom" on:pointerdown=|event: ev::PointerEvent| event.stop_propagation()>
                <button type="button" aria-label="Zoom in" on:click=move |_| nudge(1.0)>"+"</button>
                <button type="button" aria-label="Zoom out" on:click=move |_| nudge(-1.0)>"−"</button>
            </div>
            <a
                class="map-credit"
                href="https://www.openstreetmap.org/copyright"
                target="_blank"
                rel="noreferrer"
                on:pointerdown=|event: ev::PointerEvent| event.stop_propagation()
            >
                "© OpenStreetMap"
            </a>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn a_place_goes_to_the_map_and_comes_back_the_same() {
        for (latitude, longitude) in [(50.8467, 4.3525), (-36.8485, 174.7633), (0.0, 0.0)] {
            let (x, y) = project(latitude, longitude, 4096.0);
            let (back_latitude, back_longitude) = unproject(x, y, 4096.0);
            assert!(near(latitude, back_latitude), "{latitude} {back_latitude}");
            assert!(
                near(longitude, back_longitude),
                "{longitude} {back_longitude}"
            );
        }
        // Greenwich on the equator is the middle of the world.
        assert_eq!(project(0.0, 0.0, 256.0), (128.0, 128.0));
    }

    #[test]
    fn the_middle_of_the_view_is_the_middle_of_the_frame() {
        let view = View::over(50.8467, 4.3525, 12.0);
        let frame = (640.0, 360.0);
        let (x, y) = on_screen(view, frame, 50.8467, 4.3525);
        assert!(near(x, 320.0) && near(y, 180.0), "{x} {y}");
        let (latitude, longitude) = at_screen(view, frame, 320.0, 180.0);
        assert!(near(latitude, 50.8467) && near(longitude, 4.3525));
    }

    #[test]
    fn dragging_right_shows_what_was_to_the_west() {
        let view = View::over(50.0, 4.0, 10.0);
        let after = panned(view, 100.0, 0.0);
        assert!(after.longitude < view.longitude);
        assert!(near(after.latitude, view.latitude));
        // And what was under the pointer is still under it.
        let frame = (640.0, 360.0);
        let (latitude, longitude) = at_screen(view, frame, 200.0, 100.0);
        let (x, y) = on_screen(after, frame, latitude, longitude);
        assert!(near(x, 300.0) && near(y, 100.0), "{x} {y}");
    }

    #[test]
    fn zooming_keeps_what_is_under_the_pointer_under_it() {
        let view = View::over(50.0, 4.0, 10.0);
        let frame = (640.0, 360.0);
        let under = at_screen(view, frame, 500.0, 90.0);
        let closer = zoomed(view, frame, 500.0, 90.0, 12.5);
        assert_eq!(closer.zoom, 12.5);
        let (x, y) = on_screen(closer, frame, under.0, under.1);
        assert!(
            (x - 500.0).abs() < 1e-3 && (y - 90.0).abs() < 1e-3,
            "{x} {y}"
        );
        // It stops at the ends.
        assert_eq!(zoomed(view, frame, 0.0, 0.0, 99.0).zoom, MAX_ZOOM);
        assert_eq!(zoomed(view, frame, 0.0, 0.0, -5.0).zoom, MIN_ZOOM);
    }

    #[test]
    fn the_tiles_cover_the_frame_and_go_round_the_world() {
        let frame = (640.0, 360.0);
        let found = tiles(View::over(50.8467, 4.3525, 12.0), frame);
        assert!(found.iter().all(|tile| tile.z == 12 && tile.side == 256.0));
        // Every corner of the frame is under some tile.
        for (x, y) in [(0.0, 0.0), (639.0, 0.0), (0.0, 359.0), (639.0, 359.0)] {
            assert!(
                found.iter().any(|tile| tile.left <= x
                    && x < tile.left + tile.side
                    && tile.top <= y
                    && y < tile.top + tile.side),
                "nothing under {x},{y}"
            );
        }
        // Between two zooms the tiles of the lower one are drawn bigger.
        let between = tiles(View::over(50.8467, 4.3525, 12.5), frame);
        assert!(between.iter().all(|tile| tile.z == 12 && tile.side > 256.0));
        // Across the date line, tiles come from both ends of the row.
        let pacific = tiles(View::over(0.0, 180.0, 3.0), frame);
        assert!(pacific.iter().any(|tile| tile.x == 0));
        assert!(pacific.iter().any(|tile| tile.x == 7));
        // Far enough out, the same tile is drawn twice, and each drawing has a column of its own.
        let world = tiles(View::WORLD, (1200.0, 360.0));
        let mut columns: Vec<(i64, u32)> = world.iter().map(|tile| (tile.column, tile.y)).collect();
        let drawn = columns.len();
        columns.sort_unstable();
        columns.dedup();
        assert_eq!(columns.len(), drawn);
        assert!(
            world
                .iter()
                .filter(|tile| (tile.x, tile.y) == (0, 0))
                .count()
                > 1
        );
        // Never above the top of the world.
        assert!(
            tiles(View::over(84.0, 0.0, 2.0), frame)
                .iter()
                .all(|tile| tile.y < 4)
        );
    }

    #[test]
    fn a_trip_starts_where_it_is_and_ends_where_it_was_sent() {
        let (from, to) = (
            View::over(50.85, 4.35, 12.0),
            View::over(-34.6, -58.4, 14.0),
        );
        let start = between(from, to, 0.0);
        let end = between(from, to, 1.0);
        assert!(near(start.latitude, from.latitude) && near(start.zoom, from.zoom));
        assert!(near(end.latitude, to.latitude) && near(end.longitude, to.longitude));
        assert!(near(end.zoom, to.zoom));
        // Half way it has pulled back to see the ocean go by.
        assert!(between(from, to, 0.5).zoom < from.zoom);
        // A short hop doesn't.
        let hop = between(from, View::over(50.86, 4.36, 12.0), 0.5);
        assert!((hop.zoom - 12.0).abs() < 0.2, "{}", hop.zoom);
    }

    #[test]
    fn a_pin_past_the_date_line_is_drawn_the_short_way_round() {
        let view = View::over(0.0, 179.0, 6.0);
        let (x, _) = on_screen(view, (640.0, 360.0), 0.0, -179.0);
        assert!(x > 320.0 && x < 640.0, "{x}");
    }
}
