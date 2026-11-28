//! What the printer last said, kept as one picture.
//!
//! Some printers send every value in each report; others send only what changed since the last
//! one, and the whole picture only when asked (`pushall`). So a report is never read on its
//! own: [`Status::merge`] lays it over what is already known, and a value a report leaves out
//! keeps what it had.
//!
//! Everything here came off the network, so everything is bounded: numbers have to be in a
//! plausible range, text is cut to [`MOST_TEXT`] characters, and no more than [`MOST_UNITS`]
//! filament units of [`MOST_SLOTS`] slots each are kept. A value of the wrong type or out of
//! range is passed over, never trusted.

use std::collections::BTreeMap;

use serde_json::Value;

/// The most filament units kept: four with four slots and eight with one, as the public client
/// for these printers counts them.
pub const MOST_UNITS: usize = 12;
/// The most slots kept in one unit. Their ids run from 0.
pub const MOST_SLOTS: u8 = 16;
/// The most characters kept of any text.
pub const MOST_TEXT: usize = 64;

/// What the printer is doing, from `gcode_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Preparing,
    Printing,
    Paused,
    Finished,
    Failed,
}

impl Stage {
    pub const ALL: [Stage; 6] = [
        Stage::Idle,
        Stage::Preparing,
        Stage::Printing,
        Stage::Paused,
        Stage::Finished,
        Stage::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Idle => "idle",
            Stage::Preparing => "preparing",
            Stage::Printing => "printing",
            Stage::Paused => "paused",
            Stage::Finished => "finished",
            Stage::Failed => "failed",
        }
    }

    /// `None` for a state this doesn't know, which then shows as unknown instead of as the
    /// last state it did know.
    fn read(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().as_str() {
            "idle" => Stage::Idle,
            "prepare" | "slicing" => Stage::Preparing,
            "running" => Stage::Printing,
            "pause" => Stage::Paused,
            "finish" => Stage::Finished,
            "failed" => Stage::Failed,
            _ => return None,
        })
    }
}

/// One slot of a filament unit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Slot {
    /// Nothing is in it.
    pub empty: bool,
    /// The filament's type, such as `PLA`, when the printer knows it.
    pub material: Option<String>,
    /// The filament's colour as `#RRGGBB`.
    pub color: Option<String>,
    /// How much is left, 0 to 100, when the printer can tell.
    pub remaining: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub stage: Option<Stage>,
    /// 0 to 100.
    pub progress: Option<u8>,
    pub remaining_minutes: Option<u32>,
    pub layer: Option<u32>,
    pub total_layers: Option<u32>,
    /// °C.
    pub nozzle: Option<f64>,
    /// °C.
    pub bed: Option<f64>,
    /// Filament units by their id, each with its slots by theirs.
    pub units: BTreeMap<u8, BTreeMap<u8, Slot>>,
}

impl Status {
    /// Lays one message from the printer over what is known. False when it wasn't a status
    /// report (an answer to a command, something unreadable), which changes nothing.
    pub fn merge(&mut self, payload: &[u8]) -> bool {
        let Ok(message) = serde_json::from_slice::<Value>(payload) else {
            return false;
        };
        let Some(print) = message.get("print").and_then(Value::as_object) else {
            return false;
        };
        if print.get("command").and_then(Value::as_str) != Some("push_status") {
            return false;
        }
        if let Some(state) = print.get("gcode_state").and_then(Value::as_str) {
            self.stage = Stage::read(state);
        }
        let whole = |key: &str, most: f64| {
            number(print.get(key)?)
                .filter(|n| (0.0..=most).contains(n))
                .map(|n| n as u32)
        };
        if let Some(percent) = whole("mc_percent", 100.0) {
            self.progress = Some(percent as u8);
        }
        // Minutes. A print of more than 60 days isn't believed.
        if let Some(minutes) = whole("mc_remaining_time", 86_400.0) {
            self.remaining_minutes = Some(minutes);
        }
        if let Some(layer) = whole("layer_num", 1_000_000.0) {
            self.layer = Some(layer);
        }
        if let Some(layers) = whole("total_layer_num", 1_000_000.0) {
            self.total_layers = Some(layers);
        }
        let degrees = |key: &str| number(print.get(key)?).filter(|n| (-50.0..=500.0).contains(n));
        if let Some(nozzle) = degrees("nozzle_temper") {
            self.nozzle = Some(nozzle);
        }
        if let Some(bed) = degrees("bed_temper") {
            self.bed = Some(bed);
        }
        let units = print
            .get("ams")
            .and_then(|ams| ams.get("ams"))
            .and_then(Value::as_array);
        for unit in units.into_iter().flatten().take(MOST_UNITS) {
            self.merge_unit(unit);
        }
        true
    }

