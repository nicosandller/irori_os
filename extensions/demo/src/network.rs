//! The demo house's networks (`docs/specs/protocols.md` §6.8), as a real home's would be: a
//! Zigbee mesh of lights, plugs and battery sensors, a Thread mesh of Matter devices behind a
//! border router, and the cameras, doorbell and boiler on Wi-Fi with ESPHome. The garden is far
//! from everything, so its links are the weak ones.
//!
//! Its "Scan network" measures the Zigbee mesh again after a few seconds, a little differently
//! each time, the way a real scan never reads exactly the same twice.

use std::collections::BTreeMap;

use irori_protocol::types::{
    LinkQuality, Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, Timestamp,
    UniqueId, access_point_id,
};

use crate::gadgets::{Placed, placed};

const IRORI: &str = "irori";
const COORDINATOR: &str = "zigbee-coordinator";
const BORDER_ROUTER: &str = "border-router";

/// Which network a device is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum On {
    Zigbee,
    Thread,
    /// Matter, over Wi-Fi.
    MatterWifi,
    Esphome,
}

/// Where one device is: its network, what it does there, what it reaches Irori through, and
/// how well it hears that (an LQI on Zigbee, dBm otherwise).
struct Spot {
    device: &'static str,
    on: On,
    role: NetworkRole,
    through: &'static str,
    signal: i64,
}

const fn spot(
    device: &'static str,
    on: On,
    role: NetworkRole,
    through: &'static str,
    signal: i64,
) -> Spot {
    Spot {
        device,
        on,
        role,
        through,
        signal,
    }
}

use NetworkRole::{Device, Router, SleepyEndDevice};
use On::{Esphome, MatterWifi, Thread, Zigbee};

const SPOTS: &[Spot] = &[
    // The Zigbee mesh: the lights and plugs route, the battery sensors sleep.
    spot("kitchen-ceiling-light", Zigbee, Router, COORDINATOR, 212),
    spot(
        "living-room-ceiling-light",
        Zigbee,
        Router,
        COORDINATOR,
        188,
    ),
    spot("foyer-ceiling-light", Zigbee, Router, COORDINATOR, 164),
    spot(
        "living-room-wall-light",
        Zigbee,
        Router,
        "living-room-ceiling-light",
        142,
    ),
    spot(
        "dining-room-presence",
        Zigbee,
        Router,
        "kitchen-ceiling-light",
        126,
    ),
    spot("fridge-plug", Zigbee, Router, "kitchen-ceiling-light", 201),
    spot("dishwasher-plug", Zigbee, Router, "fridge-plug", 176),
    spot(
        "garage-ceiling-light",
        Zigbee,
        Router,
        "foyer-ceiling-light",
        96,
    ),
    spot("outdoor-lights", Zigbee, Router, "foyer-ceiling-light", 71),
    spot(
        "living-room-presence",
        Zigbee,
        Router,
        "living-room-wall-light",
        150,
    ),
    spot(
        "dining-room-door-1",
        Zigbee,
        SleepyEndDevice,
        "dining-room-presence",
        133,
    ),
    spot(
        "dining-room-door-2",
        Zigbee,
        SleepyEndDevice,
        "dining-room-presence",
        118,
    ),
    spot(
        "dining-room-vibration",
        Zigbee,
        SleepyEndDevice,
        "dining-room-presence",
        140,
    ),
    spot(
        "foyer-movement",
        Zigbee,
        SleepyEndDevice,
        "foyer-ceiling-light",
        181,
    ),
    spot(
        "mailbox-vibration",
        Zigbee,
        SleepyEndDevice,
        "outdoor-lights",
        44,
    ),
    spot(
        "living-room-door",
        Zigbee,
        SleepyEndDevice,
        "living-room-wall-light",
        160,
    ),
    spot(
        "living-room-climate",
        Zigbee,
        SleepyEndDevice,
        "living-room-presence",
        203,
    ),
    spot(
        "kitchen-leak",
        Zigbee,
        SleepyEndDevice,
        "dishwasher-plug",
        148,
    ),
    spot(
        "garden-climate",
        Zigbee,
        SleepyEndDevice,
        "outdoor-lights",
        62,
    ),
    spot("garden-soil", Zigbee, SleepyEndDevice, "outdoor-lights", 28),
    spot(
        "garden-light",
        Zigbee,
        SleepyEndDevice,
        "garage-ceiling-light",
        85,
    ),
    // Matter over Thread, behind the border router; the TV is Matter over Wi-Fi.
    spot("living-room-lamp", Thread, Router, BORDER_ROUTER, -58),
    spot("kitchen-thermostat", Thread, Router, BORDER_ROUTER, -64),
    spot(
        "foyer-lock",
        Thread,
        SleepyEndDevice,
        "kitchen-thermostat",
        -73,
    ),
    spot(
        "kitchen-smoke",
        Thread,
        SleepyEndDevice,
        "kitchen-thermostat",
        -69,
    ),
    spot("living-room-tv", MatterWifi, Device, "wifi", -54),
    // ESPHome on Wi-Fi.
    spot("garage-door", Esphome, Device, "wifi", -71),
    spot("garage-camera", Esphome, Device, "wifi", -77),
    spot("approach-doorbell", Esphome, Device, "wifi", -68),
    spot("living-room-camera", Esphome, Device, "wifi", -52),
    spot("garden-camera-1", Esphome, Device, "wifi", -81),
    spot("garden-camera-2", Esphome, Device, "wifi", -84),
    spot("basement-boiler", Esphome, Device, "wifi", -63),
];

