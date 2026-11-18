//! A stand-in for Tesla's sign-in and Fleet API servers on a local port: answers from what a test
//! sets, and every request kept for the test to look at.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::api::Hosts;

/// A request the fake received.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// The path, without the query.
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    /// A JSON body, or a form's fields as a JSON object.
    pub body: Value,
}

/// An answer: an HTTP status and a body. Status 0 closes the connection without answering.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub body: Value,
    pub headers: Vec<(String, String)>,
    pub delay: Duration,
}

impl Answer {
    /// 200 with Tesla's `{"response": …}` around it.
    pub fn ok(response: Value) -> Self {
        Self::with(200, json!({ "response": response }))
    }

    pub fn status(status: u16) -> Self {
        Self::with(status, json!({}))
    }

    pub fn with(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// No answer at all.
    pub fn hang_up() -> Self {
        Self::status(0)
    }
}

/// An access token like Tesla's: a JWT whose payload says the account's region. Unsigned: Irori
/// reads it, it never checks it.
pub fn token(region: &str, serial: u32) -> String {
    let part = |value: Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string().as_bytes())
    };
    format!(
        "{}.{}.sig",
        part(json!({ "alg": "RS256", "typ": "JWT" })),
        part(json!({ "ou_code": region, "serial": serial }))
    )
}

#[derive(Debug)]
pub struct State {
    /// The region the account's tokens say.
    pub region: String,
    /// How long tokens last, in seconds.
    pub expires_in: u64,
    /// Tokens handed out so far; the next refresh token is `refresh-{issued}`.
    pub issued: u32,
    /// The refresh tokens still good: each works once.
    pub refresh_good: Vec<String>,
    /// What the token endpoint answers instead, when set.
    pub token: Option<Answer>,
    /// Answers by `"METHOD /path"` (no query), checked before anything else.
    pub routes: BTreeMap<String, Vec<Answer>>,
    /// The key Tesla has on file for the domain, per region host (`na`, `eu`).
    pub registered: BTreeMap<String, String>,
    /// Cars that take signed commands, by VIN.
    pub cars: BTreeMap<String, Car>,
    pub requests: Vec<Request>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            region: "NA".into(),
            expires_in: 8 * 3600,
            issued: 0,
            refresh_good: Vec::new(),
            token: None,
            routes: BTreeMap::new(),
            registered: BTreeMap::new(),
            cars: BTreeMap::new(),
            requests: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Fake {
    pub base: String,
    pub state: Arc<Mutex<State>>,
}

impl Fake {
    pub async fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port");
        let addr: SocketAddr = listener.local_addr().expect("its address");
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&shared)));
            }
        });
        Fake {
            base: format!("http://{addr}"),
            state,
        }
    }

    /// Tesla's three servers, all this fake: `/auth`, `/na` and `/eu`.
    pub fn hosts(&self) -> Hosts {
        Hosts {
            auth: format!("{}/auth", self.base),
            na: format!("{}/na", self.base),
            eu: format!("{}/eu", self.base),
        }
    }

    pub fn with(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.state.lock().expect("fake state"));
    }

    /// Answers `route` ("GET /na/api/1/products") with these, one per request, the last one
    /// for good.
    pub fn answer(&self, route: &str, answers: Vec<Answer>) {
        self.with(|state| {
            state.routes.insert(route.to_owned(), answers);
        });
    }

    pub fn requests(&self) -> Vec<Request> {
        self.state.lock().expect("fake state").requests.clone()
    }

    /// The requests to paths ending in `tail`.
    pub fn sent(&self, tail: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|request| request.path.ends_with(tail))
            .collect()
    }

    /// Hands out a sign-in, as if the person had signed in: the tokens to start with.
    pub fn signed_in(&self) -> crate::auth::Kept {
        let mut state = self.state.lock().expect("fake state");
        let (access, refresh) = issue(&mut state);
        let region = state.region.to_ascii_lowercase();
        serde_json::from_value(json!({
            "access": access,
            "refresh": refresh,
            "expires_at": u64::MAX / 2,
            "region": region,
        }))
        .expect("kept tokens")
    }
}

