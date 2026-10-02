//! A Cast receiver that speaks TLS on a local port and records the JSON it is sent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};
use tokio_rustls::TlsAcceptor;

use crate::proto::{self, CastMessage, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER, SENDER};

#[derive(Debug, Clone)]
pub struct Script {
    pub stand_by: Option<bool>,
    pub active_input: Option<bool>,
    pub app_id: Option<String>,
    pub display_name: Option<String>,
    pub player_state: Option<String>,
    pub title: Option<String>,
    pub level: Option<f64>,
    pub muted: Option<bool>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            stand_by: None,
            active_input: None,
            app_id: None,
            display_name: None,
            player_state: None,
            title: None,
            level: Some(0.2),
            muted: Some(false),
        }
    }
}

#[derive(Debug)]
struct Device {
    stand_by: Option<bool>,
    active_input: Option<bool>,
    app_id: Option<String>,
    display_name: Option<String>,
    player_state: Option<String>,
    title: Option<String>,
    content_id: Option<String>,
    content_type: Option<String>,
    level: Option<f64>,
    muted: Option<bool>,
}

impl From<Script> for Device {
    fn from(script: Script) -> Self {
        Self {
            stand_by: script.stand_by,
            active_input: script.active_input,
            app_id: script.app_id,
            display_name: script.display_name,
            player_state: script.player_state,
            title: script.title,
            content_id: None,
            content_type: None,
            level: script.level,
            muted: script.muted,
        }
    }
}

#[derive(Debug)]
pub struct Fake {
    pub addr: std::net::SocketAddr,
    payloads: Arc<Mutex<Vec<String>>>,
    drops: Arc<AtomicU64>,
    notify: Arc<Notify>,
    shutdown: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
}

impl Fake {
    pub async fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let acceptor = TlsAcceptor::from(Arc::new(server_config()));
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let device = Arc::new(Mutex::new(Device::from(script)));
        let drops = Arc::new(AtomicU64::new(0));
        let notify = Arc::new(Notify::new());
        let (shutdown, mut stopped) = watch::channel(false);
        let payloads_task = Arc::clone(&payloads);
        let device_task = Arc::clone(&device);
        let drops_task = Arc::clone(&drops);
        let notify_task = Arc::clone(&notify);
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((tcp, _)) = accepted else { break };
                        let Ok(stream) = acceptor.accept(tcp).await else { continue };
                        let generation = drops_task.load(Ordering::Relaxed);
                        serve(
                            stream,
                            generation,
                            Arc::clone(&payloads_task),
                            Arc::clone(&device_task),
                            Arc::clone(&drops_task),
                            Arc::clone(&notify_task),
                        )
                        .await;
                    }
                    _ = stopped.changed() => break,
                }
            }
        });
        Self {
            addr,
            payloads,
            drops,
            notify,
            shutdown,
            handle,
        }
    }

    pub fn payloads(&self) -> Vec<String> {
        self.payloads.lock().expect("payloads").clone()
    }

    /// Drops the socket the receiver is using now.
    pub fn close(&self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_waiters();
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.handle.abort();
    }
}

async fn serve(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    generation: u64,
    payloads: Arc<Mutex<Vec<String>>>,
    device: Arc<Mutex<Device>>,
    drops: Arc<AtomicU64>,
    notify: Arc<Notify>,
) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    loop {
        tokio::select! {
            frame = proto::read_frame(&mut reader) => {
                let Ok(message) = frame else { return };
                if drops.load(Ordering::Relaxed) != generation {
                    return;
                }
                if message.namespace != NS_CONNECTION && message.namespace != NS_HEARTBEAT {
                    payloads.lock().expect("payloads").push(message.payload.clone());
                }
                let response = answer(&message, &device);
                for payload in response {
                    let destination = if message.namespace == NS_MEDIA {
                        message.source_id.clone()
                    } else {
                        SENDER.to_owned()
                    };
                    let namespace = if payload.contains("MEDIA_STATUS") {
                        NS_MEDIA
                    } else if payload.contains("PONG") {
                        NS_HEARTBEAT
                    } else {
                        message.namespace.as_str()
                    };
                    let frame = proto::encode(&CastMessage::new(destination, namespace, payload));
                    if writer.write_all(&frame).await.is_err() || writer.flush().await.is_err() {
                        return;
                    }
                }
            }
            _ = notify.notified() => {
                if drops.load(Ordering::Relaxed) != generation {
                    return;
                }
            }
        }
    }
}

