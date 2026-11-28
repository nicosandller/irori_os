//! A fake printer for the tests: an MQTT listener on loopback, over TLS, with a certificate of
//! its own. It takes one access code, records what it is asked, and sends what a test hands it.

use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use rumqttc::mqttbytes::v4::{
    ConnAck, ConnectReturnCode, Packet, PingResp, Publish, SubAck, SubscribeReasonCode,
};
use rumqttc::mqttbytes::{Error, QoS};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

#[derive(Debug, Default)]
pub struct Seen {
    /// The access code of every sign-in, accepted or not.
    pub codes: Vec<String>,
    pub subscribed: Vec<String>,
    /// What was published to the printer: topic and payload.
    pub requests: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
enum Push {
    Report(Vec<u8>),
    HangUp,
}

#[derive(Debug)]
pub struct Fake {
    pub port: u16,
    /// Its certificate's SHA-256 fingerprint.
    pub fingerprint: String,
    pub seen: Arc<Mutex<Seen>>,
    push: broadcast::Sender<Push>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fake {
    pub async fn start(serial: &str, code: &str) -> Self {
        let made = rcgen::generate_simple_self_signed(vec![serial.to_owned()]).expect("a cert");
        let certificate = made.cert.der().clone();
        let fingerprint = crate::link::fingerprint(&certificate);
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(made.key_pair.serialize_der().into());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports TLS")
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .expect("a usable certificate");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (push, _) = broadcast::channel(16);
        let task = tokio::spawn({
            let (seen, push) = (Arc::clone(&seen), push.clone());
            let (topic, code) = (format!("device/{serial}/report"), code.to_owned());
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let (acceptor, seen, push) =
                        (acceptor.clone(), Arc::clone(&seen), push.subscribe());
                    let (topic, code) = (topic.clone(), code.clone());
                    tokio::spawn(async move {
                        if let Ok(stream) = acceptor.accept(stream).await {
                            session(stream, &topic, &code, &seen, push).await;
                        }
                    });
                }
            }
        });
        Fake {
            port,
            fingerprint,
            seen,
            push,
            task,
        }
    }

    /// Sends a status report with these fields to whoever is connected.
    pub fn report(&self, print: serde_json::Value) {
        let mut print = print;
        print["command"] = "push_status".into();
        self.raw(
            serde_json::json!({ "print": print })
                .to_string()
                .into_bytes(),
        );
    }

    pub fn raw(&self, payload: Vec<u8>) {
        let _ = self.push.send(Push::Report(payload));
    }

    /// Drops every connection, as a printer switched off does.
    pub fn hang_up(&self) {
        let _ = self.push.send(Push::HangUp);
    }

    pub fn requests(&self) -> usize {
        self.seen.lock().expect("seen").requests.len()
    }
}

async fn session(
    mut stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    topic: &str,
    code: &str,
    seen: &Mutex<Seen>,
    mut push: broadcast::Receiver<Push>,
) {
    let (mut read, mut out) = (BytesMut::new(), BytesMut::new());
    loop {
        loop {
            match Packet::read(&mut read, 1 << 20) {
                Ok(Packet::Connect(connect)) => {
                    let given = connect
                        .login
                        .map(|login| login.password)
                        .unwrap_or_default();
                    let accepted = given == code;
                    seen.lock().expect("seen").codes.push(given);
                    let answer = if accepted {
                        ConnectReturnCode::Success
                    } else {
                        ConnectReturnCode::BadUserNamePassword
                    };
                    ConnAck::new(answer, false)
                        .write(&mut out)
                        .expect("written");
                    if !accepted {
                        let _ = stream.write_all(&out).await;
                        let _ = stream.shutdown().await;
                        return;
                    }
                }
                Ok(Packet::Subscribe(subscribe)) => {
                    let mut seen = seen.lock().expect("seen");
                    seen.subscribed
                        .extend(subscribe.filters.into_iter().map(|f| f.path));
                    let granted = vec![SubscribeReasonCode::Success(QoS::AtMostOnce)];
                    SubAck::new(subscribe.pkid, granted)
                        .write(&mut out)
                        .expect("written");
                }
                Ok(Packet::Publish(publish)) => {
                    let payload = String::from_utf8_lossy(&publish.payload).into_owned();
                    seen.lock()
                        .expect("seen")
                        .requests
                        .push((publish.topic, payload));
                }
                Ok(Packet::PingReq) => {
                    PingResp.write(&mut out).expect("written");
                }
                Ok(_) => {}
                Err(Error::InsufficientBytes(_)) => break,
                Err(_) => return,
            }
        }
        if stream.write_all(&out).await.is_err() {
            return;
        }
        out.clear();
        tokio::select! {
            got = stream.read_buf(&mut read) => {
                if !matches!(got, Ok(n) if n > 0) {
                    return;
                }
            }
            pushed = push.recv() => match pushed {
                Ok(Push::Report(payload)) => {
                    Publish::new(topic, QoS::AtMostOnce, payload).write(&mut out).expect("written");
                }
                Ok(Push::HangUp) | Err(_) => return,
            },
        }
    }
}
