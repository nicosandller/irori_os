//! The plan, alive: light and movement drawn into the rooms.
//!
//! - **A light that's on** throws a warm pool of light around it, as bright as the light is and
//!   stopped by the room's walls — clipped to the room it stands in. Every light has its pool,
//!   lit or not, so switching one fades it up or down rather than blinking it.
//! - **A motion sensor that senses someone** sends ripples out across its room while it does.
//!
//! Drawn in the plan's own centimetres, under the walls, so the walls stand over the light. It
//! redraws with every reading and every pan, in place: a pool's brightness transitions, and a
//! ripple keeps rippling rather than starting again.

use irori_types::{BinarySensorClass, Capabilities, DeviceId, Level, PlacedArea, Point, State};
use leptos::prelude::*;

use crate::api::Home;

/// How far a light's pool reaches, in centimetres: most of an ordinary room.
const POOL: i32 = 320;
/// How far a ripple spreads before it's gone.
const RIPPLE: i32 = 280;

/// How brightly a device lights its room, 0 to 1 — nothing if it isn't a light at all. Off is 0;
/// on at no stated level is full.
pub fn light_level(home: &Home, device: &DeviceId) -> Option<f64> {
    let light = home
        .entities
        .iter()
        .filter(|entity| entity.device_id.as_ref() == Some(device))
        .find(|entity| matches!(entity.capabilities, Capabilities::Light(_)))?;
    let state = home
        .states
        .iter()
        .find(|state| state.entity_id == light.id)
        .and_then(|state| state.state.as_ref());
    Some(match state {
        Some(State::Light(light)) if light.on => light
            .brightness
            .map_or(1.0, |level| f64::from(level) / 255.0),
        _ => 0.0,
    })
}

/// Whether a device is sensing someone right now: a motion, occupancy or presence sensor that
/// says so.
pub fn sensing(home: &Home, device: &DeviceId) -> bool {
    home.entities
        .iter()
        .filter(|entity| entity.device_id.as_ref() == Some(device))
        .filter(|entity| {
            matches!(
                &entity.capabilities,
                Capabilities::BinarySensor(sensor)
                    if matches!(
                        sensor.device_class,
                        Some(BinarySensorClass::Motion | BinarySensorClass::Occupancy)
                    )
            )
        })
        .any(|entity| {
            home.states.iter().any(|state| {
                state.entity_id == entity.id
                    && matches!(state.state, Some(State::BinarySensor(ref sensor)) if sensor.on)
            })
        })
}

/// Whether `point` is inside the room's outline (even–odd: a ray to the right crosses the
/// outline an odd number of times).
pub fn inside(point: Point, outline: &[Point]) -> bool {
    let (x, y) = (f64::from(point.x), f64::from(point.y));
    let mut inside = false;
    let mut j = outline.len().wrapping_sub(1);
    for (i, a) in outline.iter().enumerate() {
        let b = outline[j];
        let (ax, ay, bx, by) = (
            f64::from(a.x),
            f64::from(a.y),
            f64::from(b.x),
            f64::from(b.y),
        );
        if (ay > y) != (by > y) && x < (bx - ax) * (y - ay) / (by - ay) + ax {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Which room a point stands in, by its place in the level's list.
fn room_of(point: Point, rooms: &[PlacedArea]) -> Option<usize> {
    rooms.iter().position(|room| inside(point, &room.points))
}

fn outline(room: &PlacedArea) -> String {
    room.points
        .iter()
        .map(|point| format!("{},{}", point.x, point.y))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The light and movement on one floor.
pub fn ambience(level: &Level, home: &Home, transform: String) -> impl IntoView + use<> {
    let clip = |at: Point| room_of(at, &level.areas).map(|room| format!("url(#room-{room})"));
    let clips = level
        .areas
        .iter()
        .enumerate()
        .map(|(room, area)| {
            view! {
                <clipPath id=format!("room-{room}")>
                    <polygon points=outline(area) />
                </clipPath>
            }
        })
        .collect_view();
    let pools = level
        .devices
        .iter()
        .filter_map(|placed| {
            let lit = light_level(home, &placed.device)?;
            // Never quite nothing while on — a light at 1% still lights something.
            let strength = if lit > 0.0 { 0.25 + 0.75 * lit } else { 0.0 };
            Some(view! {
                <circle
                    class="pool"
                    cx=placed.at.x
                    cy=placed.at.y
                    r=POOL
                    fill="url(#pool)"
                    clip-path=clip(placed.at)
                    style=format!("opacity:{strength:.2}")
                />
            })
        })
        .collect_view();
    let ripples = level
        .devices
        .iter()
        .filter(|placed| sensing(home, &placed.device))
        .map(|placed| {
            let (x, y) = (placed.at.x, placed.at.y);
            view! {
                <g class="ripple" clip-path=clip(placed.at)>
                    <circle cx=x cy=y r=RIPPLE />
                    <circle class="later" cx=x cy=y r=RIPPLE />
                </g>
            }
        })
        .collect_view();
    view! {
        <g class="ambience" transform=transform>
            <defs>
                <radialGradient id="pool">
                    <stop offset="0" stop-color="#ffc46b" stop-opacity=".6" />
                    <stop offset=".45" stop-color="#ffae4a" stop-opacity=".24" />
                    <stop offset="1" stop-color="#ffae4a" stop-opacity="0" />
                </radialGradient>
                {clips}
            </defs>
            {pools}
            {ripples}
        </g>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    /// A light belongs to the room it stands in, and one outside every room to none.
    #[test]
    fn a_point_is_in_the_room_that_surrounds_it() {
        let square = [p(0, 0), p(400, 0), p(400, 300), p(0, 300)];
        assert!(inside(p(200, 150), &square));
        assert!(!inside(p(500, 150), &square));
        assert!(!inside(p(200, -10), &square));
        // An L-shaped room: the notch is outside.
        let ell = [
            p(0, 0),
            p(400, 0),
            p(400, 200),
            p(200, 200),
            p(200, 400),
            p(0, 400),
        ];
        assert!(inside(p(100, 300), &ell));
        assert!(!inside(p(300, 300), &ell));
    }
}
