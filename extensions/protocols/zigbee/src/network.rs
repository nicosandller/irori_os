//! The map of the Zigbee network (`docs/specs/protocols.md` §6.8): Irori, the coordinator it
//! runs the mesh from, and every paired device, linked by who hears whom.
//!
//! - **What's on it** is Zigbee2MQTT's `bridge/devices`: every paired device, whether it
//!   routes, and whether a battery powers it (a battery end device sleeps between messages).
//! - **Who hears whom** is Zigbee2MQTT's network map, a scan that asks every router for its
//!   neighbour table: each entry is a link, with its LQI and whether the neighbour is the
//!   router's parent or child. A scan is slow and busy on the radio, so it runs a minute after
//!   the bridge comes up, every few hours after that, and when someone presses "Scan network".
//!   Until the first answer, every device hangs off the coordinator with its link unmeasured.
//! - **The channel and PAN** are in `bridge/info`.

use std::collections::{BTreeMap, BTreeSet};

use irori_ha_discovery::bridge;
use irori_types::{
    LinkQuality, Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, Timestamp, UniqueId,
};
use serde_json::Value;

/// Irori's own node on the map.
const IRORI: &str = "irori";
/// The coordinator's id on the map before `bridge/info` says its address.
const COORDINATOR: &str = "coordinator";

/// How the coordinator is reached from this machine, from the serial port setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Cable {
    /// Plugged into this machine: a USB dongle.
    #[default]
    Usb,
    /// Over the network (`tcp://`): an Ethernet coordinator, or a dongle bridged to TCP.
    Lan,
}

impl Cable {
    pub fn of(serial_port: &str) -> Self {
        if serial_port.trim().starts_with("tcp://") {
            Self::Lan
        } else {
            Self::Usb
        }
    }
}

/// What the map is drawn from, as Zigbee2MQTT has said it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Known {
    pub cable: Cable,
    pub info: Info,
    pub devices: Vec<Paired>,
    pub scan: Option<Scan>,
}

impl Known {
    pub fn new(cable: Cable) -> Self {
        Self {
            cable,
            info: Info::default(),
            devices: Vec::new(),
            scan: None,
        }
    }
}

/// From `bridge/info`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub channel: Option<u64>,
    pub pan_id: Option<u64>,
    /// The coordinator's IEEE address.
    pub coordinator: Option<String>,
    /// Its radio's stack, as Zigbee2MQTT names it: `EmberZNet`, `zStack3x0`.
    pub adapter: Option<String>,
}

/// What `bridge/info` says about the network, or `None` for a payload that isn't bridge info.
pub fn info(payload: &[u8]) -> Option<Info> {
    let info: Value = serde_json::from_slice(payload).ok()?;
    info.get("version")?;
    let network = info.get("network");
    let coordinator = info.get("coordinator");
    Some(Info {
        channel: network
            .and_then(|network| network.get("channel"))
            .and_then(Value::as_u64),
        pan_id: network
            .and_then(|network| network.get("pan_id"))
            .and_then(Value::as_u64),
        coordinator: coordinator
            .and_then(|coordinator| coordinator.get("ieee_address"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        adapter: coordinator
            .and_then(|coordinator| coordinator.get("type"))
            .and_then(Value::as_str)
            .filter(|kind| !kind.is_empty())
            .map(str::to_owned),
    })
}

/// A paired device, from `bridge/devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    pub ieee: String,
    pub name: String,
    pub kind: Kind,
    /// A battery powers it: an end device that's asleep between messages.
    pub battery: bool,
    /// Its vendor and model, when Zigbee2MQTT knows them.
    pub model: Option<String>,
}

/// What a device does on the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Coordinator,
    Router,
    EndDevice,
}