/// The links across each mesh that aren't anyone's way to Irori: routers that hear each other.
const ZIGBEE_MESH: &[(&str, &str, i64)] = &[
    ("living-room-ceiling-light", "kitchen-ceiling-light", 118),
    ("foyer-ceiling-light", "living-room-ceiling-light", 102),
    ("dishwasher-plug", COORDINATOR, 62),
    ("living-room-presence", "dining-room-presence", 109),
];
const THREAD_MESH: &[(&str, &str, i64)] = &[("kitchen-thermostat", "living-room-lamp", -71)];

/// Every network in the house, as measured by scan number `scan` at `scanned_at`.
pub fn draw(scan: u32, scanned_at: Option<Timestamp>) -> Vec<Network> {
    let devices: BTreeMap<&str, Placed> = placed()
        .into_iter()
        .map(|placed| (placed.unique_id, placed))
        .collect();
    let irori = || NetworkNode {
        id: IRORI.to_owned(),
        name: "Irori".to_owned(),
        role: NetworkRole::Controller,
        medium: None,
        device_unique_id: None,
        detail: None,
    };
    let wifi = || NetworkNode {
        id: access_point_id(None),
        name: "Wi-Fi access point".to_owned(),
        role: NetworkRole::AccessPoint,
        medium: Some(NetworkMedium::Wifi),
        device_unique_id: None,
        detail: Some("The house's Wi-Fi".to_owned()),
    };
    let wired = |from: &str, medium, reading: &str| NetworkLink {
        from: from.to_owned(),
        to: IRORI.to_owned(),
        medium: Some(medium),
        quality: None,
        reading: Some(reading.to_owned()),
        detail: None,
    };

    let mut zigbee = Network {
        name: Some("Zigbee".to_owned()),
        detail: Some("Channel 15, PAN 0x1a62".to_owned()),
        scanned_at,
        nodes: vec![
            irori(),
            NetworkNode {
                id: COORDINATOR.to_owned(),
                name: "Zigbee coordinator".to_owned(),
                role: NetworkRole::Coordinator,
                medium: Some(NetworkMedium::Zigbee),
                device_unique_id: None,
                detail: Some("Virtual USB dongle".to_owned()),
            },
        ],
        links: vec![wired(COORDINATOR, NetworkMedium::Serial, "USB")],
    };
    let mut matter = Network {
        name: Some("Matter".to_owned()),
        detail: Some("Thread network irori-demo".to_owned()),
        scanned_at: None,
        nodes: vec![
            irori(),
            NetworkNode {
                id: BORDER_ROUTER.to_owned(),
                name: "Border router".to_owned(),
                role: NetworkRole::BorderRouter,
                medium: Some(NetworkMedium::Thread),
                device_unique_id: None,
                detail: Some("Thread border router, network irori-demo".to_owned()),
            },
            wifi(),
        ],
        links: vec![
            wired(BORDER_ROUTER, NetworkMedium::Ethernet, "LAN"),
            wired(&access_point_id(None), NetworkMedium::Ethernet, "LAN"),
        ],
    };
    let mut esphome = Network {
        name: Some("ESPHome".to_owned()),
        detail: Some("Wi-Fi, native API".to_owned()),
        scanned_at: None,
        nodes: vec![irori(), wifi()],
        links: vec![wired(
            &access_point_id(None),
            NetworkMedium::Ethernet,
            "LAN",
        )],
    };

    let mut placed_here: Vec<&str> = Vec::new();
    for spot in SPOTS {
        let Some(device) = devices.get(spot.device) else {
            continue;
        };
        placed_here.push(spot.device);
        let (network, medium) = match spot.on {
            Zigbee => (&mut zigbee, NetworkMedium::Zigbee),
            Thread => (&mut matter, NetworkMedium::Thread),
            MatterWifi => (&mut matter, NetworkMedium::Wifi),
            Esphome => (&mut esphome, NetworkMedium::Wifi),
        };
        network.nodes.push(node(device, spot.role, medium));
        network
            .links
            .push(link(spot.device, spot.through, medium, spot.signal, scan));
    }
    // A device the table doesn't place yet is on Wi-Fi, so it's still on the map.
    for (id, device) in &devices {
        if !placed_here.contains(id) {
            esphome
                .nodes
                .push(node(device, NetworkRole::Device, NetworkMedium::Wifi));
            esphome
                .links
                .push(link(id, "wifi", NetworkMedium::Wifi, -66, scan));
        }
    }
    for (from, to, lqi) in ZIGBEE_MESH {
        zigbee
            .links
            .push(link(from, to, NetworkMedium::Zigbee, *lqi, scan));
    }
    for (from, to, dbm) in THREAD_MESH {
        matter
            .links
            .push(link(from, to, NetworkMedium::Thread, *dbm, scan));
    }
    vec![zigbee, matter, esphome]
}

