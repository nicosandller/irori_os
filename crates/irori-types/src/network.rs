//! A protocol's network as it sees it: what's on it and what reaches what. The network map in
//! Settings draws every protocol's network together, around Irori. See
//! `docs/specs/protocols.md` §6.8.
//!
//! A picture, not a registry: nothing in it is addressed, and it's replaced whole each time the
//! protocol sends it. A node that's one of Irori's devices says which, so the map can show that
//! device's room, battery and messages.
//!
//! Two kinds of node are shared between networks, so the map can draw one home rather than a
//! row of islands: every network's [`NetworkRole::Controller`] is Irori itself, and an
//! [`NetworkRole::AccessPoint`] is the same access point in every network that names it by the
//! same id ([`access_point_id`]).

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{InvariantError, Timestamp, UniqueId};

/// Most networks one protocol draws. One is usual; a protocol that runs several meshes (or the
/// demo, which plays at three) draws each.
pub const MAX_NETWORKS: usize = 8;
/// Most nodes a map has: a large Thread or Zigbee mesh is a couple of hundred.
pub const MAX_NETWORK_NODES: usize = 512;
/// Most links a map has.
pub const MAX_NETWORK_LINKS: usize = 2048;
/// Longest a node's id is.
pub const MAX_NETWORK_ID: usize = 64;
/// Longest a node's or a network's name is.
pub const MAX_NETWORK_NAME: usize = 100;
/// Longest a node's, link's or network's detail is.
pub const MAX_NETWORK_DETAIL: usize = 200;
/// Longest a link's reading is: it's drawn on the link, in a small box.
pub const MAX_NETWORK_READING: usize = 16;
/// The longest [`NetworkNode::host`]: a DNS name's.
pub const MAX_NETWORK_HOST: usize = 253;

/// One network a protocol reaches its devices over.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// What it's called on the map: "Zigbee", "Matter". Without one, the extension's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A few words about it: "Channel 15, PAN 0x1a62".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// When its links were last measured, for a network whose links are measured when asked
    /// (Zigbee's scan) rather than all the time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_at: Option<Timestamp>,
    pub nodes: Vec<NetworkNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<NetworkLink>,
}

/// Something on the network: Irori itself, a coordinator, a border router, an access point, a
/// device.
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
    /// Its host name on the home's network, as mDNS gives it (`fuchsia-dce5-5ba4-40db.local`),
    /// when the protocol knows. Two protocols that see the same box (a Nest Hub is a Cast device
    /// and a Thread border router) name it the same, and the map draws it once. [`host_name`]
    /// makes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// A host name as [`NetworkNode::host`] keeps it: lowercase, without the trailing dot mDNS
/// gives it. `None` for one that's empty or too long to be a host name.
pub fn host_name(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    (!name.is_empty() && name.len() <= MAX_NETWORK_HOST).then_some(name)
}

/// What a node does on the network.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRole {
    /// Irori, or the controller it runs (the Matter server). Every network's is drawn as the one
    /// Irori.
    Controller,
    /// The radio a mesh is run from, on Irori's side: a Zigbee coordinator, a bridge.
    Coordinator,
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
    /// A cable into the machine: a USB dongle, a serial port.
    Serial,
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
    /// The measurement behind the quality, short enough to draw on the link: `LQI 212`,
    /// `-54 dBm`, `USB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading: Option<String>,
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

    /// From a Zigbee link quality indicator, 0-255.
    pub fn from_lqi(lqi: u64) -> Self {
        match lqi {
            180.. => Self::Excellent,
            100..=179 => Self::Good,
            60..=99 => Self::Fair,
            _ => Self::Poor,
        }
    }
}

/// The id an access point goes by in every network that names it, so a protocol's Wi-Fi
/// devices and another's meet at the same one: `ap-` and its BSSID as twelve lowercase hex
/// digits, or `wifi` when which one isn't known.
pub fn access_point_id(bssid: Option<[u8; 6]>) -> String {
    match bssid {
        Some(bytes) => {
            let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("ap-{hex}")
        }
        None => "wifi".to_owned(),
    }
}