/// Every device on `bridge/devices`, or `None` for a payload that isn't the list. One that
/// isn't a mesh device (a Green Power switch, one still being interviewed) is left off.
pub fn devices(payload: &[u8]) -> Option<Vec<Paired>> {
    let list: Vec<Value> = serde_json::from_slice(payload).ok()?;
    Some(
        list.iter()
            .filter_map(|device| {
                let ieee = device.get("ieee_address")?.as_str()?.to_owned();
                let kind = match device.get("type")?.as_str()? {
                    "Coordinator" => Kind::Coordinator,
                    "Router" => Kind::Router,
                    "EndDevice" => Kind::EndDevice,
                    _ => return None,
                };
                let name = device
                    .get("friendly_name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(&ieee)
                    .to_owned();
                let definition = device.get("definition");
                let text = |key| {
                    definition
                        .and_then(|definition| definition.get(key))
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                };
                let model = match (text("vendor"), text("model")) {
                    (Some(vendor), Some(model)) => Some(format!("{vendor} {model}")),
                    (None, Some(model)) => Some(model.to_owned()),
                    _ => None,
                };
                Some(Paired {
                    battery: device.get("power_source").and_then(Value::as_str) == Some("Battery"),
                    ieee,
                    name,
                    kind,
                    model,
                })
            })
            .collect(),
    )
}

/// What the last scan heard.
#[derive(Debug, Clone, PartialEq)]
pub struct Scan {
    pub at: Timestamp,
    pub heard: Vec<Heard>,
}

/// One entry of a router's neighbour table: `target` hears `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub source: String,
    pub target: String,
    pub lqi: Option<u64>,
    /// What `source` is to `target`: Zigbee's 0 parent, 1 child, 2 sibling.
    pub relationship: Option<u64>,
}

/// Zigbee2MQTT's answer to a network map request: what was heard, or why it couldn't scan.
pub fn scan(payload: &[u8], at: Timestamp) -> Result<Scan, String> {
    let answer: Value =
        serde_json::from_slice(payload).map_err(|_| "the answer wasn't JSON".to_owned())?;
    if answer.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Zigbee2MQTT couldn't scan the network")
            .to_owned());
    }
    let links = answer
        .pointer("/data/value/links")
        .and_then(Value::as_array)
        .ok_or("the answer had no links")?;
    let end = |link: &Value, side: &str| {
        link.get(side)
            .and_then(|end| end.get("ieeeAddr"))
            .or_else(|| link.get(format!("{side}IeeeAddr")))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Ok(Scan {
        at,
        heard: links
            .iter()
            .filter_map(|link| {
                Some(Heard {
                    source: end(link, "source")?,
                    target: end(link, "target")?,
                    lqi: link
                        .get("lqi")
                        .or_else(|| link.get("linkquality"))
                        .and_then(Value::as_u64),
                    relationship: link.get("relationship").and_then(Value::as_u64),
                })
            })
            .collect(),
    })
}

