//! Parsing an HA MQTT Discovery topic: `<prefix>/<component>/[<node_id>/]<object_id>/config`.

use std::fmt;

/// The HA MQTT Discovery components Irori understands. Anything else is skipped by
/// [`parse`] returning `None` — a `cover`/`climate`/`button` isn't an entity kind Irori has yet
/// (`docs/specs/protocols.md` §13 open question 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Light,
    Switch,
    Sensor,
    BinarySensor,
}

impl Component {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "light" => Some(Self::Light),
            "switch" => Some(Self::Switch),
            "sensor" => Some(Self::Sensor),
            "binary_sensor" => Some(Self::BinarySensor),
            _ => None,
        }
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Light => "light",
            Self::Switch => "switch",
            Self::Sensor => "sensor",
            Self::BinarySensor => "binary_sensor",
        })
    }
}

/// A discovery topic's segments, past the prefix: `<component>/[<node_id>/]<object_id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryTopic {
    pub component: Component,
    pub node_id: Option<String>,
    pub object_id: String,
}

/// Parses a topic against the configured discovery prefix. `None` for a component Irori doesn't
/// support, or a topic that isn't shaped like a discovery config topic at all — both are the
/// caller's cue to skip it quietly, not an error: a broker carries plenty of topics that aren't
/// discovery configs.
pub fn parse(topic: &str, discovery_prefix: &str) -> Option<DiscoveryTopic> {
    let rest = topic.strip_prefix(discovery_prefix)?.strip_prefix('/')?;
    let rest = rest.strip_suffix("/config")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let (component, node_id, object_id) = match parts.as_slice() {
        [component, object_id] => (*component, None, *object_id),
        [component, node_id, object_id] => (*component, Some(*node_id), *object_id),
        _ => return None,
    };
    if object_id.is_empty() {
        return None;
    }
    Some(DiscoveryTopic {
        component: Component::parse(component)?,
        node_id: node_id.map(str::to_owned),
        object_id: object_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_topics_with_and_without_a_node_id() {
        let a = parse("homeassistant/switch/0x1234/config", "homeassistant").expect("parses");
        assert_eq!(a.component, Component::Switch);
        assert_eq!(a.node_id, None);
        assert_eq!(a.object_id, "0x1234");

        let b = parse(
            "homeassistant/sensor/0x1234/temperature/config",
            "homeassistant",
        )
        .expect("parses");
        assert_eq!(b.component, Component::Sensor);
        assert_eq!(b.node_id.as_deref(), Some("0x1234"));
        assert_eq!(b.object_id, "temperature");
    }

    #[test]
    fn an_unsupported_component_or_wrong_shape_is_skipped_not_an_error() {
        assert!(parse("homeassistant/climate/x/config", "homeassistant").is_none());
        assert!(parse("somethingelse/switch/x/config", "homeassistant").is_none());
        assert!(parse("homeassistant/switch/x/state", "homeassistant").is_none());
        assert!(parse("homeassistant/switch//config", "homeassistant").is_none());
    }
}
