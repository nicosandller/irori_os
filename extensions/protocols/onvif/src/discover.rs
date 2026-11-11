//! Finding cameras: ONVIF's WS-Discovery.
//!
//! A `Probe` for network video transmitters goes to the multicast group (239.255.255.250:3702)
//! and straight to each host in the settings, for networks the multicast doesn't reach. Each
//! camera answers with a `ProbeMatch`: its id, where its device service is, and scopes naming
//! it. No login is needed for any of this.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::http::Target;
use crate::settings::{Settings, is_lan_ip};
use crate::soap::{find, find_all, text};

const MULTICAST: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 3702);
const PORT: u16 = 3702;
/// How long answers to one probe are waited for.
const LISTEN: Duration = Duration::from_secs(3);
/// When to look again after the first probe: soon at first, since a camera's answer can go
/// missing on Wi-Fi, then every few minutes.
const AGAIN_FIRST: [Duration; 3] = [
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(30),
];
const AGAIN: Duration = Duration::from_secs(5 * 60);
/// How many times each probe's multicast is sent, and how far apart: UDP is lost, and some
/// cameras answer only some probes.
const REPEAT: usize = 3;
const REPEAT_GAP: Duration = Duration::from_millis(250);
/// The most cameras one probe takes answers from.
const MAX_FOUND: usize = 64;
/// The most cameras one address answers for: a recorder may stand for a few, and one device
/// answering for many mustn't crowd the others out.
pub const MAX_PER_ADDRESS: usize = 4;

/// A camera that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its id, the discovery address without `urn:uuid:`. The device's handle too.
    pub id: String,
    /// The address it answered from: where everything is sent.
    pub ip: IpAddr,
    /// Its ONVIF device service.
    pub device_service: Target,
    /// What its scopes call it, for the Add device screen before it's logged into.
    pub name: String,
    /// Its model, from its scopes.
    pub hardware: Option<String>,
}

/// Starts probing in the background. Each answer goes to `found`, once per probe.
/// Looks for cameras now and every so often. `remembered` are where cameras were found before:
/// each probe also goes straight to them, since multicast over Wi-Fi gets lost.
pub fn start(
    settings: &Settings,
    remembered: Vec<IpAddr>,
    found: mpsc::Sender<Found>,
) -> JoinHandle<()> {
    let hosts = settings.hosts.clone();
    tokio::spawn(async move {
        // Cameras already found are also asked directly, in case multicast misses them.
        let mut known: Vec<IpAddr> = remembered
            .into_iter()
            .filter(|ip| is_lan_ip(*ip))
            .take(MAX_FOUND)
            .collect();
        let mut round = 0usize;
        loop {
            match probe(&hosts, &known).await {
                Ok(answers) => {
                    for answer in answers {
                        if !known.contains(&answer.ip) && known.len() < MAX_FOUND {
                            known.push(answer.ip);
                        }
                        if found.send(answer).await.is_err() {
                            return;
                        }
                    }
                }
                Err(why) => tracing::warn!("couldn't look for cameras: {why}"),
            }
            tokio::time::sleep(AGAIN_FIRST.get(round).copied().unwrap_or(AGAIN)).await;
            round += 1;
        }
    })
}

/// One probe: everything that answers within [`LISTEN`], by id.
async fn probe(hosts: &[String], known: &[IpAddr]) -> Result<Vec<Found>, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|error| error.to_string())?;
    let message = probe_message(&message_id());
    // Multicast may not leave a container; the hosts below are for that.
    for repeat in 0..REPEAT {
        if repeat > 0 {
            tokio::time::sleep(REPEAT_GAP).await;
        }
        if let Err(error) = socket.send_to(message.as_bytes(), MULTICAST).await {
            tracing::debug!("the discovery multicast didn't go out: {error}");
        }
    }
    for ip in known {
        let _ = socket
            .send_to(message.as_bytes(), SocketAddr::new(*ip, PORT))
            .await;
    }
    for host in hosts {
        let Ok(addresses) = tokio::net::lookup_host((host.as_str(), PORT)).await else {
            tracing::debug!("a camera host didn't resolve");
            continue;
        };
        for address in addresses.filter(|address| is_lan_ip(address.ip())) {
            let _ = socket.send_to(message.as_bytes(), address).await;
        }
    }
    let mut answers: BTreeMap<String, Found> = BTreeMap::new();
    let mut buffer = vec![0u8; 16 * 1024];
    let deadline = tokio::time::Instant::now() + LISTEN;
    while answers.len() < MAX_FOUND {
        let received = tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await;
        let Ok(Ok((length, from))) = received else {
            break;
        };
        if !is_lan_ip(from.ip()) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&buffer[..length]) else {
            continue;
        };
        for found in matches(text, from.ip()) {
            let from_there = answers
                .values()
                .filter(|known| known.ip == found.ip)
                .count();
            if from_there >= MAX_PER_ADDRESS || answers.len() >= MAX_FOUND {
                break;
            }
            answers.entry(found.id.clone()).or_insert(found);
        }
    }
    Ok(answers.into_values().collect())
}

fn message_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    let hex = crate::http::hex(&bytes);
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// A `Probe` for network video transmitters.
pub fn probe_message(message_id: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" \
         xmlns:a=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\" \
         xmlns:d=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\" \
         xmlns:dn=\"http://www.onvif.org/ver10/network/wsdl\">\
         <s:Header><a:Action s:mustUnderstand=\"1\">\
         http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</a:Action>\
         <a:MessageID>{message_id}</a:MessageID>\
         <a:ReplyTo><a:Address>\
         http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address></a:ReplyTo>\
         <a:To s:mustUnderstand=\"1\">urn:schemas-xmlsoap-org:ws:2005:04:discovery</a:To>\
         </s:Header><s:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe>\
         </s:Body></s:Envelope>"
    )
}

