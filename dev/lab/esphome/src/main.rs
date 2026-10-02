//! Four plaintext ESPHome boards, plus one encrypted board, announced on `_esphomelib._tcp`.
//!
//! Runs in the Pi container's network namespace (`dev/pi up --lab`). The ESPHome extension
//! discovers them the same way it discovers a board on a desk. This crate is its own workspace
//! so it is not linked into `irori` or the extension.

mod frame;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use esphome_client::API_VERSION;
use esphome_client::types::{
    BinarySensorStateResponse, ClimateStateResponse, CoverStateResponse, DeviceInfoResponse,
    EspHomeMessage, FanStateResponse, HelloResponse, ListEntitiesBinarySensorResponse,
    ListEntitiesButtonResponse, ListEntitiesClimateResponse, ListEntitiesCoverResponse,
    ListEntitiesDoneResponse, ListEntitiesFanResponse, ListEntitiesLockResponse,
    ListEntitiesMediaPlayerResponse, ListEntitiesNumberResponse, ListEntitiesSelectResponse,
    ListEntitiesSensorResponse, ListEntitiesSirenResponse, ListEntitiesTextResponse,
    ListEntitiesValveResponse, LockStateResponse, NumberStateResponse, SelectStateResponse,
    SensorStateResponse, SirenStateResponse, TextStateResponse, ValveStateResponse,
};
use esphome_client::types::{ListEntitiesWaterHeaterResponse, WaterHeaterStateResponse};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const STATUS_KEY: u32 = 1;
const SIGNAL_KEY: u32 = 2;
/// Two settings and a part Irori can't model yet, so a board shows all three ways a device's
/// entities appear: a slider, a box to type into, and "Also has…".
const LED_KEY: u32 = 3;
const INTERVAL_KEY: u32 = 4;
const FAN_KEY: u32 = 5;
const LED_MODE_KEY: u32 = 6;
const MESSAGE_KEY: u32 = 7;
const RESTART_KEY: u32 = 8;
const BLIND_KEY: u32 = 9;
const DOOR_KEY: u32 = 10;
const SPEAKER_KEY: u32 = 11;
const VALVE_KEY: u32 = 12;
const SIREN_KEY: u32 = 13;
const THERMOSTAT_KEY: u32 = 15;
const WATER_HEATER_KEY: u32 = 16;
/// 32 bytes. Printed at startup as base64 so the waiting-for-a-key panel has something to paste.
const LAB_KEY: [u8; 32] = *b"irori-lab-esphome-key-32bytes!!!";

#[derive(Clone, Copy)]
struct Board {
    name: &'static str,
    hostname: &'static str,
    mac: &'static str,
    model: &'static str,
    area: &'static str,
    signal_dbm: f32,
    encrypted: bool,
}

const BOARDS: &[Board] = &[
    Board {
        name: "Bluetooth Proxy d82104",
        hostname: "lab-bt-d82104",
        mac: "02:11:00:00:D8:21",
        model: "bluetooth-proxy",
        area: "Living room",
        signal_dbm: -48.0,
        encrypted: false,
    },
    Board {
        name: "Bluetooth Proxy 2b0900",
        hostname: "lab-bt-2b0900",
        mac: "02:11:00:00:2B:09",
        model: "bluetooth-proxy",
        area: "Living room",
        signal_dbm: -61.0,
        encrypted: false,
    },
    Board {
        name: "Living room board",
        hostname: "lab-living-room",
        mac: "02:11:00:00:00:03",
        model: "esp32",
        area: "Living room",
        signal_dbm: -55.0,
        encrypted: false,
    },
    Board {
        name: "kitchen bt proxy",
        hostname: "lab-kitchen",
        mac: "02:11:00:00:00:04",
        model: "esp32",
        area: "Kitchen",
        signal_dbm: -70.0,
        encrypted: false,
    },
    Board {
        name: "Lab lock",
        hostname: "lab-lock",
        mac: "02:11:00:00:00:EE",
        model: "esp32",
        area: "Office",
        signal_dbm: -40.0,
        encrypted: true,
    },
];

