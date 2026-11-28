//! One task per known host: read it, wait, read it again.
//!
//! A host that misses two readings in a row is reported as not answering, and asked less and
//! less often until it answers again. Nothing here remembers a monitor between readings: each
//! one resolves the name, opens a connection, and checks the answer from scratch.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::http;
use crate::measures::{self, Reading};
use crate::settings;

/// Misses before a host is reported as not answering.
const MISSES: u32 = 2;
/// The longest wait between attempts at a host that isn't answering, unless a poll is longer.
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// A monitor's local API is on the plain HTTP port.
pub const PORT: u16 = 80;

/// Where one monitor is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// As written in `known_hosts`: an address or a local name.
    pub host: String,
    pub port: u16,
}

#[derive(Debug)]
pub enum Event {
    /// The host at this place in the list answered with a reading.
    Read { host: usize, reading: Box<Reading> },
    /// It has missed two readings in a row.
    Silent { host: usize },
}

pub fn spawn(
    host: usize,
    target: Target,
    poll: Duration,
    events: mpsc::Sender<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut misses = 0u32;
        loop {
            let wait = match read(&target).await {
                Ok(reading) => {
                    misses = 0;
                    let event = Event::Read {
                        host,
                        reading: Box::new(reading),
                    };
                    if events.send(event).await.is_err() {
                        return;
                    }
                    poll
                }
                Err(why) => {
                    misses = misses.saturating_add(1);
                    tracing::debug!("the monitor at {} gave no reading: {why}", target.host);
                    if misses == MISSES && events.send(Event::Silent { host }).await.is_err() {
                        return;
                    }
                    backoff(poll, misses)
                }
            };
            tokio::time::sleep(wait).await;
        }
    })
}

/// How long to leave a host alone after `misses` in a row: a poll longer each time.
fn backoff(poll: Duration, misses: u32) -> Duration {
    poll.saturating_mul(misses).min(MAX_BACKOFF.max(poll))
}

async fn read(target: &Target) -> Result<Reading, String> {
    let address = resolve(target).await?;
    let response = http::get(address, &target.host, measures::PATH, http::EXCHANGE)
        .await
        .map_err(|error| error.to_string())?;
    // A redirect is an answer like any other that isn't a reading: it is never followed.
    if response.status != 200 {
        return Err(format!("it answered with status {}", response.status));
    }
    measures::parse(&response.body).map_err(str::to_owned)
}

/// The address to ask. A name is looked up every time, so a monitor that moves is found again,
/// and what it resolves to is only used when it is on the local network.
async fn resolve(target: &Target) -> Result<SocketAddr, String> {
    let lookup = tokio::net::lookup_host((target.host.as_str(), target.port));
    let addresses = tokio::time::timeout(http::EXCHANGE, lookup)
        .await
        .map_err(|_| "its name took too long to look up".to_owned())?
        .map_err(|error| error.to_string())?;
    pick(addresses).ok_or_else(|| "its name leads to no address on the local network".to_owned())
}

/// The local address to ask, of those a name resolves to. Monitors are on IPv4, so that comes
/// first when a name has both.
fn pick(addresses: impl Iterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let mut local: Vec<SocketAddr> = addresses
        .filter(|address| settings::is_lan_ip(address.ip()))
        .collect();
    local.sort_by_key(|address| !address.is_ipv4());
    local.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_host_is_asked_less_often_up_to_five_minutes() {
        let poll = Duration::from_secs(60);
        assert_eq!(backoff(poll, 1), Duration::from_secs(60));
        assert_eq!(backoff(poll, 2), Duration::from_secs(120));
        assert_eq!(backoff(poll, 50), Duration::from_secs(300));
        assert_eq!(backoff(poll, u32::MAX), Duration::from_secs(300));
    }

    #[test]
    fn a_slow_poll_is_never_sped_up_by_a_miss() {
        let poll = Duration::from_secs(3600);
        assert_eq!(backoff(poll, 1), poll);
        assert_eq!(backoff(poll, 9), poll);
    }

    #[test]
    fn of_a_names_addresses_only_a_local_one_is_asked_and_ipv4_first() {
        let address = |text: &str| text.parse::<SocketAddr>().expect("an address");
        // Documentation addresses stand in for public ones.
        let public = [address("192.0.2.10:80"), address("[2001:db8::10]:80")];
        assert_eq!(pick(public.into_iter()), None);
        let mixed = [
            address("192.0.2.10:80"),
            address("[::1]:80"),
            address("10.0.0.21:80"),
        ];
        assert_eq!(pick(mixed.into_iter()), Some(address("10.0.0.21:80")));
        assert_eq!(
            pick([address("[fd00::21]:80")].into_iter()),
            Some(address("[fd00::21]:80"))
        );
    }

    #[tokio::test]
    async fn a_public_address_is_never_asked() {
        // A documentation address stands in for any public one. Nothing is sent to it.
        let target = Target {
            host: "192.0.2.10".to_owned(),
            port: PORT,
        };
        let error = read(&target).await.expect_err("refused before connecting");
        assert!(error.contains("local network"), "{error}");
    }
}