/// The cameras in one `ProbeMatches` answer from `from`.
pub fn matches(answer: &str, from: IpAddr) -> Vec<Found> {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return Vec::new();
    };
    find_all(doc.root(), "ProbeMatch")
        .filter_map(|probe_match| {
            let address = find(probe_match, "EndpointReference")
                .and_then(|reference| text(reference, &["Address"]))?;
            let id = id_of(&address)?;
            let xaddrs = text(probe_match, &["XAddrs"])?;
            let device_service = pick_xaddr(&xaddrs, from)?;
            let scopes = text(probe_match, &["Scopes"]).unwrap_or_default();
            let name = scope(&scopes, "name");
            let hardware = scope(&scopes, "hardware");
            let name = name
                .clone()
                .or_else(|| hardware.clone())
                .unwrap_or_else(|| "Camera".to_owned());
            Some(Found {
                id,
                ip: from,
                device_service,
                name,
                hardware,
            })
        })
        .collect()
}

/// The id from an endpoint address: `urn:uuid:…` or `uuid:…`, lower case, without the prefix.
pub fn id_of(address: &str) -> Option<String> {
    let address = address.trim();
    let lower = address.to_ascii_lowercase();
    let id = lower
        .strip_prefix("urn:uuid:")
        .or_else(|| lower.strip_prefix("uuid:"))
        .unwrap_or(&lower);
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    ok.then(|| id.to_owned())
}

/// The device service address to use: the one on the address it answered from, else the
/// first plain-http one. Either way it's sent to `from` ([`Target`]).
fn pick_xaddr(xaddrs: &str, from: IpAddr) -> Option<Target> {
    let candidates: Vec<&str> = xaddrs.split_whitespace().collect();
    let from_text = match from {
        IpAddr::V6(ip) => format!("[{ip}]"),
        ip => ip.to_string(),
    };
    candidates
        .iter()
        .find(|url| {
            url.to_ascii_lowercase()
                .strip_prefix("http://")
                .is_some_and(|rest| {
                    rest.starts_with(&format!("{from_text}:"))
                        || rest.starts_with(&format!("{from_text}/"))
                })
        })
        .or_else(|| candidates.first())
        .and_then(|url| Target::parse(url).ok())
}

/// A scope's value: `onvif://www.onvif.org/name/Front%20door` → `Front door`.
fn scope(scopes: &str, kind: &str) -> Option<String> {
    let marker = format!("onvif://www.onvif.org/{kind}/");
    scopes
        .split_whitespace()
        .find_map(|scope| scope.strip_prefix(&marker))
        .map(percent_decode)
        .map(|value| {
            value
                .replace('_', " ")
                .trim()
                .chars()
                .take(100)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && let Some(hex) = text.get(at + 1..at + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape the Reolink on the user's network answered with, its serial left out.
    const REOLINK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<SOAP-ENV:Envelope xmlns:SOAP-ENV="http://www.w3.org/2003/05/soap-envelope"
 xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing"
 xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
 xmlns:dn="http://www.onvif.org/ver10/network/wsdl">
<SOAP-ENV:Body><d:ProbeMatches><d:ProbeMatch>
<wsa:EndpointReference><wsa:Address>urn:uuid:05121508-0115-0400-0613-ec71dbc9da16</wsa:Address></wsa:EndpointReference>
<d:Types>dn:NetworkVideoTransmitter</d:Types>
<d:Scopes>onvif://www.onvif.org/Profile/Streaming onvif://www.onvif.org/type/video_encoder onvif://www.onvif.org/name/Reolink onvif://www.onvif.org/hardware/Reolink%20Duo%202V%20PoE onvif://www.onvif.org/location/</d:Scopes>
<d:XAddrs>http://192.168.68.63:8000/onvif/device_service</d:XAddrs>
<d:MetadataVersion>1</d:MetadataVersion>
</d:ProbeMatch></d:ProbeMatches></SOAP-ENV:Body></SOAP-ENV:Envelope>"#;

    #[test]
    fn a_probe_match_names_the_camera_and_its_service() {
        let ip: IpAddr = "192.168.68.63".parse().expect("ip");
        let found = matches(REOLINK, ip);
        assert_eq!(
            found,
            [Found {
                id: "05121508-0115-0400-0613-ec71dbc9da16".into(),
                ip,
                device_service: Target {
                    port: 8000,
                    path: "/onvif/device_service".into()
                },
                name: "Reolink".into(),
                hardware: Some("Reolink Duo 2V PoE".into()),
            }]
        );
    }

    #[test]
    fn the_address_it_answered_from_is_preferred() {
        let ip: IpAddr = "192.168.1.5".parse().expect("ip");
        let target = pick_xaddr(
            "http://169.254.3.3/onvif/device_service http://192.168.1.5:2020/onvif/device_service",
            ip,
        );
        assert_eq!(target.map(|target| target.port), Some(2020));
    }

    #[test]
    fn an_id_is_kept_to_what_a_handle_can_be() {
        assert_eq!(id_of("urn:uuid:ABC-1"), Some("abc-1".into()));
        assert_eq!(id_of("uuid:abc"), Some("abc".into()));
        assert_eq!(id_of("urn:uuid:"), None);
        assert_eq!(id_of("urn:uuid:a b"), None);
    }
}
