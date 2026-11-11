//! Which camera a login goes to, remembered across restarts.
//!
//! Anything on the network can answer discovery with a known camera's id. Sending that
//! camera's login to whatever answers would hand it over: even a digest of the password can be
//! guessed at offline. So once a camera's login works, where it worked is remembered here, with
//! the fingerprint of the certificate it shows over HTTPS (every camera tried so far has one,
//! made by the camera itself).
//!
//! Before a login goes anywhere, [`check`] decides, sending nothing but a TLS handshake:
//! - a camera with nothing remembered is taken as it is, and remembered once its login works;
//! - one that shows the remembered certificate is taken as the same camera, wherever it answers
//!   from, since the handshake proves whatever answers on its HTTPS holds that certificate's key;
//! - one that had no certificate is the same camera only at the remembered address;
//! - anything else gets no login. It waits for the person to say it moved, by typing its new
//!   address (`moved` in the settings). What they type is kept for the certificate they were
//!   asked about ([`moved_key`]): a later reset that changes it again asks again.
//!
//! This stops something else on the network claiming to be the camera. It can't stop something
//! in between that passes the real camera's HTTPS through: the login then goes over the camera's
//! plain HTTP and RTSP, which is how ONVIF works.

use std::collections::BTreeMap;

use irori_protocol::ProtocolContext;
use serde::{Deserialize, Serialize};

use crate::discover::Found;
use crate::http::{self, Shown};

/// The key in the extension's store.
const KEY: &str = "trusted";
/// The most cameras remembered.
const MOST: usize = 64;

/// What's remembered of one camera.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    /// Where its login last worked: its address and its ONVIF port.
    pub address: String,
    /// The fingerprint of the certificate it showed then, if it showed one.
    #[serde(default)]
    pub certificate: Option<String>,
    /// The new address a person last confirmed for it.
    #[serde(default)]
    pub confirmed: Option<String>,
}

/// Where a camera answered from, as a [`Pin`] keeps it.
pub fn address_of(found: &Found) -> String {
    std::net::SocketAddr::new(found.ip, found.device_service.port).to_string()
}

/// What [`check`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Its login may go to it. What to remember of it once the login works.
    Trusted(Pin),
    /// It can't show it's the camera the login is for. Why, in a sentence.
    Untrusted(String),
    /// Its HTTPS didn't answer in time, so it can't be told yet. Ask again later.
    NoAnswer,
}

/// Decides whether `found` gets its camera's login. `moved` is what the person typed to say a
/// camera moved, by [`moved_key`]. Sends nothing but a TLS handshake to `https_port`. Also
/// returns the key the person's answer goes under, should they be asked.
pub async fn check(
    pin: Option<&Pin>,
    found: &Found,
    https_port: u16,
    moved: &BTreeMap<String, String>,
) -> (Verdict, String) {
    let shown = http::certificate(found.ip, https_port).await;
    let key = moved_key(&found.id, &shown);
    let confirmed = moved.get(&key).map(String::as_str);
    (decide(pin, found, &shown, confirmed), key)
}

/// Where the person's "it moved here" goes in the settings: the camera's id with the start of
/// the certificate it was asked about, so an answer only ever stands for that certificate.
pub fn moved_key(id: &str, shown: &Shown) -> String {
    match shown {
        Shown::Certificate(print) => format!("{id}@{}", print.get(..16).unwrap_or(print)),
        Shown::Nothing | Shown::NoAnswer => format!("{id}@none"),
    }
}

