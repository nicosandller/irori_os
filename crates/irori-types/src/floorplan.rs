//! The plan of the home: its walls, the openings in them, and where the devices sit.
//!
//! This is drawing, not discovery. No integration can tell Irori where a wall is, so a floorplan
//! is authored intent through and through and lives in the config directory with the rest of it
//! (`docs/specs/config.md` §3.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::DeviceId;

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

/// Everything drawn on the plan.
///
/// Empty is the ordinary state of a home nobody has drawn yet, not a missing value: there is one
/// plan, and it starts blank.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Floorplan {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub walls: Vec<Wall>,
    /// Where a device is, for the ones that have been put somewhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<PlacedDevice>,
}

impl Floorplan {
    /// Whether nothing has been drawn yet.
    pub fn is_empty(&self) -> bool {
        self.walls.is_empty() && self.devices.is_empty()
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
    /// openings have nowhere to sit) and an opening that doesn't fit in its wall.
    pub fn check(&self) -> Result<(), PlanError> {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(from: (i32, i32), to: (i32, i32)) -> Wall {
        Wall::new(Point::new(from.0, from.1), Point::new(to.0, to.1))
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

        let plan = Floorplan {
            walls: vec![Wall {
                openings: vec![Opening {
                    kind: OpeningKind::Door,
                    at: 0,
                    width: 80,
                }],
                ..Wall::new(Point::new(i32::MIN, 0), Point::new(i32::MAX, 0))
            }],
            ..Floorplan::default()
        };
        assert!(
            plan.check().is_err(),
            "a door at one end of a wall that long"
        );
    }

    #[test]
    fn a_wall_with_no_length_is_refused_because_its_doors_would_have_nowhere_to_go() {
        let plan = Floorplan {
            walls: vec![wall((0, 0), (0, 0))],
            ..Floorplan::default()
        };
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
            Floorplan {
                walls: vec![wall],
                ..Floorplan::default()
            }
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

    /// The plan a first run has: one that says nothing, and writes nothing.
    #[test]
    fn an_empty_plan_is_fine_and_knows_it_is_empty() {
        let plan = Floorplan::default();
        assert!(plan.is_empty());
        assert!(plan.check().is_ok());
        assert_eq!(serde_json::to_string(&plan).expect("json"), "{}");
    }
}
