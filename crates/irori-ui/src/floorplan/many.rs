//! Several things picked up at once: a box dragged round part of the plan, and everything
//! wholly inside it moved or taken away together.
//!
//! One thing at a time is right for drawing and wrong for rearranging: a wing of the house that
//! is a metre out, or a room drawn on the wrong floor, is a dozen walls that all want the same
//! thing done to them. What is in the box is what is *wholly* in it — a wall with one end
//! outside is a wall the box only crosses — so the edge of a selection is never a guess.

use std::collections::BTreeSet;

use irori_types::{Level, Point};

/// What is picked up together, as places in the floor's own lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Group {
    pub walls: BTreeSet<usize>,
    pub areas: BTreeSet<usize>,
    pub devices: BTreeSet<usize>,
}

/// A box on the plan, in its own centimetres: two opposite corners, either way round.
pub(super) type Span = ((f64, f64), (f64, f64));

impl Group {
    pub(super) fn is_empty(&self) -> bool {
        self.walls.is_empty() && self.areas.is_empty() && self.devices.is_empty()
    }

    /// Everything on the floor that lies wholly inside a box.
    pub(super) fn inside(level: &Level, around: Span) -> Self {
        let (low, high) = (
            (around.0.0.min(around.1.0), around.0.1.min(around.1.1)),
            (around.0.0.max(around.1.0), around.0.1.max(around.1.1)),
        );
        let within = |point: Point| {
            let (x, y) = (f64::from(point.x), f64::from(point.y));
            x >= low.0 && x <= high.0 && y >= low.1 && y <= high.1
        };
        Self {
            walls: level
                .walls
                .iter()
                .enumerate()
                .filter(|(_, wall)| within(wall.from) && within(wall.to))
                .map(|(index, _)| index)
                .collect(),
            areas: level
                .areas
                .iter()
                .enumerate()
                .filter(|(_, placed)| placed.points.iter().all(|point| within(*point)))
                .map(|(index, _)| index)
                .collect(),
            devices: level
                .devices
                .iter()
                .enumerate()
                .filter(|(_, placed)| within(placed.at))
                .map(|(index, _)| index)
                .collect(),
        }
    }

    /// Every point the group owns, for finding where it is.
    fn points(&self, level: &Level) -> Vec<Point> {
        let walls = self
            .walls
            .iter()
            .filter_map(|index| level.walls.get(*index))
            .flat_map(|wall| [wall.from, wall.to]);
        let areas = self
            .areas
            .iter()
            .filter_map(|index| level.areas.get(*index))
            .flat_map(|placed| placed.points.iter().copied());
        let devices = self
            .devices
            .iter()
            .filter_map(|index| level.devices.get(*index))
            .map(|placed| placed.at);
        walls.chain(areas).chain(devices).collect()
    }

    /// The box the group fills, to draw round it and to take hold of it by.
    pub(super) fn bounds(&self, level: &Level) -> Option<Span> {
        let points = self.points(level);
        let first = points.first()?;
        let start = (f64::from(first.x), f64::from(first.y));
        Some(points.iter().fold((start, start), |(low, high), point| {
            let (x, y) = (f64::from(point.x), f64::from(point.y));
            ((low.0.min(x), low.1.min(y)), (high.0.max(x), high.1.max(y)))
        }))
    }

    /// Whether a place on the plan is on the group: inside the box it fills, with a little
    /// room round it so a single wall — a box with no width — can still be taken hold of.
    pub(super) fn holds(&self, level: &Level, world: (f64, f64), reach: f64) -> bool {
        self.bounds(level).is_some_and(|(low, high)| {
            world.0 >= low.0 - reach
                && world.0 <= high.0 + reach
                && world.1 >= low.1 - reach
                && world.1 <= high.1 + reach
        })
    }

    /// Moves everything in the group by the same amount. Only what is in it: a wall outside
    /// that shared a corner with one inside is left where it was, which is what moving part of
    /// a plan away from the rest means. A door goes with its wall, as it always does.
    pub(super) fn shift(&self, level: &mut Level, by: (i32, i32)) {
        let moved = |point: &mut Point| {
            *point = Point::new(point.x.saturating_add(by.0), point.y.saturating_add(by.1))
        };
        for index in &self.walls {
            if let Some(wall) = level.walls.get_mut(*index) {
                moved(&mut wall.from);
                moved(&mut wall.to);
            }
        }
        for index in &self.areas {
            if let Some(placed) = level.areas.get_mut(*index) {
                placed.points.iter_mut().for_each(moved);
            }
        }
        for index in &self.devices {
            if let Some(placed) = level.devices.get_mut(*index) {
                moved(&mut placed.at);
            }
        }
    }

    /// Takes everything in the group off the floor.
    pub(super) fn remove(&self, level: &mut Level) {
        // From the far end back, so taking one away doesn't renumber the ones still to go.
        for index in self.walls.iter().rev() {
            if *index < level.walls.len() {
                level.walls.remove(*index);
            }
        }
        for index in self.areas.iter().rev() {
            if *index < level.areas.len() {
                level.areas.remove(*index);
            }
        }
        for index in self.devices.iter().rev() {
            if *index < level.devices.len() {
                level.devices.remove(*index);
            }
        }
    }

