//! The map of Matter's network: Irori, the border routers and Wi-Fi it reaches devices through,
//! and each node, read from the node's own diagnostics.
//!
//! - **How a node is connected** comes from General Diagnostics' network interfaces (Wi-Fi,
//!   Ethernet, Thread), else from which diagnostics cluster it has.
//! - **A Thread node's links** are its neighbour table: every Thread device lists the ones it
//!   hears, how well, and whether each is its child. A neighbour is named by its extended
//!   address, which a node gives as `ExtAddress` (Matter 1.4) or, before that, inside its
//!   link-local IPv6 address.
//! - **Border routers** announce themselves over mDNS (`_meshcop._udp`), with their extended
//!   address (`xa`), so the one a node hears is the one on the map.
//! - **Wi-Fi devices** hang off the access point they're on (Wi-Fi Diagnostics' BSSID), or one
//!   "Wi-Fi" when they don't say.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use irori_types::{
    LinkQuality, Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId,
};
use serde_json::Value;

use crate::model::{Node, NodeId};

const GENERAL_DIAGNOSTICS: u32 = 0x0033;
const THREAD_DIAGNOSTICS: u32 = 0x0035;
const WIFI_DIAGNOSTICS: u32 = 0x0036;
/// Irori's own node on the map.
pub const IRORI: &str = "irori";

/// A Thread border router, as it announces itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorderRouter {
    /// Its mDNS instance name: "Google Inc. Google Nest Hub #FF88".
    pub instance: String,
    /// Its vendor and model, when it says (`vn`, `mn`).
    pub vendor: Option<String>,
    pub model: Option<String>,
    /// Its Thread network's name (`nn`).
    pub network_name: Option<String>,
    /// Its extended address (`xa`), which Thread nodes name it by.
    pub ext_address: Option<u64>,
    /// Its Thread network's extended PAN id (`xp`).
    pub ext_pan_id: Option<u64>,
}

impl BorderRouter {
    /// From an mDNS `_meshcop._udp` announcement's TXT record.
    pub fn from_txt<'a>(instance: &str, txt: impl Fn(&str) -> Option<&'a [u8]>) -> Self {
        let text = |key| {
            txt(key)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let eight = |key| {
            txt(key)
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                .map(u64::from_be_bytes)
        };
        Self {
            instance: instance.to_owned(),
            vendor: text("vn"),
            model: text("mn"),
            network_name: text("nn"),
            ext_address: eight("xa"),
            ext_pan_id: eight("xp"),
        }
    }

    fn id(&self) -> String {
        match self.ext_address {
            Some(address) => format!("br-{address:016x}"),
            None => format!("br-{}", slug(&self.instance)),
        }
    }

    fn name(&self) -> String {
        match (&self.vendor, &self.model) {
            // "Google Inc." and "Google Nest Hub" is a Google Nest Hub.
            (Some(vendor), Some(model))
                if !model.starts_with(vendor.split_whitespace().next().unwrap_or(vendor)) =>
            {
                format!("{vendor} {model}")
            }
            (_, Some(model)) => model.clone(),
            _ => self
                .instance
                .split(" #")
                .next()
                .unwrap_or(&self.instance)
                .to_owned(),
        }
    }
}

/// What mDNS says about a border router.
#[derive(Debug)]
pub enum Heard {
    /// Announced, by its mDNS name.
    Seen(String, BorderRouter),
    Gone(String),
}

/// The mDNS service a Thread border router announces.
const MESHCOP: &str = "_meshcop._udp.local.";

/// Listens for border routers announcing themselves. Without mDNS there are none to hear, and
/// the map still draws: a mesh with none reaches Irori through one it doesn't announce.
pub fn listen_for_border_routers() -> (
    tokio::sync::mpsc::Receiver<Heard>,
    Option<tokio::task::JoinHandle<()>>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let browse = mdns_sd::ServiceDaemon::new()
        .and_then(|daemon| daemon.browse(MESHCOP).map(|browse| (daemon, browse)));
    let (daemon, browse) = match browse {
        Ok(started) => started,
        Err(e) => {
            tracing::warn!(error = %e, "can't listen for Thread border routers");
            return (rx, None);
        }
    };
    let task = tokio::spawn(async move {
        // The daemon has to outlive the browse.
        let _daemon = daemon;
        while let Ok(event) = browse.recv_async().await {
            let heard = match event {
                mdns_sd::ServiceEvent::ServiceResolved(service) => {
                    let name = service.get_fullname().to_owned();
                    let instance = name
                        .strip_suffix(&format!(".{MESHCOP}"))
                        .unwrap_or(&name)
                        .replace("\\032", " ");
                    let router = BorderRouter::from_txt(&instance, |key| {
                        service.get_property_val(key).flatten()
                    });
                    Heard::Seen(name, router)
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, name) => Heard::Gone(name),
                _ => continue,
            };
            if tx.send(heard).await.is_err() {
                break;
            }
        }
    });
    (rx, Some(task))
}