#[tokio::main]
async fn main() {
    let ip = lan_v4().unwrap_or(Ipv4Addr::UNSPECIFIED);
    let mdns = ServiceDaemon::new().expect("mDNS");
    println!("irori lab ESPHome on {ip}");
    println!(
        "encrypted device {} key {}",
        BOARDS[4].mac,
        base64_key(&LAB_KEY)
    );

    let mut tasks = Vec::new();
    for board in BOARDS {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .await
            .expect("a port");
        let port = listener.local_addr().expect("addr").port();
        announce(&mdns, board, ip, port);
        println!(
            "  {}  mac {}  port {port}{}",
            board.name,
            board.mac,
            if board.encrypted { "  encrypted" } else { "" }
        );
        let board = *board;
        tasks.push(tokio::spawn(async move { listen(listener, board).await }));
    }
    for task in tasks {
        let _ = task.await;
    }
}

fn announce(mdns: &ServiceDaemon, board: &Board, ip: Ipv4Addr, port: u16) {
    let mut props = HashMap::from([
        (
            "mac".to_owned(),
            board.mac.replace(':', "").to_ascii_lowercase(),
        ),
        ("friendly_name".to_owned(), board.name.to_owned()),
    ]);
    if board.encrypted {
        props.insert(
            "api_encryption".to_owned(),
            "Noise_NNpsk0_25519_ChaChaPoly_SHA256".to_owned(),
        );
    }
    let host = format!("{}.local.", board.hostname);
    let info = ServiceInfo::new(
        "_esphomelib._tcp.local.",
        board.name,
        &host,
        IpAddr::V4(ip),
        port,
        props,
    )
    .expect("service info");
    mdns.register(info).expect("announce");
}

async fn listen(listener: TcpListener, board: Board) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let board = board;
        tokio::spawn(async move {
            if board.encrypted {
                serve_encrypted(stream, board).await;
            } else {
                serve_plain(stream, board).await;
            }
        });
    }
}

async fn serve_plain(mut stream: TcpStream, board: Board) {
    let mut buffer = Vec::new();
    loop {
        let Some(message) = read_plain(&mut stream, &mut buffer).await else {
            return;
        };
        for answer in answers(message, &board) {
            if stream.write_all(&frame::encode(answer)).await.is_err() {
                return;
            }
        }
    }
}

fn answers(message: EspHomeMessage, board: &Board) -> Vec<EspHomeMessage> {
    match message {
        EspHomeMessage::HelloRequest(_) => vec![EspHomeMessage::HelloResponse(HelloResponse {
            api_version_major: API_VERSION.0,
            api_version_minor: API_VERSION.1,
            server_info: "irori-lab".to_owned(),
            name: board.hostname.to_owned(),
        })],
        EspHomeMessage::DeviceInfoRequest(_) => {
            vec![EspHomeMessage::DeviceInfoResponse(device_info(board))]
        }
        EspHomeMessage::ListEntitiesRequest(_) => entities(board),
        EspHomeMessage::SubscribeStatesRequest(_) => states(board),
        // The blind gets wherever it's sent at once, and says so; a stop leaves it where it is.
        EspHomeMessage::CoverCommandRequest(request) if request.has_position => {
            vec![EspHomeMessage::CoverStateResponse(CoverStateResponse {
                key: request.key,
                position: request.position,
                ..Default::default()
            })]
        }
        EspHomeMessage::ValveCommandRequest(request) if request.has_position => {
            vec![EspHomeMessage::ValveStateResponse(ValveStateResponse {
                key: request.key,
                position: request.position,
                ..Default::default()
            })]
        }
        // The thermostat speaks °F, as some do, and takes what it's told. It keeps nothing, so a
        // change of target answers in heat, and a change of mode at 70 °F.
        EspHomeMessage::ClimateCommandRequest(request) => {
            vec![EspHomeMessage::ClimateStateResponse(thermostat(
                if request.has_mode { request.mode } else { 3 },
                if request.has_target_temperature {
                    request.target_temperature
                } else {
                    70.0
                },
            ))]
        }
        // The water heater takes what it's told, and keeps nothing: a new target answers in
        // eco, and a new mode at 55 °C. Bit 1 of `has_fields` is the mode, 2 the target, 32 its
        // switch; bit 1 of `state` is on.
        EspHomeMessage::WaterHeaterCommandRequest(request) => {
            let switched = request.has_fields & 32 != 0;
            vec![EspHomeMessage::WaterHeaterStateResponse(water_heater(
                if request.has_fields & 1 != 0 {
                    request.mode
                } else {
                    1
                },
                if request.has_fields & 2 != 0 {
                    request.target_temperature
                } else {
                    55.0
                },
                !switched || request.state & 2 != 0,
            ))]
        }
        EspHomeMessage::SirenCommandRequest(request) => {
            vec![EspHomeMessage::SirenStateResponse(SirenStateResponse {
                key: request.key,
                state: request.state,
                ..Default::default()
            })]
        }
        // The fan does what it's told and says so; a request carries only what it changes.
        EspHomeMessage::FanCommandRequest(request) => {
            vec![EspHomeMessage::FanStateResponse(FanStateResponse {
                key: request.key,
                state: request.state || request.has_speed_level,
                speed_level: request.speed_level,
                oscillating: request.oscillating,
                ..Default::default()
            })]
        }
        // The lock does what it's told: `LockCommand` 0 unlock, 1 lock, 2 open, answered with
        // `LockState` 2 unlocked, 1 locked, 7 open.
        EspHomeMessage::LockCommandRequest(request) => {
            vec![EspHomeMessage::LockStateResponse(LockStateResponse {
                key: request.key,
                state: match request.command {
                    0 => 2,
                    2 => 7,
                    _ => 1,
                },
                ..Default::default()
            })]
        }
        // A real board would restart; this one just says so. A button reports nothing.
        EspHomeMessage::ButtonCommandRequest(request) => {
            println!("{}: button {} pressed", board.name, request.key);
            Vec::new()
        }
        EspHomeMessage::TextCommandRequest(request) => {
            vec![EspHomeMessage::TextStateResponse(TextStateResponse {
                key: request.key,
                state: request.state,
                ..Default::default()
            })]
        }
        EspHomeMessage::SelectCommandRequest(request) => {
            vec![EspHomeMessage::SelectStateResponse(SelectStateResponse {
                key: request.key,
                state: request.state,
                ..Default::default()
            })]
        }
        // A real board answers by reporting the value it now has.
        EspHomeMessage::NumberCommandRequest(request) => {
            vec![EspHomeMessage::NumberStateResponse(NumberStateResponse {
                key: request.key,
                state: request.state,
                ..Default::default()
            })]
        }
        EspHomeMessage::PingRequest(_) => {
            vec![EspHomeMessage::PingResponse(
                esphome_client::types::PingResponse {},
            )]
        }
        _ => Vec::new(),
    }
}

