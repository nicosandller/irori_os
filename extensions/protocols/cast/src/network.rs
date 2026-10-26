//! The map of the Cast receivers (`docs/specs/protocols.md` §6.8): each on the home's Wi-Fi, or
//! wired, with Irori beyond.
//!
//! A receiver says what it's connected to in `/setup/eureka_info`: whether it's wired, the access
//! point it's on (`bssid`) and how well it hears it (`signal_level`, in dBm). Not every one says:
//! a Google Home speaker gives both, a Nest Hub and a Chromecast with Google TV neither, and those
//! hang off one "Wi-Fi" node ([`irori_types::access_point_id`] of none), the one ESPHome's and
//! Matter's meet at too. Each receiver carries its mDNS host, so a Nest Hub that Matter hears as
//! a Thread border router is drawn once.

use std::collections::BTreeSet;

use irori_types::{
    LinkQuality, Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId,
    access_point_id,
};

/// Irori's own node on the map.
const IRORI: &str = "irori";

/// How a receiver says it's connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wifi {
    /// It's on a cable, not the Wi-Fi.
    pub ethernet: bool,
    /// The access point it's on.
    pub bssid: Option<[u8; 6]>,
    /// How well it hears it, in dBm.
    pub rssi: Option<i64>,
}

/// Reads [`Wifi`] off an `eureka_info` body. `None` for one that isn't JSON.
pub fn wifi_of(body: &str) -> Option<Wifi> {
    let info: serde_json::Value = serde_json::from_str(body).ok()?;
    let bssid = info
        .get("bssid")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| {
            let bytes: Vec<u8> = text
                .split(':')
                .map(|pair| u8::from_str_radix(pair, 16))
                .collect::<Result<_, _>>()
                .ok()?;
            <[u8; 6]>::try_from(bytes).ok()
        })
        .filter(|bytes| bytes.iter().any(|&byte| byte != 0));
    let rssi = info
        .get("signal_level")
        .and_then(serde_json::Value::as_i64)
        .filter(|&rssi| rssi < 0 && rssi > -128);
    Some(Wifi {
        ethernet: info
            .get("ethernet_connected")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        bssid,
        rssi,
    })
}

/// One receiver as the map needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub device: UniqueId,
    pub name: String,
    pub model: String,
    pub online: bool,
    pub host: Option<String>,
    /// What it last said about its connection; `None` until it's been asked.
    pub wifi: Option<Wifi>,
}

/// The map, from every receiver (not the speaker groups, which aren't on any network of their own).
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
    let mut access_points = BTreeSet::new();
    for member in members {
        // From its id, so it's the same node from one map to the next (and a device dragged on
        // the map stays where it was put): the receiver's UUID, 32 hex digits.
        let id = format!("cast-{}", member.device.as_str());
        let wifi = member.wifi.unwrap_or(Wifi {
            ethernet: false,
            bssid: None,
            rssi: None,
        });
        let mut detail = member.model.clone();
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
            medium: Some(if wifi.ethernet {
                NetworkMedium::Ethernet
            } else {
                NetworkMedium::Wifi
            }),
            device_unique_id: Some(member.device.clone()),
            detail: (!detail.is_empty()).then(|| cut(&detail, 200)),
            host: member.host.clone(),
        });
        if wifi.ethernet {
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
        let access_point = access_point_id(wifi.bssid);
        access_points.insert((access_point.clone(), wifi.bssid));
        links.push(NetworkLink {
            from: id,
            to: access_point,
            medium: Some(NetworkMedium::Wifi),
            quality: wifi.rssi.map(LinkQuality::from_rssi),
            reading: wifi.rssi.map(|rssi| format!("{rssi} dBm")),
            detail: match (member.wifi, wifi.rssi) {
                (_, Some(_)) => None,
                (None, None) => Some("Not asked for its Wi-Fi signal yet".to_owned()),
                (Some(_), None) => Some("It doesn't report its Wi-Fi signal".to_owned()),
            },
        });
    }
    for (access_point, bssid) in access_points {
        nodes.push(NetworkNode {
            id: access_point.clone(),
            name: if bssid.is_some() {
                "Wi-Fi access point".to_owned()
            } else {
                "Wi-Fi".to_owned()
            },
            role: NetworkRole::AccessPoint,
            medium: Some(NetworkMedium::Wifi),
            device_unique_id: None,
            detail: Some(match bssid {
                Some(bytes) => bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(":"),
                None => "The home's Wi-Fi".to_owned(),
            }),
            host: None,
        });
        links.push(NetworkLink {
            from: access_point,
            to: IRORI.to_owned(),
            medium: Some(NetworkMedium::Ip),
            quality: None,
            reading: None,
            detail: None,
        });
    }
    Network {
        name: Some("Google Cast".to_owned()),
        detail: Some("Wi-Fi and wired receivers".to_owned()),
        scanned_at: None,
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

    fn member(device: &str, wifi: Option<Wifi>) -> Member {
        Member {
            device: device.parse().expect("valid"),
            name: device.to_owned(),
            model: "Google Home".to_owned(),
            online: true,
            host: None,
            wifi,
        }
    }

    #[test]
    fn a_receiver_says_its_access_point_and_signal_or_that_it_does_not() {
        // As a Google Home speaker and a Nest Hub answer.
        let speaker = wifi_of(
            r#"{"bssid":"9c:53:22:aa:cf:b7","signal_level":-59,"noise_level":-91,
                "ethernet_connected":false,"name":"GH Speaker"}"#,
        )
        .expect("JSON");
        assert_eq!(speaker.bssid, Some([0x9c, 0x53, 0x22, 0xaa, 0xcf, 0xb7]));
        assert_eq!(speaker.rssi, Some(-59));
        let hub = wifi_of(r#"{"bssid":"","ethernet_connected":false,"name":"Kitchen Display"}"#)
            .expect("JSON");
        assert_eq!((hub.bssid, hub.rssi, hub.ethernet), (None, None, false));
        assert_eq!(wifi_of("not json"), None);

        let mut hosted = member("hub", Some(hub));
        hosted.host = Some("fuchsia-dce5-5ba4-40db.local".to_owned());
        let network = draw(&[
            member("speaker", Some(speaker)),
            hosted,
            member("new", None),
            member(
                "wired",
                Some(Wifi {
                    ethernet: true,
                    bssid: None,
                    rssi: None,
                }),
            ),
        ]);
        assert_eq!(network.validate(), Ok(()));
        let link = |from: &str| {
            network
                .links
                .iter()
                .find(|link| link.from == from)
                .expect("linked")
        };
        assert_eq!(link("cast-speaker").to, "ap-9c5322aacfb7");
        assert_eq!(link("cast-speaker").quality, Some(LinkQuality::Excellent));
        assert_eq!(link("cast-speaker").reading.as_deref(), Some("-59 dBm"));
        assert_eq!(link("cast-hub").to, "wifi");
        assert_eq!(
            link("cast-hub").detail.as_deref(),
            Some("It doesn't report its Wi-Fi signal")
        );
        assert_eq!(
            link("cast-new").detail.as_deref(),
            Some("Not asked for its Wi-Fi signal yet")
        );
        assert_eq!(link("cast-wired").to, IRORI);
        assert_eq!(link("cast-wired").medium, Some(NetworkMedium::Ethernet));
        assert_eq!(
            network.nodes[2].host.as_deref(),
            Some("fuchsia-dce5-5ba4-40db.local")
        );
        assert!(link("ap-9c5322aacfb7").to == IRORI && link("wifi").to == IRORI);
    }
}