    /// What is picked up, in words: "3 walls, 1 room and 2 devices".
    pub(super) fn said(&self) -> String {
        let count = |n: usize, one: &str, many: &str| match n {
            0 => None,
            1 => Some(format!("1 {one}")),
            n => Some(format!("{n} {many}")),
        };
        let parts: Vec<String> = [
            count(self.walls.len(), "wall", "walls"),
            count(self.areas.len(), "room", "rooms"),
            count(self.devices.len(), "device", "devices"),
        ]
        .into_iter()
        .flatten()
        .collect();
        match parts.as_slice() {
            [] => "nothing".to_owned(),
            [one] => one.clone(),
            [most @ .., last] => format!("{} and {last}", most.join(", ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{Opening, OpeningKind, PlacedArea, PlacedDevice, Wall};

    fn level() -> Level {
        let wall = |from: (i32, i32), to: (i32, i32)| {
            Wall::new(Point::new(from.0, from.1), Point::new(to.0, to.1))
        };
        let mut door = wall((0, 0), (400, 0));
        door.openings.push(Opening::new(OpeningKind::Door, 200, 80));
        Level {
            walls: vec![
                door,
                wall((400, 0), (400, 300)),
                wall((400, 300), (900, 300)),
            ],
            areas: vec![PlacedArea {
                area: "kitchen".parse().expect("an area id"),
                points: [(0, 0), (400, 0), (400, 300), (0, 300)]
                    .into_iter()
                    .map(|(x, y)| Point::new(x, y))
                    .collect(),
                label: Point::new(0, 0),
                tint: None,
            }],
            devices: vec![
                PlacedDevice::new("lamp".parse().expect("a device id"), Point::new(120, 90)),
                PlacedDevice::new("radar".parse().expect("a device id"), Point::new(700, 200)),
            ],
        }
    }

    /// The box takes what is wholly inside it, dragged from whichever corner.
    #[test]
    fn a_box_picks_up_what_is_wholly_inside_it() {
        let level = level();
        let group = Group::inside(&level, ((450.0, 350.0), (-50.0, -50.0)));
        assert_eq!(group.walls, [0, 1].into());
        assert_eq!(group.areas, [0].into());
        assert_eq!(group.devices, [0].into());
        assert_eq!(group.said(), "2 walls, 1 room and 1 device");
        assert_eq!(group.bounds(&level), Some(((0.0, 0.0), (400.0, 300.0))));

        // A wall the box only crosses is not in it.
        let crossing = Group::inside(&level, ((300.0, -50.0), (500.0, 350.0)));
        assert_eq!(crossing.walls, [1].into());
        assert!(crossing.areas.is_empty());
        assert_eq!(crossing.said(), "1 wall");
        assert!(Group::inside(&level, ((2000.0, 2000.0), (2100.0, 2100.0))).is_empty());
    }

    #[test]
    fn what_is_picked_up_moves_together_and_the_rest_stays() {
        let mut level = level();
        let group = Group::inside(&level, ((-50.0, -50.0), (450.0, 350.0)));
        group.shift(&mut level, (100, -50));
        assert_eq!(level.walls[0].from, Point::new(100, -50));
        assert_eq!(level.walls[1].to, Point::new(500, 250));
        assert_eq!(
            level.walls[0].openings[0].at, 200,
            "the door goes with its wall"
        );
        assert_eq!(level.areas[0].points[2], Point::new(500, 250));
        assert_eq!(level.devices[0].at, Point::new(220, 40));
        // The wall that shared a corner but wasn't in the box, and the device outside it.
        assert_eq!(level.walls[2].from, Point::new(400, 300));
        assert_eq!(level.devices[1].at, Point::new(700, 200));
        assert_eq!(group.bounds(&level), Some(((100.0, -50.0), (500.0, 250.0))));
    }

    #[test]
    fn what_is_picked_up_is_taken_away_together() {
        let mut level = level();
        let group = Group::inside(&level, ((-50.0, -50.0), (450.0, 350.0)));
        group.remove(&mut level);
        assert_eq!(level.walls.len(), 1);
        assert_eq!(level.walls[0].to, Point::new(900, 300));
        assert!(level.areas.is_empty());
        assert_eq!(level.devices.len(), 1);
        assert_eq!(level.devices[0].at, Point::new(700, 200));
    }

    /// A single wall fills a box with no width, and can still be taken hold of.
    #[test]
    fn a_group_is_held_by_the_box_it_fills() {
        let level = level();
        let one = Group::inside(&level, ((350.0, -10.0), (450.0, 310.0)));
        assert_eq!(one.walls, [1].into());
        assert!(one.holds(&level, (405.0, 150.0), 10.0));
        assert!(!one.holds(&level, (430.0, 150.0), 10.0));
        assert!(!Group::default().holds(&level, (0.0, 0.0), 10.0));
    }
}
