//! The map of the cameras (`docs/specs/protocols.md` §6.8): each one wired to Irori, or on the
//! home's Wi-Fi.
//!
//! A camera says which over ONVIF (`GetNetworkInterfaces`), not which access point, so the Wi-Fi
//! ones meet at the one "Wi-Fi" node ([`irori_types::access_point_id`] of none) that Matter's,
//! ESPHome's and Cast's Wi-Fi devices meet at when they don't say either. One that doesn't say
//! at all is drawn on Wi-Fi, the likelier.

use irori_types::{
    Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId, access_point_id,
};

/// Irori's own node on the map.
const IRORI: &str = "irori";

/// One camera as the map needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub device: UniqueId,
    pub name: String,
    pub model: Option<String>,
    pub online: bool,
    /// On a cable (`true`) or Wi-Fi, when it said.
    pub wired: Option<bool>,
}

/// The map, from every camera added.
pub fn draw(members: &[Member]) -> Network {
    let mut nodes = vec![NetworkNode {
        id: IRORI.to_owned(),
        name: "Irori".to_owned(),
        role: NetworkRole::Controller,
        medium: None,
        device_unique_id: None,
        detail: None,
        host: None,
    }];
    let mut links = Vec::new();
    let wifi = access_point_id(None);
    let mut on_wifi = false;
    for member in members {
        // From its id, so it's the same node from one map to the next, and a camera dragged on
        // the map stays where it was put.
        let id = node_id(&member.device);
        let wired = member.wired == Some(true);
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
            medium: Some(if wired {
                NetworkMedium::Ethernet
            } else {
                NetworkMedium::Wifi
            }),
            device_unique_id: Some(member.device.clone()),
            detail: (!detail.is_empty()).then(|| cut(&detail, 200)),
            host: None,
        });
        if wired {
            links.push(NetworkLink {
                from: id,
                to: IRORI.to_owned(),
                medium: Some(NetworkMedium::Ethernet),
                quality: None,
                reading: Some("LAN".to_owned()),
                detail: None,
            });
            continue;
        }
        on_wifi = true;
        links.push(NetworkLink {
            from: id,
            to: wifi.clone(),
            medium: Some(NetworkMedium::Wifi),
            quality: None,
            reading: None,
            detail: Some(if member.wired.is_none() {
                "It didn't say how it's connected".to_owned()
            } else {
                "It doesn't report its Wi-Fi signal".to_owned()
            }),
        });
    }
    if on_wifi {
        nodes.push(NetworkNode {
            id: wifi.clone(),
            name: "Wi-Fi".to_owned(),
            role: NetworkRole::AccessPoint,
            medium: Some(NetworkMedium::Wifi),
            device_unique_id: None,
            detail: Some("The home's Wi-Fi".to_owned()),
            host: None,
        });
        links.push(NetworkLink {
            from: wifi,
            to: IRORI.to_owned(),
            medium: Some(NetworkMedium::Ip),
            quality: None,
            reading: None,
            detail: None,
        });
    }
    Network {
        name: Some("Cameras".to_owned()),
        detail: Some("ONVIF cameras, wired and on Wi-Fi".to_owned()),
        scanned_at: None,
        nodes,
        links,
    }
}

/// A node id from a camera's id: letters, digits and `_.:-`, at most 64.
fn node_id(device: &UniqueId) -> String {
    let clean: String = device
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.:-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(56)
        .collect();
    format!("camera-{clean}")
}

fn cut(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(device: &str, wired: Option<bool>) -> Member {
        Member {
            device: device.parse().expect("valid"),
            name: device.to_owned(),
            model: Some("Duo 2V".to_owned()),
            online: true,
            wired,
        }
    }

    #[test]
    fn a_wired_camera_hangs_off_irori_and_a_wifi_one_off_the_wifi() {
        let map = draw(&[member("reolink", Some(true)), member("tapo", Some(false))]);
        let link = |from: &str| {
            map.links
                .iter()
                .find(|link| link.from == from)
                .map(|link| (link.to.clone(), link.medium))
                .expect("linked")
        };
        assert_eq!(
            link("camera-reolink"),
            ("irori".to_owned(), Some(NetworkMedium::Ethernet))
        );
        assert_eq!(
            link("camera-tapo"),
            (access_point_id(None), Some(NetworkMedium::Wifi))
        );
        assert_eq!(link(&access_point_id(None)).0, "irori");
    }

    #[test]
    fn only_wired_cameras_leave_out_the_wifi() {
        let map = draw(&[member("reolink", Some(true))]);
        assert!(
            map.nodes
                .iter()
                .all(|node| node.role != NetworkRole::AccessPoint)
        );
        assert_eq!(map.nodes.len(), 2);
    }

    #[test]
    fn a_camera_that_didnt_say_is_drawn_on_wifi_and_says_so() {
        let map = draw(&[member("someone", None)]);
        let link = map
            .links
            .iter()
            .find(|link| link.from == "camera-someone")
            .expect("linked");
        assert_eq!(link.medium, Some(NetworkMedium::Wifi));
        assert!(
            link.detail
                .as_deref()
                .is_some_and(|why| why.contains("didn't say"))
        );
    }

    #[test]
    fn node_ids_are_ones_a_map_takes() {
        let id = node_id(
            &"05121508-0115-0400-0613-ec71dbc9da16"
                .parse()
                .expect("valid"),
        );
        assert!(id.len() <= 64);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
        );
    }
}
