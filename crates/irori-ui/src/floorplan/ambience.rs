//! The plan, alive: light and movement drawn into the rooms.
//!
//! - **A light that's on** throws a warm pool of light around it, as bright as the light is and
//!   stopped by the room's walls — clipped to the room it stands in. Every light has its pool,
//!   lit or not, so switching one fades it up or down rather than blinking it.
//! - **A motion sensor that senses someone** sends ripples out across its room while it does.
//! - **A radar that senses someone** shows the field it sees — cut off by the walls, let through
//!   by the doorways — with an arc across it at the distance its target is.
//!
//! Drawn in the plan's own centimetres, under the walls, so the walls stand over the light. It
//! redraws with every reading and every pan, in place: a pool's brightness transitions, and a
//! ripple keeps rippling rather than starting again.

use irori_types::{Capabilities, DeviceId, Level, OpeningKind, PlacedArea, Point, State, Wall};
use leptos::prelude::*;

use crate::api::Home;

/// How far a light's pool reaches, in centimetres: most of an ordinary room.
const POOL: i32 = 320;
/// How far a ripple spreads before it's gone.
const RIPPLE: i32 = 280;
/// How far a radar sees, in centimetres: about what a wall-mounted mmWave sensor is good for.
pub const MMWAVE_RANGE: f64 = 500.0;
/// How many lines of sight a radar's field is traced with. Enough that a doorway a few metres
/// off still gets a few of them through it.
const RAYS: usize = 140;
/// How near a wall's face a sensor has to be, in centimetres, to count as mounted on it rather
/// than standing in front of it.
const MOUNTED: f64 = 5.0;

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

/// What a sensor at `at` can see: the far end of each line of sight, fanned evenly across its
/// field of view, each stopped by the face of the first wall in its way or by `range`.
///
/// `facing` and `view` are in degrees, clockwise from the plan's +x axis as the plan is drawn
/// (y runs down the page). A **door is a hole** — the sensor sees through it into the next room
/// — and a window is wall: glass stops a radar well enough, and nobody is tracked through one.
///
/// A sensor is usually screwed to a wall, which on the plan means standing *on* the line of
/// one. That wall mustn't blind it, so for a wall it is mounted on the sensor is taken to sit on
/// the face it looks out from: it sees into the room, and not back through its own wall.
pub fn sight(at: Point, facing: f64, view: f64, walls: &[Wall], range: f64) -> Vec<(f64, f64)> {
    struct Piece {
        from: (f64, f64),
        to: (f64, f64),
        normal: (f64, f64),
        half: f64,
        /// Where the sensor is, as far as this wall is concerned.
        eye: (f64, f64),
    }
    let eye = (f64::from(at.x), f64::from(at.y));
    let ahead = facing.to_radians();
    let ahead = (ahead.cos(), ahead.sin());

    let mut pieces = Vec::new();
    for wall in walls {
        let half = f64::from(wall.thickness) / 2.0;
        let (_, _, normal) = super::along(wall, 0.0);
        let from = (f64::from(wall.from.x), f64::from(wall.from.y));
        let (_, off) = super::on_wall_at(wall.from, wall.to, eye);
        let seen_from = if off <= half + MOUNTED {
            // Which face it looks out from: the one it's aimed away from the wall on, or — aimed
            // along the wall — the one it's standing nearer.
            let side = (eye.0 - from.0) * normal.0 + (eye.1 - from.1) * normal.1;
            let aimed = ahead.0 * normal.0 + ahead.1 * normal.1;
            let face = if aimed.abs() > 1e-6 { aimed } else { side }.signum();
            let out = face * (half + 0.1) - side;
            (eye.0 + normal.0 * out, eye.1 + normal.1 * out)
        } else {
            eye
        };
        for (start, end) in super::runs(wall, |opening| opening.kind == OpeningKind::Door) {
            let (a, _, _) = super::along(wall, start);
            let (b, _, _) = super::along(wall, end);
            pieces.push(Piece {
                from: a,
                to: b,
                normal,
                half,
                eye: seen_from,
            });
        }
    }

    (0..RAYS)
        .map(|ray| {
            let share = ray as f64 / (RAYS - 1) as f64;
            let angle = (facing - view / 2.0 + view * share).to_radians();
            let (dx, dy) = (angle.cos(), angle.sin());
            let mut reach = range;
            for piece in &pieces {
                let (ex, ey) = (piece.to.0 - piece.from.0, piece.to.1 - piece.from.1);
                let cross = dx * ey - dy * ex;
                if cross.abs() < 1e-9 {
                    continue;
                }
                let (wx, wy) = (piece.from.0 - piece.eye.0, piece.from.1 - piece.eye.1);
                let along_ray = (wx * ey - wy * ex) / cross;
                let along_wall = (wx * dy - wy * dx) / cross;
                if along_ray < 0.0 || !(0.0..=1.0).contains(&along_wall) {
                    continue;
                }
                // The ray meets the wall's middle at `along_ray`; its face is nearer by the same
                // share the half-thickness is of how far off the wall the sensor stands.
                let off = (wx * piece.normal.0 + wy * piece.normal.1).abs();
                let face = if off > piece.half {
                    along_ray * (1.0 - piece.half / off)
                } else {
                    0.0
                };
                reach = reach.min(face);
            }
            (eye.0 + dx * reach, eye.1 + dy * reach)
        })
        .collect()
}

