//! The plan of the home: its walls, the openings in them, and where the devices sit.
//!
//! This is drawing, not discovery. No integration can tell Irori where a wall is, so a floorplan
//! is authored intent through and through and lives in the config directory with the rest of it
//! (`docs/specs/config.md` §3.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::{AreaId, DeviceId, FloorId};

/// A point on the plan, in whole centimetres: `x` rightwards, `y` downwards, from an origin that
/// is wherever the person who drew it started.
///
/// Whole centimetres rather than fractions for two reasons. A plan has to compare equal to
/// itself — [`crate::Settings`] is `Eq`, and the core skips work when nothing changed — which
/// floats can't promise. And a file of round numbers diffs cleanly in the git repository the
/// config directory is meant to live in. A centimetre is finer than anyone draws a house.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "[i32; 2]", into = "[i32; 2]")]
#[schemars(
    with = "[i32; 2]",
    description = "A point on the plan: [x, y] in centimetres."
)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// How far it is from another point, in centimetres.
    ///
    /// Each coordinate becomes a `f64` before anything is subtracted. A plan is whatever
    /// somebody typed into the file, so two points really can sit at opposite ends of `i32`,
    /// and subtracting those first would overflow — a panic where the answer wanted was
    /// "very far apart", on the path [`Floorplan::check`] uses to *reject* such a plan.
    pub fn distance_to(self, other: Point) -> f64 {
        let dx = f64::from(other.x) - f64::from(self.x);
        let dy = f64::from(other.y) - f64::from(self.y);
        dx.hypot(dy)
    }
}

impl From<[i32; 2]> for Point {
    fn from([x, y]: [i32; 2]) -> Self {
        Self { x, y }
    }
}

impl From<Point> for [i32; 2] {
    fn from(point: Point) -> Self {
        [point.x, point.y]
    }
}

/// Everything drawn, a floor at a time.
///
/// Keyed by the floor's id, because a plan **is** the plan of a floor — a house with an upstairs
/// has two of them, drawn one over the other, and the floors are already a thing the home knows
/// about (`docs/specs/config.md` §3.1). A home nobody has divided into floors has nowhere to
/// draw until it has one, which is a question with an obvious answer rather than a reason for a
/// second shape of plan.
///
/// Empty is the ordinary state of a home nobody has drawn yet, not a missing value.
///
/// A floor that has since been removed keeps its plan here, unshown, the same as every other
/// entry that outlives what it points at (`docs/specs/config.md` §4): making the floor again
/// brings the drawing back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Floorplan {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub floors: BTreeMap<FloorId, Level>,
}

impl Floorplan {
    /// Whether nothing has been drawn yet, anywhere.
    pub fn is_empty(&self) -> bool {
        self.floors.values().all(Level::is_empty)
    }

    /// What's drawn on one floor, if anything is.
    pub fn level(&self, floor: &FloorId) -> Option<&Level> {
        self.floors.get(floor)
    }

    /// What's drawn on one floor, making an empty one to draw on if this is the first thing.
    pub fn level_mut(&mut self, floor: &FloorId) -> &mut Level {
        self.floors.entry(floor.clone()).or_default()
    }

    /// Forgets floors that have nothing on them, so cancelling out of an edit that touched a
    /// floor and then undid it doesn't leave a heading behind in the file.
    pub fn tidy(&mut self) {
        self.floors.retain(|_, level| !level.is_empty());
    }
}

/// One floor of the home as it is drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Level {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub walls: Vec<Wall>,
    /// The rooms of this floor, as shapes. An area with no shape here simply isn't drawn; an
    /// area is a thing the home knows about whether or not anybody has traced it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<PlacedArea>,
    /// Where a device is, for the ones that have been put somewhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<PlacedDevice>,
}

impl Level {
    pub fn is_empty(&self) -> bool {
        self.walls.is_empty() && self.areas.is_empty() && self.devices.is_empty()
    }
}

/// A room traced out on the plan: which area it is, and the shape of it.
///
/// The shape is a closed run of corners — the last joins back to the first, so the file doesn't
/// carry the first point twice and can't disagree with itself about where the room closes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlacedArea {
    pub area: AreaId,
    pub points: Vec<Point>,
}

