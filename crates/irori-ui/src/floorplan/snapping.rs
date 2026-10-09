//! Where a point being drawn lands, and what it caught on the way there.
//!
//! A plan is drawn by eye and has to come out square, so the pointer is pulled towards the
//! things a wall usually wants to meet, in this order: a corner already there, the middle of a
//! wall, the line of a wall, a place level with another wall's end, and only then the grid.
//! What caught it is said as well as where, so the canvas can draw the mark that explains why
//! the point isn't under the pointer.
//!
//! The corners of a room are read here too, because the angle a room turns through is not the
//! angle between two lines once the room has a notch in it.

use irori_types::{Level, Point, Wall};

use super::{CORNER, Snap, Viewport, angle_at, direction, on_wall_at, point_along, round};

/// How near, in screen pixels, the middle of a wall has to be to pull a point onto it. Less
/// than [`CORNER`]: the middle is somewhere a wall sometimes starts, a corner is where it
/// nearly always ends, and on a short wall the two are close enough to argue.
const MIDDLE: f64 = 14.0;

/// How near a wall the pointer has to be, in screen pixels, for that wall's middle to be
/// marked at all. Wider than [`MIDDLE`], so the mark is there to aim at before it pulls.
const SHOWN: f64 = 48.0;

/// How near the line of a wall a point has to be, in screen pixels, to land on it.
const ON_WALL: f64 = 9.0;

/// How near being level with another wall's end a point has to be, in screen pixels, to be
/// held there. The gentlest of the pulls: it is a suggestion about somewhere there's nothing.
const GUIDE: f64 = 7.0;

/// How far from parallel two lines can be and still be called parallel: the sine of two
/// degrees.
const PARALLEL: f64 = 0.035;

/// How far off a wall's line, in centimetres, a point can be and still be standing on it. A
/// point along a slanted wall is rounded to the centimetre, so it is never exactly on.
const STANDING: f64 = 0.75;

/// What a point landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Caught {
    /// A corner that was already there.
    Corner,
    /// The middle of a wall. Which wall.
    Middle(usize),
    /// Somewhere along a wall. Which wall.
    OnWall(usize),
    /// Level with something else on the plan.
    Guide,
    Grid,
}

/// A marker line to draw: from the corner a point is level with, to the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Guide {
    pub from: Point,
    pub to: Point,
}

/// Where a point goes, and why.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Placed {
    pub point: Point,
    pub caught: Caught,
    /// The lines that say what the point is level with. Empty unless a line is being drawn.
    pub guides: Vec<Guide>,
    /// The middle of the wall the pointer is beside, to mark — whether or not it caught.
    pub middle: Option<Point>,
}

/// The middle of a wall, to the centimetre.
pub(super) fn middle_of(wall: &Wall) -> Point {
    let half = |a: i32, b: i32| ((f64::from(a) + f64::from(b)) / 2.0).round() as i32;
    Point::new(half(wall.from.x, wall.to.x), half(wall.from.y, wall.to.y))
}