/// The field each placed device sees, as the path of its outline — `None` for a device nobody
/// has aimed. Only the plan decides it, so it is worked out when the plan changes and not with
/// every reading.
pub fn sightlines(level: &Level) -> Vec<Option<String>> {
    level
        .devices
        .iter()
        .map(|placed| {
            let facing = placed.facing?;
            let seen = sight(
                placed.at,
                f64::from(facing),
                f64::from(placed.view_angle()),
                &level.walls,
                MMWAVE_RANGE,
            );
            let mut path = format!("M {} {}", placed.at.x, placed.at.y);
            for (x, y) in seen {
                path.push_str(&format!(" L {x:.1} {y:.1}"));
            }
            path.push_str(" Z");
            Some(path)
        })
        .collect()
}

/// What the radars on this floor see. Each one's field is always drawn and fades in while it
/// senses someone, with an arc across it at the distance its target is; the arc's radius is a
/// style rather than an attribute so a new reading glides to its place.
///
/// `aiming` is the editor's view of the same thing: every field shown at full range with no
/// target in it, so whoever is turning the sensor can see what they're pointing it at.
pub fn fields(
    level: &Level,
    home: &Home,
    sightlines: &[Option<String>],
    aiming: bool,
) -> impl IntoView + use<> {
    level
        .devices
        .iter()
        .enumerate()
        .filter_map(|(index, placed)| {
            let outline = sightlines.get(index)?.clone()?;
            let look = super::looks(home, &placed.device);
            if !look.radar {
                return None;
            }
            let target = look.distance.filter(|_| !aiming);
            let radius = target.unwrap_or(0.0).min(MMWAVE_RANGE);
            let (x, y) = (placed.at.x, placed.at.y);
            let (fill, clip) = (format!("field-{index}"), format!("field-clip-{index}"));
            Some(view! {
                <g class="field" class:sensing=aiming || look.sensing>
                    <defs>
                        <radialGradient
                            id=fill.clone()
                            gradientUnits="userSpaceOnUse"
                            cx=x cy=y r=MMWAVE_RANGE
                        >
                            <stop class="field-stop" offset="0" stop-opacity=".24" />
                            <stop class="field-stop" offset="1" stop-opacity=".02" />
                        </radialGradient>
                        <clipPath id=clip.clone()>
                            <path d=outline.clone() />
                        </clipPath>
                    </defs>
                    <path
                        class="field-shape"
                        d=outline
                        fill=format!("url(#{fill})")
                        vector-effect="non-scaling-stroke"
                    />
                    <g
                        class="field-target"
                        class:hidden=target.is_none()
                        clip-path=format!("url(#{clip})")
                    >
                        <circle class="halo" cx=x cy=y style=format!("r:{radius:.0}px")
                            vector-effect="non-scaling-stroke" />
                        <circle class="arc" cx=x cy=y style=format!("r:{radius:.0}px")
                            vector-effect="non-scaling-stroke" />
                    </g>
                </g>
            })
        })
        .collect_view()
}

