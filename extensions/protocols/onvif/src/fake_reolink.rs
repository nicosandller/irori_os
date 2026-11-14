//! A fake Reolink API on a loopback port, over HTTPS with a version 1 certificate like the real
//! ones, for tests.
//!
//! It answers the way the user's Duo 2V does: its abilities are the ones captured from it, and
//! its lights start as it had them. It counts logins and logouts, and keeps every change it was
//! asked for.

use std::sync::{Arc, Mutex};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use crate::fake::read;

const TOKEN: &str = "f4k3t0k3n";

/// What the API was asked, for a test to look at.
#[derive(Debug, Default, Clone)]
pub struct Seen {
    pub logins: u32,
    pub logouts: u32,
    /// Each change, as `cmd param`.
    pub changes: Vec<String>,
}

#[derive(Debug)]
struct Lights {
    spotlight: (u64, u64),
    infrared: &'static str,
    status: &'static str,
}

#[derive(Clone)]
pub struct FakeReolink {
    pub port: u16,
    seen: Arc<Mutex<Seen>>,
}

impl FakeReolink {
    /// Starts one that takes `username` and `password`.
    pub async fn start(username: &str, password: &str) -> Self {
        Self::start_showing(username, password, false).await
    }

    /// Starts one showing another certificate than [`FakeReolink::start`]'s: something else in a
    /// camera's place, or the camera after a reset.
    pub async fn start_other(username: &str, password: &str) -> Self {
        Self::start_showing(username, password, true).await
    }

    async fn start_showing(username: &str, password: &str, other: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let lights = Arc::new(Mutex::new(Lights {
            spotlight: (0, 85),
            infrared: "Auto",
            status: "On",
        }));
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls(other)));
        let login = (username.to_owned(), password.to_owned());
        let shared = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let seen = Arc::clone(&shared);
                let lights = Arc::clone(&lights);
                let login = login.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let Some(asked) = read(&mut stream).await else {
                        return;
                    };
                    let body = answer(&asked.path, &asked.body, &login, &seen, &lights);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(body.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { port, seen }
    }

    pub fn seen(&self) -> Seen {
        self.seen.lock().expect("seen").clone()
    }
}

fn answer(
    path: &str,
    body: &str,
    login: &(String, String),
    seen: &Mutex<Seen>,
    lights: &Mutex<Lights>,
) -> String {
    let asked: Value = serde_json::from_str(body).unwrap_or_default();
    let item = &asked[0];
    let cmd = item["cmd"].as_str().unwrap_or_default().to_owned();
    let param = &item["param"];
    let fine = |value: Value| json!([{ "cmd": cmd, "code": 0, "value": value }]).to_string();
    let error = |code: i64, detail: &str| {
        json!([{ "cmd": cmd, "code": 1, "error": { "rspCode": code, "detail": detail } }])
            .to_string()
    };
    if cmd == "Login" {
        seen.lock().expect("seen").logins += 1;
        let user = &param["User"];
        if user["userName"] != login.0.as_str() || user["password"] != login.1.as_str() {
            return error(-7, "login failed");
        }
        return fine(json!({ "Token": { "leaseTime": 3600, "name": TOKEN } }));
    }
    if !path.contains(&format!("token={TOKEN}")) {
        return error(-6, "please login first");
    }
    let mut lights = lights.lock().expect("lights");
    match cmd.as_str() {
        "Logout" => {
            seen.lock().expect("seen").logouts += 1;
            fine(json!({}))
        }
        "GetAbility" => {
            fine(serde_json::from_str(include_str!("testdata/reolink-ability.json")).expect("JSON"))
        }
        "GetWhiteLed" => fine(json!({ "WhiteLed": {
            "channel": 0, "mode": 1, "state": lights.spotlight.0, "bright": lights.spotlight.1
        }})),
        "GetIrLights" => fine(json!({ "IrLights": { "state": lights.infrared } })),
        "GetPowerLed" => fine(json!({ "PowerLed": { "channel": 0, "state": lights.status } })),
        "SetWhiteLed" | "SetIrLights" | "SetPowerLed" | "AudioAlarmPlay" => {
            seen.lock()
                .expect("seen")
                .changes
                .push(format!("{cmd} {param}"));
            match cmd.as_str() {
                "SetWhiteLed" => {
                    let led = &param["WhiteLed"];
                    lights.spotlight.0 = led["state"].as_u64().unwrap_or(lights.spotlight.0);
                    lights.spotlight.1 = led["bright"].as_u64().unwrap_or(lights.spotlight.1);
                }
                "SetIrLights" => {
                    lights.infrared = if param["IrLights"]["state"] == "Off" {
                        "Off"
                    } else {
                        "Auto"
                    };
                }
                "SetPowerLed" => {
                    lights.status = if param["PowerLed"]["state"] == "Off" {
                        "Off"
                    } else {
                        "On"
                    };
                }
                _ => {}
            }
            fine(json!({ "rspCode": 200 }))
        }
        _ => error(-9, "not support"),
    }
}

/// Serves the version 1 test certificate, or with `other` a second one. Through a resolver,
/// since the usual way checks the key against the certificate by parsing it, which a version 1
/// certificate doesn't pass.
fn tls(other: bool) -> rustls::ServerConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let (key, cert): (&[u8], &[u8]) = if other {
        (
            include_bytes!("testdata/other-key.der"),
            include_bytes!("testdata/other-cert.der"),
        )
    } else {
        (
            include_bytes!("testdata/v1-key.der"),
            include_bytes!("testdata/v1-cert.der"),
        )
    };
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
    let signer = provider.key_provider.load_private_key(key).expect("a key");
    let cert = CertificateDer::from(cert.to_vec());
    let certified = Arc::new(CertifiedKey::new(vec![cert], signer));
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(One(certified)))
}

#[derive(Debug)]
struct One(Arc<CertifiedKey>);

impl ResolvesServerCert for One {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }
}