/// One node as the map needs it: the node, the Irori device it is, and its name.
pub struct Member<'a> {
    pub node: &'a Node,
    pub device: UniqueId,
    pub name: String,
}

/// How a node says it's connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    Thread,
    Wifi,
    Ethernet,
    Unknown,
}

/// The map, from every node and the border routers heard.
pub fn map(members: &[Member<'_>], routers: &[BorderRouter]) -> Network {
    let mut nodes = vec![NetworkNode {
        id: IRORI.to_owned(),
        name: "Irori".to_owned(),
        role: NetworkRole::Controller,
        medium: None,
        device_unique_id: None,
        detail: Some("Matter controller".to_owned()),
    }];
    let mut links: BTreeMap<(String, String), NetworkLink> = BTreeMap::new();
    let mut add_link =
        |from: &str, to: &str, medium, quality: Option<LinkQuality>, reading: Option<String>| {
            let key = if from < to {
                (from.to_owned(), to.to_owned())
            } else {
                (to.to_owned(), from.to_owned())
            };
            let link = links.entry(key).or_insert_with(|| NetworkLink {
                from: from.to_owned(),
                to: to.to_owned(),
                medium: Some(medium),
                quality,
                reading: reading.clone(),
                detail: None,
            });
            // Both ends report a link: keep the better word on it, and what it was read as.
            if quality > link.quality {
                link.quality = quality;
                link.reading = reading;
            }
        };

    // Border routers are on the home's network, beside Irori.
    let mut by_address: BTreeMap<u64, String> = BTreeMap::new();
    for router in routers {
        let id = router.id();
        if nodes.iter().any(|node| node.id == id) {
            continue;
        }
        if let Some(address) = router.ext_address {
            by_address.insert(address, id.clone());
        }
        nodes.push(NetworkNode {
            id: id.clone(),
            name: cut(&router.name(), 100),
            role: NetworkRole::BorderRouter,
            medium: Some(NetworkMedium::Thread),
            device_unique_id: None,
            detail: Some(cut(
                &match &router.network_name {
                    Some(network) => format!("Thread border router, network {network}"),
                    None => "Thread border router".to_owned(),
                },
                200,
            )),
        });
        add_link(&id, IRORI, NetworkMedium::Ip, None, None);
    }

    // Each node's own place first, so a neighbour can be found by its address.
    let id_of = |node: NodeId| format!("node-{node:x}");
    for member in members {
        let node = member.node;
        if let Some(address) = ext_address(node) {
            by_address.insert(address, id_of(node.id));
        }
    }

    let mut access_points: BTreeSet<String> = BTreeSet::new();
    let mut threaded: Vec<String> = Vec::new();
    let mut strangers: BTreeMap<u64, String> = BTreeMap::new();
    for member in members {
        let node = member.node;
        let id = id_of(node.id);
        let link = connection(node);
        let role = match link {
            Link::Thread => thread_role(node),
            _ => NetworkRole::Device,
        };
        let mut detail = Vec::new();
        match link {
            Link::Thread => detail.push(role_words(role).to_owned()),
            Link::Wifi => detail.push("Wi-Fi".to_owned()),
            Link::Ethernet => detail.push("Wired".to_owned()),
            Link::Unknown => {}
        }
        if let Some(channel) = node
            .number(0, THREAD_DIAGNOSTICS, 0)
            .filter(|_| link == Link::Thread)
        {
            detail.push(format!("channel {channel}"));
        }
        if !node.available {
            detail.push("not answering".to_owned());
        }
        nodes.push(NetworkNode {
            id: id.clone(),
            name: cut(&member.name, 100),
            role,
            medium: Some(match link {
                Link::Thread => NetworkMedium::Thread,
                Link::Wifi => NetworkMedium::Wifi,
                Link::Ethernet => NetworkMedium::Ethernet,
                Link::Unknown => NetworkMedium::Ip,
            }),
            device_unique_id: Some(member.device.clone()),
            detail: (!detail.is_empty()).then(|| cut(&capitalised(&detail.join(", ")), 200)),
        });
        match link {
            Link::Wifi => {
                let access_point = node
                    .value(0, WIFI_DIAGNOSTICS, 0)
                    .and_then(Value::as_str)
                    .and_then(|bssid| base64::engine::general_purpose::STANDARD.decode(bssid).ok())
                    .filter(|bytes| bytes.len() == 6)
                    .map_or_else(
                        || "wifi".to_owned(),
                        |bytes| {
                            let hex: Vec<String> =
                                bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                            format!("ap-{}", hex.join(""))
                        },
                    );
                access_points.insert(access_point.clone());
                let rssi = node
                    .number(0, WIFI_DIAGNOSTICS, 4)
                    .map(|rssi| rssi as i64)
                    .filter(|&rssi| rssi < 0 && rssi > -128);
                add_link(
                    &id,
                    &access_point,
                    NetworkMedium::Wifi,
                    rssi.map(LinkQuality::from_rssi),
                    rssi.map(dbm),
                );
            }
            Link::Ethernet => add_link(
                &id,
                IRORI,
                NetworkMedium::Ethernet,
                None,
                Some("LAN".to_owned()),
            ),
            Link::Unknown => add_link(&id, IRORI, NetworkMedium::Ip, None, None),
            Link::Thread => {
                threaded.push(id.clone());
                for neighbour in neighbours(node) {
                    let other = match by_address.get(&neighbour.address) {
                        Some(other) => other.clone(),
                        // A router no one here knows passes messages on too: it's drawn, so
                        // the path through it is. A stranger's child isn't.
                        None if neighbour.router => strangers
                            .entry(neighbour.address)
                            .or_insert_with(|| format!("thread-{:016x}", neighbour.address))
                            .clone(),
                        None => continue,
                    };
                    let (from, to) = if neighbour.child {
                        (other.as_str(), id.as_str())
                    } else {
                        (id.as_str(), other.as_str())
                    };
                    add_link(
                        from,
                        to,
                        NetworkMedium::Thread,
                        neighbour.quality,
                        neighbour.reading.clone(),
                    );
                }
            }
        }
    }
    for (address, id) in &strangers {
        if by_address.contains_key(address) {
            continue;
        }
        nodes.push(NetworkNode {
            id: id.clone(),
            name: "Thread router".to_owned(),
            role: NetworkRole::Router,
            medium: Some(NetworkMedium::Thread),
            device_unique_id: None,
            detail: Some("On the Thread network, not one of Irori's devices".to_owned()),
        });
    }
    for access_point in &access_points {
        let name = if access_point == "wifi" {
            "Wi-Fi".to_owned()
        } else {
            "Wi-Fi access point".to_owned()
        };
        let detail = access_point.strip_prefix("ap-").map(|hex| {
            let pairs: Vec<&str> = (0..hex.len())
                .step_by(2)
                .map(|at| &hex[at..at + 2])
                .collect();
            pairs.join(":")
        });
        nodes.push(NetworkNode {
            id: access_point.clone(),
            name,
            role: NetworkRole::AccessPoint,
            medium: Some(NetworkMedium::Wifi),
            device_unique_id: None,
            detail,
        });
        add_link(access_point, IRORI, NetworkMedium::Ip, None, None);
    }

    // A Thread mesh that no announced border router is part of still reaches Irori through
    // one: drawn as one, so the mesh isn't an island.
    // The Thread networks the border routers say they run, for a word about the whole.
    let thread_names: BTreeSet<&str> = routers
        .iter()
        .filter_map(|router| router.network_name.as_deref())
        .collect();
    let detail = match thread_names.len() {
        0 => None,
        1 => thread_names
            .first()
            .map(|name| cut(&format!("Thread network {name}"), 200)),
        _ => Some(cut(
            &format!(
                "Thread networks {}",
                thread_names.into_iter().collect::<Vec<_>>().join(", ")
            ),
            200,
        )),
    };
    let mut network = Network {
        name: Some("Matter".to_owned()),
        detail,
        scanned_at: None,
        nodes,
        links: links.into_values().collect(),
    };
    let reached = reachable(&network);
    let stranded: Vec<&String> = threaded
        .iter()
        .filter(|id| !reached.contains(id.as_str()))
        .collect();
    if !stranded.is_empty() {
        let unseen = "br-unseen".to_owned();
        network.nodes.push(NetworkNode {
            id: unseen.clone(),
            name: "Border router".to_owned(),
            role: NetworkRole::BorderRouter,
            medium: Some(NetworkMedium::Thread),
            device_unique_id: None,
            detail: Some(
                "Thread devices reach Irori through one; it isn't announcing itself".to_owned(),
            ),
        });
        network.links.push(NetworkLink {
            from: unseen.clone(),
            to: IRORI.to_owned(),
            medium: Some(NetworkMedium::Ip),
            quality: None,
            reading: None,
            detail: None,
        });
        // One link into each stranded piece of mesh, through a router when it has one.
        let mut joined: BTreeSet<String> = BTreeSet::new();
        for id in stranded {
            if joined.contains(id.as_str()) {
                continue;
            }
            let piece = component(&network, id);
            let entry = network
                .nodes
                .iter()
                .filter(|node| piece.contains(node.id.as_str()))
                .find(|node| matches!(node.role, NetworkRole::Router))
                .map_or_else(|| id.clone(), |node| node.id.clone());
            network.links.push(NetworkLink {
                from: entry,
                to: unseen.clone(),
                medium: Some(NetworkMedium::Thread),
                quality: None,
                reading: None,
                detail: Some("Through the Thread network".to_owned()),
            });
            joined.extend(piece);
        }
    }
    network
}

/// Every node with a path to Irori.
fn reachable(network: &Network) -> BTreeSet<String> {
    component(network, IRORI)
}

/// The nodes `start` is joined to, itself included.
fn component(network: &Network, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::from([start.to_owned()]);
    let mut queue = vec![start.to_owned()];
    while let Some(at) = queue.pop() {
        for link in &network.links {
            let other = if link.from == at {
                &link.to
            } else if link.to == at {
                &link.from
            } else {
                continue;
            };
            if seen.insert(other.clone()) {
                queue.push(other.clone());
            }
        }
    }
    seen
}

/// Wi-Fi, Ethernet or Thread, from the interface that's up; else from the diagnostics it has.
fn connection(node: &Node) -> Link {
    let interfaces = node
        .value(0, GENERAL_DIAGNOSTICS, 0)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let kinds: Vec<u64> = interfaces
        .iter()
        .filter(|interface| interface.get("1").and_then(Value::as_bool) != Some(false))
        .filter_map(|interface| interface.get("7").and_then(Value::as_u64))
        .collect();
    if kinds.contains(&4) {
        return Link::Thread;
    }
    if kinds.contains(&1) {
        return Link::Wifi;
    }
    if kinds.contains(&2) {
        return Link::Ethernet;
    }
    if node.value(0, THREAD_DIAGNOSTICS, 1).is_some() {
        Link::Thread
    } else if node.has_cluster(0, WIFI_DIAGNOSTICS) {
        Link::Wifi
    } else {
        Link::Unknown
    }
}

/// Its role on the Thread network (Thread Network Diagnostics' RoutingRole).
fn thread_role(node: &Node) -> NetworkRole {
    match node.value(0, THREAD_DIAGNOSTICS, 1).and_then(Value::as_u64) {
        Some(2) => NetworkRole::SleepyEndDevice,
        Some(5 | 6) => NetworkRole::Router,
        _ => NetworkRole::EndDevice,
    }
}

fn role_words(role: NetworkRole) -> &'static str {
    match role {
        NetworkRole::Router => "Thread router",
        NetworkRole::SleepyEndDevice => "Thread, asleep between messages",
        _ => "Thread",
    }
}