/// The light and movement on one floor.
pub fn ambience(
    level: &Level,
    home: &Home,
    sightlines: &[Option<String>],
    transform: String,
) -> impl IntoView + use<> {
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
    // A radar shows its field instead: the ripples are for sensors that can't say where.
    let ripples = level
        .devices
        .iter()
        .filter(|placed| {
            let look = super::looks(home, &placed.device);
            look.pulse || (look.radar && look.sensing && placed.facing.is_none())
        })
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
                // The colours come from the stylesheet (`.pool-near`, `.pool-far`): a tone can
                // be a `color-mix`, which an SVG attribute can't hold.
                <radialGradient id="pool">
                    <stop class="pool-near" offset="0" stop-opacity=".5" />
                    <stop class="pool-far" offset=".45" stop-opacity=".18" />
                    <stop class="pool-far" offset="1" stop-opacity="0" />
                </radialGradient>
                {clips}
            </defs>
            {pools}
            {fields(level, home, sightlines, false)}
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

    /// A 4 × 3 m room, its walls 10 cm thick.
    fn room() -> Vec<Wall> {
        vec![
            Wall::new(p(0, 0), p(400, 0)),
            Wall::new(p(400, 0), p(400, 300)),
            Wall::new(p(400, 300), p(0, 300)),
            Wall::new(p(0, 300), p(0, 0)),
        ]
    }

    /// A radar sees as far as the face of the wall in front of it and no further, however far
    /// it could see in the open.
    #[test]
    fn a_wall_stops_what_a_sensor_sees() {
        let seen = sight(p(200, 150), 0.0, 60.0, &room(), MMWAVE_RANGE);
        assert_eq!(seen.len(), RAYS);
        let face = 400.0 - f64::from(Wall::DEFAULT_THICKNESS) / 2.0;
        assert!(
            seen.iter().all(|(x, _)| *x <= face + 1e-6),
            "nothing past the wall 2 m away, less half its thickness"
        );
        let furthest = seen.iter().map(|(x, _)| *x).fold(f64::MIN, f64::max);
        assert!(
            (furthest - face).abs() < 1e-6,
            "and it gets that far: {furthest}"
        );

        let open = sight(p(200, 150), 0.0, 60.0, &[], MMWAVE_RANGE);
        assert!(
            open.iter()
                .all(|(x, y)| ((x - 200.0).hypot(y - 150.0) - MMWAVE_RANGE).abs() < 1e-6),
            "with no walls it sees to the end of its range"
        );
    }

    /// A door is a hole a radar sees through; a window is wall.
    #[test]
    fn a_door_lets_a_sensor_see_into_the_next_room() {
        let cut = |kind| {
            let mut walls = room();
            walls[1].openings.push(irori_types::Opening {
                kind,
                at: 150,
                width: 80,
            });
            sight(p(200, 150), 0.0, 60.0, &walls, MMWAVE_RANGE)
        };
        let through = cut(OpeningKind::Door);
        let straight = through[RAYS / 2];
        assert!(
            straight.0 > 600.0,
            "straight through the doorway: {straight:?}"
        );
        // The first and last lines of sight are aimed well wide of the doorway.
        for wide in [through[0], through[RAYS - 1]] {
            assert!(
                wide.0 <= 395.0 + 1e-6,
                "stopped either side of it: {wide:?}"
            );
        }
        assert!(
            cut(OpeningKind::Window)
                .iter()
                .all(|(x, _)| *x <= 395.0 + 1e-6),
            "glass is wall"
        );
    }

    /// A sensor screwed to a wall stands on that wall's line, or a few centimetres either side
    /// of it. The wall it's on doesn't blind it: it looks out from the face it's aimed from.
    #[test]
    fn a_sensor_on_a_wall_looks_out_from_the_face_it_is_aimed_from() {
        for x in [0, 3, -3] {
            let into = sight(p(x, 150), 0.0, 60.0, &room(), MMWAVE_RANGE);
            let straight = into[RAYS / 2];
            assert!(
                (straight.0 - 395.0).abs() < 1e-6,
                "from {x} cm off the wall's line, across the room: {straight:?}"
            );
        }
        // Turned round, the same sensor is on the outside of the house looking at the garden.
        let out = sight(p(0, 150), 180.0, 60.0, &room(), MMWAVE_RANGE);
        let straight = out[RAYS / 2];
        assert!((straight.0 + MMWAVE_RANGE).abs() < 0.1, "{straight:?}");

        // Standing clear of the wall behind it, it is simply in front of that wall.
        let clear = sight(p(60, 150), 180.0, 60.0, &room(), MMWAVE_RANGE);
        assert!(
            (clear[RAYS / 2].0 - 5.0).abs() < 1e-6,
            "{:?}",
            clear[RAYS / 2]
        );
    }

    #[test]
    fn only_a_device_that_has_been_aimed_has_a_field() {
        let lamp = irori_types::PlacedDevice::new(
            "demo_lamp".parse().expect("a valid device id"),
            p(100, 100),
        );
        let radar = irori_types::PlacedDevice {
            facing: Some(90),
            ..irori_types::PlacedDevice::new(
                "demo_mmwave".parse().expect("a valid device id"),
                p(200, 0),
            )
        };
        let level = Level {
            walls: room(),
            devices: vec![lamp, radar],
            ..Level::default()
        };
        let lines = sightlines(&level);
        assert_eq!(lines[0], None);
        let path = lines[1].as_deref().expect("the radar is aimed");
        assert!(
            path.starts_with("M 200 0 L ") && path.ends_with(" Z"),
            "{path}"
        );
    }
}