    fn merge_unit(&mut self, unit: &Value) {
        let Some(id) = unit.get("id").and_then(small) else {
            return;
        };
        if !self.units.contains_key(&id) && self.units.len() >= MOST_UNITS {
            return;
        }
        let slots = self.units.entry(id).or_default();
        let reported = unit.get("tray").and_then(Value::as_array);
        for tray in reported.into_iter().flatten().take(usize::from(MOST_SLOTS)) {
            let Some(tray) = tray.as_object() else {
                continue;
            };
            let Some(id) = tray.get("id").and_then(small).filter(|id| *id < MOST_SLOTS) else {
                continue;
            };
            let slot = slots.entry(id).or_default();
            // A slot with nothing in it is reported as its id alone.
            if tray.len() == 1 {
                *slot = Slot {
                    empty: true,
                    ..Slot::default()
                };
                continue;
            }
            slot.empty = false;
            if let Some(material) = tray.get("tray_type").and_then(Value::as_str) {
                slot.material = Some(cut(material)).filter(|material| !material.is_empty());
            }
            if let Some(color) = tray.get("tray_color").and_then(Value::as_str) {
                slot.color = rgb(color);
            }
            // Below 0 is how the printer says it can't tell.
            if let Some(left) = tray.get("remain").and_then(number) {
                slot.remaining = Some(left)
                    .filter(|n| (0.0..=100.0).contains(n))
                    .map(|n| n as u8);
            }
        }
    }
}

/// A number, however the printer wrote it: `4`, `22.7`, `"4"`, `"22.7"`.
fn number(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) if text.len() <= 16 => text.trim().parse().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// An id: a whole number from 0 to 255.
fn small(value: &Value) -> Option<u8> {
    number(value)
        .filter(|n| n.fract() == 0.0 && (0.0..=255.0).contains(n))
        .map(|n| n as u8)
}

