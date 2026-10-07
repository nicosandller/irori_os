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

/// The longest the picture of the home gets, when nothing asks for less.
pub const HOME_CAP: usize = 6_000;

/// A short picture of the whole home, in at most `cap` characters. Past that, the rest is
/// counted rather than pasted, so a large house still fits in front of a small model.
pub fn home_brief(lines: &[BriefLine], cap: usize) -> String {
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
        if out.len() + row.len() > cap {
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

/// One automation, as the list of them tells it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutomationLine {
    pub name: String,
    pub id: String,
    /// `on`, `turned off`, or why it can't run.
    pub state: String,
    pub problems: usize,
    /// How its last run came out, and when. `None` when it has never run.
    pub last_run: Option<String>,
    /// Times a trigger nearly fired and didn't, that are still remembered.
    pub near_misses: usize,
}

/// Every automation in a line each, in at most `budget` characters. `lines` is `Err` with the
/// reason when the Automations extension couldn't be asked.
pub fn automations_brief(lines: Result<&[AutomationLine], &str>, budget: usize) -> String {
    let mut out = String::from("\nAutomations, each with its id:\n");
    let lines = match lines {
        Ok(lines) => lines,
        Err(why) => {
            out.push_str(&format!("- Not available: {}\n", cut(why, 200)));
            return out;
        }
    };
    if lines.is_empty() {
        out.push_str("- None yet.\n");
        return out;
    }
    let mut written = 0usize;
    for line in lines {
        let mut row = format!("- {} (`{}`) — {}", line.name, line.id, line.state);
        match &line.last_run {
            Some(last) => row.push_str(&format!("; last run {}", cut(last, 160))),
            None => row.push_str("; has not run yet"),
        }
        if line.problems > 0 {
            row.push_str(&format!("; problems: {}", line.problems));
        }
        if line.near_misses > 0 {
            row.push_str(&format!("; near-misses: {}", line.near_misses));
        }
        row.push('\n');
        if out.len() + row.len() > budget {
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
/// counted instead), then the older lines of the log. `on_page` is whether the person is asking
/// from Settings itself, or from the chat about the whole home, which is told the same things.
pub fn settings_brief(picture: &SettingsPicture, budget: usize, on_page: bool) -> String {
    let mut out = String::new();
    for tight in 0..=3 {
        out = settings_at(picture, tight, on_page);
        if out.len() <= budget {
            return out;
        }
    }
    cut(&out, budget)
}

fn settings_at(picture: &SettingsPicture, tight: u8, on_page: bool) -> String {
    let mut out = String::from(if on_page {
        "The person is on Irori's Settings page and is asking about Irori itself: its settings, \
         the machine, floors and areas, extensions, and its log. This is what Settings holds \
         right now.\n"
    } else {
        "\nIrori itself, as its Settings page has it right now: the machine, the assistant, \
         floors and areas, extensions, and its log.\n"
    });
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
        let home = home_brief(
            &[
                line("Lamp", "light.lamp", "on"),
                line("Kettle", "switch.kettle", "off"),
            ],
            HOME_CAP,
        );
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
        let text = settings_brief(&picture(), 10_000, true);
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
        let full = settings_brief(&picture(), 10_000, true);
        let small = settings_brief(&picture(), full.len() - 1, true);
        assert!(!small.contains("broker.local"), "{small}");
        assert!(small.contains("Kitchen: Kettle, Lamp"), "{small}");
        let smaller = settings_brief(&picture(), small.len() - 1, true);
        assert!(smaller.contains("Kitchen: 2 devices"), "{smaller}");
        let smallest = settings_brief(&picture(), smaller.len() - 1, true);
        assert!(!smallest.contains("WARN line 14\n"), "{smallest}");
        assert!(smallest.contains("WARN line 19"), "{smallest}");
        assert!(settings_brief(&picture(), 50, true).chars().count() <= 50);
    }

    #[test]
    fn the_home_is_told_with_its_floors() {
        let mut lamp = line("Lamp", "light.lamp", "on");
        lamp.floor = "Ground".into();
        assert!(home_brief(&[lamp], HOME_CAP).contains("- Ground, Kitchen — Lamp"));
    }

    #[test]
    fn a_small_cap_counts_the_devices_it_has_no_room_for() {
        let lines: Vec<BriefLine> = (0..40)
            .map(|n| line(&format!("Lamp {n}"), &format!("light.lamp_{n}"), "on"))
            .collect();
        let text = home_brief(&lines, 600);
        assert!(text.len() < 700, "{}", text.len());
        assert!(
            text.contains("Lamp 0") && !text.contains("Lamp 39"),
            "{text}"
        );
        assert!(text.contains("more, not listed here"), "{text}");
    }

    #[test]
    fn the_same_settings_are_told_to_the_chat_about_the_home() {
        let text = settings_brief(&picture(), 10_000, false);
        assert!(!text.contains("is on Irori's Settings page"), "{text}");
        assert!(text.contains("Kitchen: Kettle, Lamp"), "{text}");
        assert!(text.contains("WARN line 19"), "{text}");
    }

    #[test]
    fn automations_are_told_a_line_each_with_what_went_last() {
        let lines = [
            AutomationLine {
                name: "TV area lighting".into(),
                id: "tv_area_lighting".into(),
                state: "on".into(),
                problems: 0,
                last_run: Some("completed 3 minutes ago (ended at lights on → no)".into()),
                near_misses: 2,
            },
            AutomationLine {
                name: "Porch".into(),
                id: "porch".into(),
                state: "can't run: no such entity".into(),
                problems: 1,
                last_run: None,
                near_misses: 0,
            },
        ];
        let text = automations_brief(Ok(&lines), 2_000);
        assert!(
            text.contains(
                "- TV area lighting (`tv_area_lighting`) — on; last run completed 3 minutes ago \
                 (ended at lights on → no); near-misses: 2\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "- Porch (`porch`) — can't run: no such entity; has not run yet; problems: 1"
            ),
            "{text}"
        );
        let short = automations_brief(Ok(&lines), 200);
        assert!(short.contains("And 1 more"), "{short}");
        assert!(automations_brief(Ok(&[]), 500).contains("None yet"));
        assert!(
            automations_brief(Err("the extension isn't running"), 500)
                .contains("Not available: the extension isn't running")
        );
    }
}
