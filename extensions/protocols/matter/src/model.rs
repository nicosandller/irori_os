//! Matter's own data model, as the Matter server hands it over: a node, its endpoints, and the
//! value of every attribute it has read, keyed by `endpoint/cluster/attribute`.
//!
//! Nothing here knows about Irori. It's the half of this extension a different controller
//! (`rs-matter`, say) would have to fill in the same way.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde_json::Value;

/// A node's id on Irori's fabric.
pub type NodeId = u64;

/// Where an attribute lives: `endpoint/cluster/attribute`, the key the Matter server uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path {
    pub endpoint: u16,
    pub cluster: u32,
    pub attribute: u32,
}

impl Path {
    pub const fn new(endpoint: u16, cluster: u32, attribute: u32) -> Self {
        Self {
            endpoint,
            cluster,
            attribute,
        }
    }
}

impl FromStr for Path {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.split('/');
        let (Some(endpoint), Some(cluster), Some(attribute), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(format!("`{text}` isn't endpoint/cluster/attribute"));
        };
        let number = |part: &str| {
            part.parse::<u64>()
                .map_err(|_| format!("`{text}` isn't endpoint/cluster/attribute"))
        };
        Ok(Self {
            endpoint: u16::try_from(number(endpoint)?)
                .map_err(|_| format!("`{text}` has an endpoint out of range"))?,
            cluster: u32::try_from(number(cluster)?)
                .map_err(|_| format!("`{text}` has a cluster out of range"))?,
            attribute: u32::try_from(number(attribute)?)
                .map_err(|_| format!("`{text}` has an attribute out of range"))?,
        })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.endpoint, self.cluster, self.attribute)
    }
}

/// Cluster ids this extension reads (Matter Application Cluster Specification).
pub mod cluster {
    pub const DESCRIPTOR: u32 = 0x001D;
    pub const BASIC_INFORMATION: u32 = 0x0028;
    pub const OTA_SOFTWARE_UPDATE_REQUESTOR: u32 = 0x002A;
    pub const POWER_SOURCE: u32 = 0x002F;
    pub const BRIDGED_DEVICE_BASIC_INFORMATION: u32 = 0x0039;
    pub const SWITCH: u32 = 0x003B;
    pub const BOOLEAN_STATE: u32 = 0x0045;
    pub const SMOKE_CO_ALARM: u32 = 0x005C;
    pub const VALVE: u32 = 0x0081;
    pub const ELECTRICAL_POWER: u32 = 0x0090;
    pub const ELECTRICAL_ENERGY: u32 = 0x0091;
    pub const ON_OFF: u32 = 0x0006;
    pub const LEVEL_CONTROL: u32 = 0x0008;
    pub const DOOR_LOCK: u32 = 0x0101;
    pub const WINDOW_COVERING: u32 = 0x0102;
    pub const THERMOSTAT: u32 = 0x0201;
    pub const FAN_CONTROL: u32 = 0x0202;
    pub const COLOR_CONTROL: u32 = 0x0300;
    pub const ILLUMINANCE: u32 = 0x0400;
    pub const TEMPERATURE: u32 = 0x0402;
    pub const PRESSURE: u32 = 0x0403;
    pub const FLOW: u32 = 0x0404;
    pub const HUMIDITY: u32 = 0x0405;
    pub const OCCUPANCY: u32 = 0x0406;
}

/// Device type ids (Matter Device Library Specification).
pub mod device_type {
    pub const BRIDGED_NODE: u32 = 0x0013;
    pub const CONTACT_SENSOR: u32 = 0x0015;
    pub const WATER_FREEZE_DETECTOR: u32 = 0x0041;
    pub const WATER_LEAK_DETECTOR: u32 = 0x0043;
    pub const RAIN_SENSOR: u32 = 0x0044;
    pub const ON_OFF_LIGHT: u32 = 0x0100;
    pub const DIMMABLE_LIGHT: u32 = 0x0101;
    pub const DIMMER_SWITCH: u32 = 0x0104;
    pub const COLOR_DIMMER_SWITCH: u32 = 0x0105;
    pub const ON_OFF_PLUG_IN_UNIT: u32 = 0x010A;
    pub const DIMMABLE_PLUG_IN_UNIT: u32 = 0x010B;
    pub const COLOR_TEMPERATURE_LIGHT: u32 = 0x010C;
    pub const EXTENDED_COLOR_LIGHT: u32 = 0x010D;
    pub const MOUNTED_DIMMABLE_LOAD_CONTROL: u32 = 0x0110;
}

/// Something to send a node: a cluster command, or an attribute to write. A service call
/// becomes one or a few of these (`map`), sent in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A cluster command, by its name in matter.js (`moveToLevelWithOnOff`), with its fields by
    /// theirs. `timed`: the cluster requires a timed interaction for it (Door Lock's).
    Invoke {
        endpoint: u16,
        cluster: u32,
        name: &'static str,
        payload: Value,
        timed: bool,
    },
    Write {
        path: Path,
        value: Value,
    },
}

impl Command {
    pub fn invoke(endpoint: u16, cluster: u32, name: &'static str, payload: Value) -> Self {
        Self::Invoke {
            endpoint,
            cluster,
            name,
            payload,
            timed: false,
        }
    }

    pub fn write(endpoint: u16, cluster: u32, attribute: u32, value: Value) -> Self {
        Self::Write {
            path: Path::new(endpoint, cluster, attribute),
            value,
        }
    }
}

/// The attributes every cluster has, beside its own (`0xFFF8`…`0xFFFD`).
pub const FEATURE_MAP: u32 = 0xFFFC;

/// One node, as the Matter server last described it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: NodeId,
    /// The server can reach it right now.
    pub available: bool,
    pub attributes: BTreeMap<Path, Value>,
}

