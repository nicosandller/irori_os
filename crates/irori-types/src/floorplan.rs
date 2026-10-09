//! The plan of the home: its walls, the openings in them, and where the devices sit.
//!
//! This is drawing, not discovery. No protocol can tell Irori where a wall is, so a floorplan
//! is authored intent through and through and lives in the config directory with the rest of it
//! (`docs/specs/config.md` §3.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::{AreaId, DeviceId, EntityId, FloorId};

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
    /// At least [`PlacedArea::FEWEST_POINTS`] of them: fewer is a line, not a room, and
    /// [`Level::check`] turns it down — so the schema does too.
    #[schemars(length(min = 3))]
    pub points: Vec<Point>,
    /// Where the room's name is drawn, as an offset from [`PlacedArea::middle`]. Dragged to move
    /// the label, and kept as an offset rather than a place of its own so the label travels with
    /// the room: stretch a wall and the name stays where it was put, relative to the room.
    #[serde(
        default = "default_label",
        skip_serializing_if = "PlacedArea::label_is_middle"
    )]
    pub label: Point,
    /// The colour its floor is washed with. Absent, the room gets one picked from its id, as
    /// every room did before this could be said — so a plan nobody has coloured writes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<Tint>,
}

fn default_label() -> Point {
    Point::new(0, 0)
}

/// A colour a room's floor can be washed with.
///
/// A name rather than a colour value, because the plan is drawn on paper by day and on slate by
/// night and no single value reads on both: the page decides what each name looks like on the
/// surface it is drawing on. A short list rather than a free choice for the same reason — and
/// because rooms side by side only need telling apart, not matching the curtains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Tint {
    Ember,
    Moss,
    Slate,
    Sand,
    Plum,
    Teal,
    Rose,
    Sky,
    Olive,
    Stone,
}

impl Tint {
    /// Every tint, in the order a palette shows them.
    pub const ALL: [Tint; 10] = [
        Tint::Ember,
        Tint::Moss,
        Tint::Slate,
        Tint::Sand,
        Tint::Plum,
        Tint::Teal,
        Tint::Rose,
        Tint::Sky,
        Tint::Olive,
        Tint::Stone,
    ];

