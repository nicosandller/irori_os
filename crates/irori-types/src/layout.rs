//! Floors and rooms an extension proposes (`docs/specs/protocols.md` §6.9).
//!
//! A bridge to another home system knows how that home is divided: Home Assistant has its own
//! floors and areas. The extension says so with `suggest_layout`, the core keeps the latest
//! proposal while it runs, and the owner can bring the floors and rooms Irori doesn't have yet
//! over in one step. Nothing is made until they do.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Name, Settings};

/// The most floors and rooms one proposal keeps; anything past them is dropped. Far more than
/// a home has, so a runaway list can't grow the core's memory.
pub const MAX_LAYOUT_FLOORS: usize = 256;
pub const MAX_LAYOUT_AREAS: usize = 1024;

/// The floors and rooms an extension proposes, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestedLayout {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub floors: Vec<SuggestedFloor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<SuggestedArea>,
}

/// A floor an extension proposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestedFloor {
    pub name: Name,
    /// 0 is the entrance level, negative is below ground. Absent is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<i8>,
}

/// A room an extension proposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestedArea {
    pub name: Name,
    /// The name of the floor it is on: one in the same proposal, or one Irori already has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<Name>,
}

/// How two names are compared: trimmed, ignoring case, as `Settings::area_named` does.
pub fn same_name(a: &Name, b: &Name) -> bool {
    key(a) == key(b)
}

fn key(name: &Name) -> String {
    name.as_str().trim().to_lowercase()
}

impl SuggestedLayout {
    pub fn is_empty(&self) -> bool {
        self.floors.is_empty() && self.areas.is_empty()
    }

    /// The proposal as the core keeps it: a name said twice is kept once (the first time it is
    /// said), and the lists stop at [`MAX_LAYOUT_FLOORS`] and [`MAX_LAYOUT_AREAS`]. One pass over
    /// each list: a message can repeat a name hundreds of thousands of times, and this runs on
    /// one of the core's worker threads.
    #[must_use]
    pub fn tidied(self) -> SuggestedLayout {
        let mut seen = HashSet::new();
        let mut floors = Vec::new();
        for floor in self.floors {
            if floors.len() == MAX_LAYOUT_FLOORS {
                break;
            }
            if seen.insert(key(&floor.name)) {
                floors.push(floor);
            }
        }
        let mut seen = HashSet::new();
        let mut areas = Vec::new();
        for area in self.areas {
            if areas.len() == MAX_LAYOUT_AREAS {
                break;
            }
            if seen.insert(key(&area.name)) {
                areas.push(area);
            }
        }
        SuggestedLayout { floors, areas }
    }

    /// Only what `settings` doesn't have yet: floors and rooms whose names aren't taken. A room
    /// keeps the name of its floor whether or not that floor is new.
    #[must_use]
    pub fn missing_from(&self, settings: &Settings) -> SuggestedLayout {
        let floors: HashSet<String> = settings.floors.iter().map(|f| key(&f.name)).collect();
        let areas: HashSet<String> = settings.areas.iter().map(|a| key(&a.name)).collect();
        SuggestedLayout {
            floors: self
                .floors
                .iter()
                .filter(|floor| !floors.contains(&key(&floor.name)))
                .cloned()
                .collect(),
            areas: self
                .areas
                .iter()
                .filter(|area| !areas.contains(&key(&area.name)))
                .cloned()
                .collect(),
        }
    }

    /// Only what is also in `shown`, the proposal the owner was looking at when they accepted:
    /// the same floor at the same level, the same room on the same floor. Anything the extension
    /// added or changed since then waits until the owner has seen it.
    #[must_use]
    pub fn only_shown(&self, shown: &SuggestedLayout) -> SuggestedLayout {
        let floors: HashSet<_> = shown.floors.iter().map(floor_key).collect();
        let areas: HashSet<_> = shown.areas.iter().map(area_key).collect();
        SuggestedLayout {
            floors: self
                .floors
                .iter()
                .filter(|floor| floors.contains(&floor_key(floor)))
                .cloned()
                .collect(),
            areas: self
                .areas
                .iter()
                .filter(|area| areas.contains(&area_key(area)))
                .cloned()
                .collect(),
        }
    }
}

fn floor_key(floor: &SuggestedFloor) -> (String, i8) {
    (key(&floor.name), floor.level.unwrap_or(0))
}