fn issue(state: &mut State) -> (String, String) {
    state.issued += 1;
    let refresh = format!("refresh-{}", state.issued);
    state.refresh_good.push(refresh.clone());
    (token(&state.region, state.issued), refresh)
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut buffer = Vec::new();
    loop {
        let Some(request) = read_request(&mut stream, &mut buffer).await else {
            return;
        };
        let answer = {
            let mut state = state.lock().expect("fake state");
            let answer = answer(&mut state, &request);
            state.requests.push(request);
            answer
        };
        tokio::time::sleep(answer.delay).await;
        if answer.status == 0 {
            return;
        }
        let body = answer.body.to_string();
        let mut head = format!(
            "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
            answer.status,
            body.len()
        );
        for (name, value) in &answer.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        let response = format!("{head}\r\n{body}");
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

fn answer(state: &mut State, request: &Request) -> Answer {
    let route = format!("{} {}", request.method, request.path);
    if let Some(answers) = state.routes.get_mut(&route) {
        return if answers.len() > 1 {
            answers.remove(0)
        } else {
            answers
                .first()
                .cloned()
                .unwrap_or_else(|| Answer::status(404))
        };
    }
    let parts: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
    match (request.method.as_str(), parts.as_slice()) {
        ("POST", ["auth", "oauth2", "v3", "token"]) => {
            if let Some(answer) = &state.token {
                return answer.clone();
            }
            let field = |name: &str| {
                request
                    .body
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            match field("grant_type").as_str() {
                "refresh_token" => {
                    let given = field("refresh_token");
                    let Some(at) = state.refresh_good.iter().position(|good| *good == given) else {
                        return Answer::with(
                            401,
                            json!({ "error": "login_required", "error_description": "The refresh_token has already been used" }),
                        );
                    };
                    state.refresh_good.remove(at);
                    granted(state)
                }
                "authorization_code" => granted(state),
                "client_credentials" => Answer::with(
                    200,
                    json!({ "access_token": format!("partner-{}", field("audience")), "expires_in": 28_800 }),
                ),
                _ => Answer::with(400, json!({ "error": "unsupported_grant_type" })),
            }
        }
        ("POST", [region, "api", "1", "partner_accounts"]) => {
            let region = (*region).to_owned();
            state
                .registered
                .entry(region)
                .or_insert_with(|| "registered".into());
            Answer::ok(json!({ "domain": request.body.get("domain") }))
        }
        ("POST", [_, "api", "1", "vehicles", vin, "signed_command"])
            if state.cars.contains_key(*vin) =>
        {
            let car = state.cars.get_mut(*vin).expect("the car");
            car.signed(vin, &request.body)
        }
        ("POST", [_, "api", "1", "vehicles", vin, "wake_up"]) if state.cars.contains_key(*vin) => {
            let car = state.cars.get_mut(*vin).expect("the car");
            car.wakes += 1;
            if car.asleep && car.waking.is_none() {
                car.waking = Some(car.wake_takes);
            }
            Answer::ok(json!({ "vin": vin, "state": if car.asleep { "asleep" } else { "online" } }))
        }
        ("GET", [region, "api", "1", "partner_accounts", "public_key"]) => {
            match state.registered.get(*region) {
                Some(key) => Answer::ok(json!({ "public_key": key })),
                None => Answer::with(404, json!({ "error": "not found" })),
            }
        }
        _ => Answer::status(404),
    }
}

fn granted(state: &mut State) -> Answer {
    let (access, refresh) = issue(state);
    Answer::with(
        200,
        json!({ "access_token": access, "refresh_token": refresh, "expires_in": state.expires_in, "token_type": "Bearer" }),
    )
}

/// One request off the connection, or `None` when it closed.
async fn read_request(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<Request> {
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|length| length.parse().ok())
        .unwrap_or(0);
    let start = head_end + 4;
    while buffer.len() < start + length {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let raw = &buffer[start..start + length];
    let body = serde_json::from_slice(raw).unwrap_or_else(|_| form(raw));
    buffer.drain(..start + length);
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(Request {
        method,
        path: path.to_owned(),
        query: pairs(query.as_bytes()),
        headers,
        body,
    })
}

fn pairs(raw: &[u8]) -> BTreeMap<String, String> {
    irori_cloud::reqwest::Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(raw)))
        .map(|url| url.query_pairs().into_owned().collect())
        .unwrap_or_default()
}

fn form(raw: &[u8]) -> Value {
    if raw.is_empty() {
        return Value::Null;
    }
    Value::Object(
        pairs(raw)
            .into_iter()
            .map(|(key, value)| (key, Value::String(value)))
            .collect(),
    )
}

/// A car that takes signed commands the way a real one does: its own key, a session per
/// computer, a counter that only goes forward, and every tag checked. The checks are written out
/// here on their own, after `protocol.md`, not borrowed from `crate::signed`, so a mistake there
/// can't hide by being made twice.
#[derive(Debug, Clone)]
pub struct Car {
    pub key: p256::SecretKey,
    pub epoch: Vec<u8>,
    /// The last counter taken, per computer (2 security, 3 infotainment).
    pub counters: BTreeMap<i32, u32>,
    /// Its clock, in seconds.
    pub clock: u32,
    /// Whether Irori's key is on it.
    pub paired: bool,
    pub asleep: bool,
    /// How many more messages it answers "asleep" after a wake.
    pub wake_takes: u32,
    pub waking: Option<u32>,
    pub wakes: u32,
    /// What it acted on: the computer and the command.
    pub done: Vec<(i32, Vec<u8>)>,
    /// Answers to commands instead of OK, one a command, while there are any.
    pub answers: Vec<Vec<u8>>,
    /// How many more commands it answers "busy, wait" (and doesn't act on).
    pub busy: u32,
}

impl Car {
    pub fn new() -> Self {
        Self {
            key: crate::keys::Key::generate()
                .expect("a key")
                .secret()
                .clone(),
            epoch: vec![0x4c; 16],
            counters: BTreeMap::new(),
            clock: 1_000,
            paired: true,
            asleep: false,
            wake_takes: 2,
            waking: None,
            wakes: 0,
            done: Vec::new(),
            answers: Vec::new(),
            busy: 0,
        }
    }

    fn signed(&mut self, vin: &str, body: &Value) -> Answer {
        use prost::Message as _;

        use crate::signed::proto::{
            Destination, MessageStatus, RoutableMessage, SessionInfo, SignatureData, destination,
            key_identity, routable, signature_data,
        };

        if self.asleep {
            match self.waking {
                Some(0) => {
                    self.asleep = false;
                    self.waking = None;
                }
                Some(left) => {
                    self.waking = Some(left - 1);
                    return Answer::with(408, json!({ "error": "vehicle unavailable: asleep" }));
                }
                None => {
                    return Answer::with(408, json!({ "error": "vehicle unavailable: asleep" }));
                }
            }
        }
        let bytes = body
            .get("routable_message")
            .and_then(Value::as_str)
            .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
            .expect("a routable message");
        let message = RoutableMessage::decode(bytes.as_slice()).expect("a routable message");
        let domain = match message.to_destination.as_ref().and_then(|d| d.sub.clone()) {
            Some(destination::Sub::Domain(domain)) => domain,
            _ => panic!("no domain"),
        };
        let mut reply = RoutableMessage {
            to_destination: message.from_destination.clone(),
            from_destination: Some(Destination {
                sub: Some(destination::Sub::Domain(domain)),
            }),
            request_uuid: message.uuid.clone(),
            ..RoutableMessage::default()
        };
        let counter = self.counters.get(&domain).copied().unwrap_or(0);
        let client = match &message.payload {
            Some(routable::Payload::SessionInfoRequest(request)) => request.public_key.clone(),
            _ => match &message.signature {
                Some(routable::Signature::Data(SignatureData {
                    signer_identity: Some(identity),
                    ..
                })) => match &identity.kind {
                    Some(key_identity::Kind::PublicKey(key)) => key.clone(),
                    None => Vec::new(),
                },
                _ => Vec::new(),
            },
        };
        let keys = car_keys(&self.key, &client);
        // The session info, tagged for this message.
        let info = |reply: &mut RoutableMessage, status: i32| {
            let info = SessionInfo {
                counter,
                public_key: if status == 0 {
                    crate::signed::public_bytes(&self.key)
                } else {
                    Vec::new()
                },
                epoch: self.epoch.clone(),
                clock_time: self.clock,
                status,
                ..SessionInfo::default()
            }
            .encode_to_vec();
            if status == 0
                && let Some((_, session_info_key)) = &keys
            {
                let mut metadata = vec![0, 1, 6, 2, vin.len() as u8];
                metadata.extend_from_slice(vin.as_bytes());
                metadata.extend_from_slice(&[6, message.uuid.len() as u8]);
                metadata.extend_from_slice(&message.uuid);
                metadata.push(0xff);
                let tag = mac(session_info_key, &[&metadata, &info]);
                reply.signature = Some(routable::Signature::Data(SignatureData {
                    signer_identity: None,
                    kind: Some(signature_data::Kind::SessionInfoTag(
                        crate::signed::proto::HmacSignatureData { tag },
                    )),
                }));
            }
            reply.payload = Some(routable::Payload::SessionInfo(info));
        };
        let fault = |reply: &mut RoutableMessage, fault: i32| {
            reply.signed_message_status = Some(MessageStatus {
                operation_status: 2,
                signed_message_fault: fault,
            });
        };
        match &message.payload {
            Some(routable::Payload::SessionInfoRequest(_)) => {
                info(&mut reply, if self.paired { 0 } else { 1 });
            }
            Some(routable::Payload::Command(command)) => {
                let signature = match &message.signature {
                    Some(routable::Signature::Data(SignatureData {
                        kind: Some(signature_data::Kind::HmacPersonalized(data)),
                        ..
                    })) => data.clone(),
                    _ => panic!("an unsigned command"),
                };
                let paired = self.paired;
                let Some((command_key, _)) = keys.clone().filter(|_| paired) else {
                    fault(&mut reply, 3);
                    return reply_with(&reply);
                };
                let mut metadata = vec![0, 1, 8, 1, 1, domain as u8, 2, vin.len() as u8];
                metadata.extend_from_slice(vin.as_bytes());
                metadata.extend_from_slice(&[3, signature.epoch.len() as u8]);
                metadata.extend_from_slice(&signature.epoch);
                metadata.extend_from_slice(&[4, 4]);
                metadata.extend_from_slice(&signature.expires_at.to_be_bytes());
                metadata.extend_from_slice(&[5, 4]);
                metadata.extend_from_slice(&signature.counter.to_be_bytes());
                metadata.push(0xff);
                let tag_ok = mac(&command_key, &[&metadata, command]) == signature.tag;
                if signature.epoch != self.epoch {
                    fault(&mut reply, 15);
                    info(&mut reply, 0);
                } else if signature.counter <= counter {
                    fault(&mut reply, 6);
                    info(&mut reply, 0);
                } else if !tag_ok {
                    fault(&mut reply, 5);
                    info(&mut reply, 0);
                } else if signature.expires_at < self.clock {
                    fault(&mut reply, 17);
                    info(&mut reply, 0);
                } else if self.busy > 0 {
                    self.busy -= 1;
                    self.counters.insert(domain, signature.counter);
                    reply.signed_message_status = Some(MessageStatus {
                        operation_status: 1,
                        signed_message_fault: 0,
                    });
                } else {
                    self.counters.insert(domain, signature.counter);
                    self.done.push((domain, command.clone()));
                    let answer = if self.answers.is_empty() {
                        Vec::new()
                    } else {
                        self.answers.remove(0)
                    };
                    reply.payload = Some(routable::Payload::Command(answer));
                }
            }
            _ => panic!("neither a handshake nor a command"),
        }
        reply_with(&reply)
    }
}

impl Default for Car {
    fn default() -> Self {
        Self::new()
    }
}

fn reply_with(reply: &crate::signed::proto::RoutableMessage) -> Answer {
    use prost::Message as _;
    Answer::ok(json!(
        base64::engine::general_purpose::STANDARD.encode(reply.encode_to_vec())
    ))
}

fn mac(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    use hmac::{KeyInit, Mac};
    let mut mac =
        <hmac::Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key).expect("any key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().to_vec()
}

/// The command key and the session info key the car shares with `client`.
fn car_keys(car: &p256::SecretKey, client: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    use sha1::Digest as _;
    let client = p256::PublicKey::from_sec1_bytes(client).ok()?;
    let point = p256::ecdh::diffie_hellman(car.to_nonzero_scalar(), client.as_affine());
    let shared = sha1::Sha1::digest(point.raw_secret_bytes());
    let shared = &shared[..16];
    Some((
        mac(shared, &[b"authenticated command"]),
        mac(shared, &[b"session info"]),
    ))
}