impl PlacedArea {
    /// The fewest corners a shape can have and still be one.
    pub const FEWEST_POINTS: usize = 3;

    /// The middle of it, for putting the room's name. The average of the corners rather than the
    /// true centroid: it is where a label looks right, and for the rectangles and L-shapes rooms
    /// actually are, the two are close enough that nobody could tell them apart.
    pub fn middle(&self) -> Option<Point> {
        if self.points.is_empty() {
            return None;
        }
        let count = self.points.len() as f64;
        let (x, y) = self.points.iter().fold((0.0, 0.0), |(x, y), point| {
            (x + f64::from(point.x), y + f64::from(point.y))
        });
        Some(Point::new((x / count) as i32, (y / count) as i32))
    }
}

/// A straight run of wall between two points.
///
/// Openings live **inside** the wall they are cut into rather than pointing at it by id. A door
/// is a hole in a wall and has nowhere else to be, so this way a wall can't be deleted out from
/// under its own doors and no id has to be handed out to make the link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Wall {
    pub from: Point,
    pub to: Point,
    /// How thick it is, in centimetres. Drawn, not structural: it only decides how heavy the
    /// line looks.
    #[serde(default = "default_thickness")]
    pub thickness: u32,
    /// The doors and windows cut into it, in no particular order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub openings: Vec<Opening>,
}

fn default_thickness() -> u32 {
    Wall::DEFAULT_THICKNESS
}

impl Wall {
    /// What a wall is drawn as until somebody says otherwise: an interior wall in a home is
    /// about 10 cm, and an exterior one more.
    pub const DEFAULT_THICKNESS: u32 = 10;

    /// How thin and how thick a wall may be drawn, in centimetres. A wall thinner than a
    /// centimetre wouldn't be visible at any zoom; one thicker than a metre isn't a wall.
    pub const THICKNESS_RANGE: std::ops::RangeInclusive<u32> = 1..=100;

    pub fn new(from: Point, to: Point) -> Self {
        Self {
            from,
            to,
            thickness: default_thickness(),
            openings: Vec::new(),
        }
    }

    /// How long it is, in centimetres.
    pub fn length(&self) -> f64 {
        self.from.distance_to(self.to)
    }
}

/// A hole in a wall, placed by how far along that wall it is.
///
/// Along the wall rather than at a point of its own, so moving a wall carries its doors with it
/// and a door can never end up floating beside the wall it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Opening {
    pub kind: OpeningKind,
    /// Centimetres from the wall's `from` end to the middle of the opening.
    pub at: i32,
    /// How wide it is, in centimetres.
    pub width: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OpeningKind {
    Door,
    Window,
}

impl OpeningKind {
    /// What one of these is usually called, for the editor to say.
    pub fn label(self) -> &'static str {
        match self {
            OpeningKind::Door => "Door",
            OpeningKind::Window => "Window",
        }
    }

    /// How wide one starts out, in centimetres: a doorway, and a window that isn't a slit.
    pub fn default_width(self) -> u32 {
        match self {
            OpeningKind::Door => 80,
            OpeningKind::Window => 100,
        }
    }
}

/// A device put somewhere on the plan.
///
/// The device may not be in the home right now — unplugged, or its extension uninstalled — and
/// the entry stays anyway, as every other config entry does (`docs/specs/config.md` §4). It
/// simply isn't drawn until the device is back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlacedDevice {
    pub device: DeviceId,
    pub at: Point,
}

/// Why a plan was refused. Checked where the plan is saved, so a file edited by hand and a page
/// that sends nonsense are judged by the same rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError(String);

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlanError {}

impl Floorplan {
    /// Checks the things that would make a plan impossible to draw rather than merely odd.
    ///
    /// Deliberately short. A wall crossing another one, a device in the garden, two doors on top
    /// of each other — all of those are somebody's house, or somebody mid-edit, and none of them
    /// stop the plan being drawn. What does: a wall with no length (it has no direction, so its
    /// openings have nowhere to sit), an opening that doesn't fit in its wall, and a room shape
    /// with too few corners to be a shape.
    pub fn check(&self) -> Result<(), PlanError> {
        for (floor, level) in &self.floors {
            level
                .check()
                .map_err(|PlanError(why)| PlanError(format!("on floor `{floor}`, {why}")))?;
        }
        Ok(())
    }
}