/// Where a new point goes: onto a corner that's already there, the middle of a wall or the
/// line of one if any is within reach, level with another wall's end if a line is being drawn
/// and it nearly is, and onto the grid otherwise.
///
/// `except` leaves one corner out — the one being dragged — along with every wall standing on
/// it. Without it a corner could never be moved off the grid square it started on, because it
/// would keep snapping to itself, or to the middle of the wall it is the end of.
///
/// `run_from` is the start of the line being drawn, when there is one. Only a line has an end
/// that can be level with anything, so only then are there guides.
pub(super) fn place(
    level: &Level,
    world: (f64, f64),
    view: Viewport,
    snap: Snap,
    except: Option<Point>,
    run_from: Option<Point>,
) -> Placed {
    let per_pixel = view.cm_per_pixel();
    let away = |point: Point| (world.0 - f64::from(point.x)).hypot(world.1 - f64::from(point.y));
    // A wall that can't be caught: one being dragged by its end, or the one the line being
    // drawn has just left, which it would otherwise fold straight back onto.
    let free = |wall: &Wall| {
        except.is_none_or(|except| wall.from != except && wall.to != except)
            && run_from.is_none_or(|from| !stands_on(wall, from))
    };

    let middle = level
        .walls
        .iter()
        .filter(|wall| free(wall))
        .filter_map(|wall| {
            let (_, off) = on_wall_at(wall.from, wall.to, world);
            (off <= SHOWN * per_pixel).then(|| (off, middle_of(wall)))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, middle)| middle);
    let placed = |point, caught, guides| Placed {
        point,
        caught,
        guides,
        middle,
    };

    let corner = level
        .walls
        .iter()
        .flat_map(|wall| [wall.from, wall.to])
        .filter(|corner| Some(*corner) != except)
        .map(|corner| (away(corner), corner))
        .filter(|(off, _)| *off <= CORNER * per_pixel)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, corner)) = corner {
        return placed(corner, Caught::Corner, Vec::new());
    }

    let halfway = level
        .walls
        .iter()
        .enumerate()
        .filter(|(_, wall)| free(wall))
        .map(|(index, wall)| (away(middle_of(wall)), index, middle_of(wall)))
        .filter(|(off, _, _)| *off <= MIDDLE * per_pixel)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, index, point)) = halfway {
        return placed(point, Caught::Middle(index), Vec::new());
    }

    let step = snap.step();
    let grid = Point::new(round(world.0, step), round(world.1, step));
    // Along a wall the grid still counts, measured from the wall's own end: a wall started
    // "1.20 m from the corner" is what somebody with a tape measure means. And the wall only
    // has the point while it is nearer than the grid is: a reach is in pixels, and zoomed out
    // it is wider than a grid square, so without this a point one square off a wall could
    // never be put down.
    let beside = level
        .walls
        .iter()
        .enumerate()
        .filter(|(_, wall)| free(wall))
        .filter_map(|(index, wall)| {
            let (at, off) = on_wall_at(wall.from, wall.to, world);
            (off <= ON_WALL * per_pixel && off <= away(grid)).then_some((off, index, at))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, index, at)) = beside
        && let Some(wall) = level.walls.get(index)
    {
        let at = f64::from(round(at, step)).clamp(0.0, wall.length());
        let (point, _, _) = point_along(wall.from, wall.to, at);
        return placed(
            Point::new(point.0.round() as i32, point.1.round() as i32),
            Caught::OnWall(index),
            Vec::new(),
        );
    }

    let Some(from) = run_from else {
        return placed(grid, Caught::Grid, Vec::new());
    };
    match level_with(level, world, from, grid, GUIDE * per_pixel, &free) {
        Some((point, guides)) => placed(point, Caught::Guide, guides),
        None => placed(grid, Caught::Grid, Vec::new()),
    }
}

