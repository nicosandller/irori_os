//! The map of the account's devices (`docs/specs/protocols.md` §6.8): Irori reaches SwitchBot's
//! cloud over the internet, the cloud reaches each hub and each device on Wi-Fi of its own, and a
//! hub reaches the devices behind it over Bluetooth. A device that depends on a hub is drawn
//! hanging off it, so a hub going offline shows what goes with it.

use irori_types::{Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId};

use crate::cloud::Listed;

const IRORI: &str = "irori";
const CLOUD: &str = "cloud";

/// The map, from the devices Irori uses.
pub fn draw<'a>(devices: impl IntoIterator<Item = &'a Listed>) -> Network {
    let devices: Vec<&Listed> = devices.into_iter().collect();
    let ids: std::collections::BTreeSet<&str> = devices.iter().map(|d| d.id.as_str()).collect();
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
    let mut nodes = vec![
        node(IRORI, "Irori", NetworkRole::Controller, None),
        node(
            CLOUD,
            "SwitchBot cloud",
            NetworkRole::Cloud,
            Some(NetworkMedium::Internet),
        ),
    ];
    let mut links = vec![link(CLOUD, IRORI, NetworkMedium::Internet)];
    for listed in devices {
        let Ok(unique_id) = UniqueId::try_from(listed.id.as_str()) else {
            continue;
        };
        // A hub not on the map (one Irori doesn't use) can't be linked to; the device hangs off
        // the cloud instead.
        let (medium, up, over) = match listed.hub.as_deref().filter(|hub| ids.contains(hub)) {
            Some(hub) => (NetworkMedium::Bluetooth, hub, NetworkMedium::Bluetooth),
            None => (NetworkMedium::Wifi, CLOUD, NetworkMedium::Internet),
        };
        nodes.push(NetworkNode {
            device_unique_id: Some(unique_id),
            detail: Some(listed.device_type.clone()),
            ..node(&listed.id, &listed.name, NetworkRole::Device, Some(medium))
        });
        links.push(link(&listed.id, up, over));
    }
    Network {
        name: Some("SwitchBot".to_owned()),
        detail: Some("Through SwitchBot's cloud".to_owned()),
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

    #[test]
    fn a_device_behind_a_hub_hangs_off_it_and_the_hub_off_the_cloud() {
        let devices = [listed("HUB1", None), listed("METER1", Some("HUB1"))];
        let map = draw(&devices);
        map.validate().expect("a map the core accepts");
        let orphan = [listed("METER2", Some("GONE"))];
        draw(&orphan)
            .validate()
            .expect("a device whose hub isn't drawn hangs off the cloud");
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
    }
}