/// Text off the network: no control characters, and no longer than [`MOST_TEXT`].
fn cut(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MOST_TEXT)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `RRGGBBAA` as the printer sends it, to `#RRGGBB`.
fn rgb(color: &str) -> Option<String> {
    (color.len() == 8 && color.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{}", color[..6].to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(print: Value) -> Vec<u8> {
        let mut print = print;
        print["command"] = json!("push_status");
        json!({ "print": print }).to_string().into_bytes()
    }

    /// A whole picture: a print under way, and one unit with two filaments and an empty slot.
    fn full() -> Status {
        let mut status = Status::default();
        assert!(status.merge(&report(json!({
            "gcode_state": "RUNNING", "mc_percent": 40, "mc_remaining_time": 75,
            "layer_num": 12, "total_layer_num": 120,
            "nozzle_temper": 219.5, "bed_temper": 60.0,
            "ams": { "ams": [{ "id": "0", "tray": [
                { "id": "0", "tray_type": "PLA", "tray_color": "00ae42ff", "remain": 80 },
                { "id": "1", "tray_type": "PETG", "tray_color": "FFFFFFFF", "remain": -1 },
                { "id": "2" },
            ]}]},
        }))));
        status
    }

    #[test]
    fn a_full_report_is_read() {
        let status = full();
        assert_eq!(status.stage, Some(Stage::Printing));
        assert_eq!(status.progress, Some(40));
        assert_eq!(status.remaining_minutes, Some(75));
        assert_eq!((status.layer, status.total_layers), (Some(12), Some(120)));
        assert_eq!((status.nozzle, status.bed), (Some(219.5), Some(60.0)));
        let slots = &status.units[&0];
        assert_eq!(slots[&0].material.as_deref(), Some("PLA"));
        assert_eq!(slots[&0].color.as_deref(), Some("#00AE42"));
        assert_eq!(slots[&0].remaining, Some(80));
        assert_eq!(slots[&1].remaining, None);
        assert!(slots[&2].empty && !slots[&0].empty);
    }

    #[test]
    fn a_partial_report_changes_only_what_it_names() {
        let mut status = full();
        let before = status.clone();
        assert!(status.merge(&report(json!({ "mc_percent": 41, "nozzle_temper": 220.0 }))));
        assert_eq!(status.progress, Some(41));
        assert_eq!(status.nozzle, Some(220.0));
        let untouched = Status {
            progress: before.progress,
            nozzle: before.nozzle,
            ..status.clone()
        };
        assert_eq!(untouched, before);
    }

    #[test]
    fn a_report_of_one_slot_keeps_the_others() {
        let mut status = full();
        let before = status.clone();
        assert!(status.merge(&report(json!({
            "ams": { "ams": [{ "id": "0", "tray": [{ "id": "0", "remain": 79 }] }] }
        }))));
        let slots = &status.units[&0];
        assert_eq!(slots[&0].remaining, Some(79));
        assert_eq!(slots[&0].material.as_deref(), Some("PLA"));
        assert_eq!(slots[&1], before.units[&0][&1]);
        assert_eq!(slots[&2], before.units[&0][&2]);
        // And a slot that is emptied loses what it held.
        assert!(status.merge(&report(json!({
            "ams": { "ams": [{ "id": "0", "tray": [{ "id": "1" }] }] }
        }))));
        let emptied = Slot {
            empty: true,
            ..Slot::default()
        };
        assert_eq!(status.units[&0][&1], emptied);
    }

    #[test]
    fn numbers_written_as_text_are_read_and_nonsense_is_passed_over() {
        let mut status = full();
        let before = status.clone();
        assert!(status.merge(&report(json!({
            "mc_percent": "55", "layer_num": "13", "bed_temper": "61.5"
        }))));
        assert_eq!(status.progress, Some(55));
        assert_eq!(status.layer, Some(13));
        assert_eq!(status.bed, Some(61.5));
        let mut status = before.clone();
        assert!(status.merge(&report(json!({
            "mc_percent": 250, "mc_remaining_time": -1, "layer_num": null,
            "total_layer_num": [1], "nozzle_temper": 9000, "bed_temper": "NaN",
            "ams": { "ams": [{ "id": "0", "tray": [{ "id": 99, "tray_type": "PLA" }] }] },
        }))));
        assert_eq!(status, before);
    }

    #[test]
    fn a_state_it_does_not_know_shows_as_unknown() {
        let mut status = full();
        assert!(status.merge(&report(json!({ "gcode_state": "SOMETHING_NEW" }))));
        assert_eq!(status.stage, None);
        for (said, stage) in [
            ("IDLE", Stage::Idle),
            ("PREPARE", Stage::Preparing),
            ("PAUSE", Stage::Paused),
            ("FINISH", Stage::Finished),
            ("FAILED", Stage::Failed),
        ] {
            assert!(status.merge(&report(json!({ "gcode_state": said }))));
            assert_eq!(status.stage, Some(stage));
        }
    }

    #[test]
    fn what_is_not_a_status_report_changes_nothing() {
        let mut status = full();
        let before = status.clone();
        assert!(!status.merge(b"not json"));
        assert!(!status.merge(br#"{"print":{"command":"pause","mc_percent":1}}"#));
        assert!(!status.merge(br#"{"info":{"command":"get_version"}}"#));
        assert_eq!(status, before);
    }

    #[test]
    fn units_slots_and_text_are_bounded() {
        let mut status = Status::default();
        let units: Vec<Value> = (0..40)
            .map(|unit| {
                let trays: Vec<Value> = (0..40)
                    .map(|tray| json!({ "id": tray, "tray_type": "X".repeat(5000) }))
                    .collect();
                json!({ "id": unit, "tray": trays })
            })
            .collect();
        assert!(status.merge(&report(json!({ "ams": { "ams": units } }))));
        assert_eq!(status.units.len(), MOST_UNITS);
        // A second report can't add the units the first one was refused.
        assert!(status.merge(&report(
            json!({ "ams": { "ams": [{ "id": 200, "tray": [] }] } })
        )));
        assert_eq!(status.units.len(), MOST_UNITS);
        for slots in status.units.values() {
            assert_eq!(slots.len(), usize::from(MOST_SLOTS));
            for slot in slots.values() {
                assert_eq!(slot.material.as_ref().map(String::len), Some(MOST_TEXT));
            }
        }
    }
}
