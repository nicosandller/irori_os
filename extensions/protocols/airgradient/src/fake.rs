//! A monitor on a loopback port: it answers `GET /measures/current` with whatever JSON the test
//! puts in it, in the shape AirGradient documents. Nothing here is a real monitor.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::monitor::Target;

#[derive(Debug)]
pub struct State {
    /// What `/measures/current` answers.
    pub measures: Value,
    /// Every connection is closed unanswered, as a monitor without power fails.
    pub gone: bool,
    /// Every connection is accepted and then left without an answer.
    pub stalled: bool,
    /// Each answer carries a serial number no earlier one did.
    pub new_serial_each_time: bool,
    /// Sent in place of the reading, as it is: status line, headers and body.
    pub raw: Option<Vec<u8>>,
    /// The request lines seen, so a test can tell what was asked.
    pub requests: Vec<String>,
}

pub struct Fake {
    pub address: SocketAddr,
    pub state: Arc<Mutex<State>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Fake {
    pub fn target(&self) -> Target {
        Target {
            host: self.address.ip().to_string(),
            port: self.address.port(),
        }
    }

    pub fn set(&self, field: &str, value: Value) {
        self.state.lock().expect("state").measures[field] = value;
    }

    pub fn remove(&self, field: &str) {
        let mut state = self.state.lock().expect("state");
        if let Some(fields) = state.measures.as_object_mut() {
            fields.remove(field);
        }
    }

    pub fn gone(&self, gone: bool) {
        self.state.lock().expect("state").gone = gone;
    }

    pub fn raw(&self, raw: Option<Vec<u8>>) {
        self.state.lock().expect("state").raw = raw;
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.lock().expect("state").requests.clone()
    }
}

/// An indoor monitor's answer, with every reading the extension takes. The serial number is
/// made up.
pub fn indoor(serial: &str) -> Value {
    json!({
        "wifi": -52,
        "serialno": serial,
        "rco2": 612,
        "pm01": 3,
        "pm02": 7,
        "pm10": 8,
        "pm02Compensated": 6.4,
        "pm003Count": 442,
        "atmp": 21.87,
        "atmpCompensated": 21.87,
        "rhum": 43,
        "rhumCompensated": 43,
        "tvocIndex": 100,
        "tvocRaw": 33051,
        "noxIndex": 1,
        "noxRaw": 16307,
        "boot": 6,
        "bootCount": 6,
        "ledMode": "pm",
        "firmware": "3.1.3",
        "model": "I-9PSL"
    })
}

pub async fn start(measures: Value) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let state = Arc::new(Mutex::new(State {
        measures,
        gone: false,
        stalled: false,
        new_serial_each_time: false,
        raw: None,
        requests: Vec::new(),
    }));
    let shared = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, Arc::clone(&shared)));
        }
    });
    Fake {
        address,
        state,
        handle,
    }
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    }
    let line = String::from_utf8_lossy(&request)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    let answer = {
        let mut state = state.lock().expect("state");
        state.requests.push(line.clone());
        if state.gone {
            return;
        }
        if state.new_serial_each_time {
            let serial = format!("f{:011x}", state.requests.len());
            state.measures["serialno"] = Value::String(serial);
        }
        match &state.raw {
            _ if state.stalled => None,
            Some(raw) => Some(raw.clone()),
            None if line == "GET /measures/current HTTP/1.1" => {
                Some(respond("200 OK", &state.measures.to_string()))
            }
            None => Some(respond("404 Not Found", "Not found")),
        }
    };
    let Some(answer) = answer else {
        // Hold the connection open and say nothing.
        std::future::pending::<()>().await;
        return;
    };
    let _ = stream.write_all(&answer).await;
    let _ = stream.shutdown().await;
}

pub fn respond(status: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}
