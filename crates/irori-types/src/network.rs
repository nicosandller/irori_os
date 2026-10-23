//! A protocol's network as it sees it: what's on it and what reaches what. The settings window
//! of its extension draws it as the network map. See `docs/specs/protocols.md` §6.8.
//!
//! A picture, not a registry: nothing in it is addressed, and it's replaced whole each time the
//! protocol sends it. A node that's one of Irori's devices says which, so the map can show that
//! device's messages travelling across it.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{InvariantError, UniqueId};

/// Most nodes a map has: a large Thread or Zigbee mesh is a couple of hundred.
pub const MAX_NETWORK_NODES: usize = 512;
/// Most links a map has.
pub const MAX_NETWORK_LINKS: usize = 2048;
/// Longest a node's id is.
pub const MAX_NETWORK_ID: usize = 64;
/// Longest a node's name is.
pub const MAX_NETWORK_NAME: usize = 100;
/// Longest a node's or link's detail is.
pub const MAX_NETWORK_DETAIL: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub nodes: Vec<NetworkNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<NetworkLink>,
}

/// Something on the network: Irori itself, a border router, a Wi-Fi access point, a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkNode {
    /// Its id in this map, which links name: 1-64 of letters, digits and `_.:-`.
    pub id: String,
    pub name: String,
    pub role: NetworkRole,
    /// How it's connected, when the protocol knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<NetworkMedium>,
    /// The device it is, when it's one of the protocol's devices in Irori.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_unique_id: Option<UniqueId>,
    /// A few words more: "Router, channel 25", "Google Nest Hub".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What a node does on the network.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRole {
    /// Irori, or the controller it runs (the Matter server, a Zigbee coordinator).
    Controller,
    /// Joins a mesh to the home's network: a Thread border router.
    BorderRouter,
    /// A Wi-Fi access point.
    AccessPoint,
    /// Passes a mesh's messages on: a Thread or Zigbee router.
    Router,
    /// On a mesh, through a router, and always listening.
    EndDevice,
    /// On a mesh, through a router, and asleep most of the time: a battery sensor.
    SleepyEndDevice,
    /// On the home's own network, Wi-Fi or wired.
    Device,
}

/// How a node is connected, or what a link is over.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMedium {
    Thread,
    Wifi,
    Ethernet,
    Zigbee,
    Bluetooth,
    /// The home's network, not known which way.
    Ip,
}

/// Two nodes that reach each other directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkLink {
    /// The node further from the controller, when that's known.
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<NetworkMedium>,
    /// How good the link is, when the protocol can tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<LinkQuality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A link's quality, in the four steps a person reads it in. Raw numbers (an RSSI, an LQI)
/// change from one report to the next; these don't, unless the link really does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkQuality {
    Poor,
    Fair,
    Good,
    Excellent,
}

impl LinkQuality {
    /// From a received signal strength, in dBm.
    pub fn from_rssi(dbm: i64) -> Self {
        match dbm {
            -60.. => Self::Excellent,
            -70..=-61 => Self::Good,
            -80..=-71 => Self::Fair,
            _ => Self::Poor,
        }
    }
}