fn answer(message: &CastMessage, device: &Mutex<Device>) -> Vec<String> {
    if message.namespace == NS_HEARTBEAT {
        return vec![r#"{"type":"PONG"}"#.to_owned()];
    }
    if message.namespace == NS_CONNECTION {
        return Vec::new();
    }
    let payload: serde_json::Value = serde_json::from_str(&message.payload).unwrap_or_default();
    let kind = payload
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let mut device = device.lock().expect("device");
    match kind {
        "GET_STATUS" if message.namespace == NS_RECEIVER => vec![receiver_status(&device)],
        "GET_STATUS" if message.namespace == NS_MEDIA => vec![media_status(&device)],
        "LAUNCH" => {
            device.app_id = Some(crate::map::DEFAULT_MEDIA_RECEIVER.to_owned());
            device.display_name = Some("Default Media Receiver".to_owned());
            device.player_state = None;
            device.title = None;
            device.stand_by = Some(false);
            device.active_input = Some(true);
            vec![receiver_status(&device)]
        }
        "STOP" if message.namespace == NS_RECEIVER => {
            device.app_id = None;
            device.display_name = None;
            device.player_state = None;
            device.title = None;
            vec![receiver_status(&device)]
        }
        "SET_VOLUME" => {
            if let Some(level) = payload
                .get("volume")
                .and_then(|volume| volume.get("level"))
                .and_then(|level| level.as_f64())
            {
                device.level = Some(level);
            }
            if let Some(muted) = payload
                .get("volume")
                .and_then(|volume| volume.get("muted"))
                .and_then(|muted| muted.as_bool())
            {
                device.muted = Some(muted);
            }
            vec![receiver_status(&device)]
        }
        "LOAD" => {
            if let Some(media) = payload.get("media") {
                device.content_id = media
                    .get("contentId")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                device.content_type = media
                    .get("contentType")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                device.title = media
                    .pointer("/metadata/title")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
            }
            device.player_state = Some("PLAYING".to_owned());
            vec![media_status(&device)]
        }
        "PLAY" => {
            device.player_state = Some("PLAYING".to_owned());
            vec![media_status(&device)]
        }
        "PAUSE" => {
            device.player_state = Some("PAUSED".to_owned());
            vec![media_status(&device)]
        }
        "STOP" => {
            device.player_state = Some("IDLE".to_owned());
            vec![media_status(&device)]
        }
        _ => Vec::new(),
    }
}

fn receiver_status(device: &Device) -> String {
    let mut status = serde_json::Map::new();
    if device.level.is_some() || device.muted.is_some() {
        let mut volume = serde_json::Map::new();
        if let Some(level) = device.level {
            volume.insert("level".into(), serde_json::json!(level));
        }
        if let Some(muted) = device.muted {
            volume.insert("muted".into(), serde_json::json!(muted));
        }
        status.insert("volume".into(), serde_json::Value::Object(volume));
    }
    if let Some(stand_by) = device.stand_by {
        status.insert("isStandBy".into(), serde_json::json!(stand_by));
    }
    if let Some(active) = device.active_input {
        status.insert("isActiveInput".into(), serde_json::json!(active));
    }
    if let Some(app) = &device.app_id {
        status.insert(
            "applications".into(),
            serde_json::json!([{
                "appId": app,
                "displayName": device.display_name.clone().unwrap_or_else(|| "App".to_owned()),
                "sessionId": "session-1",
                "transportId": "transport-1",
            }]),
        );
    } else {
        status.insert("applications".into(), serde_json::json!([]));
    }
    serde_json::json!({ "type": "RECEIVER_STATUS", "status": status }).to_string()
}

fn media_status(device: &Device) -> String {
    let status = if let Some(player) = &device.player_state {
        serde_json::json!([{
            "mediaSessionId": 1,
            "playerState": player,
            "currentTime": 1.0,
            "media": {
                "contentId": device.content_id.clone().unwrap_or_default(),
                "contentType": device.content_type.clone().unwrap_or_else(|| "video".to_owned()),
                "metadata": {
                    "metadataType": 0,
                    "title": device.title.clone().unwrap_or_default(),
                }
            }
        }])
    } else {
        serde_json::json!([])
    };
    serde_json::json!({ "type": "MEDIA_STATUS", "status": status }).to_string()
}

fn server_config() -> rustls::ServerConfig {
    let certified =
        rcgen::generate_simple_self_signed(vec!["cast.local".to_owned()]).expect("a certificate");
    let cert = CertificateDer::from(certified.cert.der().to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports TLS")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("the certificate is usable")
}