fn decide(pin: Option<&Pin>, found: &Found, shown: &Shown, confirmed: Option<&str>) -> Verdict {
    let address = address_of(found);
    let certificate = match shown {
        Shown::Certificate(print) => Some(print.clone()),
        Shown::Nothing | Shown::NoAnswer => None,
    };
    let Some(pin) = pin else {
        if *shown == Shown::NoAnswer {
            return Verdict::NoAnswer;
        }
        return Verdict::Trusted(Pin {
            address,
            certificate,
            confirmed: None,
        });
    };
    // The person said it moved here, for what it shows now.
    let ip = found.ip.to_string();
    if confirmed.is_some_and(|typed| typed.trim() == ip) {
        if *shown == Shown::NoAnswer {
            return Verdict::NoAnswer;
        }
        return Verdict::Trusted(Pin {
            address,
            certificate,
            confirmed: Some(ip),
        });
    }
    let kept = |certificate: Option<String>| {
        Verdict::Trusted(Pin {
            address: address.clone(),
            certificate,
            confirmed: pin.confirmed.clone(),
        })
    };
    match (&pin.certificate, shown) {
        (Some(known), Shown::Certificate(print)) if known == print => kept(Some(print.clone())),
        (Some(_), Shown::Certificate(_)) => Verdict::Untrusted(format!(
            "it answered from {ip} with a different certificate than the camera had"
        )),
        (Some(_), Shown::NoAnswer) => Verdict::NoAnswer,
        (Some(_), Shown::Nothing) => Verdict::Untrusted(format!(
            "it answered from {ip} without the certificate the camera had"
        )),
        // It had none to show: only where it was is it known.
        (None, _) if pin.address == address => {
            if *shown == Shown::NoAnswer {
                kept(None)
            } else {
                kept(certificate)
            }
        }
        (None, _) => Verdict::Untrusted(format!(
            "it answered from {ip}, not from where its login last worked"
        )),
    }
}

/// Every camera's [`Pin`], by its id.
#[derive(Debug, Default)]
pub struct Trust(BTreeMap<String, Pin>);

