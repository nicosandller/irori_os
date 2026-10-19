//! How a device is found: TP-Link's discovery probe on UDP 20002, broadcast on this network and
//! sent to each known host. Every Tapo device and recent Kasa firmware answers it, before any
//! login, with its MAC, model and how it wants to be spoken to.
//!
//! A broadcast doesn't cross into another VLAN; a probe sent to the address does, so plugs on an
//! IoT network go in `known_hosts`. One that doesn't answer stays on the list.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde_json::Value;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::settings::Settings;
use crate::smart;

pub const PORT: u16 = 20002;
const ROUND: Duration = Duration::from_secs(60);
const LISTEN: Duration = Duration::from_secs(3);
/// The probe without a key: version 2, probe, no payload, its CRC. Devices answer it in clear.
pub const PROBE: [u8; 16] = [
    0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46, 0x3c, 0xb5, 0xd3,
];

/// One device that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub mac: String,
    pub address: SocketAddr,
    pub model: String,
    pub device_type: String,
    /// `KLAP` for every device this reads; anything else is listed as unsupported.
    pub encryption: String,
    /// Set when this came from `known_hosts`, so a later success clears that host's failure.
    pub known_host: Option<String>,
}

impl Found {
    pub fn readable(&self) -> bool {
        self.encryption == "KLAP" && self.device_type.starts_with("SMART.")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    Seen(Found),
    /// A known host didn't answer this round.
    Unreachable {
        host: String,
    },
}

pub fn start(settings: &Settings, events: mpsc::Sender<Discovery>) -> tokio::task::JoinHandle<()> {
    let hosts = settings.known_hosts.clone();
    tokio::spawn(async move {
        loop {
            match round(&hosts).await {
                Ok(found) => {
                    for event in found {
                        if events.send(event).await.is_err() {
                            return;
                        }
                    }
                }
                Err(error) => tracing::warn!("TP-Link discovery couldn't run: {error}"),
            }
            tokio::time::sleep(ROUND).await;
        }
    })
}

/// One probe of the network and every known host.
async fn round(hosts: &[String]) -> std::io::Result<Vec<Discovery>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    if let Err(error) = socket.send_to(&PROBE, (Ipv4Addr::BROADCAST, PORT)).await {
        tracing::debug!("the discovery broadcast couldn't be sent: {error}");
    }

    let mut asked: BTreeMap<IpAddr, String> = BTreeMap::new();
    let mut events = Vec::new();
    for host in hosts {
        let resolved = tokio::net::lookup_host((host.as_str(), PORT))
            .await
            .ok()
            .and_then(|mut addresses| addresses.find(SocketAddr::is_ipv4));
        let Some(address) = resolved else {
            events.push(Discovery::Unreachable { host: host.clone() });
            continue;
        };
        asked.insert(address.ip(), host.clone());
        if socket.send_to(&PROBE, address).await.is_err() {
            tracing::debug!("a discovery probe couldn't be sent");
        }
    }

    let mut seen: BTreeMap<IpAddr, Found> = BTreeMap::new();
    let mut buffer = vec![0u8; 4096];
    let deadline = tokio::time::Instant::now() + LISTEN;
    while let Ok(received) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await
    {
        let Ok((length, from)) = received else {
            continue;
        };
        if let Some(mut found) = parse(&buffer[..length], from.ip()) {
            found.known_host = asked.get(&from.ip()).cloned();
            seen.insert(from.ip(), found);
        }
    }

    for (ip, host) in &asked {
        if !seen.contains_key(ip) {
            events.push(Discovery::Unreachable { host: host.clone() });
        }
    }
    events.extend(seen.into_values().map(Discovery::Seen));
    Ok(events)
}

/// A device's answer: a 16-byte header, then JSON.
pub fn parse(datagram: &[u8], from: IpAddr) -> Option<Found> {
    let json: Value = serde_json::from_slice(datagram.get(16..)?).ok()?;
    if json.get("error_code").and_then(Value::as_i64) != Some(0) {
        return None;
    }
    let result = json.get("result")?;
    let text = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default()
    };
    let scheme = result.get("mgt_encrypt_schm");
    let port = scheme
        .and_then(|scheme| scheme.get("http_port"))
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or(80);
    Some(Found {
        mac: smart::mac(&text("mac"))?,
        address: SocketAddr::new(from, port),
        model: text("device_model"),
        device_type: text("device_type"),
        encryption: scheme
            .and_then(|scheme| scheme.get("encrypt_type"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        known_host: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_tapo_answer() {
        let mut datagram = vec![0u8; 16];
        datagram.extend_from_slice(
            br#"{"result":{"device_id":"abc","device_model":"PP1(EU)","device_type":"SMART.TAPOPLUG","ip":"192.168.1.21","mac":"02-00-00-00-00-02","mgt_encrypt_schm":{"is_support_https":false,"encrypt_type":"KLAP","http_port":80,"lv":2}},"error_code":0}"#,
        );
        let found = parse(&datagram, "192.168.1.21".parse().expect("ip")).expect("valid");
        assert_eq!(found.mac, "02:00:00:00:00:02");
        assert_eq!(found.address, "192.168.1.21:80".parse().expect("addr"));
        assert_eq!(found.model, "PP1(EU)");
        assert!(found.readable());
    }

    #[test]
    fn an_answer_without_klap_is_not_readable() {
        let mut datagram = vec![0u8; 16];
        datagram.extend_from_slice(
            br#"{"result":{"device_model":"P100","device_type":"SMART.TAPOPLUG","mac":"00-11-22-33-44-55","mgt_encrypt_schm":{"encrypt_type":"AES","http_port":80}},"error_code":0}"#,
        );
        let found = parse(&datagram, "10.0.0.2".parse().expect("ip")).expect("valid");
        assert!(!found.readable());
        assert_eq!(
            parse(&datagram[..10], "10.0.0.2".parse().expect("ip")),
            None
        );
    }
}