/// Its own extended address: `ExtAddress`, else the one inside its Thread link-local address
/// (the interface id is the extended address with its universal/local bit flipped).
fn ext_address(node: &Node) -> Option<u64> {
    if let Some(address) = node
        .value(0, THREAD_DIAGNOSTICS, 0x003F)
        .and_then(address_of)
    {
        return Some(address);
    }
    let interfaces = node.value(0, GENERAL_DIAGNOSTICS, 0)?.as_array()?;
    interfaces
        .iter()
        .filter(|interface| interface.get("7").and_then(Value::as_u64) == Some(4))
        .filter_map(|interface| interface.get("6")?.as_array())
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
        .find(|bytes| bytes.len() == 16 && bytes[0] == 0xfe && bytes[1] & 0xc0 == 0x80)
        .map(|bytes| {
            let id: [u8; 8] = bytes[8..].try_into().expect("eight bytes");
            u64::from_be_bytes(id) ^ (0x02 << 56)
        })
}

/// An extended address as the server writes one: a number (it can be past what JavaScript
/// counts exactly, so the server writes it whole), or a decimal or hex string.
fn address_of(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text
            .parse()
            .ok()
            .or_else(|| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()),
        _ => None,
    }
}

struct Neighbour {
    address: u64,
    quality: Option<LinkQuality>,
    /// What the quality was read from: `-58 dBm`, or `LQI 2` (Thread's 0-3).
    reading: Option<String>,
    /// It's this node's child.
    child: bool,
    /// It routes: a full Thread device that isn't anyone's child here.
    router: bool,
}