fn node(device: &Placed, role: NetworkRole, medium: NetworkMedium) -> NetworkNode {
    NetworkNode {
        id: device.unique_id.to_owned(),
        name: device.name.to_owned(),
        role,
        medium: Some(medium),
        device_unique_id: UniqueId::try_from(device.unique_id).ok(),
        detail: Some(format!("{}, {}", capitalised(device.model), device.room)),
    }
}

/// A link as scan number `scan` measured it. Zigbee's LQI wanders between scans; the first one
/// (scan 0) reads the table as written.
fn link(from: &str, to: &str, medium: NetworkMedium, signal: i64, scan: u32) -> NetworkLink {
    let (quality, reading) = if medium == NetworkMedium::Zigbee {
        let lqi = (signal + wander(from, to, scan)).clamp(10, 255);
        let lqi = u64::try_from(lqi).unwrap_or(0);
        (LinkQuality::from_lqi(lqi), format!("LQI {lqi}"))
    } else {
        (LinkQuality::from_rssi(signal), format!("{signal} dBm"))
    };
    NetworkLink {
        from: from.to_owned(),
        to: to.to_owned(),
        medium: Some(medium),
        quality: Some(quality),
        reading: Some(reading),
        detail: None,
    }
}

/// How far a link's LQI reads from its table value at scan number `scan`: up to 12 either way,
/// the same for the same link and scan.
fn wander(from: &str, to: &str, scan: u32) -> i64 {
    if scan == 0 {
        return 0;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in from.bytes().chain(to.bytes()).chain(scan.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    i64::try_from(hash % 25).unwrap_or(0) - 12
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_device_in_the_house_is_on_one_network_once() {
        let networks = draw(0, None);
        assert_eq!(Network::validate_all(&networks), Ok(()));
        let mut seen = BTreeSet::new();
        for network in &networks {
            for node in &network.nodes {
                if let Some(device) = &node.device_unique_id {
                    assert!(seen.insert(device.as_str().to_owned()), "{device} twice");
                }
            }
        }
        let house: BTreeSet<String> = placed()
            .iter()
            .map(|device| device.unique_id.to_owned())
            .collect();
        assert_eq!(seen, house);
        // And the table places every one of them: none fell through to the Wi-Fi default.
        let tabled: BTreeSet<&str> = SPOTS.iter().map(|spot| spot.device).collect();
        assert_eq!(tabled.len(), house.len());
    }

    #[test]
    fn every_link_names_a_node_its_network_has() {
        for network in draw(3, None) {
            assert_eq!(network.validate(), Ok(()), "{:?}", network.name);
        }
    }

    #[test]
    fn the_garden_is_where_the_weak_links_are() {
        let networks = draw(0, None);
        let soil = networks[0]
            .links
            .iter()
            .find(|link| link.from == "garden-soil")
            .expect("the soil sensor");
        assert_eq!(soil.quality, Some(LinkQuality::Poor));
        assert_eq!(soil.reading.as_deref(), Some("LQI 28"));
    }

    #[test]
    fn a_scan_reads_a_little_differently_each_time() {
        let first = draw(1, None);
        let second = draw(2, None);
        assert_ne!(first[0].links, second[0].links);
        assert_eq!(draw(1, None), first, "the same scan reads the same");
        // Thread and Wi-Fi aren't scanned.
        assert_eq!(first[1].links, second[1].links);
        assert!((-12..=12).contains(&wander("a", "b", 7)));
    }
}