impl Trust {
    /// What the store has. A store that can't be read leaves nothing remembered, with a warning:
    /// each camera is then taken as it first answers, as it was before anything was kept.
    pub async fn load(ctx: &ProtocolContext) -> Self {
        match ctx.load(KEY).await {
            Ok(Some(value)) => match serde_json::from_value(value) {
                Ok(map) => Self(map),
                Err(_) => {
                    tracing::warn!("the trusted cameras couldn't be read; starting afresh");
                    Self::default()
                }
            },
            Ok(None) => Self::default(),
            Err(_) => {
                tracing::warn!("the trusted cameras couldn't be loaded");
                Self::default()
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&Pin> {
        self.0.get(id)
    }

    /// Remembers what a camera whose login just worked is, if that's news.
    pub async fn pin(&mut self, ctx: &ProtocolContext, id: &str, pin: Pin) {
        if self.0.get(id) == Some(&pin) || (!self.0.contains_key(id) && self.0.len() >= MOST) {
            return;
        }
        self.0.insert(id.to_owned(), pin);
        let value = serde_json::to_value(&self.0).unwrap_or_default();
        if ctx.store(KEY, value).await.is_err() {
            tracing::warn!("a trusted camera couldn't be remembered");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Target;
    use std::net::IpAddr;

    fn found(ip: &str, port: u16) -> Found {
        Found {
            id: "cam".into(),
            ip: ip.parse::<IpAddr>().expect("an address"),
            device_service: Target {
                port,
                path: "/onvif/device_service".into(),
            },
            name: "Cam".into(),
            hardware: None,
        }
    }

    fn shown(print: &str) -> Shown {
        Shown::Certificate(print.into())
    }

    fn pin(address: &str, certificate: Option<&str>) -> Pin {
        Pin {
            address: address.into(),
            certificate: certificate.map(str::to_owned),
            confirmed: None,
        }
    }

    #[test]
    fn a_camera_with_nothing_remembered_is_taken_as_it_answers() {
        let here = found("192.168.1.5", 80);
        assert_eq!(
            decide(None, &here, &shown("aa"), None),
            Verdict::Trusted(pin("192.168.1.5:80", Some("aa")))
        );
        assert_eq!(
            decide(None, &here, &Shown::Nothing, None),
            Verdict::Trusted(pin("192.168.1.5:80", None))
        );
        assert_eq!(
            decide(None, &here, &Shown::NoAnswer, None),
            Verdict::NoAnswer
        );
    }

    #[test]
    fn its_certificate_follows_it_anywhere_and_nothing_else_does() {
        let known = pin("192.168.1.5:80", Some("aa"));
        let moved = found("192.168.1.9", 80);
        assert_eq!(
            decide(Some(&known), &moved, &shown("aa"), None),
            Verdict::Trusted(pin("192.168.1.9:80", Some("aa"))),
            "the same key, somewhere new"
        );
        assert!(matches!(
            decide(Some(&known), &moved, &shown("bb"), None),
            Verdict::Untrusted(_)
        ));
        assert!(matches!(
            decide(Some(&known), &moved, &Shown::Nothing, None),
            Verdict::Untrusted(_)
        ));
        assert!(
            matches!(
                decide(Some(&known), &found("192.168.1.5", 80), &shown("bb"), None),
                Verdict::Untrusted(_)
            ),
            "the right address with the wrong key is something else in its place"
        );
        assert_eq!(
            decide(Some(&known), &moved, &Shown::NoAnswer, None),
            Verdict::NoAnswer
        );
    }

    #[test]
    fn a_camera_without_a_certificate_is_known_by_where_it_was() {
        let known = pin("192.168.1.5:80", None);
        assert_eq!(
            decide(
                Some(&known),
                &found("192.168.1.5", 80),
                &Shown::Nothing,
                None
            ),
            Verdict::Trusted(known.clone())
        );
        assert_eq!(
            decide(Some(&known), &found("192.168.1.5", 80), &shown("aa"), None),
            Verdict::Trusted(pin("192.168.1.5:80", Some("aa"))),
            "a certificate it starts showing is kept from then on"
        );
        assert!(matches!(
            decide(
                Some(&known),
                &found("192.168.1.9", 80),
                &Shown::Nothing,
                None
            ),
            Verdict::Untrusted(_)
        ));
        assert!(
            matches!(
                decide(
                    Some(&known),
                    &found("192.168.1.5", 8080),
                    &Shown::Nothing,
                    None
                ),
                Verdict::Untrusted(_)
            ),
            "another port on the same machine is another thing"
        );
    }

    #[test]
    fn a_typed_address_lets_it_in_there_once() {
        let known = pin("192.168.1.5:80", Some("aa"));
        let moved = found("192.168.1.9", 80);
        let Verdict::Trusted(confirmed) =
            decide(Some(&known), &moved, &shown("cc"), Some(" 192.168.1.9 "))
        else {
            panic!("the person said it moved there");
        };
        assert_eq!(confirmed.certificate.as_deref(), Some("cc"));
        assert_eq!(confirmed.confirmed.as_deref(), Some("192.168.1.9"));
        assert!(
            matches!(
                decide(Some(&known), &moved, &shown("cc"), Some("192.168.1.7")),
                Verdict::Untrusted(_)
            ),
            "only the address that was typed"
        );
        // What was typed stands for the certificate it was asked about: a new one there later
        // is asked about again, under another key, and can be confirmed again.
        let typed = BTreeMap::from([(
            moved_key("cam", &shown("cccccccccccccccccccc")),
            "192.168.1.9".to_owned(),
        )]);
        let key = moved_key("cam", &shown("dddddddddddddddddddd"));
        assert!(!typed.contains_key(&key));
        assert!(matches!(
            decide(Some(&confirmed), &moved, &shown("dd"), None),
            Verdict::Untrusted(_)
        ));
        assert!(matches!(
            decide(Some(&confirmed), &moved, &shown("dd"), Some("192.168.1.9")),
            Verdict::Trusted(_)
        ));
        assert_eq!(moved_key("cam", &shown("ab")), "cam@ab");
        assert_eq!(moved_key("cam", &Shown::Nothing), "cam@none");
    }
}
