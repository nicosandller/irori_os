//! The map of the ESPHome devices (`docs/specs/protocols.md` §6.8): each on the home's Wi-Fi,
//! and Irori beyond it.
//!
//! ESPHome devices don't say which access point they're on, so they all hang off one "Wi-Fi"
//! node ([`irori_types::access_point_id`] of none), the same one Matter's Wi-Fi devices meet at
//! when they don't say either. How well each hears it is its Wi-Fi signal sensor, for a device
//! that has one (`sensor: - platform: wifi_signal`).

use irori_types::{
    LinkQuality, Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId,
    access_point_id,
};

/// Irori's own node on the map.
const IRORI: &str = "irori";

/// One device as the map needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub device: UniqueId,
    pub name: String,
    pub model: Option<String>,
    pub online: bool,
    /// Its Wi-Fi signal, in dBm, as its sensor last said.
    pub rssi: Option<i64>,
}

/// The map, from every device connected to.
pub fn draw(members: &[Member]) -> Network {
    let wifi = access_point_id(None);
    let mut nodes = vec![
        NetworkNode {
            id: IRORI.to_owned(),
            name: "Irori".to_owned(),
            role: NetworkRole::Controller,
            medium: None,
            device_unique_id: None,
            detail: None,
        },
        NetworkNode {
            id: wifi.clone(),
            name: "Wi-Fi".to_owned(),
            role: NetworkRole::AccessPoint,
            medium: Some(NetworkMedium::Wifi),
            device_unique_id: None,
            detail: Some("The home's Wi-Fi".to_owned()),
        },
    ];
    let mut links = vec![NetworkLink {
        from: wifi.clone(),
        to: IRORI.to_owned(),
        medium: Some(NetworkMedium::Ip),
        quality: None,
        reading: None,
        detail: None,
    }];
    let mut taken = std::collections::BTreeSet::new();
    for member in members {
        let id = node_id(&member.device, &taken);
        taken.insert(id.clone());
        let mut detail = member.model.clone().unwrap_or_default();
        if !member.online {
            if !detail.is_empty() {
                detail.push_str(", ");
            }
            detail.push_str("not connected");
        }
        nodes.push(NetworkNode {
            id: id.clone(),
            name: cut(&member.name, 100),
            role: NetworkRole::Device,
            medium: Some(NetworkMedium::Wifi),
            device_unique_id: Some(member.device.clone()),
            detail: (!detail.is_empty()).then(|| cut(&detail, 200)),
        });
        links.push(NetworkLink {
            from: id,
            to: wifi.clone(),
            medium: Some(NetworkMedium::Wifi),
            quality: member.rssi.map(LinkQuality::from_rssi),
            reading: member.rssi.map(|rssi| format!("{rssi} dBm")),
            detail: None,
        });
    }
    Network {
        name: Some("ESPHome".to_owned()),
        detail: Some("Wi-Fi, native API".to_owned()),
        scanned_at: None,
        nodes,
        links,
    }
}

/// Whether the map wants sending again: something about what's on it changed, or a signal moved
/// enough to read differently. A signal wanders a couple of dBm from one minute to the next,
/// and resending the whole map for that would only be noise.
pub fn worth_sending(last: Option<&[Member]>, now: &[Member]) -> bool {
    let Some(last) = last else {
        return true;
    };
    if last.len() != now.len() {
        return true;
    }
    last.iter().zip(now).any(|(was, is)| {
        let moved = match (was.rssi, is.rssi) {
            (Some(was), Some(is)) => {
                (was - is).abs() >= 5 || LinkQuality::from_rssi(was) != LinkQuality::from_rssi(is)
            }
            (None, None) => false,
            _ => true,
        };
        was.device != is.device
            || was.name != is.name
            || was.model != is.model
            || was.online != is.online
            || moved
    })
}

/// A device's node id: its own id (a MAC address, for ESPHome) behind `device:`, so it can't be
/// mistaken for Irori's or the Wi-Fi's, with anything a node id can't hold made `_`. The same
/// each time it's drawn, so a device moved on the map stays where it was put.
fn node_id(device: &UniqueId, taken: &std::collections::BTreeSet<String>) -> String {
    let own: String = device
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(50)
        .collect();
    let id = format!("device:{own}");
    if !taken.contains(&id) {
        return id;
    }
    // Two ids that only differed in what was made `_`, or past what was kept.
    (2..)
        .map(|n| format!("{id}-{n}"))
        .find(|id| !taken.contains(id))
        .expect("there's always a next number")
}

fn cut(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(device: &str, rssi: Option<i64>) -> Member {
        Member {
            device: UniqueId::try_from(device).expect("an id"),
            name: "Garage door".to_owned(),
            model: Some("esp32dev".to_owned()),
            online: true,
            rssi,
        }
    }

    #[test]
    fn every_device_hangs_off_the_wifi_with_its_signal() {
        let network = draw(&[
            member("a4cf12b3c4d5", Some(-71)),
            member("a4cf12b3c4d6", None),
        ]);
        assert_eq!(network.validate(), Ok(()));
        assert_eq!(network.name.as_deref(), Some("ESPHome"));
        let wifi = network
            .nodes
            .iter()
            .find(|node| node.role == NetworkRole::AccessPoint)
            .expect("the Wi-Fi");
        assert_eq!(wifi.id, "wifi");
        let heard = network
            .links
            .iter()
            .find(|link| link.from == "device:a4cf12b3c4d5")
            .expect("linked");
        assert_eq!(heard.to, "wifi");
        assert_eq!(heard.quality, Some(LinkQuality::Fair));
        assert_eq!(heard.reading.as_deref(), Some("-71 dBm"));
        let unknown = network
            .links
            .iter()
            .find(|link| link.from == "device:a4cf12b3c4d6")
            .expect("linked");
        assert_eq!(unknown.quality, None);
        assert!(
            network
                .links
                .iter()
                .any(|link| link.from == "wifi" && link.to == IRORI)
        );
    }

    /// An id a node can't hold still makes a node, the same one every time, and nothing can
    /// pass for the Wi-Fi.
    #[test]
    fn every_device_gets_a_node_id_of_its_own() {
        let network = draw(&[
            member("wifi", None),
            member("living room/esp", None),
            member("living room?esp", None),
        ]);
        assert_eq!(network.validate(), Ok(()));
        let ids: Vec<&str> = network.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "irori",
                "wifi",
                "device:wifi",
                "device:living_room_esp",
                "device:living_room_esp-2",
            ]
        );
    }

    #[test]
    fn a_device_away_says_so() {
        let mut away = member("a4cf12b3c4d5", None);
        away.online = false;
        let network = draw(&[away]);
        let node = network
            .nodes
            .iter()
            .find(|node| node.id == "device:a4cf12b3c4d5")
            .expect("drawn");
        assert_eq!(node.detail.as_deref(), Some("esp32dev, not connected"));
    }

    #[test]
    fn a_signal_wandering_a_little_isnt_worth_a_new_map() {
        let was = vec![member("a4cf12b3c4d5", Some(-63))];
        assert!(worth_sending(None, &was));
        assert!(!worth_sending(
            Some(&was),
            &[member("a4cf12b3c4d5", Some(-65))]
        ));
        assert!(worth_sending(
            Some(&was),
            &[member("a4cf12b3c4d5", Some(-69))]
        ));
        // -59 is a step up even though it's only four away.
        assert!(worth_sending(
            Some(&was),
            &[member("a4cf12b3c4d5", Some(-59))]
        ));
        assert!(worth_sending(Some(&was), &[member("a4cf12b3c4d5", None)]));
    }
}
