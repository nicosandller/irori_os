//! The map of the devices (`docs/specs/protocols.md` §6.8). Through the cloud, Irori reaches
//! SwitchBot's servers over the internet, they reach each hub and each device on Wi-Fi of its
//! own, and a hub reaches the devices behind it over Bluetooth: a device that depends on a hub
//! hangs off it, so a hub going offline shows what goes with it. A device heard nearby is also
//! linked straight to Irori over Bluetooth, with its signal.

use std::collections::BTreeSet;

use irori_types::{
    LinkQuality, Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId,
};

use crate::cloud::Listed;

const IRORI: &str = "irori";
const CLOUD: &str = "cloud";

/// One device as the map needs it.
#[derive(Debug, Clone, Copy)]
pub struct Drawn<'a> {
    pub listed: &'a Listed,
    /// The account lists it, so the cloud reaches it.
    pub through_cloud: bool,
    /// Heard over Bluetooth lately, with its signal in dBm when the radio said.
    pub heard: Option<Option<i16>>,
}

/// The map. `cloud` is whether there's an account to reach devices through.
pub fn draw<'a>(devices: impl IntoIterator<Item = Drawn<'a>>, cloud: bool) -> Network {
    let devices: Vec<Drawn<'a>> = devices.into_iter().collect();
    let ids: BTreeSet<&str> = devices.iter().map(|d| d.listed.id.as_str()).collect();
    let node = |id: &str, name: &str, role, medium| NetworkNode {
        id: id.to_owned(),
        name: name.to_owned(),
        role,
        medium,
        device_unique_id: None,
        detail: None,
        host: None,
    };
    let link = |from: &str, to: &str, medium| NetworkLink {
        from: from.to_owned(),
        to: to.to_owned(),
        medium: Some(medium),
        quality: None,
        reading: None,
        detail: None,
    };
    let mut nodes = vec![node(IRORI, "Irori", NetworkRole::Controller, None)];
    let mut links = Vec::new();
    if cloud {
        nodes.push(node(
            CLOUD,
            "SwitchBot cloud",
            NetworkRole::Cloud,
            Some(NetworkMedium::Internet),
        ));
        links.push(link(CLOUD, IRORI, NetworkMedium::Internet));
    }
    for device in &devices {
        let listed = device.listed;
        let Ok(unique_id) = UniqueId::try_from(listed.id.as_str()) else {
            continue;
        };
        // A hub not on the map (one Irori doesn't use) can't be linked to; the device hangs off
        // the cloud instead.
        let hub = listed.hub.as_deref().filter(|hub| ids.contains(hub));
        let through_cloud = cloud && device.through_cloud;
        let medium = match (device.heard, hub, through_cloud) {
            (Some(_), _, _) | (None, Some(_), true) | (None, _, false) => NetworkMedium::Bluetooth,
            (None, None, true) => NetworkMedium::Wifi,
        };
        nodes.push(NetworkNode {
            device_unique_id: Some(unique_id),
            detail: Some(listed.device_type.clone()),
            ..node(&listed.id, &listed.name, NetworkRole::Device, Some(medium))
        });
        if through_cloud {
            links.push(match hub {
                Some(hub) => link(&listed.id, hub, NetworkMedium::Bluetooth),
                None => link(&listed.id, CLOUD, NetworkMedium::Internet),
            });
        }
        if device.heard.is_some() || !through_cloud {
            let rssi = device.heard.flatten();
            links.push(NetworkLink {
                quality: rssi.map(|dbm| LinkQuality::from_rssi(i64::from(dbm))),
                reading: rssi.map(|dbm| format!("{dbm} dBm")),
                ..link(&listed.id, IRORI, NetworkMedium::Bluetooth)
            });
        }
    }
    let heard = devices.iter().any(|d| d.heard.is_some());
    Network {
        name: Some("SwitchBot".to_owned()),
        detail: Some(
            match (cloud, heard) {
                (true, true) => "Through SwitchBot's cloud, and over Bluetooth nearby",
                (true, false) => "Through SwitchBot's cloud",
                (false, _) => "Over Bluetooth nearby",
            }
            .to_owned(),
        ),
        scanned_at: None,
        nodes,
        links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(id: &str, hub: Option<&str>) -> Listed {
        Listed {
            id: id.to_owned(),
            name: id.to_owned(),
            device_type: "Meter".to_owned(),
            hub: hub.map(str::to_owned),
        }
    }

    fn through_cloud(listed: &Listed) -> Drawn<'_> {
        Drawn {
            listed,
            through_cloud: true,
            heard: None,
        }
    }

    #[test]
    fn a_device_behind_a_hub_hangs_off_it_and_the_hub_off_the_cloud() {
        let devices = [listed("HUB1", None), listed("METER1", Some("HUB1"))];
        let map = draw(devices.iter().map(through_cloud), true);
        map.validate().expect("a map the core accepts");
        let hub = map.links.iter().find(|l| l.from == "HUB1").expect("linked");
        assert_eq!(
            (hub.to.as_str(), hub.medium),
            ("cloud", Some(NetworkMedium::Internet))
        );
        let meter = map
            .links
            .iter()
            .find(|l| l.from == "METER1")
            .expect("linked");
        assert_eq!(
            (meter.to.as_str(), meter.medium),
            ("HUB1", Some(NetworkMedium::Bluetooth))
        );
        let orphan = [listed("METER2", Some("GONE"))];
        draw(orphan.iter().map(through_cloud), true)
            .validate()
            .expect("a device whose hub isn't drawn hangs off the cloud");
    }

    #[test]
    fn a_device_heard_nearby_is_linked_to_irori_with_its_signal() {
        let meter = listed("METER1", None);
        let map = draw(
            [Drawn {
                listed: &meter,
                through_cloud: false,
                heard: Some(Some(-61)),
            }],
            false,
        );
        map.validate().expect("a map the core accepts");
        assert!(map.nodes.iter().all(|n| n.role != NetworkRole::Cloud));
        let link = &map.links[0];
        assert_eq!((link.from.as_str(), link.to.as_str()), ("METER1", "irori"));
        assert_eq!(link.reading.as_deref(), Some("-61 dBm"));
        assert!(link.quality.is_some());
    }
}
