//! One TLS session with one receiver.
//!
//! Cast certificates are not in the public web PKI, so the peer certificate is accepted on the
//! local network. The handshake signature is still checked, which keeps the record layer real.
//! The trust boundary is the LAN, the same as a plaintext device that announces itself.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::time::Instant;

use irori_protocol::{IncomingCall, ServiceError};
use irori_types::{ContextId, MediaPlayerState};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;

use crate::discover::Found;
use crate::map::{self, DEFAULT_MEDIA_RECEIVER, LoadBody, Outcome, Snap, Step};
use crate::proto::{
    self, CastMessage, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER, RECEIVER,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(5);
const COMMAND_DEADLINE: Duration = Duration::from_secs(8);

#[derive(Debug)]
pub enum SessionEvent {
    Status {
        generation: u64,
        uuid: String,
        state: MediaPlayerState,
        caused_by: Option<ContextId>,
    },
    Offline {
        generation: u64,
        uuid: String,
    },
}

pub fn spawn(
    found: Found,
    generation: u64,
    ignore_cec: bool,
    commands: mpsc::Receiver<IncomingCall>,
    events: mpsc::Sender<SessionEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(found, generation, ignore_cec, commands, events))
}

async fn run(
    found: Found,
    generation: u64,
    ignore_cec: bool,
    mut commands: mpsc::Receiver<IncomingCall>,
    events: mpsc::Sender<SessionEvent>,
) {
    let speaker = map::device_class(&found.model) == irori_types::MediaPlayerClass::Speaker;
    let uuid = found.uuid.clone();
    let address = found.address;
    let mut delay = Duration::from_secs(1);
    loop {
        match connect(address).await {
            Ok(stream) => {
                delay = Duration::from_secs(1);
                let stop = session(
                    stream,
                    generation,
                    uuid.clone(),
                    speaker,
                    ignore_cec,
                    &mut commands,
                    &events,
                )
                .await;
                if stop {
                    return;
                }
                tracing::debug!(uuid = %uuid, "Cast device disconnected");
                if events
                    .send(SessionEvent::Offline {
                        generation,
                        uuid: uuid.clone(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                tracing::debug!(uuid = %uuid, %error, "Cast device didn't connect");
            }
        }
        if !backoff(delay, &mut commands).await {
            return;
        }
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}

/// `true` when the protocol is shutting this device down.
async fn session(
    stream: tokio_rustls::client::TlsStream<TcpStream>,
    generation: u64,
    uuid: String,
    speaker: bool,
    ignore_cec: bool,
    commands: &mut mpsc::Receiver<IncomingCall>,
    events: &mpsc::Sender<SessionEvent>,
) -> bool {
    // One socket can't be borrowed for both sides of `select`, so the two halves are split.
    let (mut reader, mut writer) = tokio::io::split(stream);
    if send(&mut writer, &connect_message(RECEIVER)).await.is_err() {
        return false;
    }
    let mut request_id = 0u64;
    if send(
        &mut writer,
        &status_message(RECEIVER, NS_RECEIVER, next_id(&mut request_id)),
    )
    .await
    .is_err()
    {
        return false;
    }
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;

    let mut wire = Wire {
        snap: Snap::default(),
        connected_transport: None,
        pending: None,
        cause: None,
        wait_deadline: None,
        request_id,
    };
    let device = Device {
        speaker,
        ignore_cec,
        generation,
        uuid: &uuid,
        events,
    };

    loop {
        let deadline = wire.wait_deadline;
        tokio::select! {
            frame = proto::read_frame(&mut reader) => {
                let message = match frame {
                    Ok(message) => message,
                    Err(_) => {
                        drop_pending(&mut wire.pending);
                        return false;
                    }
                };
                if handle(message, &mut writer, &mut wire, &device)
                    .await
                    .is_err()
                {
                    drop_pending(&mut wire.pending);
                    return false;
                }
            }
            _ = heartbeat.tick() => {
                if send(&mut writer, &CastMessage::new(RECEIVER, NS_HEARTBEAT, r#"{"type":"PING"}"#)).await.is_err() {
                    drop_pending(&mut wire.pending);
                    return false;
                }
            }
            call = commands.recv() => {
                let Some(call) = call else {
                    drop_pending(&mut wire.pending);
                    return true;
                };
                if wire.pending.is_some() {
                    call.reply(Err(ServiceError::failed(
                        "the Cast device is busy with another command",
                    )));
                    continue;
                }
                match begin(call, &wire.snap, &mut wire.cause) {
                    Begun::Replied => {}
                    Begun::Wait(next) => {
                        wire.wait_deadline = Some(next.deadline);
                        wire.pending = Some(*next);
                        if pump(&mut writer, &mut wire).await.is_err() {
                            drop_pending(&mut wire.pending);
                            return false;
                        }
                    }
                }
            }
            _ = wait_until(deadline), if deadline.is_some() => {
                if let Some(next) = wire.pending.take() {
                    next.call.reply(Err(ServiceError::failed("the Cast device didn't answer")));
                }
                wire.wait_deadline = None;
            }
        }
    }
}

/// The receiver's last status, and the command still waiting on it.
struct Wire {
    snap: Snap,
    connected_transport: Option<String>,
    pending: Option<Pending>,
    cause: Option<ContextId>,
    wait_deadline: Option<Instant>,
    request_id: u64,
}

/// Which device this session belongs to. It doesn't change for the life of the socket.
struct Device<'a> {
    speaker: bool,
    ignore_cec: bool,
    generation: u64,
    uuid: &'a str,
    events: &'a mpsc::Sender<SessionEvent>,
}

async fn handle<W: AsyncWrite + Unpin>(
    message: CastMessage,
    writer: &mut W,
    wire: &mut Wire,
    device: &Device<'_>,
) -> Result<(), String> {
    let payload: serde_json::Value =
        serde_json::from_str(&message.payload).unwrap_or(serde_json::Value::Null);
    let kind = payload
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match (message.namespace.as_str(), kind) {
        (NS_HEARTBEAT, "PING") => {
            send(
                writer,
                &CastMessage::new(message.source_id, NS_HEARTBEAT, r#"{"type":"PONG"}"#),
            )
            .await?;
        }
        (NS_RECEIVER, "RECEIVER_STATUS") => {
            map::apply_receiver(&mut wire.snap, &payload);
            follow_transport(writer, wire).await?;
            pump(writer, wire).await?;
            publish(wire, device).await?;
        }
        (NS_MEDIA, "MEDIA_STATUS") => {
            map::apply_media(&mut wire.snap, &payload);
            pump(writer, wire).await?;
            publish(wire, device).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn follow_transport<W: AsyncWrite + Unpin>(
    writer: &mut W,
    wire: &mut Wire,
) -> Result<(), String> {
    let Some(transport) = wire.snap.transport_id.clone() else {
        wire.connected_transport = None;
        return Ok(());
    };
    if wire.connected_transport.as_deref() == Some(transport.as_str()) {
        return Ok(());
    }
    send(writer, &connect_message(&transport)).await?;
    send(
        writer,
        &status_message(&transport, NS_MEDIA, next_id(&mut wire.request_id)),
    )
    .await?;
    wire.connected_transport = Some(transport);
    Ok(())
}

async fn publish(wire: &mut Wire, device: &Device<'_>) -> Result<(), String> {
    let state = map::playback(&wire.snap, device.speaker, device.ignore_cec);
    device
        .events
        .send(SessionEvent::Status {
            generation: device.generation,
            uuid: device.uuid.to_owned(),
            state,
            caused_by: wire.cause.take(),
        })
        .await
        .map_err(|_| "stopped".to_owned())
}

struct Pending {
    call: IncomingCall,
    steps: VecDeque<Step>,
    sent: bool,
    deadline: Instant,
    context: ContextId,
}

enum Begun {
    Replied,
    Wait(Box<Pending>),
}

fn begin(call: IncomingCall, snap: &Snap, cause: &mut Option<ContextId>) -> Begun {
    let context = call.call.context.id.clone();
    match map::plan(&call.call.service, snap) {
        Outcome::Ready => {
            *cause = Some(context);
            call.reply(Ok(()));
            Begun::Replied
        }
        Outcome::Fail(message) => {
            call.reply(Err(ServiceError::failed(message)));
            Begun::Replied
        }
        Outcome::Steps(steps) => Begun::Wait(Box::new(Pending {
            call,
            steps: VecDeque::from(steps),
            sent: false,
            deadline: Instant::now() + COMMAND_DEADLINE,
            context,
        })),
    }
}

/// Writes whatever the current step can send now. A finished command is replied to here.
async fn pump<W: AsyncWrite + Unpin>(writer: &mut W, wire: &mut Wire) -> Result<(), String> {
    let (messages, finished) = advance(wire);
    for message in &messages {
        send(writer, message).await?;
    }
    if let Some(result) = finished
        && let Some(done) = wire.pending.take()
    {
        if result.is_ok() {
            wire.cause = Some(done.context);
        }
        wire.wait_deadline = None;
        done.call.reply(result);
    }
    Ok(())
}

fn advance(wire: &mut Wire) -> (Vec<CastMessage>, Option<Result<(), ServiceError>>) {
    let Wire {
        snap,
        pending,
        request_id,
        ..
    } = wire;
    let Some(pending) = pending.as_mut() else {
        return (Vec::new(), None);
    };
    let mut messages = Vec::new();
    loop {
        let Some(step) = pending.steps.front().cloned() else {
            return (messages, Some(Ok(())));
        };
        match step {
            Step::Quit => match snap.session_id.clone() {
                None => {
                    pending.steps.pop_front();
                    pending.sent = false;
                }
                Some(session) if !pending.sent => {
                    messages.push(quit(&session, next_id(request_id)));
                    pending.sent = true;
                    if pending.steps.len() == 1 {
                        pending.steps.pop_front();
                        return (messages, Some(Ok(())));
                    }
                    return (messages, None);
                }
                Some(_) => return (messages, None),
            },
            Step::Launch => {
                if !pending.sent {
                    messages.push(launch(next_id(request_id)));
                    pending.sent = true;
                }
                let launched = snap.app_id.as_deref() == Some(DEFAULT_MEDIA_RECEIVER)
                    && snap.transport_id.is_some();
                if pending.steps.len() == 1 {
                    pending.steps.pop_front();
                    return (messages, Some(Ok(())));
                }
                if launched {
                    pending.steps.pop_front();
                    pending.sent = false;
                    continue;
                }
                return (messages, None);
            }
            Step::Load(body) => {
                let Some(transport) = snap.transport_id.clone() else {
                    return (messages, None);
                };
                messages.push(load_message(&transport, &body, next_id(request_id)));
                pending.steps.pop_front();
                return (messages, Some(Ok(())));
            }
            Step::Volume { level, muted } => {
                messages.push(volume_message(level, muted, next_id(request_id)));
                pending.steps.pop_front();
                return (messages, Some(Ok(())));
            }
            Step::Media { action, position } => {
                let (Some(transport), Some(session)) =
                    (snap.transport_id.clone(), snap.media_session_id)
                else {
                    pending.steps.clear();
                    return (
                        messages,
                        Some(Err(ServiceError::failed("nothing is playing"))),
                    );
                };
                messages.push(media_message(
                    &transport,
                    action,
                    session,
                    position,
                    next_id(request_id),
                ));
                pending.steps.pop_front();
                return (messages, Some(Ok(())));
            }
        }
    }
}

fn drop_pending(pending: &mut Option<Pending>) {
    if let Some(pending) = pending.take() {
        pending.call.reply(Err(ServiceError::unavailable(
            "the Cast device can't be reached",
        )));
    }
}

async fn backoff(delay: Duration, commands: &mut mpsc::Receiver<IncomingCall>) -> bool {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return true,
            call = commands.recv() => {
                match call {
                    Some(call) => call.reply(Err(ServiceError::unavailable(
                        "the Cast device can't be reached",
                    ))),
                    None => return false,
                }
            }
        }
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn connect_message(destination: &str) -> CastMessage {
    CastMessage::new(
        destination,
        NS_CONNECTION,
        r#"{"type":"CONNECT","origin":{}}"#,
    )
}

fn status_message(destination: &str, namespace: &str, request_id: u64) -> CastMessage {
    json_message(
        destination,
        namespace,
        &serde_json::json!({ "type": "GET_STATUS", "requestId": request_id }),
    )
}

fn launch(request_id: u64) -> CastMessage {
    json_message(
        RECEIVER,
        NS_RECEIVER,
        &serde_json::json!({
            "type": "LAUNCH",
            "appId": DEFAULT_MEDIA_RECEIVER,
            "requestId": request_id,
        }),
    )
}

fn quit(session: &str, request_id: u64) -> CastMessage {
    json_message(
        RECEIVER,
        NS_RECEIVER,
        &serde_json::json!({
            "type": "STOP",
            "sessionId": session,
            "requestId": request_id,
        }),
    )
}

fn volume_message(level: Option<f64>, muted: Option<bool>, request_id: u64) -> CastMessage {
    let mut volume = serde_json::Map::new();
    if let Some(level) = level {
        volume.insert("level".into(), serde_json::json!(level));
    }
    if let Some(muted) = muted {
        volume.insert("muted".into(), serde_json::json!(muted));
    }
    json_message(
        RECEIVER,
        NS_RECEIVER,
        &serde_json::json!({ "type": "SET_VOLUME", "volume": volume, "requestId": request_id }),
    )
}

fn load_message(transport: &str, body: &LoadBody, request_id: u64) -> CastMessage {
    json_message(
        transport,
        NS_MEDIA,
        &serde_json::json!({
            "type": "LOAD",
            "media": body.media_json(),
            "autoplay": true,
            "requestId": request_id,
        }),
    )
}

fn media_message(
    transport: &str,
    action: &str,
    session: i64,
    position: Option<f64>,
    request_id: u64,
) -> CastMessage {
    let mut payload = serde_json::json!({
        "type": action,
        "mediaSessionId": session,
        "requestId": request_id,
    });
    if let Some(position) = position {
        payload["currentTime"] = serde_json::json!(position);
    }
    json_message(transport, NS_MEDIA, &payload)
}

fn json_message(destination: &str, namespace: &str, value: &serde_json::Value) -> CastMessage {
    let payload = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned());
    CastMessage::new(destination, namespace, payload)
}

fn next_id(id: &mut u64) -> u64 {
    *id = id.wrapping_add(1);
    if *id == 0 {
        *id = 1;
    }
    *id
}

async fn send<W: AsyncWrite + Unpin>(writer: &mut W, message: &CastMessage) -> Result<(), String> {
    writer
        .write_all(&proto::encode(message))
        .await
        .map_err(|_| "the Cast device closed the connection".to_owned())?;
    writer
        .flush()
        .await
        .map_err(|_| "the Cast device closed the connection".to_owned())?;
    Ok(())
}

async fn connect(
    address: SocketAddr,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| "the Cast device didn't answer".to_owned())?
        .map_err(|_| "the Cast device didn't answer".to_owned())?;
    let name = ServerName::try_from("cast.local").expect("cast.local is a name");
    tokio::time::timeout(CONNECT_TIMEOUT, connector().connect(name, tcp))
        .await
        .map_err(|_| "the Cast device didn't finish TLS".to_owned())?
        .map_err(|_| "the Cast device didn't finish TLS".to_owned())
}

fn connector() -> TlsConnector {
    TlsConnector::from(Arc::clone(client_config()))
}

fn client_config() -> &'static Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(LanCerts {
            provider: Arc::clone(&provider),
        });
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports TLS")
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Arc::new(config)
    })
}

/// Accepts the receiver's certificate and still checks the handshake signature.
#[derive(Debug)]
struct LanCerts {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for LanCerts {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
