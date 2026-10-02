//! How a receiver is found: mDNS `_googlecast._tcp`, and the known hosts in settings.
//!
//! A known host is asked for `http://{host}:8008/setup/eureka_info` with a plain HTTP/1.0 GET.
//! One that doesn't answer stays on the list; it is not removed.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::settings::{self, Settings};

const SERVICE: &str = "_googlecast._tcp.local.";
const CAST_PORT: u16 = 8009;
const EUREKA_PORT: u16 = 8008;
const POLL: Duration = Duration::from_secs(30);
const EXCHANGE: Duration = Duration::from_secs(3);
const BODY_CAP: usize = 64 * 1024;

/// One receiver, ready to be connected to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub uuid: String,
    pub name: String,
    pub model: String,
    pub version: String,
    pub address: SocketAddr,
    /// Set when this came from `known_hosts`, so a later success clears that host's failure.
    pub known_host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    Seen(Found),
    /// A known host didn't answer. The host is kept for the next poll.
    Unreachable {
        host: String,
    },
}

/// Starts discovery. Fails only when mDNS itself can't start and there is no known host to fall
/// back on, so the core restarts the protocol.
pub fn start(
    settings: &Settings,
    events: mpsc::Sender<Discovery>,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let daemon = match ServiceDaemon::new() {
        Ok(daemon) => Some(daemon),
        Err(error) => {
            if settings.known_hosts.is_empty() {
                return Err(format!("Cast discovery isn't available: {error}"));
            }
            tracing::warn!("Cast discovery isn't available; known hosts are still polled");
            None
        }
    };
    let browse = match daemon.as_ref() {
        Some(daemon) => match daemon.browse(SERVICE) {
            Ok(receiver) => Some(receiver),
            Err(error) => {
                if settings.known_hosts.is_empty() {
                    return Err(format!("Cast discovery isn't available: {error}"));
                }
                tracing::warn!("Cast discovery isn't available; known hosts are still polled");
                None
            }
        },
        None => None,
    };
    let hosts = settings.known_hosts.clone();
    Ok(tokio::spawn(async move {
        // The daemon has to outlive the browse receiver.
        let _daemon = daemon;
        run(browse, hosts, events).await;
    }))
}