impl Network {
    /// Ids are well formed and once each, every link names two of them, and nothing is too
    /// long or too many. The core refuses a map that isn't.
    pub fn validate(&self) -> Result<(), InvariantError> {
        let fail = |why: String| Err(InvariantError(why));
        let longer = |text: &Option<String>, most: usize| {
            text.as_ref()
                .is_some_and(|text| text.chars().count() > most)
        };
        if self
            .name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
            || longer(&self.name, MAX_NETWORK_NAME)
        {
            return fail(format!(
                "a network's name must be 1-{MAX_NETWORK_NAME} characters"
            ));
        }
        if longer(&self.detail, MAX_NETWORK_DETAIL) {
            return fail(format!(
                "a network's detail is longer than {MAX_NETWORK_DETAIL} characters"
            ));
        }
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
            if longer(&node.detail, MAX_NETWORK_DETAIL) {
                return fail(format!(
                    "node {:?}'s detail is longer than {MAX_NETWORK_DETAIL} characters",
                    node.id
                ));
            }
            if node
                .host
                .as_deref()
                .is_some_and(|host| host_name(host).as_deref() != Some(host))
            {
                return fail(format!(
                    "node {:?}'s host must be a lowercase host name of 1-{MAX_NETWORK_HOST} \
                     characters, without a trailing dot",
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
            if longer(&link.detail, MAX_NETWORK_DETAIL) {
                return fail(format!(
                    "a link's detail is longer than {MAX_NETWORK_DETAIL} characters"
                ));
            }
            if longer(&link.reading, MAX_NETWORK_READING) {
                return fail(format!(
                    "a link's reading is longer than {MAX_NETWORK_READING} characters"
                ));
            }
        }
        Ok(())
    }

    /// Every one of a protocol's networks is valid, and there aren't too many.
    pub fn validate_all(networks: &[Network]) -> Result<(), InvariantError> {
        if networks.len() > MAX_NETWORKS {
            return Err(InvariantError(format!(
                "{} networks is too many; at most {MAX_NETWORKS}",
                networks.len()
            )));
        }
        networks.iter().try_for_each(Network::validate)
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
            host: None,
        }
    }

    fn link(from: &str, to: &str) -> NetworkLink {
        NetworkLink {
            from: from.to_owned(),
            to: to.to_owned(),
            medium: Some(NetworkMedium::Thread),
            quality: Some(LinkQuality::Good),
            reading: None,
            detail: None,
        }
    }

    fn network(nodes: Vec<NetworkNode>, links: Vec<NetworkLink>) -> Network {
        Network {
            nodes,
            links,
            ..Network::default()
        }
    }

    #[test]
    fn a_map_names_each_node_once_and_links_only_nodes_it_has() {
        let good = network(
            vec![
                node("irori", NetworkRole::Controller),
                node("br-1", NetworkRole::BorderRouter),
            ],
            vec![link("br-1", "irori")],
        );
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

        let mut odd = good.clone();
        odd.nodes[1].id = "a b".to_owned();
        assert!(odd.validate().is_err());

        let mut wordy = good.clone();
        wordy.links[0].reading = Some("a reading far too long to draw".to_owned());
        assert!(wordy.validate().is_err());

        let mut unnamed = good.clone();
        unnamed.name = Some("  ".to_owned());
        assert!(unnamed.validate().is_err());

        let mut hosted = good.clone();
        hosted.nodes[1].host = host_name("Fuchsia-DCE5-5ba4-40db.local.");
        assert_eq!(
            hosted.nodes[1].host.as_deref(),
            Some("fuchsia-dce5-5ba4-40db.local")
        );
        assert_eq!(hosted.validate(), Ok(()));
        hosted.nodes[1].host = Some("Fuchsia.local.".to_owned());
        assert!(hosted.validate().is_err(), "only as host_name makes it");

        assert!(Network::validate_all(&vec![good; MAX_NETWORKS + 1]).is_err());
    }

    #[test]
    fn signal_strength_reads_in_four_steps() {
        assert_eq!(LinkQuality::from_rssi(-45), LinkQuality::Excellent);
        assert_eq!(LinkQuality::from_rssi(-61), LinkQuality::Good);
        assert_eq!(LinkQuality::from_rssi(-75), LinkQuality::Fair);
        assert_eq!(LinkQuality::from_rssi(-89), LinkQuality::Poor);
        assert_eq!(LinkQuality::from_lqi(212), LinkQuality::Excellent);
        assert_eq!(LinkQuality::from_lqi(126), LinkQuality::Good);
        assert_eq!(LinkQuality::from_lqi(71), LinkQuality::Fair);
        assert_eq!(LinkQuality::from_lqi(28), LinkQuality::Poor);
    }

    #[test]
    fn an_access_point_has_one_id_whoever_names_it() {
        assert_eq!(
            access_point_id(Some([0x74, 0xac, 0xb9, 0x01, 0x02, 0x0f])),
            "ap-74acb901020f"
        );
        assert_eq!(access_point_id(None), "wifi");
    }

    #[test]
    fn a_map_reads_as_it_was_written() {
        let text = r#"{"name":"Matter","scanned_at":"2026-10-23T10:00:00Z",
            "nodes":[{"id":"irori","name":"Irori","role":"controller"},
            {"id":"lamp","name":"Lamp","role":"sleepy_end_device","medium":"thread",
             "device_unique_id":"ABC-1"}],
            "links":[{"from":"lamp","to":"irori","medium":"thread","quality":"fair",
             "reading":"-74 dBm"}]}"#;
        let network: Network = serde_json::from_str(text).expect("a map");
        assert_eq!(network.nodes[1].role, NetworkRole::SleepyEndDevice);
        assert_eq!(network.links[0].quality, Some(LinkQuality::Fair));
        assert_eq!(network.links[0].reading.as_deref(), Some("-74 dBm"));
        assert_eq!(network.validate(), Ok(()));
    }
}