    /// The word it is written as in the file, which is also what the page calls it.
    pub fn name(self) -> &'static str {
        match self {
            Tint::Ember => "ember",
            Tint::Moss => "moss",
            Tint::Slate => "slate",
            Tint::Sand => "sand",
            Tint::Plum => "plum",
            Tint::Teal => "teal",
            Tint::Rose => "rose",
            Tint::Sky => "sky",
            Tint::Olive => "olive",
            Tint::Stone => "stone",
        }
    }

    /// The tint a room has until somebody picks one: chosen from its id, so the same room is
    /// the same colour every time and two rooms side by side are almost never alike. A sum of
    /// the id's bytes rather than a hash with a random seed, which would differ between the
    /// server and the page. From the first six only, which are the ones every plan drawn before
    /// tints could be chosen already wears.
    pub fn of(area: &AreaId) -> Tint {
        const FIRST: u32 = 6;
        let sum = area.as_str().bytes().fold(0u32, |sum, byte| {
            sum.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
        Tint::ALL[(sum % FIRST) as usize]
    }
}

impl PlacedArea {
    /// The colour this room is drawn in: the one picked for it, or its own.
    pub fn shade(&self) -> Tint {
        self.tint.unwrap_or_else(|| Tint::of(&self.area))
    }
}

impl PlacedArea {
    /// The fewest corners a shape can have and still be one.
    pub const FEWEST_POINTS: usize = 3;

    /// Whether a point falls inside the shape.
    ///
    /// By the crossing-number rule: count the edges a ray cast from the point crosses, and an
    /// odd count means inside. Not a bounding box, because the L-shaped and worse rooms real
    /// homes have would swallow half the hallway. Lives here rather than in the editor because
    /// the server asks the same question when it works out which room a device is standing in.
    pub fn contains(&self, point: Point) -> bool {
        let (x, y) = (f64::from(point.x), f64::from(point.y));
        let mut within = false;
        let Some(last) = self.points.last() else {
            return false;
        };
        let mut previous = (f64::from(last.x), f64::from(last.y));
        for corner in &self.points {
            let current = (f64::from(corner.x), f64::from(corner.y));
            if (current.1 > y) != (previous.1 > y) {
                let span = previous.1 - current.1;
                if span.abs() > f64::EPSILON {
                    let crossing = current.0 + (y - current.1) / span * (previous.0 - current.0);
                    if x < crossing {
                        within = !within;
                    }
                }
            }
            previous = current;
        }
        within
    }

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

    /// Whether the name hasn't been moved: a label at the middle is the default and needn't be
    /// written to the file.
    fn label_is_middle(label: &Point) -> bool {
        label.x == 0 && label.y == 0
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
    ///
    /// At least one: a wall with no thickness isn't drawable, and [`Level::check`] turns it
    /// down — so the schema has to turn it down too, or a document could pass validation and
    /// still be refused on load.
    #[serde(default = "default_thickness")]
    #[schemars(range(min = 1))]
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
    /// How wide it is, in centimetres. At least one, for the reason [`Wall::thickness`] gives.
    #[schemars(range(min = 1))]
    pub width: u32,
    /// Which side of the wall it opens towards, looking along the wall from `from` to `to`.
    /// For a window that is the side its sashes swing out to, which is normally outside.
    #[serde(default, skip_serializing_if = "is_default")]
    pub side: Side,
    /// Which end of the gap a door is hung from. A window is hinged at both jambs, so this
    /// says nothing about one.
    #[serde(default, skip_serializing_if = "is_default")]
    pub hinge: Hinge,
    /// The contact sensor that says whether it's open: a binary sensor on the door or window
    /// itself, where `on` means open. Without one it is drawn shut. Like every other reference
    /// in the config, one that points at an entity no longer in the home is kept and not obeyed
    /// (`docs/specs/config.md` §4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor: Option<EntityId>,
}

impl Opening {
    /// A hole of one kind, somewhere along a wall: opening to the left, hung from the near end,
    /// and with nothing to say whether it's open.
    pub fn new(kind: OpeningKind, at: i32, width: u32) -> Self {
        Self {
            kind,
            at,
            width,
            side: Side::default(),
            hinge: Hinge::default(),
            sensor: None,
        }
    }
}

/// Which side of its wall an opening swings to, looking along the wall from `from` to `to`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// Which end of its gap a door is hung from: the one nearer the wall's `from` end, or the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Hinge {
    #[default]
    Near,
    Far,
}

/// Whether a value is the one it would have been given anyway, and so needn't be written.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
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
    /// Which way a directional sensor points, in degrees clockwise from the plan's +x axis:
    /// 0 is rightwards, 90 is down the page. Authored, because nothing a radar reports says
    /// which wall it was screwed to. Absent for a device that looks every way at once, and for
    /// one nobody has aimed yet.
    ///
    /// Under a full turn: [`Level::check`] turns down 360 and over, so the schema does too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 359))]
    pub facing: Option<u16>,
    /// How wide it sees, in degrees, inside [`PlacedDevice::FIELD_OF_VIEW_RANGE`].
    /// [`PlacedDevice::DEFAULT_FIELD_OF_VIEW`] when `facing` is set and this isn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 20, max = 180))]
    pub field_of_view: Option<u16>,
}

impl PlacedDevice {
    /// How wide a directional sensor sees until somebody says otherwise: about what a
    /// wall-mounted mmWave radar covers.
    pub const DEFAULT_FIELD_OF_VIEW: u16 = 100;

    /// How narrow and how wide a field of view may be, in degrees. Narrower than 20° is a
    /// line, not a field; wider than a half turn is looking through the wall it's mounted on.
    pub const FIELD_OF_VIEW_RANGE: std::ops::RangeInclusive<u16> = 20..=180;

    /// A device put down at a point, not aimed anywhere.
    pub fn new(device: DeviceId, at: Point) -> Self {
        Self {
            device,
            at,
            facing: None,
            field_of_view: None,
        }
    }