/// Where the end of a line from `from` is level with something already drawn, if the pointer
/// is nearly there: straight across or straight down from a corner, or — for a line running
/// alongside a slanted wall — square across from that wall's end, which is where a second wall
/// the same length as the first stops.
fn level_with(
    level: &Level,
    world: (f64, f64),
    from: Point,
    grid: Point,
    reach: f64,
    free: &impl Fn(&Wall) -> bool,
) -> Option<(Point, Vec<Guide>)> {
    let corners: Vec<Point> = level
        .walls
        .iter()
        .flat_map(|wall| [wall.from, wall.to])
        // The line's own start counts: straight across from where it began is the squarest
        // line there is. It gets no marker, because the line itself is the marker.
        .chain([from])
        .collect();
    let away = |point: &Point| (world.0 - f64::from(point.x)).hypot(world.1 - f64::from(point.y));
    // The nearest in the one direction, and of those the nearest corner to draw the line to.
    // A corner only holds the point while it is at least as near as the grid line is, for the
    // reason a wall does: so that the square beside it can still be reached. A corner the
    // grid lands level with anyway still gets its line, because it is still worth knowing.
    let nearest = |along: &dyn Fn(&Point) -> i32, pointer: f64, grid: i32| {
        let off = |corner: &Point| (pointer - f64::from(along(corner))).abs();
        let to_grid = (pointer - f64::from(grid)).abs();
        corners
            .iter()
            .filter(|corner| {
                off(corner) <= reach && (along(corner) == grid || off(corner) <= to_grid)
            })
            .min_by(|a, b| off(a).total_cmp(&off(b)).then(away(a).total_cmp(&away(b))))
            .copied()
    };
    let across = nearest(&|corner| corner.x, world.0, grid.x);
    let down = nearest(&|corner| corner.y, world.1, grid.y);
    if across.is_some() || down.is_some() {
        let point = Point::new(
            across.map_or(grid.x, |corner| corner.x),
            down.map_or(grid.y, |corner| corner.y),
        );
        let guides = [across, down]
            .into_iter()
            .flatten()
            .filter(|corner| *corner != from && *corner != point)
            .map(|corner| Guide {
                from: corner,
                to: point,
            })
            .collect();
        return Some((point, guides));
    }

    // Alongside a slanted wall. Walls that run straight across or down were covered above.
    let (fx, fy) = (f64::from(from.x), f64::from(from.y));
    let (dx, dy) = (world.0 - fx, world.1 - fy);
    let length = dx.hypot(dy);
    if length < 1.0 {
        return None;
    }
    let heading = (dx / length, dy / length);
    level
        .walls
        .iter()
        .filter(|wall| free(wall))
        .filter_map(|wall| {
            let run = direction(wall.from, wall.to);
            if run.0.abs() < 1e-9 || run.1.abs() < 1e-9 {
                return None;
            }
            if (heading.0 * run.1 - heading.1 * run.0).abs() > PARALLEL {
                return None;
            }
            // The wall's direction, turned to point the way the line is going.
            let way = if heading.0 * run.0 + heading.1 * run.1 < 0.0 {
                (-run.0, -run.1)
            } else {
                run
            };
            let along = dx * way.0 + dy * way.1;
            [wall.from, wall.to]
                .into_iter()
                .filter_map(|end| {
                    let level = (f64::from(end.x) - fx) * way.0 + (f64::from(end.y) - fy) * way.1;
                    let off = (level - along).abs();
                    (level > 0.0 && off <= reach).then(|| {
                        let point = Point::new(
                            (fx + way.0 * level).round() as i32,
                            (fy + way.1 * level).round() as i32,
                        );
                        (off, point, end)
                    })
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, point, end)| {
            (
                point,
                vec![Guide {
                    from: end,
                    to: point,
                }],
            )
        })
}

/// A point put on the grid and nothing else, for when the pull of everything already drawn is
/// in the way: held off with a key, the pointer goes where the grid says.
pub(super) fn on_grid(world: (f64, f64), snap: Snap) -> Placed {
    let step = snap.step();
    Placed {
        point: Point::new(round(world.0, step), round(world.1, step)),
        caught: Caught::Grid,
        guides: Vec::new(),
        middle: None,
    }
}

/// Whether a point stands on a wall: at one of its ends, or along it.
fn stands_on(wall: &Wall, point: Point) -> bool {
    let world = (f64::from(point.x), f64::from(point.y));
    on_wall_at(wall.from, wall.to, world).1 < STANDING
}

/// Whether a line is a twin of a wall already drawn: alongside it and the same length, which is
/// what the two long sides of a room are.
pub(super) fn twin(level: &Level, from: Point, to: Point) -> bool {
    let length = from.distance_to(to);
    if length < 1.0 {
        return false;
    }
    let heading = direction(from, to);
    level.walls.iter().any(|wall| {
        let run = direction(wall.from, wall.to);
        (heading.0 * run.1 - heading.1 * run.0).abs() <= PARALLEL
            && (wall.length() - length).abs() < 0.5
            && !(stands_on(wall, from) && stands_on(wall, to))
    })
}

/// The walls a run of wall started off: the far ends of the lines its first corner stands on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Hosts {
    behind: Vec<Point>,
    /// Whether the run started part-way along a wall rather than at the end of one. Then the
    /// two far ends are the same wall, seen both ways.
    along: bool,
}

/// What a run starting at `node` starts off: every wall that ends there, or failing that the
/// wall it is part-way along.
///
/// Asked once, when the run starts, about the point the run started on — not guessed later from
/// whatever happens to share a corner with it.
pub(super) fn hosts(level: &Level, node: Point) -> Hosts {
    let ending: Vec<Point> = level
        .walls
        .iter()
        .filter_map(|wall| {
            if wall.from == node {
                Some(wall.to)
            } else if wall.to == node {
                Some(wall.from)
            } else {
                None
            }
        })
        // More than a few walls at one corner and the angles are a thicket, not a reading.
        .take(3)
        .collect();
    if !ending.is_empty() {
        return Hosts {
            behind: ending,
            along: false,
        };
    }
    match level.walls.iter().find(|wall| stands_on(wall, node)) {
        Some(wall) => Hosts {
            behind: vec![wall.from, wall.to],
            along: true,
        },
        None => Hosts::default(),
    }
}

impl Hosts {
    /// The corners to read as the line from `node` reaches for `to`: each as the far end of the
    /// wall already there. A run that started at a corner reads the angle to every wall meeting
    /// there. One that started part-way along a wall makes two angles that add up to a straight
    /// line, and reads the smaller: it is the one that says how far off square the new wall is.
    pub(super) fn readings(&self, node: Point, to: Point) -> Vec<Point> {
        if !self.along {
            return self.behind.clone();
        }
        self.behind
            .iter()
            .copied()
            .min_by(|a, b| angle_at(*a, node, to).total_cmp(&angle_at(*b, node, to)))
            .into_iter()
            .collect()
    }
}

/// One corner of a room: where it is, the angle the room turns through there, and which way is
/// into the room from it, for somewhere to write the angle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Turned {
    pub at: Point,
    pub degrees: f64,
    pub inward: (f64, f64),
}

