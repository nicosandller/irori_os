//! The snapshot handed to the model with each turn. It is rebuilt every time, so a lamp that
//! changed since the last message is what the model sees now, and it is not stored as if the
//! person had typed it.

/// One line of a home or a device: where, what, and the word it is reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefLine {
    /// The floor the area is on. Empty when the area has none, or the device has no area.
    pub floor: String,
    pub area: String,
    pub name: String,
    pub entity: String,
    pub value: String,
}

const HOME_CAP: usize = 6_000;

/// A short picture of the whole home. Past the cap, the rest is counted rather than pasted, so
/// a large house still fits in front of a small model.
pub fn home_brief(lines: &[BriefLine]) -> String {
    let mut out = String::from(
        "This is the person's home, as Irori has it right now. Answer about this home. \
         You can look up one device with tools when the line here is not enough.\n",
    );
    let mut written = 0usize;
    for line in lines {
        let row = format!(
            "- {}{} — {} ({}) — {}\n",
            if line.floor.is_empty() {
                String::new()
            } else {
                format!("{}, ", line.floor)
            },
            if line.area.is_empty() {
                "no room"
            } else {
                &line.area
            },
            line.name,
            line.entity,
            line.value
        );
        if out.len() + row.len() > HOME_CAP {
            break;
        }
        out.push_str(&row);
        written += 1;
    }
    if written < lines.len() {
        out.push_str(&format!(
            "And {} more, not listed here.\n",
            lines.len() - written
        ));
    }
    out
}

/// Everything Irori knows about one device, and not the rest of the house.
pub fn device_brief(device: &str, area: &str, lines: &[BriefLine]) -> String {
    let mut out = format!("The person is asking about the device \"{device}\"");
    if !area.is_empty() {
        out.push_str(&format!(" in {area}"));
    }
    out.push_str(".\n");
    if lines.is_empty() {
        out.push_str("Irori has no entities for it yet.\n");
        return out;
    }
    for line in lines {
        out.push_str(&format!("- {} — {}\n", line.entity, line.value));
    }
    out
}

/// One area as Settings shows it: where it is, and what was put in it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Place {
    /// Empty for an area on no floor.
    pub floor: String,
    /// Empty for the devices that are in no area.
    pub area: String,
    pub devices: Vec<String>,
}

/// One extension, with the settings a person may read. A secret is never among them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtensionLine {
    pub name: String,
    /// `running`, or what is wrong with it.
    pub state: String,
    pub version: String,
    pub settings: Vec<(String, String)>,
}

/// What the Settings page knows, gathered by the binary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingsPicture {
    /// Sentences about the instance and the machine it runs on.
    pub system: Vec<String>,
    /// Sentences about the assistant's own configuration. Never the key.
    pub assistant: Vec<String>,
    /// Every floor by name, lowest first, so one with no areas is still known.
    pub floors: Vec<String>,
    pub places: Vec<Place>,
    pub extensions: Vec<ExtensionLine>,
    /// Lines, warnings and errors in Irori's own log.
    pub log_counts: (usize, usize, usize),
    /// The newest warnings and errors, oldest first.
    pub log_tail: Vec<String>,
}

/// How much of the newest trouble in the log is pasted in, with room and without.
const LOG_TAIL: usize = 12;
const LOG_TAIL_TIGHT: usize = 5;
/// The longest a pasted log line gets.
const LOG_LINE: usize = 240;

/// Settings in words, inside `budget` characters. What gives way first is what matters least
/// to a question about settings: extensions' own settings, then devices by name (they are
/// counted instead), then the older lines of the log.
pub fn settings_brief(picture: &SettingsPicture, budget: usize) -> String {
    let mut out = String::new();
    for tight in 0..=3 {
        out = settings_at(picture, tight);
        if out.len() <= budget {
            return out;
        }
    }
    cut(&out, budget)
}

