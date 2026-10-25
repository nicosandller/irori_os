//! A stand-in for SwitchBot's cloud on a local port: the device list, each device's status, and
//! commands, answered from what a test sets, and every request kept for the test to look at.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A request the fake received.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

/// An answer: an HTTP status, and the envelope's `statusCode` and `body`.
#[derive(Debug, Clone)]
pub struct Answer {
    pub http: u16,
    pub code: i64,
    pub body: Value,
}

impl Answer {
    pub fn ok(body: Value) -> Self {
        Self {
            http: 200,
            code: 100,
            body,
        }
    }

    pub fn code(code: i64) -> Self {
        Self {
            http: 200,
            code,
            body: json!({}),
        }
    }

    pub fn http(http: u16) -> Self {
        Self {
            http,
            code: 0,
            body: json!({}),
        }
    }
}

#[derive(Debug, Default)]
pub struct State {
    /// What `GET /v1.1/devices` answers.
    pub devices: Option<Answer>,
    /// What `GET /v1.1/devices/{id}/status` answers, by id.
    pub status: BTreeMap<String, Answer>,
    /// What `POST /v1.1/devices/{id}/commands` answers, by id. Ok when not set.
    pub commands: BTreeMap<String, Answer>,
    /// How long a status answer takes, so a test can be sure of what happens first.
    pub status_delay: std::time::Duration,
    pub requests: Vec<Request>,
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

    pub fn with(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.state.lock().expect("fake state"));
    }

    pub fn requests(&self) -> Vec<Request> {
        self.state.lock().expect("fake state").requests.clone()
    }
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut buffer = Vec::new();
    loop {
        let Some(request) = read_request(&mut stream, &mut buffer).await else {
            return;
        };
        let (answer, delay) = {
            let mut state = state.lock().expect("fake state");
            let answer = answer(&state, &request);
            let delay = if request.path.ends_with("/status") {
                state.status_delay
            } else {
                std::time::Duration::ZERO
            };
            state.requests.push(request);
            (answer, delay)
        };
        tokio::time::sleep(delay).await;
        let body = if answer.http == 200 {
            json!({ "statusCode": answer.code, "body": answer.body, "message": "" }).to_string()
        } else {
            String::new()
        };
        let response = format!(
            "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            answer.http,
            body.len()
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

fn answer(state: &State, request: &Request) -> Answer {
    let parts: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
    match (request.method.as_str(), parts.as_slice()) {
        ("GET", ["v1.1", "devices"]) => state
            .devices
            .clone()
            .unwrap_or_else(|| Answer::ok(json!({ "deviceList": [], "infraredRemoteList": [] }))),
        ("GET", ["v1.1", "devices", id, "status"]) => state
            .status
            .get(*id)
            .cloned()
            .unwrap_or_else(|| Answer::code(152)),
        ("POST", ["v1.1", "devices", id, "commands"]) => state
            .commands
            .get(*id)
            .cloned()
            .unwrap_or_else(|| Answer::ok(json!({}))),
        _ => Answer::http(404),
    }
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
    let path = first.next()?.to_owned();
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
    let body = serde_json::from_slice(&buffer[start..start + length]).unwrap_or(Value::Null);
    buffer.drain(..start + length);
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}
