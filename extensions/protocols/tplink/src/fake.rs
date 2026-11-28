//! A Tapo device on a loopback port: KLAP's two handshakes, signed and encrypted requests, and
//! the reading methods of a metering plug or a metering strip, and the one that switches a relay.
//! Shaped after what real devices answer, including an error code for each method they don't
//! have. Every switch it is sent is kept, so a test can say that nothing was.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::klap::{self, AuthHash, Keys};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plug,
    Strip,
}

#[derive(Debug, Clone)]
pub struct Outlet {
    pub id: &'static str,
    pub name: &'static str,
    pub on: bool,
    pub position: u64,
}

/// A switch the device was sent, whether or not it took it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// The outlet it was for; `None` for the device's own relay.
    pub child: Option<String>,
    pub on: bool,
}

#[derive(Debug)]
pub struct State {
    pub on: bool,
    pub power_mw: u64,
    pub outlets: Vec<Outlet>,
    /// The next request is answered 403, as an expired session is.
    pub expire_next: bool,
    /// Every connection is closed unanswered, as an unplugged device's fails.
    pub gone: bool,
    /// Both handshakes and the component list pass, but every reading is answered 403.
    pub refuse_requests: bool,
    /// This many readings answer neither meter method.
    pub meter_misses: u32,
    /// This many outlet readings answer neither meter method.
    pub outlet_meter_misses: u32,
    /// The plug says it has no meter methods at all.
    pub no_meter: bool,
    /// The strip lists an entry first that has no `device_id`, so it can't be read.
    pub unreadable_outlet: bool,
    pub handshakes: u32,
    /// Every switch received, in order.
    pub writes: Vec<Write>,
    /// A switch is answered with this error code and changes nothing.
    pub refuse_switch: Option<i64>,
    /// A switch is accepted and the relay stays as it was.
    pub ignore_switch: bool,
    /// Every connection is taken and never answered, as a device that has hung does.
    pub stall: bool,
    /// A switch is taken and acted on, and the connection closes before the answer is sent.
    pub lose_switch_answer: bool,
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

struct Session {
    local: [u8; 16],
    remote: [u8; 16],
    keys: Option<Keys>,
}

pub async fn start(kind: Kind, auth: AuthHash) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let state = Arc::new(Mutex::new(State {
        on: true,
        power_mw: 80_500,
        outlets: vec![
            Outlet {
                id: "8022AAAA",
                name: "Desk Lamp ",
                on: true,
                position: 2,
            },
            Outlet {
                id: "8022BBBB",
                name: "",
                on: false,
                position: 1,
            },
        ],
        expire_next: false,
        gone: false,
        refuse_requests: false,
        meter_misses: 0,
        outlet_meter_misses: 0,
        no_meter: false,
        unreadable_outlet: false,
        handshakes: 0,
        writes: Vec::new(),
        refuse_switch: None,
        ignore_switch: false,
        stall: false,
        lose_switch_answer: false,
    }));
    let shared = Arc::clone(&state);
    let session = Arc::new(Mutex::new(Session {
        local: [0; 16],
        remote: [0; 16],
        keys: None,
    }));
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let state = Arc::clone(&shared);
            let session = Arc::clone(&session);
            tokio::spawn(async move {
                let _ = serve(stream, kind, auth, state, session).await;
            });
        }
    });
    Fake {
        address,
        state,
        handle,
    }
}