fn device_info(board: &Board) -> DeviceInfoResponse {
    DeviceInfoResponse {
        name: board.hostname.to_owned(),
        friendly_name: board.name.to_owned(),
        mac_address: board.mac.to_owned(),
        model: board.model.to_owned(),
        manufacturer: "Espressif".to_owned(),
        esphome_version: "2026.7.4".to_owned(),
        suggested_area: board.area.to_owned(),
        ..Default::default()
    }
}

fn entities(_board: &Board) -> Vec<EspHomeMessage> {
    vec![
        EspHomeMessage::ListEntitiesBinarySensorResponse(ListEntitiesBinarySensorResponse {
            key: STATUS_KEY,
            name: "Status".to_owned(),
            device_class: "connectivity".to_owned(),
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesSensorResponse(ListEntitiesSensorResponse {
            key: SIGNAL_KEY,
            name: "Wi-Fi signal".to_owned(),
            unit_of_measurement: "dBm".to_owned(),
            device_class: "signal_strength".to_owned(),
            state_class: 1,
            entity_category: 2, // diagnostic
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesNumberResponse(ListEntitiesNumberResponse {
            key: LED_KEY,
            name: "LED brightness".to_owned(),
            min_value: 0.0,
            max_value: 100.0,
            step: 1.0,
            unit_of_measurement: "%".to_owned(),
            mode: 2,            // slider
            entity_category: 1, // config
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesNumberResponse(ListEntitiesNumberResponse {
            key: INTERVAL_KEY,
            name: "Update interval".to_owned(),
            min_value: 10.0,
            max_value: 3600.0,
            step: 10.0,
            unit_of_measurement: "s".to_owned(),
            device_class: "duration".to_owned(),
            mode: 1,            // box
            entity_category: 1, // config
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesSelectResponse(ListEntitiesSelectResponse {
            key: LED_MODE_KEY,
            name: "LED mode".to_owned(),
            options: vec![
                "off".to_owned(),
                "status".to_owned(),
                "always on".to_owned(),
            ],
            entity_category: 1, // config
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesTextResponse(ListEntitiesTextResponse {
            key: MESSAGE_KEY,
            name: "Display message".to_owned(),
            max_length: 32,
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesButtonResponse(ListEntitiesButtonResponse {
            key: RESTART_KEY,
            name: "Restart".to_owned(),
            device_class: "restart".to_owned(),
            entity_category: 1, // config
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesCoverResponse(ListEntitiesCoverResponse {
            key: BLIND_KEY,
            name: "Window blind".to_owned(),
            device_class: "blind".to_owned(),
            supports_position: true,
            supports_stop: true,
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesLockResponse(ListEntitiesLockResponse {
            key: DOOR_KEY,
            name: "Door lock".to_owned(),
            supports_open: true,
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesFanResponse(ListEntitiesFanResponse {
            key: FAN_KEY,
            name: "Cooling fan".to_owned(),
            supports_speed: true,
            supported_speed_count: 3,
            supports_oscillation: true,
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesValveResponse(ListEntitiesValveResponse {
            key: VALVE_KEY,
            name: "Water valve".to_owned(),
            device_class: "water".to_owned(),
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesSirenResponse(ListEntitiesSirenResponse {
            key: SIREN_KEY,
            name: "Buzzer".to_owned(),
            tones: vec!["beep".to_owned(), "alarm".to_owned()],
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesClimateResponse(ListEntitiesClimateResponse {
            key: THERMOSTAT_KEY,
            name: "Thermostat".to_owned(),
            supports_current_temperature: true,
            supports_action: true,
            // Off, cool, heat, auto.
            supported_modes: vec![0, 2, 3, 6],
            visual_min_temperature: 50.0,
            visual_max_temperature: 86.0,
            visual_target_temperature_step: 1.0,
            // Home, away, eco.
            supported_presets: vec![1, 2, 5],
            temperature_unit: 1,
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesWaterHeaterResponse(ListEntitiesWaterHeaterResponse {
            key: WATER_HEATER_KEY,
            name: "Hot water".to_owned(),
            min_temperature: 40.0,
            max_temperature: 65.0,
            target_temperature_step: 1.0,
            // Eco, performance.
            supported_modes: vec![1, 3],
            // Current temperature, target, modes, on and off.
            supported_features: 1 | 2 | 4 | 16,
            ..Default::default()
        }),
        // Irori has no media player kind yet: this shows as "Also has…".
        EspHomeMessage::ListEntitiesMediaPlayerResponse(ListEntitiesMediaPlayerResponse {
            key: SPEAKER_KEY,
            name: "Speaker".to_owned(),
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesDoneResponse(ListEntitiesDoneResponse {}),
    ]
}

/// The thermostat in `mode` aiming for `target` °F, in a 68 °F room: heating while it's below.
fn thermostat(mode: i32, target: f32) -> ClimateStateResponse {
    let heating = mode == 3 && target > 68.0;
    ClimateStateResponse {
        key: THERMOSTAT_KEY,
        mode,
        current_temperature: 68.0,
        target_temperature: target,
        // `ClimateAction`: off, heating, idle.
        action: match (mode, heating) {
            (0, _) => 0,
            (_, true) => 3,
            _ => 4,
        },
        ..Default::default()
    }
}

/// The water heater in `mode` aiming for `target` °C, its water at 52 °C.
fn water_heater(mode: i32, target: f32, on: bool) -> WaterHeaterStateResponse {
    WaterHeaterStateResponse {
        key: WATER_HEATER_KEY,
        mode,
        current_temperature: 52.0,
        target_temperature: target,
        state: if on { 2 } else { 0 },
        ..Default::default()
    }
}

fn states(board: &Board) -> Vec<EspHomeMessage> {
    vec![
        EspHomeMessage::BinarySensorStateResponse(BinarySensorStateResponse {
            key: STATUS_KEY,
            state: true,
            ..Default::default()
        }),
        EspHomeMessage::SensorStateResponse(SensorStateResponse {
            key: SIGNAL_KEY,
            state: board.signal_dbm,
            missing_state: false,
            ..Default::default()
        }),
        EspHomeMessage::NumberStateResponse(NumberStateResponse {
            key: LED_KEY,
            state: 40.0,
            ..Default::default()
        }),
        EspHomeMessage::SirenStateResponse(SirenStateResponse {
            key: SIREN_KEY,
            state: false,
            ..Default::default()
        }),
        EspHomeMessage::ClimateStateResponse(thermostat(3, 70.0)),
        EspHomeMessage::WaterHeaterStateResponse(water_heater(1, 55.0, true)),
        EspHomeMessage::ValveStateResponse(ValveStateResponse {
            key: VALVE_KEY,
            position: 1.0,
            ..Default::default()
        }),
        EspHomeMessage::FanStateResponse(FanStateResponse {
            key: FAN_KEY,
            state: true,
            speed_level: 1,
            ..Default::default()
        }),
        EspHomeMessage::LockStateResponse(LockStateResponse {
            key: DOOR_KEY,
            state: 1,
            ..Default::default()
        }),
        EspHomeMessage::CoverStateResponse(CoverStateResponse {
            key: BLIND_KEY,
            position: 0.6,
            ..Default::default()
        }),
        EspHomeMessage::TextStateResponse(TextStateResponse {
            key: MESSAGE_KEY,
            state: "Hello from the lab".to_owned(),
            ..Default::default()
        }),
        EspHomeMessage::SelectStateResponse(SelectStateResponse {
            key: LED_MODE_KEY,
            state: "status".to_owned(),
            ..Default::default()
        }),
        EspHomeMessage::NumberStateResponse(NumberStateResponse {
            key: INTERVAL_KEY,
            state: 60.0,
            ..Default::default()
        }),
    ]
}

async fn read_plain(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<EspHomeMessage> {
    loop {
        if let Some(message) = frame::take(buffer) {
            return Some(message);
        }
        let mut chunk = [0_u8; 1024];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
}

const NOISE: u8 = 0x01;

async fn serve_encrypted(mut stream: TcpStream, board: Board) {
    let mut buffer = Vec::new();
    let Some(_hello) = read_noise(&mut stream, &mut buffer).await else {
        let refusal = [&[NOISE][..], b"Bad indicator byte"].concat();
        let _ = stream.write_all(&noise_frame(&refusal)).await;
        return;
    };
    let Some(handshake) = read_noise(&mut stream, &mut buffer).await else {
        return;
    };
    let mut responder = snow::Builder::new(
        "Noise_NNpsk0_25519_ChaChaPoly_SHA256"
            .parse()
            .expect("pattern"),
    )
    .prologue(b"NoiseAPIInit\x00\x00")
    .expect("prologue")
    .psk(0, &LAB_KEY)
    .expect("psk")
    .build_responder()
    .expect("responder");

    let server_hello = [&[NOISE][..], b"lab\x00", board.mac.as_bytes(), b"\x00"].concat();
    if stream.write_all(&noise_frame(&server_hello)).await.is_err() {
        return;
    }
    let mut scratch = vec![0_u8; 65535];
    if handshake.is_empty()
        || responder
            .read_message(&handshake[1..], &mut scratch)
            .is_err()
    {
        let refusal = [&[NOISE][..], b"Handshake MAC failure"].concat();
        let _ = stream.write_all(&noise_frame(&refusal)).await;
        return;
    }
    let Ok(size) = responder.write_message(&[], &mut scratch) else {
        return;
    };
    let reply = [&[0x00][..], &scratch[..size]].concat();
    if stream.write_all(&noise_frame(&reply)).await.is_err() {
        return;
    }
    let Ok(mut transport) = responder.into_transport_mode() else {
        return;
    };
    loop {
        let Some(sealed) = read_noise(&mut stream, &mut buffer).await else {
            return;
        };
        let Ok(size) = transport.read_message(&sealed, &mut scratch) else {
            return;
        };
        let Ok(message) = EspHomeMessage::try_from(scratch[..size].to_vec()) else {
            continue;
        };
        for answer in answers(message, &board) {
            let plain: Vec<u8> = answer.into();
            let mut sealed = vec![0_u8; 65535];
            let Ok(size) = transport.write_message(&plain, &mut sealed) else {
                return;
            };
            if stream
                .write_all(&noise_frame(&sealed[..size]))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

fn noise_frame(payload: &[u8]) -> Vec<u8> {
    let length = u16::try_from(payload.len())
        .unwrap_or(u16::MAX)
        .to_be_bytes();
    [&[NOISE][..], &length, payload].concat()
}

async fn read_noise(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    loop {
        if buffer.len() >= 3 {
            if buffer[0] != NOISE {
                return None;
            }
            let length = usize::from(u16::from_be_bytes([buffer[1], buffer[2]]));
            if buffer.len() >= 3 + length {
                return Some(buffer.drain(..3 + length).skip(3).collect());
            }
        }
        let mut chunk = [0_u8; 1024];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
}

fn lan_v4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    match sock.local_addr().ok()? {
        SocketAddr::V4(addr) if !addr.ip().is_unspecified() => Some(*addr.ip()),
        _ => None,
    }
}

fn base64_key(key: &[u8; 32]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in key.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
