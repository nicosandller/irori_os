//! What a broker's discovery configs offer that Irori has no entity kind for yet, kept as the
//! list a protocol sends with `set_unmodeled` (`docs/specs/protocols.md` §6.7).
//!
//! Discovery arrives one retained message per component, so the list is built up topic by
//! topic: a config adds or replaces its entry, and an empty payload (the component removed)
//! takes it away.

use std::collections::BTreeMap;

use irori_types::{Name, ObjectId, Unmodeled};

use crate::discovery::parse_device;

#[derive(Debug, Default)]
pub struct Tracker {
    by_topic: BTreeMap<String, Unmodeled>,
}

impl Tracker {
    /// Takes in a config for `component` (one `topic::unsupported_component` named) at `topic`.
    /// Returns whether the list changed, i.e. whether it's worth sending again.
    pub fn apply(&mut self, topic: &str, component: &str, payload: &[u8]) -> bool {
        let entry = (!payload.is_empty())
            .then(|| entry(component, payload))
            .flatten();
        let before = self.by_topic.get(topic).cloned();
        match entry {
            Some(entry) => self.by_topic.insert(topic.to_owned(), entry),
            None => self.by_topic.remove(topic),
        };
        before.as_ref() != self.by_topic.get(topic)
    }

    /// The whole list, as `set_unmodeled` wants it.
    pub fn list(&self) -> Vec<Unmodeled> {
        self.by_topic.values().cloned().collect()
    }
}

/// What a config says about itself: its device, its name. A payload too broken to read is still
/// listed, by its component alone, since something is there.
fn entry(component: &str, payload: &[u8]) -> Option<Unmodeled> {
    let platform = ObjectId::try_from(component).ok()?;
    let root: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
    let device_unique_id = parse_device(&root)
        .ok()
        .flatten()
        .map(|device| device.unique_id);
    let name = root
        .get("name")
        .and_then(serde_json::Value::as_str)
        .and_then(|name| Name::try_from(name.trim()).ok());
    Some(Unmodeled {
        device_unique_id,
        platform,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAN: &[u8] = br#"{"unique_id": "0x1234_fan", "name": "Ceiling fan",
        "device": {"identifiers": ["zigbee2mqtt_0x1234"], "name": "Bedroom fan"}}"#;

    #[test]
    fn a_config_is_listed_on_its_device_until_it_is_removed() {
        let mut tracker = Tracker::default();
        let topic = "homeassistant/fan/0x1234/fan/config";
        assert!(tracker.apply(topic, "fan", FAN));
        let listed = tracker.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].platform.as_str(), "fan");
        assert_eq!(
            listed[0].name.as_ref().map(Name::as_str),
            Some("Ceiling fan")
        );
        assert_eq!(
            listed[0].device_unique_id.as_ref().map(|d| d.as_str()),
            Some("zigbee2mqtt_0x1234")
        );

        // The same config again (a retained message on reconnect) changes nothing.
        assert!(!tracker.apply(topic, "fan", FAN));
        // An empty payload is the component going away.
        assert!(tracker.apply(topic, "fan", b""));
        assert!(tracker.list().is_empty());
    }

    #[test]
    fn a_config_without_a_device_is_still_listed() {
        let mut tracker = Tracker::default();
        assert!(tracker.apply("homeassistant/cover/blind/config", "cover", b"not json"));
        let listed = tracker.list();
        assert_eq!(listed[0].platform.as_str(), "cover");
        assert_eq!(listed[0].device_unique_id, None);
    }
}