/// The map, from what's known.
pub fn draw(known: &Known) -> Network {
    let coordinator_ieee = known.info.coordinator.clone().or_else(|| {
        known
            .devices
            .iter()
            .find(|device| device.kind == Kind::Coordinator)
            .map(|device| device.ieee.clone())
    });
    let coordinator = coordinator_ieee
        .clone()
        .unwrap_or_else(|| COORDINATOR.to_owned());
    let (cable_medium, cable_word) = match known.cable {
        Cable::Usb => (NetworkMedium::Serial, "USB"),
        Cable::Lan => (NetworkMedium::Ethernet, "LAN"),
    };
    let mut nodes = vec![
        NetworkNode {
            id: IRORI.to_owned(),
            name: "Irori".to_owned(),
            role: NetworkRole::Controller,
            medium: None,
            device_unique_id: None,
            detail: Some("Runs Zigbee2MQTT".to_owned()),
            host: None,
        },
        NetworkNode {
            id: coordinator.clone(),
            name: "Zigbee coordinator".to_owned(),
            role: NetworkRole::Coordinator,
            medium: Some(NetworkMedium::Zigbee),
            device_unique_id: coordinator_ieee
                .as_deref()
                .and_then(|ieee| UniqueId::try_from(bridge::device_identifier(ieee, true)).ok()),
            detail: known
                .info
                .adapter
                .as_ref()
                .map(|adapter| cut(&format!("{adapter} radio"), 200)),
            host: None,
        },
    ];
    let mut links = vec![NetworkLink {
        from: coordinator.clone(),
        to: IRORI.to_owned(),
        medium: Some(cable_medium),
        quality: None,
        reading: Some(cable_word.to_owned()),
        detail: None,
    }];

    let mut on_map: BTreeSet<String> = BTreeSet::from([coordinator.clone()]);
    for device in &known.devices {
        if device.kind == Kind::Coordinator || !on_map.insert(device.ieee.clone()) {
            continue;
        }
        nodes.push(NetworkNode {
            id: cut(&device.ieee, 64),
            name: cut(&device.name, 100),
            role: match (device.kind, device.battery) {
                (Kind::Router, _) => NetworkRole::Router,
                (_, true) => NetworkRole::SleepyEndDevice,
                _ => NetworkRole::EndDevice,
            },
            medium: Some(NetworkMedium::Zigbee),
            device_unique_id: UniqueId::try_from(bridge::device_identifier(&device.ieee, false))
                .ok(),
            detail: device.model.as_ref().map(|model| cut(model, 200)),
            host: None,
        });
    }

    // Each pair of devices once, whichever of them said it, with the better of the two LQIs.
    let mut heard: BTreeMap<(String, String), (NetworkLink, Option<u64>)> = BTreeMap::new();
    let mut linked: BTreeSet<String> = BTreeSet::new();
    for entry in known.scan.iter().flat_map(|scan| &scan.heard) {
        if entry.source == entry.target
            || !on_map.contains(&entry.source)
            || !on_map.contains(&entry.target)
        {
            continue;
        }
        // The far end, from the coordinator, is the child.
        let (from, to) = match entry.relationship {
            Some(0) => (&entry.target, &entry.source),
            _ => (&entry.source, &entry.target),
        };
        let key = if from < to {
            (from.clone(), to.clone())
        } else {
            (to.clone(), from.clone())
        };
        let (link, best) = heard.entry(key).or_insert_with(|| {
            let link = NetworkLink {
                from: from.clone(),
                to: to.clone(),
                medium: Some(NetworkMedium::Zigbee),
                quality: None,
                reading: None,
                detail: None,
            };
            (link, None)
        });
        if entry.lqi > *best || link.reading.is_none() {
            *best = entry.lqi;
            link.quality = entry.lqi.map(LinkQuality::from_lqi);
            link.reading = entry.lqi.map(|lqi| format!("LQI {lqi}"));
        }
        // A child's link to its parent is how it's reached, whichever way the table said it.
        if entry.relationship == Some(0) || entry.relationship == Some(1) {
            link.from = from.clone();
            link.to = to.clone();
        }
        linked.insert(entry.source.clone());
        linked.insert(entry.target.clone());
    }
    links.extend(heard.into_values().map(|(link, _)| link));

    // A device the scan didn't hear (asleep through it, joined since, or gone) still hangs off
    // the coordinator, its link unmeasured, so it's on the map rather than floating.
    let unheard = if known.scan.is_some() {
        "Not heard in the last scan"
    } else {
        "Not scanned yet"
    };
    for node in &nodes {
        if matches!(
            node.role,
            NetworkRole::Controller | NetworkRole::Coordinator
        ) || linked.contains(&node.id)
        {
            continue;
        }
        links.push(NetworkLink {
            from: node.id.clone(),
            to: coordinator.clone(),
            medium: Some(NetworkMedium::Zigbee),
            quality: None,
            reading: None,
            detail: Some(unheard.to_owned()),
        });
    }

    let mut about = Vec::new();
    if let Some(channel) = known.info.channel {
        about.push(format!("Channel {channel}"));
    }
    if let Some(pan) = known.info.pan_id {
        about.push(format!("PAN 0x{pan:04x}"));
    }
    Network {
        name: Some("Zigbee".to_owned()),
        detail: (!about.is_empty()).then(|| about.join(", ")),
        scanned_at: known.scan.as_ref().map(|scan| scan.at),
        nodes,
        links,
    }
}