impl Network {
    /// Ids are well formed and once each, every link names two of them, and nothing is too
    /// long or too many. The core refuses a map that isn't.
    pub fn validate(&self) -> Result<(), InvariantError> {
        let fail = |why: String| Err(InvariantError(why));
        if self.nodes.len() > MAX_NETWORK_NODES {
            return fail(format!(
                "{} nodes is too many; at most {MAX_NETWORK_NODES}",
                self.nodes.len()
            ));
        }
        if self.links.len() > MAX_NETWORK_LINKS {
            return fail(format!(
                "{} links is too many; at most {MAX_NETWORK_LINKS}",
                self.links.len()
            ));
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            let fine = !node.id.is_empty()
                && node.id.len() <= MAX_NETWORK_ID
                && node
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'));
            if !fine {
                return fail(format!(
                    "node id {:?} must be 1-{MAX_NETWORK_ID} letters, digits and `_.:-`",
                    node.id
                ));
            }
            if !ids.insert(node.id.as_str()) {
                return fail(format!("node id {:?} is used twice", node.id));
            }
            if node.name.trim().is_empty() || node.name.chars().count() > MAX_NETWORK_NAME {
                return fail(format!(
                    "node {:?}'s name must be 1-{MAX_NETWORK_NAME} characters",
                    node.id
                ));
            }
            if node
                .detail
                .as_ref()
                .is_some_and(|detail| detail.chars().count() > MAX_NETWORK_DETAIL)
            {
                return fail(format!(
                    "node {:?}'s detail is longer than {MAX_NETWORK_DETAIL} characters",
                    node.id
                ));
            }
        }
        for link in &self.links {
            for end in [&link.from, &link.to] {
                if !ids.contains(end.as_str()) {
                    return fail(format!("a link names {end:?}, which isn't a node"));
                }
            }
            if link.from == link.to {
                return fail(format!("a link from {:?} to itself", link.from));
            }
            if link
                .detail
                .as_ref()
                .is_some_and(|detail| detail.chars().count() > MAX_NETWORK_DETAIL)
            {
                return fail(format!(
                    "a link's detail is longer than {MAX_NETWORK_DETAIL} characters"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, role: NetworkRole) -> NetworkNode {
        NetworkNode {
            id: id.to_owned(),
            name: id.to_owned(),
            role,
            medium: None,
            device_unique_id: None,
            detail: None,
        }
    }

    fn link(from: &str, to: &str) -> NetworkLink {
        NetworkLink {
            from: from.to_owned(),
            to: to.to_owned(),
            medium: Some(NetworkMedium::Thread),
            quality: Some(LinkQuality::Good),
            detail: None,
        }
    }

    #[test]
    fn a_map_names_each_node_once_and_links_only_nodes_it_has() {
        let good = Network {
            nodes: vec![
                node("irori", NetworkRole::Controller),
                node("br-1", NetworkRole::BorderRouter),
            ],
            links: vec![link("br-1", "irori")],
        };
        assert_eq!(good.validate(), Ok(()));

        let mut twice = good.clone();
        twice.nodes.push(node("br-1", NetworkRole::Router));
        assert!(twice.validate().is_err());

        let mut dangling = good.clone();
        dangling.links.push(link("lamp", "irori"));
        assert!(dangling.validate().is_err());

        let mut looped = good.clone();
        looped.links.push(link("irori", "irori"));
        assert!(looped.validate().is_err());

        let mut odd = good;
        odd.nodes[1].id = "a b".to_owned();
        assert!(odd.validate().is_err());
    }

    #[test]
    fn signal_strength_reads_in_four_steps() {
        assert_eq!(LinkQuality::from_rssi(-45), LinkQuality::Excellent);
        assert_eq!(LinkQuality::from_rssi(-61), LinkQuality::Good);
        assert_eq!(LinkQuality::from_rssi(-75), LinkQuality::Fair);
        assert_eq!(LinkQuality::from_rssi(-89), LinkQuality::Poor);
    }

    #[test]
    fn a_map_reads_as_it_was_written() {
        let text = r#"{"nodes":[{"id":"irori","name":"Irori","role":"controller"},
            {"id":"lamp","name":"Lamp","role":"sleepy_end_device","medium":"thread",
             "device_unique_id":"ABC-1"}],
            "links":[{"from":"lamp","to":"irori","medium":"thread","quality":"fair"}]}"#;
        let network: Network = serde_json::from_str(text).expect("a map");
        assert_eq!(network.nodes[1].role, NetworkRole::SleepyEndDevice);
        assert_eq!(network.links[0].quality, Some(LinkQuality::Fair));
        assert_eq!(network.validate(), Ok(()));
    }
}