    /// How wide it sees: what was set, or the default.
    pub fn view_angle(&self) -> u16 {
        self.field_of_view.unwrap_or(Self::DEFAULT_FIELD_OF_VIEW)
    }
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
        // A device is one object and is in one place, so it is on the plan once — across every
        // floor, not once per floor as a room's shape is. A room can honestly be traced on two
        // floors (a stairwell, a double-height hall); a lamp cannot be in two of them, and a
        // plan that said so would leave whoever read it to pick, which is not a thing a reader
        // should have to do.
        let mut placed = BTreeMap::new();
        for (floor, level) in &self.floors {
            for device in &level.devices {
                if let Some(already) = placed.insert(&device.device, floor) {
                    return Err(PlanError(format!(
                        "`{}` is drawn on two floors, `{already}` and `{floor}`; a device is in \
                         one place",
                        device.device
                    )));
                }
            }
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
        for placed in &self.devices {
            if let Some(facing) = placed.facing
                && facing >= 360
            {
                return Err(PlanError(format!(
                    "`{}` faces {facing}°; a direction is 0 to 359",
                    placed.device
                )));
            }
            if let Some(view) = placed.field_of_view
                && !PlacedDevice::FIELD_OF_VIEW_RANGE.contains(&view)
            {
                return Err(PlanError(format!(
                    "`{}` has a field of view of {view}°; it has to be {} to {}",
                    placed.device,
                    PlacedDevice::FIELD_OF_VIEW_RANGE.start(),
                    PlacedDevice::FIELD_OF_VIEW_RANGE.end()
                )));
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
            label: Point::new(0, 0),
            tint: None,
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
                openings: vec![Opening::new(OpeningKind::Door, 0, 80)],
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
            wall.openings
                .push(Opening::new(OpeningKind::Door, at, width));
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

    /// The counterpart of the rule above: a room can honestly be in two places, a device
    /// cannot. Whoever read a plan that said otherwise would have to pick one.
    #[test]
    fn a_device_is_drawn_once_in_the_whole_home() {
        let lamp = || {
            PlacedDevice::new(
                "demo_lamp".parse().expect("a valid device id"),
                Point::new(120, 90),
            )
        };
        let level = Level {
            devices: vec![lamp()],
            ..Level::default()
        };
        assert!(on_ground(level.clone()).check().is_ok(), "once is fine");

        let twice = Floorplan {
            floors: [
                (ground(), level.clone()),
                ("upstairs".parse().expect("a valid floor id"), level),
            ]
            .into(),
        };
        let error = twice.check().expect_err("the same lamp on two floors");
        assert!(error.to_string().contains("demo_lamp"), "{error}");

        let same_floor = Floorplan {
            floors: [(
                ground(),
                Level {
                    devices: vec![lamp(), lamp()],
                    ..Level::default()
                },
            )]
            .into(),
        };
        assert!(same_floor.check().is_err(), "or twice on one floor");
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

    /// An L-shaped room is why this counts crossings rather than testing a box: the notch has
    /// to be outside the room even though it's inside its bounds.
    #[test]
    fn what_is_inside_a_room_counts_crossings() {
        let ell = area(
            "hall",
            &[
                (0, 0),
                (400, 0),
                (400, 200),
                (200, 200),
                (200, 400),
                (0, 400),
            ],
        );
        assert!(ell.contains(Point::new(100, 100)), "the top-left");
        assert!(ell.contains(Point::new(300, 100)), "the arm");
        assert!(ell.contains(Point::new(100, 300)), "the leg");
        assert!(!ell.contains(Point::new(300, 300)), "the notch is outside");
        assert!(!ell.contains(Point::new(500, 100)), "and so is the garden");
        assert!(
            !area("empty", &[]).contains(Point::new(0, 0)),
            "a shape with no corners holds nothing"
        );
    }

    #[test]
    fn a_rooms_label_goes_in_the_middle_of_it() {
        let square = area("kitchen", &[(0, 0), (400, 0), (400, 300), (0, 300)]);
        assert_eq!(square.middle(), Some(Point::new(200, 150)));
        assert_eq!(area("empty", &[]).middle(), None);
    }

    /// A label somebody moved is written next to the middle, as the offset it was dragged to; a
    /// label nobody touched is the default and doesn't clutter the file.
    #[test]
    fn a_moved_label_is_written_and_a_middle_one_is_not() {
        let moved = PlacedArea {
            label: Point::new(30, -40),
            ..area("kitchen", &[(0, 0), (400, 0), (400, 300), (0, 300)])
        };
        let json = serde_json::to_string(&moved).expect("json");
        assert!(json.contains("\"label\":[30,-40]"), "{json}");
        let back: PlacedArea = serde_json::from_str(&json).expect("json");
        assert_eq!(back.label, Point::new(30, -40));

        let at_the_middle = area("kitchen", &[(0, 0), (400, 0), (400, 300), (0, 300)]);
        let json = serde_json::to_string(&at_the_middle).expect("json");
        assert!(!json.contains("label"), "{json}");
        let from_file: PlacedArea = serde_json::from_str(
            r#"{"area":"kitchen","points":[[0,0],[400,0],[400,300],[0,300]]}"#,
        )
        .expect("an old plan, without a label");
        assert_eq!(from_file.label, Point::new(0, 0));
    }

    /// A room keeps the colour it was given, says it by name, and a room nobody coloured says
    /// nothing and wears the colour its id picks — the same one every time.
    #[test]
    fn a_room_wears_the_colour_it_was_given_or_its_own() {
        let square = &[(0, 0), (400, 0), (400, 300), (0, 300)][..];
        let plain = area("kitchen", square);
        assert!(!serde_json::to_string(&plain).expect("json").contains("tint"));
        assert_eq!(plain.shade(), Tint::of(&plain.area));
        assert_eq!(plain.shade(), area("kitchen", square).shade());

        let painted = PlacedArea {
            tint: Some(Tint::Sky),
            ..plain
        };
        let json = serde_json::to_string(&painted).expect("json");
        assert!(json.contains(r#""tint":"sky""#), "{json}");
        let back: PlacedArea = serde_json::from_str(&json).expect("json");
        assert_eq!(back.shade(), Tint::Sky);

        for tint in Tint::ALL {
            let written = serde_json::to_string(&tint).expect("json");
            assert_eq!(written, format!("\"{}\"", tint.name()));
        }
        assert!(serde_json::from_str::<Tint>(r##""#c4552b""##).is_err());
    }

    /// A door drawn before it could be told which way it swings reads as it always did, and
    /// writes nothing new; one that has been told keeps what it was told.
    #[test]
    fn an_opening_only_writes_what_was_said_about_it() {
        let plain = r#"{"kind":"door","at":200,"width":80}"#;
        let door: Opening = serde_json::from_str(plain).expect("an old door");
        assert_eq!(door, Opening::new(OpeningKind::Door, 200, 80));
        assert_eq!(
            (door.side, door.hinge, &door.sensor),
            (Side::Left, Hinge::Near, &None)
        );
        assert_eq!(serde_json::to_string(&door).expect("json"), plain);

        let told = Opening {
            side: Side::Right,
            hinge: Hinge::Far,
            sensor: Some(
                "binary_sensor.front_door_contact"
                    .parse()
                    .expect("a valid entity id"),
            ),
            ..Opening::new(OpeningKind::Door, 200, 80)
        };
        let json = serde_json::to_string(&told).expect("json");
        assert!(
            json.contains(
                r#""side":"right","hinge":"far","sensor":"binary_sensor.front_door_contact""#
            ),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Opening>(&json).expect("json"), told);
    }

    /// A radar is aimed somewhere on the compass and sees a sensible width, and a plan drawn
    /// before either could be said still loads and still writes the same file.
    #[test]
    fn a_device_faces_a_real_direction_and_sees_a_sensible_width() {
        let aimed = |facing, field_of_view| {
            on_ground(Level {
                devices: vec![PlacedDevice {
                    facing,
                    field_of_view,
                    ..PlacedDevice::new(
                        "demo_mmwave".parse().expect("a valid device id"),
                        Point::new(0, 0),
                    )
                }],
                ..Level::default()
            })
            .check()
        };
        assert!(aimed(Some(0), None).is_ok());
        assert!(aimed(Some(359), Some(20)).is_ok());
        assert!(aimed(Some(90), Some(180)).is_ok());
        assert!(
            aimed(None, Some(100)).is_ok(),
            "a width with nowhere to point is only unused"
        );
        let error = aimed(Some(400), None).expect_err("more than a full turn");
        assert!(error.to_string().contains("demo_mmwave"), "{error}");
        assert!(aimed(Some(360), None).is_err(), "a full turn is 0");
        assert!(aimed(Some(90), Some(5)).is_err(), "a line, not a field");
        assert!(aimed(Some(90), Some(181)).is_err(), "through its own wall");

        let old: PlacedDevice =
            serde_json::from_str(r#"{"device":"demo_lamp","at":[120,90]}"#).expect("an old plan");
        assert_eq!(old.facing, None);
        assert_eq!(old.view_angle(), PlacedDevice::DEFAULT_FIELD_OF_VIEW);
        assert_eq!(
            serde_json::to_string(&old).expect("json"),
            r#"{"device":"demo_lamp","at":[120,90]}"#,
            "and nothing new is written for it"
        );
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