fn cut(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"{"version":"2.1.3","network":{"channel":15,"pan_id":6754,
        "extended_pan_id":"0xdddddddddddddddd"},
        "coordinator":{"ieee_address":"0x00124b0001020304","type":"EmberZNet"}}"#;

    const DEVICES: &str = r#"[
        {"ieee_address":"0x00124b0001020304","type":"Coordinator","friendly_name":"Coordinator"},
        {"ieee_address":"0x000b57fffe0a0001","type":"Router","friendly_name":"Kitchen ceiling",
         "power_source":"Mains (single phase)",
         "definition":{"vendor":"IKEA","model":"LED2101G4"}},
        {"ieee_address":"0x00158d0000aa0002","type":"EndDevice","friendly_name":"Hallway motion",
         "power_source":"Battery","definition":{"vendor":"Aqara","model":"RTCGQ14LM"}},
        {"ieee_address":"0x00158d0000aa0003","type":"EndDevice","friendly_name":"Shed sensor",
         "power_source":"Battery"},
        {"ieee_address":"0x0000000000000099","type":"GreenPower","friendly_name":"Switch"}
    ]"#;

    // Z2M 2.x's raw answer, trimmed to the fields read: the coordinator hears the kitchen
    // light as its child, and the light hears the motion sensor as its child.
    const SCAN: &str = r#"{"status":"ok","data":{"type":"raw","routes":false,"value":{
        "nodes":[],
        "links":[
          {"source":{"ieeeAddr":"0x000b57fffe0a0001","networkAddress":1},
           "target":{"ieeeAddr":"0x00124b0001020304","networkAddress":0},
           "lqi":212,"relationship":1,"depth":1},
          {"source":{"ieeeAddr":"0x00124b0001020304","networkAddress":0},
           "target":{"ieeeAddr":"0x000b57fffe0a0001","networkAddress":1},
           "lqi":198,"relationship":0,"depth":0},
          {"sourceIeeeAddr":"0x00158d0000aa0002","targetIeeeAddr":"0x000b57fffe0a0001",
           "linkquality":71,"relationship":1},
          {"source":{"ieeeAddr":"0xffffffffffffffff"},"target":{"ieeeAddr":"0x000b57fffe0a0001"},
           "lqi":40,"relationship":2}
        ]}}}"#;

    fn known() -> Known {
        let mut known = Known::new(Cable::Usb);
        known.info = info(INFO.as_bytes()).expect("bridge info");
        known.devices = devices(DEVICES.as_bytes()).expect("a device list");
        known
    }

    fn find<'a>(network: &'a Network, id: &str) -> &'a NetworkNode {
        network
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("no node {id}"))
    }

    fn link<'a>(network: &'a Network, from: &str, to: &str) -> Option<&'a NetworkLink> {
        network
            .links
            .iter()
            .find(|link| link.from == from && link.to == to)
    }

    #[test]
    fn before_a_scan_every_device_hangs_off_the_coordinator_unmeasured() {
        let network = draw(&known());
        assert_eq!(network.validate(), Ok(()));
        assert_eq!(network.name.as_deref(), Some("Zigbee"));
        assert_eq!(network.detail.as_deref(), Some("Channel 15, PAN 0x1a62"));
        assert_eq!(network.scanned_at, None);
        let coordinator = find(&network, "0x00124b0001020304");
        assert_eq!(coordinator.role, NetworkRole::Coordinator);
        assert_eq!(
            coordinator.device_unique_id.as_ref().map(UniqueId::as_str),
            Some("zigbee2mqtt_bridge_0x00124b0001020304")
        );
        let cable = link(&network, "0x00124b0001020304", IRORI).expect("the cable");
        assert_eq!(cable.reading.as_deref(), Some("USB"));
        assert_eq!(cable.medium, Some(NetworkMedium::Serial));
        // The light routes; the battery sensors sleep; the Green Power switch isn't drawn.
        assert_eq!(
            find(&network, "0x000b57fffe0a0001").role,
            NetworkRole::Router
        );
        let motion = find(&network, "0x00158d0000aa0002");
        assert_eq!(motion.role, NetworkRole::SleepyEndDevice);
        assert_eq!(motion.detail.as_deref(), Some("Aqara RTCGQ14LM"));
        assert_eq!(
            motion.device_unique_id.as_ref().map(UniqueId::as_str),
            Some("zigbee2mqtt_0x00158d0000aa0002")
        );
        assert!(
            !network
                .nodes
                .iter()
                .any(|node| node.id == "0x0000000000000099")
        );
        let unscanned = link(&network, "0x00158d0000aa0002", "0x00124b0001020304")
            .expect("hangs off the coordinator");
        assert_eq!(unscanned.quality, None);
        assert_eq!(unscanned.detail.as_deref(), Some("Not scanned yet"));
    }

    #[test]
    fn a_scan_links_who_hears_whom_with_the_better_lqi_and_the_child_further_out() {
        let mut known = known();
        let at = Timestamp::from_second(1_790_000_000).expect("a time");
        known.scan = Some(scan(SCAN.as_bytes(), at).expect("a scan"));
        let network = draw(&known);
        assert_eq!(network.validate(), Ok(()));
        assert_eq!(network.scanned_at, Some(at));

        // Heard from both ends: one link, the light further out, at the better LQI.
        let light = link(&network, "0x000b57fffe0a0001", "0x00124b0001020304")
            .expect("the light is the coordinator's child");
        assert_eq!(light.reading.as_deref(), Some("LQI 212"));
        assert_eq!(light.quality, Some(LinkQuality::Excellent));
        assert!(link(&network, "0x00124b0001020304", "0x000b57fffe0a0001").is_none());

        // The motion sensor reaches the coordinator through the light.
        let motion =
            link(&network, "0x00158d0000aa0002", "0x000b57fffe0a0001").expect("through the light");
        assert_eq!(motion.quality, Some(LinkQuality::Fair));
        assert!(link(&network, "0x00158d0000aa0002", "0x00124b0001020304").is_none());

        // The shed sensor wasn't heard; a stranger's neighbour entry isn't drawn.
        let shed = link(&network, "0x00158d0000aa0003", "0x00124b0001020304").expect("unheard");
        assert_eq!(shed.detail.as_deref(), Some("Not heard in the last scan"));
        assert!(
            !network
                .nodes
                .iter()
                .any(|node| node.id == "0xffffffffffffffff")
        );
    }

    #[test]
    fn a_coordinator_over_the_network_is_on_the_lan() {
        let mut known = known();
        known.cable = Cable::of("tcp://192.168.1.40:6638");
        let network = draw(&known);
        let cable = link(&network, "0x00124b0001020304", IRORI).expect("the cable");
        assert_eq!(cable.reading.as_deref(), Some("LAN"));
        assert_eq!(Cable::of("/dev/ttyUSB0"), Cable::Usb);
    }

    #[test]
    fn a_scan_that_failed_says_why() {
        let at = Timestamp::from_second(0).expect("a time");
        assert_eq!(
            scan(br#"{"status":"error","error":"Timeout"}"#, at),
            Err("Timeout".to_owned())
        );
        assert!(scan(b"nope", at).is_err());
    }

    #[test]
    fn with_nothing_known_yet_the_map_is_irori_and_a_coordinator() {
        let network = draw(&Known::new(Cable::Usb));
        assert_eq!(network.validate(), Ok(()));
        assert_eq!(network.nodes.len(), 2);
        assert_eq!(find(&network, COORDINATOR).device_unique_id, None);
    }
}