async fn serve(
    mut stream: TcpStream,
    kind: Kind,
    auth: AuthHash,
    state: Arc<Mutex<State>>,
    session: Arc<Mutex<Session>>,
) -> std::io::Result<()> {
    if state.lock().expect("state").gone {
        return Ok(());
    }
    if state.lock().expect("state").stall {
        std::future::pending::<()>().await;
    }
    let (path, body) = read_request(&mut stream).await?;
    let (status, cookie, answer) = respond(&path, &body, kind, &auth, &state, &session);
    if status == LOST {
        return Ok(());
    }
    let mut head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n",
        answer.len()
    );
    if cookie {
        head.push_str("Set-Cookie: TP_SESSIONID=FAKE0001;TIMEOUT=86400\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&answer).await?;
    stream.shutdown().await
}

/// Stands in for a status when the connection is to close with nothing sent.
const LOST: u16 = 0;

fn respond(
    path: &str,
    body: &[u8],
    kind: Kind,
    auth: &AuthHash,
    state: &Mutex<State>,
    session: &Mutex<Session>,
) -> (u16, bool, Vec<u8>) {
    let mut session = session.lock().expect("session");
    if path == "/app/handshake1" {
        session.local.copy_from_slice(&body[..16]);
        session.remote = [0x5a; 16];
        session.keys = None;
        let mut answer = session.remote.to_vec();
        answer.extend_from_slice(&klap::server_proof(&session.local, &session.remote, auth));
        state.lock().expect("state").handshakes += 1;
        return (200, true, answer);
    }
    if path == "/app/handshake2" {
        if body == klap::client_proof(&session.local, &session.remote, auth) {
            session.keys = Some(Keys::derive(&session.local, &session.remote, auth));
            return (200, false, Vec::new());
        }
        return (403, false, Vec::new());
    }
    let Some(seq) = path.strip_prefix("/app/request?seq=") else {
        return (404, false, Vec::new());
    };
    let seq: i32 = seq.parse().expect("seq");
    let mut state = state.lock().expect("state");
    if std::mem::take(&mut state.expire_next) {
        session.keys = None;
    }
    let Some(keys) = &session.keys else {
        return (403, false, Vec::new());
    };
    let plain = keys.open(seq, body).expect("a request this session sealed");
    let request: Value = serde_json::from_slice(&plain).expect("json");
    let learning = request["params"]["requests"][0]["method"] == "component_nego";
    if state.refuse_requests && !learning {
        session.keys = None;
        return (403, false, Vec::new());
    }
    let before = state.writes.len();
    let response = handle(kind, &request, &mut state);
    if state.lose_switch_answer && state.writes.len() > before {
        return (LOST, false, Vec::new());
    }
    let reads_meter = request["params"]["requests"]
        .as_array()
        .is_some_and(|requests| requests.iter().any(|r| r["method"] == "get_emeter_data"));
    if reads_meter && state.meter_misses > 0 {
        state.meter_misses -= 1;
    }
    if request["method"] == "control_child" && state.outlet_meter_misses > 0 {
        state.outlet_meter_misses -= 1;
    }
    let plain = serde_json::to_vec(&response).expect("json");
    (200, false, keys.seal_at(seq, &plain))
}

async fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        raw.extend_from_slice(&chunk[..read]);
        let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&raw[..end]).to_string();
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0);
        if raw.len() >= end + 4 + length {
            let path = head
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            return Ok((path, raw[end + 4..end + 4 + length].to_vec()));
        }
    }
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn batch(kind: Kind, child: Option<&str>, request: &Value, state: &State) -> Value {
    let responses: Vec<Value> = request["params"]["requests"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|inner| {
            let method = inner["method"].as_str().unwrap_or_default();
            match answer(kind, child, method, &inner["params"], state) {
                Ok(result) => json!({ "method": method, "result": result, "error_code": 0 }),
                Err(code) => json!({ "method": method, "error_code": code }),
            }
        })
        .collect();
    json!({ "error_code": 0, "result": { "responses": responses } })
}

/// A switch: kept, then taken, refused or (with `ignore_switch`) taken and not acted on. The
/// `Err` is the error code the device answers with.
fn switch(kind: Kind, child: Option<&str>, params: &Value, state: &mut State) -> Result<(), i64> {
    let Some(on) = params["device_on"].as_bool() else {
        return Err(-1008);
    };
    state.writes.push(Write {
        child: child.map(str::to_owned),
        on,
    });
    if let Some(code) = state.refuse_switch {
        return Err(code);
    }
    let relay = match (kind, child) {
        (Kind::Plug, None) => Some(&mut state.on),
        (Kind::Strip, Some(child)) => state
            .outlets
            .iter_mut()
            .find(|outlet| outlet.id == child)
            .map(|outlet| &mut outlet.on),
        // A strip has no relay of its own.
        _ => None,
    };
    let relay = relay.ok_or(-1008)?;
    if !state.ignore_switch {
        *relay = on;
    }
    Ok(())
}

fn handle(kind: Kind, request: &Value, state: &mut State) -> Value {
    match request["method"].as_str().unwrap_or_default() {
        "multipleRequest" => batch(kind, None, request, state),
        // On its own, never in a batch: how it is sent.
        "set_device_info" => match switch(kind, None, &request["params"], state) {
            Ok(()) => json!({ "error_code": 0 }),
            Err(code) => json!({ "error_code": code }),
        },
        "control_child" => {
            let child = request["params"]["device_id"].as_str().unwrap_or_default();
            if kind != Kind::Strip || !state.outlets.iter().any(|outlet| outlet.id == child) {
                return json!({ "error_code": -1001 });
            }
            let inner = &request["params"]["requestData"];
            if inner["method"] == "set_device_info" {
                let code = match switch(kind, Some(child), &inner["params"], state) {
                    Ok(()) => 0,
                    Err(code) => code,
                };
                return json!({
                    "error_code": 0, "result": { "responseData": { "error_code": code } }
                });
            }
            let mut inner = batch(kind, Some(child), &request["params"]["requestData"], state);
            // Real strips leave the inner batch's `error_code` out.
            if let Some(batch) = inner.as_object_mut() {
                batch.remove("error_code");
            }
            json!({ "error_code": 0, "result": { "responseData": inner } })
        }
        _ => json!({ "error_code": -1002 }),
    }
}

