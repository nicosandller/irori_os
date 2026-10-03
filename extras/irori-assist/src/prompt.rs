//! The snapshot handed to the model with each turn. It is rebuilt every time, so a lamp that
//! changed since the last message is what the model sees now, and it is not stored as if the
//! person had typed it.

/// One line of a home or a device: where, what, and the word it is reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefLine {
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
            "- {} — {} ({}) — {}\n",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line(name: &str, entity: &str, value: &str) -> BriefLine {
        BriefLine {
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
}