impl Level {
    fn check(&self) -> Result<(), PlanError> {
        for (index, wall) in self.walls.iter().enumerate() {
            let which = index + 1;
            if wall.from == wall.to {
                return Err(PlanError(format!(
                    "wall {which} starts and ends in the same place, so it isn't a wall"
                )));
            }
            if wall.thickness == 0 {
                return Err(PlanError(format!("wall {which} has no thickness")));
            }
            let length = wall.length();
            for opening in &wall.openings {
                if opening.width == 0 {
                    return Err(PlanError(format!(
                        "a {} in wall {which} has no width",
                        opening.kind.label().to_lowercase()
                    )));
                }
                let half = f64::from(opening.width) / 2.0;
                let at = f64::from(opening.at);
                if at - half < -0.5 || at + half > length + 0.5 {
                    return Err(PlanError(format!(
                        "a {} in wall {which} hangs off the end of it",
                        opening.kind.label().to_lowercase()
                    )));
                }
            }
        }
        let mut traced = std::collections::BTreeSet::new();
        for placed in &self.areas {
            if placed.points.len() < PlacedArea::FEWEST_POINTS {
                return Err(PlanError(format!(
                    "the shape of `{}` has {} corner(s); a room needs at least {}",
                    placed.area,
                    placed.points.len(),
                    PlacedArea::FEWEST_POINTS
                )));
            }
            // One shape per room per floor. A room in two pieces on one floor is a room somebody
            // drew twice, and letting it through would leave the editor with no way to say which
            // of them a click meant.
            if !traced.insert(&placed.area) {
                return Err(PlanError(format!(
                    "`{}` is drawn twice on this floor",
                    placed.area
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(from: (i32, i32), to: (i32, i32)) -> Wall {
        Wall::new(Point::new(from.0, from.1), Point::new(to.0, to.1))
    }

    fn ground() -> FloorId {
        "ground".parse().expect("a valid floor id")
    }

    fn on_ground(level: Level) -> Floorplan {
        Floorplan {
            floors: [(ground(), level)].into(),
        }
    }

    fn area(id: &str, points: &[(i32, i32)]) -> PlacedArea {
        PlacedArea {
            area: id.parse().expect("a valid area id"),
            points: points.iter().map(|(x, y)| Point::new(*x, *y)).collect(),
        }
    }

    #[test]
    fn a_point_is_written_as_a_pair_of_whole_centimetres() {
        let point = Point::new(120, -30);
        assert_eq!(serde_json::to_string(&point).expect("json"), "[120,-30]");
        assert_eq!(
            serde_json::from_str::<Point>("[120,-30]").expect("json"),
            point
        );
    }

    /// A plan is whatever somebody typed into the file, so two points really can sit at
    /// opposite ends of `i32`. Measuring that must answer "very far apart" rather than panic,
    /// because the measuring is what `check` uses to turn it down.
    #[test]
    fn two_points_at_the_ends_of_the_world_can_still_be_measured() {
        let far = Point::new(i32::MIN, i32::MIN).distance_to(Point::new(i32::MAX, i32::MAX));
        assert!(far.is_finite() && far > 6e9, "{far}");

        let plan = on_ground(Level {
            walls: vec![Wall {
                openings: vec![Opening {
                    kind: OpeningKind::Door,
                    at: 0,
                    width: 80,
                }],
                ..Wall::new(Point::new(i32::MIN, 0), Point::new(i32::MAX, 0))
            }],
            ..Level::default()
        });
        assert!(
            plan.check().is_err(),
            "a door at one end of a wall that long"
        );
    }

    #[test]
    fn a_wall_with_no_length_is_refused_because_its_doors_would_have_nowhere_to_go() {
        let plan = on_ground(Level {
            walls: vec![wall((0, 0), (0, 0))],
            ..Level::default()
        });
        assert!(plan.check().is_err());
    }

    #[test]
    fn an_opening_has_to_fit_in_the_wall_it_is_cut_into() {
        let fits = |at, width| {
            let mut wall = wall((0, 0), (400, 0));
            wall.openings.push(Opening {
                kind: OpeningKind::Door,
                at,
                width,
            });
            on_ground(Level {
                walls: vec![wall],
                ..Level::default()
            })
            .check()
            .is_ok()
        };

        assert!(fits(200, 80), "in the middle");
        assert!(fits(40, 80), "flush with the near end");
        assert!(fits(360, 80), "flush with the far end");
        assert!(!fits(20, 80), "hanging off the near end");
        assert!(!fits(380, 80), "hanging off the far end");
        assert!(!fits(200, 0), "no width at all");
    }

    /// Which floor is wrong is the first thing anyone needs to know about a plan that won't
    /// load, because it says which part of the file to go and look at.
    #[test]
    fn a_refusal_says_which_floor_it_is_about() {
        let plan = Floorplan {
            floors: [
                (ground(), Level::default()),
                (
                    "upstairs".parse().expect("a valid floor id"),
                    Level {
                        walls: vec![wall((0, 0), (0, 0))],
                        ..Level::default()
                    },
                ),
            ]
            .into(),
        };
        let error = plan.check().expect_err("a wall with no length").to_string();
        assert!(error.contains("upstairs"), "{error}");
    }

    #[test]
    fn a_room_needs_enough_corners_to_be_a_shape_and_may_only_be_drawn_once() {
        let square = &[(0, 0), (400, 0), (400, 300), (0, 300)][..];
        assert!(
            on_ground(Level {
                areas: vec![area("kitchen", square)],
                ..Level::default()
            })
            .check()
            .is_ok()
        );
        assert!(
            on_ground(Level {
                areas: vec![area("kitchen", &[(0, 0), (400, 0)])],
                ..Level::default()
            })
            .check()
            .is_err(),
            "two corners is a line, not a room"
        );
        assert!(
            on_ground(Level {
                areas: vec![area("kitchen", square), area("kitchen", square)],
                ..Level::default()
            })
            .check()
            .is_err(),
            "the same room drawn twice on one floor"
        );
    }

    /// The same room may be traced on two floors — a stairwell, a double-height hall — because
    /// the rule is one shape per floor, not one shape in the home.
    #[test]
    fn a_room_may_be_traced_on_more_than_one_floor() {
        let square = &[(0, 0), (400, 0), (400, 300), (0, 300)][..];
        let level = Level {
            areas: vec![area("stairs", square)],
            ..Level::default()
        };
        let plan = Floorplan {
            floors: [
                (ground(), level.clone()),
                ("upstairs".parse().expect("a valid floor id"), level),
            ]
            .into(),
        };
        assert!(plan.check().is_ok());
    }

    #[test]
    fn a_rooms_label_goes_in_the_middle_of_it() {
        let square = area("kitchen", &[(0, 0), (400, 0), (400, 300), (0, 300)]);
        assert_eq!(square.middle(), Some(Point::new(200, 150)));
        assert_eq!(area("empty", &[]).middle(), None);
    }

    /// The plan a first run has: one that says nothing, and writes nothing.
    #[test]
    fn an_empty_plan_is_fine_and_knows_it_is_empty() {
        let plan = Floorplan::default();
        assert!(plan.is_empty());
        assert!(plan.check().is_ok());
        assert_eq!(serde_json::to_string(&plan).expect("json"), "{}");
    }

    /// A floor somebody drew on and then cleared is not a floor with a plan.
    #[test]
    fn a_floor_with_nothing_on_it_is_tidied_away() {
        let mut plan = Floorplan::default();
        plan.level_mut(&ground());
        assert!(plan.is_empty(), "nothing has been drawn on it");
        plan.tidy();
        assert!(plan.floors.is_empty());
        assert_eq!(serde_json::to_string(&plan).expect("json"), "{}");
    }
}