/// The angle at every corner of a room, measured on the inside.
///
/// Not the angle between the two sides, which never passes a straight line: the notch of an
/// L-shaped room is 270° of room, and reading it as 90° would make the corners of a room add up
/// to nothing in particular. Which side is inside comes from which way round the shape was
/// traced, so it doesn't matter which way that was.
pub(super) fn room_angles(points: &[Point]) -> Vec<Turned> {
    let count = points.len();
    if count < 3 {
        return Vec::new();
    }
    let at = |index: usize| {
        let point = points[index % count];
        (f64::from(point.x), f64::from(point.y))
    };
    // Twice the shape's area, signed by the way round it goes.
    let around: f64 = (0..count)
        .map(|index| {
            let (here, next) = (at(index), at(index + 1));
            here.0 * next.1 - next.0 * here.1
        })
        .sum();
    let side = if around < 0.0 { -1.0 } else { 1.0 };
    (0..count)
        .map(|index| {
            let (before, corner, after) = (
                points[(index + count - 1) % count],
                points[index],
                points[(index + 1) % count],
            );
            let (arriving, leaving) = (direction(before, corner), direction(corner, after));
            let bend = (arriving.0 * leaving.1 - arriving.1 * leaving.0) * side;
            let between = angle_at(before, corner, after);
            let degrees = if bend < -1e-9 {
                360.0 - between
            } else {
                between
            };
            // Into the room: between the two sides for an ordinary corner, away from them for a
            // notch, and square off the side for a corner that is no corner at all.
            let (back, on) = (direction(corner, before), leaving);
            let mut inward = (back.0 + on.0, back.1 + on.1);
            let reach = inward.0.hypot(inward.1);
            if reach < 1e-6 {
                inward = (-arriving.1 * side, arriving.0 * side);
            } else {
                let turn = if bend < -1e-9 { -1.0 } else { 1.0 };
                inward = (inward.0 / reach * turn, inward.1 / reach * turn);
            }
            Turned {
                at: corner,
                degrees,
                inward,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(from: (i32, i32), to: (i32, i32)) -> Wall {
        Wall::new(Point::new(from.0, from.1), Point::new(to.0, to.1))
    }

    type Ends = ((i32, i32), (i32, i32));

    fn plan(walls: &[Ends]) -> Level {
        Level {
            walls: walls.iter().map(|(from, to)| wall(*from, *to)).collect(),
            ..Level::default()
        }
    }

    fn points(corners: &[(i32, i32)]) -> Vec<Point> {
        corners.iter().map(|(x, y)| Point::new(*x, *y)).collect()
    }

    /// One pixel to the centimetre, so a reach in pixels reads as centimetres below.
    fn view() -> Viewport {
        Viewport {
            scale: 1.0,
            pan: (0.0, 0.0),
        }
    }

    #[test]
    fn the_middle_of_a_wall_pulls_and_a_corner_pulls_harder() {
        let level = plan(&[((0, 0), (400, 0))]);
        let at = |world| place(&level, world, view(), Snap::Grid, None, None);

        let caught = at((193.0, 8.0));
        assert_eq!(caught.point, Point::new(200, 0));
        assert_eq!(caught.caught, Caught::Middle(0));

        // Beside the wall but not near its middle: on the wall, on the grid along it, and the
        // middle still marked to aim at.
        let along = at((118.0, 4.0));
        assert_eq!(along.point, Point::new(120, 0));
        assert_eq!(along.caught, Caught::OnWall(0));
        assert_eq!(along.middle, Some(Point::new(200, 0)));

        // A wall so short its middle is within a corner's reach: the corner has it.
        let short = plan(&[((0, 0), (20, 0))]);
        let end = place(&short, (12.0, 0.0), view(), Snap::Grid, None, None);
        assert_eq!(end.caught, Caught::Corner);

        // Out in the open there is nothing to mark and nothing to catch on.
        let open = at((200.0, 300.0));
        assert_eq!(
            (open.caught, open.middle, open.point),
            (Caught::Grid, None, Point::new(200, 300))
        );
    }

    /// A reach is in pixels, and zoomed out it is wider than a square of the grid. Whatever is
    /// already drawn must not make the square beside it somewhere a point can't go.
    #[test]
    fn the_grid_square_beside_a_wall_or_a_corner_can_still_be_reached() {
        let level = plan(&[((0, 0), (400, 0))]);
        // A third of a pixel to the centimetre: every reach here is wider than the grid.
        let far = Viewport {
            scale: 0.3,
            pan: (0.0, 0.0),
        };
        // One square below the wall, away from its middle and its ends.
        let below = place(&level, (118.0, 9.0), far, Snap::Grid, None, None);
        assert_eq!(below.point, Point::new(120, 10));
        // A line ending one square past the corner above it, not level with it.
        let from = Point::new(0, 300);
        let past = place(&level, (409.0, 300.0), far, Snap::Grid, None, Some(from));
        assert_eq!(past.point, Point::new(410, 300));
        // And level with it when that is where the grid lands anyway, with the line to say so.
        let level_with = place(&level, (402.0, 300.0), far, Snap::Grid, None, Some(from));
        assert_eq!(level_with.point, Point::new(400, 300));
        assert_eq!(level_with.guides.len(), 1);
        // Held off, there is only the grid, even on top of a corner.
        assert_eq!(
            on_grid((397.0, 4.0), Snap::Custom(5)).point,
            Point::new(395, 5)
        );
    }

    #[test]
    fn the_middle_of_an_odd_wall_is_a_whole_centimetre() {
        assert_eq!(middle_of(&wall((0, 0), (135, 0))), Point::new(68, 0));
        // Ends too far apart to add as whole numbers still have a middle.
        assert_eq!(
            middle_of(&wall((-2_000_000_000, 0), (2_000_000_000, 6))),
            Point::new(0, 3)
        );
    }

    /// A corner being dragged is the end of its own wall, and that wall's middle moves with
    /// it. Chasing it would shrink the wall to nothing.
    #[test]
    fn a_dragged_corner_ignores_the_wall_it_is_the_end_of() {
        let level = plan(&[((0, 0), (400, 0))]);
        let dragged = Point::new(400, 0);
        let to = place(
            &level,
            (203.0, 4.0),
            view(),
            Snap::Grid,
            Some(dragged),
            None,
        );
        assert_eq!(to.caught, Caught::Grid);
        assert_eq!(to.point, Point::new(200, 0));
        assert_eq!(to.middle, None);
    }

    #[test]
    fn a_line_stops_level_with_the_end_of_the_wall_beside_it() {
        // A wall along the top, and a second one being drawn under it from under its start.
        // The top wall was drawn off the grid, so the grid alone would miss its end.
        let level = plan(&[((0, 0), (403, 0))]);
        let from = Point::new(0, 300);
        let to = place(&level, (406.0, 300.0), view(), Snap::Grid, None, Some(from));
        assert_eq!(to.point, Point::new(403, 300));
        assert_eq!(to.caught, Caught::Guide);
        assert_eq!(
            to.guides,
            vec![Guide {
                from: Point::new(403, 0),
                to: Point::new(403, 300),
            }]
        );
        assert!(twin(&level, from, to.point), "the same length, alongside");

        // Nowhere near level with anything: the grid, and no marker.
        let free = place(&level, (250.0, 300.0), view(), Snap::Grid, None, Some(from));
        assert_eq!(free.point, Point::new(250, 300));
        assert!(free.guides.is_empty());
        assert!(!twin(&level, from, free.point));

        // With no line being drawn there is no end to be level with anything.
        let idle = place(&level, (406.0, 300.0), view(), Snap::Grid, None, None);
        assert_eq!(idle.caught, Caught::Grid);
    }

    #[test]
    fn a_line_beside_a_slanted_wall_stops_square_across_from_its_end() {
        // A wall running off at a slant, and a line drawn alongside it from square across from
        // its start. Nothing here is straight across or down from anything, so only the wall's
        // own direction can say where the second wall should stop.
        let level = plan(&[((0, 0), (300, 200))]);
        let from = Point::new(-100, 150);
        let to = place(&level, (203.0, 348.0), view(), Snap::Grid, None, Some(from));
        assert_eq!(to.point, Point::new(200, 350));
        assert_eq!(to.caught, Caught::Guide);
        assert_eq!(
            to.guides,
            vec![Guide {
                from: Point::new(300, 200),
                to: Point::new(200, 350),
            }]
        );
        assert!(twin(&level, from, to.point));
    }

    #[test]
    fn a_wall_is_not_its_own_twin() {
        let level = plan(&[((0, 0), (400, 0))]);
        assert!(!twin(&level, Point::new(0, 0), Point::new(400, 0)));
        assert!(twin(&level, Point::new(0, 100), Point::new(400, 100)));
        assert!(!twin(&level, Point::new(0, 100), Point::new(0, 500)));
    }

    #[test]
    fn a_run_reads_its_angle_off_the_wall_it_started_on() {
        let level = plan(&[((0, 0), (400, 0)), ((400, 0), (400, 300))]);

        // From a corner: every wall that meets there.
        let corner = hosts(&level, Point::new(400, 0));
        assert_eq!(
            corner.readings(Point::new(400, 0), Point::new(600, -200)),
            points(&[(0, 0), (400, 300)])
        );

        // From part-way along: the smaller of the two angles, whichever side that is.
        let node = Point::new(200, 0);
        let along = hosts(&level, node);
        assert_eq!(
            along.readings(node, Point::new(300, 100)),
            points(&[(400, 0)])
        );
        assert_eq!(
            along.readings(node, Point::new(100, 100)),
            points(&[(0, 0)])
        );

        // From open floor: nothing to read an angle off.
        assert_eq!(hosts(&level, Point::new(200, 200)), Hosts::default());
    }

    /// The notch of an L-shaped room is why this isn't the angle between two lines.
    #[test]
    fn the_corners_of_a_room_are_read_on_the_inside() {
        let ell = points(&[
            (0, 0),
            (400, 0),
            (400, 200),
            (200, 200),
            (200, 400),
            (0, 400),
        ]);
        let read = |corners: &[Point]| {
            room_angles(corners)
                .iter()
                .map(|turned| turned.degrees.round() as i32)
                .collect::<Vec<_>>()
        };
        assert_eq!(read(&ell), vec![90, 90, 90, 270, 90, 90]);

        // Traced the other way round it is the same room.
        let mut back = ell.clone();
        back.reverse();
        assert_eq!(read(&back), vec![90, 90, 270, 90, 90, 90]);

        // The corners of any shape add up to a half turn for each corner past the second.
        let odd = points(&[(0, 0), (500, 40), (430, 310), (260, 180), (-40, 420)]);
        let sum: f64 = room_angles(&odd).iter().map(|turned| turned.degrees).sum();
        assert!((sum - 540.0).abs() < 1e-6, "{sum}");

        assert!(room_angles(&points(&[(0, 0), (400, 0)])).is_empty());
    }

    #[test]
    fn an_angle_is_written_inside_the_room() {
        let ell = points(&[
            (0, 0),
            (400, 0),
            (400, 200),
            (200, 200),
            (200, 400),
            (0, 400),
        ]);
        let room = irori_types::PlacedArea {
            area: "hall".parse().expect("a valid area id"),
            points: ell.clone(),
            label: Point::new(0, 0),
            tint: None,
        };
        for turned in room_angles(&ell) {
            let inside = Point::new(
                turned.at.x + (turned.inward.0 * 20.0).round() as i32,
                turned.at.y + (turned.inward.1 * 20.0).round() as i32,
            );
            assert!(room.contains(inside), "{turned:?}");
        }
    }
}