fn answer(
    kind: Kind,
    child: Option<&str>,
    method: &str,
    params: &Value,
    state: &State,
) -> Result<Value, i64> {
    match (kind, child, method) {
        (Kind::Plug, None, "component_nego") => Ok(json!({ "component_list": [
            { "id": "device", "ver_code": 2 },
            { "id": "energy_monitoring", "ver_code": 2 },
            { "id": "led", "ver_code": 1 }
        ]})),
        (Kind::Strip, None, "component_nego") => Ok(json!({ "component_list": [
            { "id": "device", "ver_code": 2 },
            { "id": "led", "ver_code": 1 },
            { "id": "energy_monitoring", "ver_code": 2 },
            { "id": "child_device", "ver_code": 1 },
            { "id": "control_child", "ver_code": 2 }
        ]})),
        (Kind::Plug, None, "get_device_info") => Ok(json!({
            "device_id": "PLUG0001", "model": "PP1", "nickname": b64("Lamp"),
            "device_on": state.on, "rssi": -48, "mac": "02-00-00-00-00-01",
            "fw_ver": "1.0.0", "hw_ver": "1.0"
        })),
        // A strip's own reading is its first outlet's; it must not be shown as the strip's.
        (Kind::Strip, None, "get_energy_usage") => Ok(json!({
            "today_energy": 400, "current_power": 2_900_000
        })),
        (Kind::Strip, None, "get_device_info") => Ok(json!({
            "device_id": "STRIP001", "model": "PS4", "nickname": "",
            "rssi": -61, "mac": "02-00-00-00-00-03",
            "fw_ver": "1.0.0", "hw_ver": "1.0"
        })),
        (Kind::Plug, None, "get_energy_usage" | "get_emeter_data") if state.no_meter => Err(-1002),
        (Kind::Plug, None, "get_energy_usage" | "get_emeter_data") if state.meter_misses > 0 => {
            Err(-1)
        }
        (Kind::Plug, None, "get_energy_usage") => Ok(json!({
            "today_energy": 1500, "month_energy": 30000, "current_power": state.power_mw
        })),
        (Kind::Plug, None, "get_emeter_data") => Ok(json!({
            "power_mw": state.power_mw, "voltage_mv": 241_200, "current_ma": 412,
            "energy_wh": 300_000
        })),
        (_, None, "get_led_info") => Ok(json!({ "led_rule": "always", "led_status": true })),
        (Kind::Strip, None, "get_child_device_list") => {
            // One outlet per page, so paging is exercised.
            let start = params["start_index"].as_u64().unwrap_or(0) as usize;
            let mut entries: Vec<Value> = state
                .outlets
                .iter()
                .map(|outlet| {
                    json!({
                        "device_id": outlet.id, "nickname": b64(outlet.name),
                        "device_on": outlet.on, "position": outlet.position,
                        "model": "PS4", "fw_ver": "1.0.0", "hw_ver": "1.0"
                    })
                })
                .collect();
            if state.unreadable_outlet {
                entries.insert(0, json!({ "device_on": true, "position": 3 }));
            }
            let sum = entries.len();
            let page: Vec<Value> = entries.into_iter().skip(start).take(1).collect();
            Ok(json!({ "child_device_list": page, "start_index": start, "sum": sum }))
        }
        (Kind::Strip, None, "get_child_device_component_list") => {
            // Paged one outlet at a time too.
            let start = params["start_index"].as_u64().unwrap_or(0) as usize;
            let page: Vec<Value> = state
                .outlets
                .iter()
                .skip(start)
                .take(1)
                .map(|outlet| {
                    json!({
                        "device_id": outlet.id,
                        "component_list": [{ "id": "energy_monitoring", "ver_code": 2 }]
                    })
                })
                .collect();
            Ok(json!({
                "child_component_list": page, "start_index": start, "sum": state.outlets.len()
            }))
        }
        // An outlet here meters through `get_energy_usage` only.
        (Kind::Strip, Some(_), "get_energy_usage") if state.outlet_meter_misses > 0 => Err(-1),
        (Kind::Strip, Some(_), "get_energy_usage") => Ok(json!({
            "today_energy": 400, "current_power": 2_900_000
        })),
        _ => Err(-1),
    }
}
