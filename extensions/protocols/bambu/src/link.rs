//! The connection to the printer: its own MQTT, over TLS, signed in with the access code.
//!
//! **What is trusted.** The printer shows a certificate that no public authority issued for
//! its address, so the usual check can't pass. Instead:
//! - the handshake's signature is always checked against the certificate, so whoever answers
//!   holds that certificate's key;
//! - with a fingerprint to expect (typed into the settings, or remembered from the last time
//!   the access code was accepted), only that certificate is taken, and the access code is not
//!   sent to anything else;
//! - with none, on the very first connection, the certificate shown is taken as it is.
//!
//! Whether Bambu Lab issued the certificate, and whether it names this printer's serial
//! number, is not checked.
//!
//! Real network I/O lives only here. `lib.rs` sees [`LinkEvent`]s.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rumqttc::{
    AsyncClient, ConnectReturnCode, ConnectionError, Event, MqttOptions, Packet, QoS, StateError,
    TlsConfiguration, Transport,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::settings::Settings;

/// The largest message taken from the printer. A whole status is a few tens of kilobytes; a
/// bigger message drops the connection, since the MQTT library can't skip one.
pub const MOST_MESSAGE: usize = 256 * 1024;

/// Asks the printer for its whole status. Printers that only send what changed need it once
/// per connection; it changes nothing on the printer.
const PUSHALL: &str = r#"{"pushing":{"sequence_id":"0","command":"pushall"}}"#;

/// How long to wait before connecting again.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// After the first failure, doubled after each one that follows soon.
    pub first: Duration,
    /// The longest wait, and the wait after a refusal that trying again can't fix.
    pub longest: Duration,
    /// How long a connection has to hold for the wait to go back to `first`.
    pub steady: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            longest: Duration::from_secs(60),
            steady: Duration::from_secs(30),
        }
    }
}

/// Why there is no connection, in words for the extension's card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Down {
    Unreachable(String),
    WrongCode,
    /// The printer showed another certificate than expected: this one's fingerprint.
    WrongCertificate(String),
    TooBig(usize),
}

impl fmt::Display for Down {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Down::Unreachable(error) => write!(
                f,
                "the printer can't be reached ({error}); check its address, and that its access code is still the one in the settings"
            ),
            Down::WrongCode => f.write_str(
                "the printer refused the access code; enter the one its screen shows now",
            ),
            Down::WrongCertificate(shown) => write!(
                f,
                "the printer's address answered with a different certificate than expected, so the access code wasn't sent. If the printer was reset or replaced, enter the new certificate's fingerprint in the settings to trust it: {shown}"
            ),
            Down::TooBig(size) => write!(
                f,
                "the printer sent a message of {size} bytes, over the {MOST_MESSAGE} this takes"
            ),
        }
    }
}

#[derive(Debug)]
pub enum LinkEvent {
    /// The access code was accepted, by a printer showing the certificate with this fingerprint.
    Connected {
        certificate: String,
    },
    /// A message on the printer's report topic.
    Report(Vec<u8>),
    Down(Down),
}

/// A certificate's fingerprint: SHA-256 of its DER, as lowercase hex.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What the last handshake showed.
#[derive(Debug, Default)]
struct Shown {
    fingerprint: Option<String>,
    refused: bool,
}

#[derive(Debug)]
struct Pinned {
    provider: Arc<rustls::crypto::CryptoProvider>,
    expect: Option<String>,
    shown: Arc<Mutex<Shown>>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let print = fingerprint(end_entity.as_ref());
        let refused = self.expect.as_ref().is_some_and(|expect| *expect != print);
        *self.shown.lock().expect("not poisoned") = Shown {
            fingerprint: Some(print),
            refused,
        };
        if refused {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.provider.signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, cert, dss, algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.provider.signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, cert, dss, algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// TLS 1.2 only: the public client for these printers notes a firmware that never answers a
/// TLS 1.3 hello.
fn tls(expect: Option<String>, shown: Arc<Mutex<Shown>>) -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Pinned {
        provider: Arc::clone(&provider),
        expect,
        shown,
    });
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS12])
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// Connects and keeps connecting, in the background, until the receiver is dropped or the task
/// is aborted. `expect` is the only certificate taken, by fingerprint, when there is one.
pub fn connect(
    settings: &Settings,
    expect: Option<String>,
    timing: Timing,
) -> Result<(mpsc::Receiver<LinkEvent>, tokio::task::JoinHandle<()>), String> {
    let shown = Arc::new(Mutex::new(Shown::default()));
    let client_id = format!("irori-{}", std::process::id());
    let mut options = MqttOptions::new(client_id, settings.host.clone(), settings.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_max_packet_size(MOST_MESSAGE, 4096);
    options.set_credentials("bblp", settings.access_code.expose());
    let config = tls(expect, Arc::clone(&shown))?;
    options.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(
        Arc::new(config),
    )));
    let report = format!("device/{}/report", settings.serial);
    let request = format!("device/{}/request", settings.serial);

    let (client, mut event_loop) = AsyncClient::new(options, 16);
    let (tx, rx) = mpsc::channel(64);
    let handle = tokio::spawn(async move {
        let mut connected_at: Option<Instant> = None;
        let mut retry: Option<Duration> = None;
        loop {
            let event = match event_loop.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    connected_at = Some(Instant::now());
                    // The `try_` forms: only this task empties the request queue, so it can't
                    // wait on it. Both go out on the next polls.
                    let asked = client
                        .try_subscribe(report.as_str(), QoS::AtMostOnce)
                        .and_then(|()| {
                            client.try_publish(request.as_str(), QoS::AtMostOnce, false, PUSHALL)
                        });
                    if let Err(error) = asked {
                        tracing::warn!(%error, "couldn't ask the printer for its status");
                    }
                    let certificate = shown.lock().expect("not poisoned").fingerprint.clone();
                    LinkEvent::Connected {
                        certificate: certificate.unwrap_or_default(),
                    }
                }
                Ok(Event::Incoming(Packet::Publish(publish))) if publish.topic == report => {
                    LinkEvent::Report(publish.payload.to_vec())
                }
                Ok(_) => continue,
                Err(error) => {
                    let down = why(&error, &shown);
                    let steady = connected_at
                        .take()
                        .is_some_and(|since| since.elapsed() >= timing.steady);
                    let pause = match (&down, retry) {
                        (Down::WrongCode | Down::WrongCertificate(_), _) => timing.longest,
                        (_, Some(last)) if !steady => (last * 2).min(timing.longest),
                        _ => timing.first,
                    };
                    retry = Some(pause);
                    tracing::warn!(retry_in = ?pause, "no connection to the printer: {down}");
                    if tx.send(LinkEvent::Down(down)).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(pause).await;
                    continue;
                }
            };
            if tx.send(event).await.is_err() {
                return; // the protocol stopped
            }
        }
    });
    Ok((rx, handle))
}

fn why(error: &ConnectionError, shown: &Mutex<Shown>) -> Down {
    let mut shown = shown.lock().expect("not poisoned");
    if std::mem::take(&mut shown.refused) {
        return Down::WrongCertificate(shown.fingerprint.clone().unwrap_or_default());
    }
    match error {
        ConnectionError::ConnectionRefused(
            ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized,
        ) => Down::WrongCode,
        ConnectionError::MqttState(StateError::Deserialization(
            rumqttc::mqttbytes::Error::PayloadSizeLimitExceeded(size),
        )) => Down::TooBig(*size),
        other => Down::Unreachable(other.to_string()),
    }
}