fn settings_at(picture: &SettingsPicture, tight: u8) -> String {
    let mut out = String::from(
        "The person is on Irori's Settings page and is asking about Irori itself: its settings, \
         the machine, floors and areas, extensions, and its log. This is what Settings holds \
         right now.\n",
    );
    out.push_str("\nSystem:\n");
    for line in &picture.system {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str(
        "\nAppearance:\n- Motion (animations) is remembered by each browser, so Irori cannot \
         see whether it is on.\n",
    );
    out.push_str("\nAssistant:\n");
    for line in &picture.assistant {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str("\nFloors and areas, with the devices assigned to each area:\n");
    if picture.floors.is_empty() && picture.places.is_empty() {
        out.push_str("- None yet.\n");
    }
    let listed = |out: &mut String, place: &Place| {
        let name = if place.area.is_empty() {
            "Devices in no area"
        } else {
            &place.area
        };
        let devices = match (place.devices.len(), tight >= 2) {
            (0, _) => "no devices".to_owned(),
            (1, true) => "1 device".to_owned(),
            (n, true) => format!("{n} devices"),
            (_, false) => place.devices.join(", "),
        };
        out.push_str(&format!("  - {name}: {devices}\n"));
    };
    for floor in &picture.floors {
        out.push_str(&format!("- Floor \"{floor}\":\n"));
        let mut any = false;
        for place in picture.places.iter().filter(|place| &place.floor == floor) {
            any = true;
            listed(&mut out, place);
        }
        if !any {
            out.push_str("  - no areas\n");
        }
    }
    let loose: Vec<&Place> = picture
        .places
        .iter()
        .filter(|place| place.floor.is_empty())
        .collect();
    if !loose.is_empty() {
        out.push_str("- On no floor:\n");
        for place in loose {
            listed(&mut out, place);
        }
    }
    out.push_str("\nUsers:\n- None yet. Anyone who can reach Irori can use it.\n");
    out.push_str("\nExtensions:\n");
    if picture.extensions.is_empty() {
        out.push_str("- None installed.\n");
    }
    for extension in &picture.extensions {
        out.push_str(&format!(
            "- {} {} — {}\n",
            extension.name, extension.version, extension.state
        ));
        if tight == 0 {
            for (key, value) in &extension.settings {
                out.push_str(&format!("  - {key}: {}\n", cut(value, 120)));
            }
        }
    }
    let (lines, warnings, errors) = picture.log_counts;
    out.push_str(&format!(
        "\nLogs: Irori's own log holds its last {lines} lines, {warnings} of them warnings and \
         {errors} errors. It starts again on every restart.\n"
    ));
    let most = if tight >= 3 { LOG_TAIL_TIGHT } else { LOG_TAIL };
    let from = picture.log_tail.len().saturating_sub(most);
    if let Some(tail) = picture.log_tail.get(from..).filter(|tail| !tail.is_empty()) {
        out.push_str("The newest warnings and errors, oldest first:\n");
        for line in tail {
            out.push_str(&format!("  {}\n", cut(line, LOG_LINE)));
        }
    }
    out
}

/// `text`, or its first `most` characters and an ellipsis.
fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(most.saturating_sub(1)).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(name: &str, entity: &str, value: &str) -> BriefLine {
        BriefLine {
            floor: String::new(),
            area: "Kitchen".into(),
            name: name.into(),
            entity: entity.into(),
            value: value.into(),
        }
    }

    #[test]
    fn a_device_brief_does_not_carry_another_device() {
        let text = device_brief("Lamp", "Hall", &[line("Lamp", "light.lamp", "on")]);
        assert!(text.contains("light.lamp"));
        assert!(!text.contains("other"));
        let home = home_brief(&[
            line("Lamp", "light.lamp", "on"),
            line("Kettle", "switch.kettle", "off"),
        ]);
        assert!(home.contains("Kettle"));
        assert!(
            !device_brief("Lamp", "Hall", &[line("Lamp", "light.lamp", "on")]).contains("Kettle")
        );
    }

    fn picture() -> SettingsPicture {
        SettingsPicture {
            system: vec!["Irori 0.9.1.".into()],
            assistant: vec!["A cloud model, gpt, answers. Its key is set.".into()],
            floors: vec!["Ground".into(), "Attic".into()],
            places: vec![
                Place {
                    floor: "Ground".into(),
                    area: "Kitchen".into(),
                    devices: vec!["Kettle".into(), "Lamp".into()],
                },
                Place {
                    floor: String::new(),
                    area: "Shed".into(),
                    devices: Vec::new(),
                },
                Place {
                    floor: String::new(),
                    area: String::new(),
                    devices: vec!["Stray plug".into()],
                },
            ],
            extensions: vec![ExtensionLine {
                name: "MQTT".into(),
                state: "running".into(),
                version: "1.0.0".into(),
                settings: vec![("host".into(), "broker.local".into())],
            }],
            log_counts: (40, 2, 1),
            log_tail: (0..20).map(|n| format!("WARN line {n}")).collect(),
        }
    }

    #[test]
    fn settings_are_told_floor_by_floor_and_area_by_area() {
        let text = settings_brief(&picture(), 10_000);
        let ground = text.find("Floor \"Ground\"").expect("the ground floor");
        let kitchen = text.find("Kitchen: Kettle, Lamp").expect("the kitchen");
        let attic = text.find("Floor \"Attic\"").expect("the attic");
        assert!(ground < kitchen && kitchen < attic, "{text}");
        assert!(text.contains("no areas"), "{text}");
        assert!(text.contains("Shed: no devices"), "{text}");
        assert!(text.contains("Devices in no area: Stray plug"), "{text}");
        assert!(text.contains("host: broker.local"), "{text}");
        // The newest twelve of the twenty.
        assert!(text.contains("WARN line 19") && text.contains("WARN line 8"));
        assert!(!text.contains("WARN line 7\n"), "{text}");
    }

    #[test]
    fn a_tight_budget_gives_up_detail_before_it_gives_up_the_shape() {
        let full = settings_brief(&picture(), 10_000);
        let small = settings_brief(&picture(), full.len() - 1);
        assert!(!small.contains("broker.local"), "{small}");
        assert!(small.contains("Kitchen: Kettle, Lamp"), "{small}");
        let smaller = settings_brief(&picture(), small.len() - 1);
        assert!(smaller.contains("Kitchen: 2 devices"), "{smaller}");
        let smallest = settings_brief(&picture(), smaller.len() - 1);
        assert!(!smallest.contains("WARN line 14\n"), "{smallest}");
        assert!(smallest.contains("WARN line 19"), "{smallest}");
        assert!(settings_brief(&picture(), 50).chars().count() <= 50);
    }

    #[test]
    fn the_home_is_told_with_its_floors() {
        let mut lamp = line("Lamp", "light.lamp", "on");
        lamp.floor = "Ground".into();
        assert!(home_brief(&[lamp]).contains("- Ground, Kitchen — Lamp"));
    }
}