fn neighbours(node: &Node) -> Vec<Neighbour> {
    let Some(table) = node
        .value(0, THREAD_DIAGNOSTICS, 7)
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    table
        .iter()
        .filter_map(|entry| {
            let address = address_of(entry.get("0")?)?;
            let rssi = entry
                .get("6")
                .and_then(Value::as_i64)
                .filter(|&rssi| rssi < 0 && rssi > -128);
            // Link quality 0-3, the Thread way, when there's no signal strength.
            let raw_lqi = entry.get("5").and_then(Value::as_u64);
            let lqi = raw_lqi.map(|lqi| match lqi {
                3.. => LinkQuality::Excellent,
                2 => LinkQuality::Good,
                1 => LinkQuality::Fair,
                0 => LinkQuality::Poor,
            });
            let child = entry.get("13").and_then(Value::as_bool) == Some(true);
            Some(Neighbour {
                address,
                quality: rssi.map(LinkQuality::from_rssi).or(lqi),
                reading: rssi
                    .map(dbm)
                    .or_else(|| raw_lqi.map(|lqi| format!("LQI {lqi}"))),
                child,
                router: !child && entry.get("11").and_then(Value::as_bool) != Some(false),
            })
        })
        .collect()
}

/// A signal strength as it's drawn on its link.
fn dbm(rssi: i64) -> String {
    format!("{rssi} dBm")
}