fn area_key(area: &SuggestedArea) -> (String, Option<String>) {
    (key(&area.name), area.floor.as_ref().map(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Area, Floor};

    fn name(text: &str) -> Name {
        text.parse().expect("a name")
    }

    fn floor(text: &str) -> SuggestedFloor {
        SuggestedFloor {
            name: name(text),
            level: None,
        }
    }

    fn area(text: &str, on: Option<&str>) -> SuggestedArea {
        SuggestedArea {
            name: name(text),
            floor: on.map(name),
        }
    }

    #[test]
    fn a_name_said_twice_is_kept_once() {
        let layout = SuggestedLayout {
            floors: vec![floor("Ground"), floor("ground"), floor("Upstairs")],
            areas: vec![
                area("Kitchen", Some("Ground")),
                area("KITCHEN", Some("Upstairs")),
                area("Office", None),
            ],
        }
        .tidied();
        assert_eq!(layout.floors, vec![floor("Ground"), floor("Upstairs")]);
        assert_eq!(
            layout.areas,
            vec![area("Kitchen", Some("Ground")), area("Office", None)]
        );
    }

    #[test]
    fn a_runaway_list_is_cut_short() {
        let layout = SuggestedLayout {
            floors: (0..MAX_LAYOUT_FLOORS + 10)
                .map(|n| floor(&format!("Floor {n}")))
                .collect(),
            areas: (0..MAX_LAYOUT_AREAS + 10)
                .map(|n| area(&format!("Room {n}"), None))
                .collect(),
        }
        .tidied();
        assert_eq!(layout.floors.len(), MAX_LAYOUT_FLOORS);
        assert_eq!(layout.areas.len(), MAX_LAYOUT_AREAS);
    }

    #[test]
    fn names_irori_has_are_not_proposed_again() {
        let settings = Settings {
            floors: vec![Floor {
                id: "ground".parse().expect("an id"),
                name: name("Ground"),
                level: 0,
            }],
            areas: vec![Area {
                id: "kitchen".parse().expect("an id"),
                name: name("Kitchen"),
                floor_id: None,
            }],
            ..Settings::default()
        };
        let layout = SuggestedLayout {
            floors: vec![floor("GROUND"), floor("Upstairs")],
            areas: vec![
                area("kitchen", Some("Ground")),
                area("Office", Some("Upstairs")),
            ],
        };
        let missing = layout.missing_from(&settings);
        assert_eq!(missing.floors, vec![floor("Upstairs")]);
        assert_eq!(missing.areas, vec![area("Office", Some("Upstairs"))]);
        assert!(
            SuggestedLayout::default()
                .missing_from(&settings)
                .is_empty()
        );
    }

    #[test]
    fn a_list_of_repeats_is_read_to_its_end_and_kept_once() {
        let layout = SuggestedLayout {
            floors: Vec::new(),
            areas: (0..50_000)
                .map(|n| area(if n % 2 == 0 { "Hall" } else { "HALL" }, None))
                .chain([area("Office", None)])
                .collect(),
        }
        .tidied();
        assert_eq!(layout.areas, vec![area("Hall", None), area("Office", None)]);
    }

    #[test]
    fn only_what_the_owner_was_shown_is_kept() {
        let now = SuggestedLayout {
            floors: vec![
                SuggestedFloor {
                    name: name("Ground"),
                    level: None,
                },
                SuggestedFloor {
                    name: name("Upstairs"),
                    level: Some(1),
                },
            ],
            areas: vec![
                area("Kitchen", Some("Ground")),
                area("Bedroom", Some("Upstairs")),
                area("Added since", None),
            ],
        };
        let shown = SuggestedLayout {
            floors: vec![
                // Left out is level 0, and case doesn't matter.
                SuggestedFloor {
                    name: name("GROUND"),
                    level: Some(0),
                },
                // It has moved since.
                SuggestedFloor {
                    name: name("Upstairs"),
                    level: Some(2),
                },
                floor("Gone since"),
            ],
            areas: vec![
                area("kitchen", Some("ground")),
                // On another floor when they saw it.
                area("Bedroom", Some("Ground")),
            ],
        };
        let kept = now.only_shown(&shown);
        assert_eq!(kept.floors, vec![now.floors[0].clone()]);
        assert_eq!(kept.areas, vec![area("Kitchen", Some("Ground"))]);
        assert!(now.only_shown(&SuggestedLayout::default()).is_empty());
    }

    #[test]
    fn it_reads_the_wire_shape() {
        let layout: SuggestedLayout = serde_json::from_str(
            r#"{"floors":[{"name":"Ground","level":0}],"areas":[{"name":"Kitchen","floor":"Ground"},{"name":"Hall"}]}"#,
        )
        .expect("it reads");
        assert_eq!(layout.floors[0].level, Some(0));
        assert_eq!(layout.areas[1].floor, None);
        assert!(serde_json::from_str::<SuggestedLayout>(r#"{"areas":[{"name":""}]}"#).is_err());
    }
}