impl Node {
    /// Reads the server's `MatterNodeData`. An attribute under a key that isn't
    /// `endpoint/cluster/attribute` is left out rather than failing the whole node.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let id = value
            .get("node_id")
            .and_then(Value::as_u64)
            .ok_or("a node without a node_id")?;
        let available = value
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let attributes = value
            .get("attributes")
            .and_then(Value::as_object)
            .map(|attributes| {
                attributes
                    .iter()
                    .filter_map(|(key, value)| Some((key.parse().ok()?, value.clone())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            id,
            available,
            attributes,
        })
    }

    pub fn get(&self, endpoint: u16, cluster: u32, attribute: u32) -> Option<&Value> {
        self.attributes
            .get(&Path::new(endpoint, cluster, attribute))
    }

    /// The attribute's value, unless it's absent or `null`.
    pub fn value(&self, endpoint: u16, cluster: u32, attribute: u32) -> Option<&Value> {
        self.get(endpoint, cluster, attribute)
            .filter(|value| !value.is_null())
    }

    /// Whether the node read this attribute at all, even if its value is `null` for now.
    pub fn has(&self, endpoint: u16, cluster: u32, attribute: u32) -> bool {
        self.get(endpoint, cluster, attribute).is_some()
    }

    pub fn text(&self, endpoint: u16, cluster: u32, attribute: u32) -> Option<&str> {
        self.value(endpoint, cluster, attribute)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

    pub fn number(&self, endpoint: u16, cluster: u32, attribute: u32) -> Option<f64> {
        self.value(endpoint, cluster, attribute)
            .and_then(Value::as_f64)
    }

    /// A cluster's feature bits (`FeatureMap`), zero if it didn't say.
    pub fn features(&self, endpoint: u16, cluster: u32) -> u64 {
        self.value(endpoint, cluster, FEATURE_MAP)
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    /// Whether the endpoint has the cluster at all: any one of its attributes was read.
    pub fn has_cluster(&self, endpoint: u16, cluster: u32) -> bool {
        self.attributes
            .range(Path::new(endpoint, cluster, 0)..=Path::new(endpoint, cluster, u32::MAX))
            .next()
            .is_some()
    }

    /// Every endpoint the node has an attribute on, in order.
    pub fn endpoints(&self) -> BTreeSet<u16> {
        self.attributes.keys().map(|path| path.endpoint).collect()
    }

    /// The device types an endpoint says it is (`Descriptor.DeviceTypeList`).
    pub fn device_types(&self, endpoint: u16) -> Vec<u32> {
        self.value(endpoint, cluster::DESCRIPTOR, 0)
            .and_then(Value::as_array)
            .map(|types| {
                types
                    .iter()
                    .filter_map(|entry| entry.get("0").and_then(Value::as_u64))
                    .filter_map(|id| u32::try_from(id).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is(&self, endpoint: u16, device_type: u32) -> bool {
        self.device_types(endpoint).contains(&device_type)
    }

    /// The endpoints an endpoint is made of (`Descriptor.PartsList`).
    pub fn parts(&self, endpoint: u16) -> Vec<u16> {
        self.value(endpoint, cluster::DESCRIPTOR, 3)
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(Value::as_u64)
                    .filter_map(|id| u16::try_from(id).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Takes in one attribute's new value.
    pub fn set(&mut self, path: Path, value: Value) {
        self.attributes.insert(path, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_reads_and_writes_the_way_the_server_keys_it() {
        let path: Path = "1/1030/0".parse().expect("a path");
        assert_eq!(path, Path::new(1, cluster::OCCUPANCY, 0));
        assert_eq!(path.to_string(), "1/1030/0");
        for bad in ["1/6", "1/6/0/2", "a/6/0", "70000/6/0", ""] {
            assert!(bad.parse::<Path>().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_node_dump_gives_its_endpoints_types_and_values() {
        let node = Node::from_json(&serde_json::json!({
            "node_id": 3,
            "available": true,
            "is_bridge": false,
            "attributes": {
                "0/40/5": " Lab plug ",
                "0/40/3": "",
                "1/29/0": [{"0": 266, "1": 4}],
                "1/29/3": [2, 3],
                "1/6/0": true,
                "1/6/16387": null,
                "not/a/path": 1
            }
        }))
        .expect("a node");
        assert_eq!(node.id, 3);
        assert!(node.available);
        assert_eq!(node.endpoints(), BTreeSet::from([0, 1]));
        assert_eq!(node.device_types(1), vec![266]);
        assert!(node.is(1, device_type::ON_OFF_PLUG_IN_UNIT));
        assert_eq!(node.parts(1), vec![2, 3]);
        assert_eq!(
            node.text(0, cluster::BASIC_INFORMATION, 5),
            Some("Lab plug")
        );
        assert_eq!(node.text(0, cluster::BASIC_INFORMATION, 3), None, "blank");
        assert!(node.has(1, cluster::ON_OFF, 16387));
        assert!(node.has_cluster(1, cluster::ON_OFF));
        assert!(!node.has_cluster(1, cluster::LEVEL_CONTROL));
        assert!(!node.has_cluster(0, cluster::ON_OFF));
        assert_eq!(node.value(1, cluster::ON_OFF, 16387), None, "null");
        assert_eq!(node.attributes.len(), 6, "the bad key is left out");
    }

    /// A node id past JavaScript's safe integers still arrives whole.
    #[test]
    fn a_large_node_id_is_read_exactly() {
        let node = Node::from_json(&serde_json::json!({
            "node_id": 18_446_744_073_709_551_000_u64,
            "attributes": {}
        }))
        .expect("a node");
        assert_eq!(node.id, 18_446_744_073_709_551_000);
        assert!(!node.available, "unsaid is unavailable");
    }
}