async fn run(
    mut browse: Option<mdns_sd::Receiver<ServiceEvent>>,
    hosts: Vec<String>,
    events: mpsc::Sender<Discovery>,
) {
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = recv_browse(&mut browse) => {
                let Some(event) = event else { break };
                if let ServiceEvent::ServiceResolved(service) = event
                    && let Some(found) = from_resolved(&service)
                    && events.send(Discovery::Seen(found)).await.is_err()
                {
                    break;
                }
            }
            _ = poll.tick(), if !hosts.is_empty() => {
                for host in &hosts {
                    let discovery = match fetch_eureka(host).await {
                        Some(found) => Discovery::Seen(found),
                        None => Discovery::Unreachable { host: host.clone() },
                    };
                    if events.send(discovery).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

async fn recv_browse(browse: &mut Option<mdns_sd::Receiver<ServiceEvent>>) -> Option<ServiceEvent> {
    let Some(browse) = browse else {
        std::future::pending().await
    };
    browse.recv_async().await.ok()
}

pub(crate) fn from_resolved(service: &mdns_sd::ResolvedService) -> Option<Found> {
    identify(
        service.get_port(),
        service
            .get_addresses()
            .iter()
            .map(mdns_sd::ScopedIp::to_ip_addr),
        |key| service.get_property_val_str(key).map(str::to_owned),
        None,
    )
}

/// `port` 0 is the Cast default, 8009. An unspecified address is skipped. IPv4 wins when both
/// families were announced.
pub(crate) fn identify(
    port: u16,
    addresses: impl Iterator<Item = IpAddr>,
    mut txt: impl FnMut(&str) -> Option<String>,
    known_host: Option<String>,
) -> Option<Found> {
    let uuid = txt("id").and_then(|value| settings::uuid_of(value.trim()))?;
    let port = if port == 0 { CAST_PORT } else { port };
    let addresses: Vec<IpAddr> = addresses.filter(|ip| !ip.is_unspecified()).collect();
    let ip = addresses
        .iter()
        .copied()
        .find(IpAddr::is_ipv4)
        .or_else(|| addresses.first().copied())?;
    let name = txt("fn").unwrap_or_else(|| "Cast".to_owned());
    Some(Found {
        uuid,
        name,
        model: txt("md").unwrap_or_default(),
        version: txt("ve").unwrap_or_default(),
        address: SocketAddr::new(ip, port),
        known_host,
    })
}

/// Asks one known host who it is. `None` means it didn't answer; the caller keeps the host.
pub(crate) async fn fetch_eureka(host: &str) -> Option<Found> {
    read_eureka(host, EUREKA_PORT).await
}

async fn read_eureka(host: &str, port: u16) -> Option<Found> {
    let exchange = async {
        let stream = connect_eureka(host, port).await.ok()?;
        let ip = stream.peer_addr().ok()?.ip();
        let mut stream = stream;
        let header = host_header(host);
        let request = format!(
            "GET /setup/eureka_info HTTP/1.0\r\nHost: {header}:{port}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut buf = vec![0; BODY_CAP];
        let mut read = 0;
        while read < buf.len() {
            let n = stream.read(&mut buf[read..]).await.ok()?;
            if n == 0 {
                break;
            }
            read += n;
        }
        let text = String::from_utf8_lossy(&buf[..read]);
        let body = text
            .split_once("\r\n\r\n")
            .or_else(|| text.split_once("\n\n"))
            .map(|(_, body)| body)
            .unwrap_or(&text);
        let info = parse_eureka(body)?;
        Some(Found {
            uuid: info.uuid,
            name: info.name,
            model: String::new(),
            version: info.version,
            address: SocketAddr::new(ip, CAST_PORT),
            known_host: Some(host.to_owned()),
        })
    };
    tokio::time::timeout(EXCHANGE, exchange)
        .await
        .ok()
        .flatten()
}

async fn connect_eureka(host: &str, port: u16) -> std::io::Result<TcpStream> {
    if let Some(ip) = host_ip(host) {
        TcpStream::connect((ip, port)).await
    } else {
        TcpStream::connect((host, port)).await
    }
}

fn host_ip(host: &str) -> Option<IpAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    host.parse().ok()
}

pub(crate) fn host_header(host: &str) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

struct Eureka {
    uuid: String,
    name: String,
    version: String,
}

fn parse_eureka(body: &str) -> Option<Eureka> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let uuid = value
        .get("ssdp_udn")
        .and_then(serde_json::Value::as_str)
        .and_then(settings::uuid_of)?;
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Cast")
        .to_owned();
    let version = value
        .get("cast_build_revision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some(Eureka {
        uuid,
        name,
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::Ipv4Addr;

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_ascii_lowercase(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn an_announcement_prefers_ipv4_and_a_real_port() {
        let txt = props(&[
            ("id", "uuid:00112233-4455-6677-8899-AABBCCDDEEFF"),
            ("fn", "Living room"),
            ("md", "Chromecast"),
            ("ve", "1.56"),
        ]);
        let found = identify(
            8009,
            [
                IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ]
            .into_iter(),
            |key| txt.get(&key.to_ascii_lowercase()).cloned(),
            None,
        )
        .expect("a receiver");
        assert_eq!(found.uuid, "00112233445566778899aabbccddeeff");
        assert_eq!(found.name, "Living room");
        assert_eq!(found.model, "Chromecast");
        assert_eq!(found.version, "1.56");
        assert_eq!(found.address, "192.168.1.20:8009".parse().expect("addr"));
    }

    #[test]
    fn a_missing_port_becomes_the_cast_default() {
        let txt = props(&[("id", "00112233445566778899aabbccddeeff")]);
        let found = identify(
            0,
            [IpAddr::V4(Ipv4Addr::LOCALHOST)].into_iter(),
            |key| txt.get(key).cloned(),
            None,
        )
        .expect("a receiver");
        assert_eq!(found.address.port(), CAST_PORT);
    }

    #[test]
    fn an_announcement_without_an_id_is_skipped() {
        let found = identify(
            8009,
            [IpAddr::V4(Ipv4Addr::LOCALHOST)].into_iter(),
            |_| None,
            None,
        );
        assert!(found.is_none());
    }

    #[test]
    fn eureka_info_gives_the_id_the_name_and_the_build() {
        let info = parse_eureka(
            r#"{"name":"Living room","ssdp_udn":"uuid:00112233-4455-6677-8899-aabbccddeeff","cast_build_revision":"1.56.0"}"#,
        )
        .expect("eureka");
        assert_eq!(info.uuid, "00112233445566778899aabbccddeeff");
        assert_eq!(info.name, "Living room");
        assert_eq!(info.version, "1.56.0");
    }

    #[test]
    fn an_ipv6_host_header_is_bracketed() {
        assert_eq!(host_header("fe80::1"), "[fe80::1]");
        assert_eq!(host_header("192.168.1.10"), "192.168.1.10");
        assert_eq!(host_header("tv.local"), "tv.local");
    }

    #[tokio::test]
    async fn a_known_host_is_read_with_a_plain_get() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let body = r#"{"name":"Den","ssdp_udn":"uuid:00112233-4455-6677-8899-aabbccddeeff","cast_build_revision":"9"}"#;
        let response = format!(
            "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0; 1024];
            let n = stream.read(&mut buf).await.expect("read");
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = request_tx.send(request);
            stream.write_all(response.as_bytes()).await.expect("write");
        });
        let found = read_eureka("127.0.0.1", port)
            .await
            .expect("the listener answered");
        let request = request_rx.await.expect("the request was recorded");
        assert!(
            request.starts_with("GET /setup/eureka_info HTTP/1.0\r\n"),
            "{request}"
        );
        assert!(request.contains("Host: 127.0.0.1:"), "{request}");
        assert_eq!(found.name, "Den");
        assert_eq!(found.uuid, "00112233445566778899aabbccddeeff");
        assert_eq!(found.version, "9");
        assert_eq!(found.address.port(), CAST_PORT);
        assert_eq!(found.known_host.as_deref(), Some("127.0.0.1"));
    }
}
