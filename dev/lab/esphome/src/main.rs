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
    BinarySensorStateResponse, DeviceInfoResponse, EspHomeMessage, HelloResponse,
    ListEntitiesBinarySensorResponse, ListEntitiesDoneResponse, ListEntitiesSensorResponse,
    SensorStateResponse,
};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const STATUS_KEY: u32 = 1;
const SIGNAL_KEY: u32 = 2;
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
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesDoneResponse(ListEntitiesDoneResponse {}),
    ]
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
