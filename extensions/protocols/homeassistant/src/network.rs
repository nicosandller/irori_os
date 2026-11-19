//! The map of what comes over (`docs/specs/protocols.md` §6.8). Irori reaches Home Assistant on
//! the home's network, and Home Assistant reaches each device: straight, or through the device
//! Home Assistant says it's reached through (`via_device_id`: a Hue bulb through its bridge, a
//! Zigbee device through Zigbee2MQTT). How a device is connected is told by what Home Assistant
//! knows of it: a Zigbee or Bluetooth address, or a MAC address on the home's network. An online
//! service Home Assistant lists as a device (the weather, a calendar, an AI assistant) is reached
//! over the internet.

use std::collections::{BTreeSet, HashMap};

use irori_types::{Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId};

use crate::model::HaDevice;

const IRORI: &str = "irori";
const HOME_ASSISTANT: &str = "homeassistant";
/// A map holds at most 512 nodes; Irori and Home Assistant are two of them.
const MOST_DEVICES: usize = 510;

/// The map of `devices`, each one that comes over, parents before the devices behind them.
pub fn draw(devices: &[&HaDevice]) -> Network {
    // A node's id is 1-64 of letters, digits and `_.:-`; Home Assistant's are 32 hex digits.
    let devices: Vec<&HaDevice> = devices
        .iter()
        .copied()
        .filter(|d| {
            (1..=64).contains(&d.id.len())
                && d.id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
                && d.id != IRORI
                && d.id != HOME_ASSISTANT
        })
        .take(MOST_DEVICES)
        .collect();
    let ids: BTreeSet<&str> = devices.iter().map(|d| d.id.as_str()).collect();
    let parent_of: HashMap<&str, &str> = devices
        .iter()
        .filter_map(|device| {
            let parent = device.via_device_id.as_deref()?;
            (parent != device.id && ids.contains(parent)).then_some((device.id.as_str(), parent))
        })
        .collect();
    let parents: BTreeSet<&str> = parent_of.values().copied().collect();
    let node = |id: &str, name: &str, role, medium| NetworkNode {
        id: id.to_owned(),
        name: name.to_owned(),
        role,
        medium,
        device_unique_id: None,
        detail: None,
        host: None,
    };
    let link = |from: &str, to: &str, medium: Option<NetworkMedium>| NetworkLink {
        from: from.to_owned(),
        to: to.to_owned(),
        medium,
        quality: None,
        reading: None,
        detail: None,
    };
    let mut nodes = vec![
        node(IRORI, "Irori", NetworkRole::Controller, None),
        node(
            HOME_ASSISTANT,
            "Home Assistant",
            NetworkRole::Coordinator,
            Some(NetworkMedium::Ip),
        ),
    ];
    let mut links = vec![link(HOME_ASSISTANT, IRORI, Some(NetworkMedium::Ip))];
    for device in &devices {
        let Ok(unique_id) = UniqueId::try_from(device.id.as_str()) else {
            continue;
        };
        let medium = medium(device);
        let parent = parent_of.get(device.id.as_str()).copied();
        let role = if parents.contains(device.id.as_str()) {
            // Others are reached through it: a bridge, a hub, a coordinator.
            if parent.is_some() {
                NetworkRole::Router
            } else {
                NetworkRole::Coordinator
            }
        } else if medium == Some(NetworkMedium::Zigbee) {
            NetworkRole::EndDevice
        } else {
            NetworkRole::Device
        };
        let detail = [device.manufacturer.as_deref(), device.model.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let name = device
            .name_by_user
            .as_deref()
            .or(device.name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Home Assistant device");
        let name: String = name.chars().take(100).collect();
        let detail: String = detail.chars().take(200).collect();
        nodes.push(NetworkNode {
            device_unique_id: Some(unique_id),
            detail: (!detail.is_empty()).then_some(detail),
            ..node(&device.id, &name, role, medium)
        });
        links.push(link(&device.id, parent.unwrap_or(HOME_ASSISTANT), medium));
    }
    Network {
        name: Some("Home Assistant".to_owned()),
        detail: Some("Through Home Assistant".to_owned()),
        scanned_at: None,
        nodes,
        links,
    }
}

/// How a device is connected, as far as Home Assistant knows.
fn medium(device: &HaDevice) -> Option<NetworkMedium> {
    if device.entry_type.as_deref() == Some("service") {
        return Some(NetworkMedium::Internet);
    }
    let has = |kind: &str| device.connections.iter().any(|(k, _)| k == kind);
    // ZHA's devices are known by their Zigbee address; Zigbee2MQTT's, over MQTT, by an id it
    // makes from it (`zigbee2mqtt_0x00158d…`).
    let zigbee = device.identifiers.iter().any(|(integration, id)| {
        integration == "zha" || (integration == "mqtt" && id.starts_with("zigbee2mqtt_0x"))
    });
    if has("zigbee") || zigbee {
        Some(NetworkMedium::Zigbee)
    } else if has("bluetooth") {
        Some(NetworkMedium::Bluetooth)
    } else if has("mac") {
        Some(NetworkMedium::Ip)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn device(value: serde_json::Value) -> HaDevice {
        serde_json::from_value(value).expect("device")
    }

    #[test]
    fn devices_hang_off_home_assistant_or_what_they_re_reached_through() {
        let bridge = device(json!({
            "id": "hue", "name": "Hue Bridge", "manufacturer": "Signify", "model": "BSB002",
            "connections": [["mac", "00:17:88:01:02:03"]],
        }));
        let bulb = device(json!({
            "id": "bulb", "name": "Kitchen light", "via_device_id": "hue",
            "connections": [["zigbee", "00:11:22:33:44:55:66:77"]],
        }));
        let weather = device(json!({
            "id": "owm", "name": "OpenWeatherMap", "entry_type": "service",
        }));
        let sensor = device(json!({
            "id": "z2m", "name": "Door", "identifiers": [["mqtt", "zigbee2mqtt_0x00158d0001"]],
        }));
        let lost = device(json!({ "id": "lost", "name": "Somewhere", "via_device_id": "gone" }));
        let map = draw(&[&bridge, &bulb, &weather, &sensor, &lost]);
        map.validate().expect("a map Irori takes");

        let role = |id: &str| {
            map.nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| (n.role, n.medium))
        };
        assert_eq!(role("irori"), Some((NetworkRole::Controller, None)));
        assert_eq!(
            role("hue"),
            Some((NetworkRole::Coordinator, Some(NetworkMedium::Ip)))
        );
        assert_eq!(
            role("bulb"),
            Some((NetworkRole::EndDevice, Some(NetworkMedium::Zigbee)))
        );
        assert_eq!(
            role("owm"),
            Some((NetworkRole::Device, Some(NetworkMedium::Internet)))
        );
        assert_eq!(
            role("z2m"),
            Some((NetworkRole::EndDevice, Some(NetworkMedium::Zigbee)))
        );
        assert_eq!(role("lost"), Some((NetworkRole::Device, None)));

        let to = |id: &str| {
            map.links
                .iter()
                .find(|l| l.from == id)
                .map(|l| l.to.as_str())
        };
        assert_eq!(to("homeassistant"), Some("irori"));
        assert_eq!(to("hue"), Some("homeassistant"));
        assert_eq!(to("bulb"), Some("hue"));
        assert_eq!(to("owm"), Some("homeassistant"));
        // Reached through a device that doesn't come over: straight from Home Assistant.
        assert_eq!(to("lost"), Some("homeassistant"));

        let hue = map.nodes.iter().find(|n| n.id == "hue").expect("hue");
        assert_eq!(hue.detail.as_deref(), Some("Signify BSB002"));
        assert_eq!(
            hue.device_unique_id.as_ref().map(UniqueId::as_str),
            Some("hue")
        );
    }
}