fn slug(text: &str) -> String {
    let slug: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    slug.trim_matches('-').to_owned()
}

fn cut(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Node {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("a fixture");
        Node::from_json(&serde_json::from_str(&text).expect("json")).expect("a node")
    }

    fn member<'a>(node: &'a Node, name: &str) -> Member<'a> {
        Member {
            node,
            device: UniqueId::try_from(format!("dev-{}", node.id)).expect("an id"),
            name: name.to_owned(),
        }
    }

    fn find<'a>(network: &'a Network, id: &str) -> &'a NetworkNode {
        network
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("no {id} in {network:#?}"))
    }

    fn linked<'a>(network: &'a Network, a: &str, b: &str) -> Option<&'a NetworkLink> {
        network
            .links
            .iter()
            .find(|link| (link.from == a && link.to == b) || (link.from == b && link.to == a))
    }

    #[test]
    fn a_wired_and_a_wifi_device_hang_off_irori_and_the_wifi() {
        let lab = fixture("lab_plug");
        let mut wifi = fixture("mock_dimmable_light");
        wifi.id = 9;
        let network = map(&[member(&lab, "Plug"), member(&wifi, "Lamp")], &[]);
        assert_eq!(network.validate(), Ok(()));
        let plug = find(&network, &format!("node-{:x}", lab.id));
        assert_eq!(plug.medium, Some(NetworkMedium::Ethernet));
        assert!(linked(&network, &plug.id, IRORI).is_some());
        // The lamp says which access point it's on, and how well it hears it (-61 dBm).
        let lamp = find(&network, "node-9");
        assert_eq!(lamp.medium, Some(NetworkMedium::Wifi));
        let to_ap = network
            .links
            .iter()
            .find(|link| link.from == "node-9")
            .expect("a link");
        assert!(
            to_ap.to.starts_with("ap-") || to_ap.to == "wifi",
            "{to_ap:?}"
        );
        assert_eq!(to_ap.quality, Some(LinkQuality::Good));
        assert_eq!(to_ap.reading.as_deref(), Some("-61 dBm"));
        assert!(linked(&network, &to_ap.to, IRORI).is_some());
        assert_eq!(
            linked(&network, &plug.id, IRORI).and_then(|link| link.reading.as_deref()),
            Some("LAN")
        );
        assert_eq!(network.name.as_deref(), Some("Matter"));
    }

    #[test]
    fn a_thread_router_hears_its_neighbours_and_reaches_irori_through_a_border_router() {
        // An Eve Energy plug: a Thread router with eight neighbours.
        let eve = fixture("eve_energy_plug");
        let neighbours = neighbours(&eve);
        assert_eq!(neighbours.len(), 8);
        // One of them is a border router announcing itself with that address.
        let router = BorderRouter {
            instance: "Google Inc. Google Nest Hub #FF88".to_owned(),
            vendor: Some("Google Inc.".to_owned()),
            model: Some("Google Nest Hub".to_owned()),
            network_name: Some("MyHome23".to_owned()),
            ext_address: Some(neighbours[1].address),
            ext_pan_id: None,
        };
        let network = map(&[member(&eve, "Eve Energy")], std::slice::from_ref(&router));
        assert_eq!(network.validate(), Ok(()));
        let node = find(&network, &format!("node-{:x}", eve.id));
        assert_eq!(node.role, NetworkRole::Router);
        assert_eq!(node.medium, Some(NetworkMedium::Thread));
        let border = find(&network, &router.id());
        assert_eq!(border.name, "Google Nest Hub");
        assert_eq!(border.role, NetworkRole::BorderRouter);
        let heard = linked(&network, &node.id, &border.id).expect("the router hears it");
        assert!(
            heard
                .reading
                .as_deref()
                .is_some_and(|reading| reading.ends_with(" dBm") || reading.starts_with("LQI ")),
            "{heard:?}"
        );
        assert!(linked(&network, &border.id, IRORI).is_some());
        // The whole is the Thread network the border router runs.
        assert_eq!(network.detail.as_deref(), Some("Thread network MyHome23"));
        // Its other routing neighbours are on the map too, as routers Irori doesn't know.
        assert!(
            network
                .nodes
                .iter()
                .any(|node| node.id.starts_with("thread-") && node.role == NetworkRole::Router)
        );
        // And nothing new appears: the unseen border router is only for a stranded mesh.
        assert!(!network.nodes.iter().any(|node| node.id == "br-unseen"));
    }

    #[test]
    fn a_sleepy_sensor_with_no_border_router_announced_still_reaches_irori() {
        let sensor = fixture("eve_contact_sensor");
        let network = map(&[member(&sensor, "Door")], &[]);
        assert_eq!(network.validate(), Ok(()));
        let node = find(&network, &format!("node-{:x}", sensor.id));
        assert_eq!(node.role, NetworkRole::SleepyEndDevice);
        assert!(find(&network, "br-unseen").role == NetworkRole::BorderRouter);
        assert!(reachable(&network).contains(&node.id), "{network:#?}");
    }

    #[test]
    fn two_thread_devices_that_hear_each_other_are_one_link() {
        let eve = fixture("eve_energy_plug");
        let mut other = fixture("silabs_light_switch");
        other.id = 77;
        // The switch's own address is one the plug hears.
        let heard = neighbours(&eve)[0].address;
        other.set(
            crate::model::Path::new(0, THREAD_DIAGNOSTICS, 0x003F),
            serde_json::json!(heard),
        );
        let network = map(&[member(&eve, "Plug"), member(&other, "Switch")], &[]);
        let plug = format!("node-{:x}", eve.id);
        let between: Vec<_> = network
            .links
            .iter()
            .filter(|link| {
                (link.from == plug && link.to == "node-4d")
                    || (link.from == "node-4d" && link.to == plug)
            })
            .collect();
        assert_eq!(between.len(), 1, "{network:#?}");
    }

    #[test]
    fn a_border_router_reads_from_its_announcement() {
        let txt = |key: &str| -> Option<&'static [u8]> {
            match key {
                "vn" => Some(b"Apple Inc."),
                "mn" => Some(b"HomePod"),
                "nn" => Some(b"MyHome"),
                "xa" => Some(&[0xde, 0x67, 0xaa, 0x6b, 0xb1, 0xf1, 0xff, 0x88]),
                _ => None,
            }
        };
        let router = BorderRouter::from_txt("HomePod #1234", txt);
        assert_eq!(router.ext_address, Some(0xde67_aa6b_b1f1_ff88));
        assert_eq!(router.name(), "Apple Inc. HomePod");
        assert_eq!(router.id(), "br-de67aa6bb1f1ff88");
    }

    #[test]
    fn an_address_reads_from_a_number_or_text() {
        assert_eq!(
            address_of(&serde_json::json!(13418684826835773064_u64)),
            Some(13418684826835773064)
        );
        assert_eq!(
            address_of(&serde_json::json!("13418684826835773064")),
            Some(13418684826835773064)
        );
        assert_eq!(
            address_of(&serde_json::json!("0xde67aa6bb1f1ff88")),
            Some(0xde67_aa6b_b1f1_ff88)
        );
    }
}
