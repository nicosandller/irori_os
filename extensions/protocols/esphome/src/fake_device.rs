//! A stand-in ESPHome device, so the integration can be tested without hardware.
//!
//! It speaks the plaintext framing of ESPHome's native API — a `0x00` marker, then the payload
//! length and message type as LEB128 varints — and answers the handshake, the entity listing,
//! the state subscription, and commands. The message bodies themselves come from the same
//! generated types the integration uses, so only the framing is written out here.
//!
//! [`start_encrypted`] is the same device with an encryption key: the device side of ESPHome's
//! `Noise_NNpsk0_25519_ChaChaPoly_SHA256` handshake, then every message encrypted. Real
//! cryptography, so a wrong key fails the way it does against real firmware.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use esphome_client::types::{
    BinarySensorStateResponse, DeviceInfoResponse, EspHomeMessage, HelloResponse,
    LightCommandRequest, LightStateResponse, ListEntitiesBinarySensorResponse,
    ListEntitiesDoneResponse, ListEntitiesLightResponse, ListEntitiesSensorResponse,
    ListEntitiesSwitchResponse, SensorStateResponse, SwitchCommandRequest, SwitchStateResponse,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

pub const MAC: &str = "AA:BB:CC:DD:EE:FF";
pub const LIGHT_KEY: u32 = 11;
pub const SWITCH_KEY: u32 = 22;
pub const SENSOR_KEY: u32 = 33;
pub const MOTION_KEY: u32 = 44;
/// An entity of a kind Irori doesn't model yet, to check it's left out rather than mangled.
pub const FAN_KEY: u32 = 55;

/// What the fake device was asked to do, for the test to check.
#[derive(Debug, Default)]
pub struct Commands {
    pub lights: Vec<LightCommandRequest>,
    pub switches: Vec<SwitchCommandRequest>,
}

/// Starts the device on a port of the operating system's choosing.
pub async fn start() -> (SocketAddr, Arc<Mutex<Commands>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port on loopback");
    let address = listener.local_addr().expect("a bound address");
    let commands = Arc::new(Mutex::new(Commands::default()));
    let recorded = Arc::clone(&commands);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move { serve(stream, recorded).await });
        }
    });
    (address, commands)
}

async fn serve(mut stream: TcpStream, commands: Arc<Mutex<Commands>>) {
    let mut buffer = Vec::new();
    loop {
        let Some(message) = read(&mut stream, &mut buffer).await else {
            return;
        };
        for answer in answer(message, &commands) {
            if write(&mut stream, answer).await.is_none() {
                return;
            }
        }
    }
}

/// Starts a device that only talks encrypted, with `key`.
pub async fn start_encrypted(key: [u8; 32]) -> (SocketAddr, Arc<Mutex<Commands>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port on loopback");
    let address = listener.local_addr().expect("a bound address");
    let commands = Arc::new(Mutex::new(Commands::default()));
    let recorded = Arc::clone(&commands);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move { serve_encrypted(stream, key, recorded).await });
        }
    });
    (address, commands)
}

const NOISE: u8 = 0x01;

async fn serve_encrypted(mut stream: TcpStream, key: [u8; 32], commands: Arc<Mutex<Commands>>) {
    let mut buffer = Vec::new();
    // A client speaking plaintext gets a Noise refusal back, which is how real firmware tells it
    // encryption is required (ESPHome's `send_explicit_handshake_reject_`).
    let Some(hello) = read_noise(&mut stream, &mut buffer).await else {
        let refusal = [&[NOISE][..], b"Bad indicator byte"].concat();
        let _ = stream.write_all(&noise_frame(&refusal)).await;
        return;
    };
    let _ = hello; // the client's hello says only "Noise, please"
    let Some(handshake) = read_noise(&mut stream, &mut buffer).await else {
        return;
    };
    let mut responder = snow::Builder::new(
        "Noise_NNpsk0_25519_ChaChaPoly_SHA256"
            .parse()
            .expect("a valid pattern"),
    )
    .prologue(b"NoiseAPIInit\x00\x00")
    .expect("a prologue")
    .psk(0, &key)
    .expect("a psk")
    .build_responder()
    .expect("a responder");

    // Who it is, sent before the handshake answer: marker, name, NUL, MAC, NUL.
    let server_hello = [&[NOISE][..], b"fake\x00", MAC.as_bytes(), b"\x00"].concat();
    if stream.write_all(&noise_frame(&server_hello)).await.is_err() {
        return;
    }
    let mut scratch = vec![0_u8; 65535];
    // `handshake[0]` is a zero marker; the Noise message follows. With the wrong key the tag on
    // it doesn't verify, and firmware answers with a refusal instead of a handshake.
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
        for answer in answer(message, &commands) {
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

/// `0x01`, the payload length as two big-endian bytes, then the payload.
fn noise_frame(payload: &[u8]) -> Vec<u8> {
    let length = u16::try_from(payload.len())
        .unwrap_or(u16::MAX)
        .to_be_bytes();
    [&[NOISE][..], &length, payload].concat()
}

/// Reads one Noise frame's payload, or `None` when the connection ends or isn't speaking Noise.
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
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

/// What the device says back to a message, encrypted or not.
fn answer(message: EspHomeMessage, commands: &Mutex<Commands>) -> Vec<EspHomeMessage> {
    {
        let answers: Vec<EspHomeMessage> = match message {
            EspHomeMessage::HelloRequest(_) => vec![EspHomeMessage::HelloResponse(HelloResponse {
                api_version_major: esphome_client::API_VERSION.0,
                api_version_minor: esphome_client::API_VERSION.1,
                server_info: "fake device".to_owned(),
                name: "fake".to_owned(),
            })],
            EspHomeMessage::DeviceInfoRequest(_) => {
                vec![EspHomeMessage::DeviceInfoResponse(device_info())]
            }
            EspHomeMessage::ListEntitiesRequest(_) => entities(),
            EspHomeMessage::SubscribeStatesRequest(_) => states(),
            EspHomeMessage::LightCommandRequest(request) => {
                let on = request.state;
                let brightness = request.brightness;
                commands
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .lights
                    .push(request);
                // A real device confirms by reporting its new state, not by acknowledging.
                vec![EspHomeMessage::LightStateResponse(LightStateResponse {
                    key: LIGHT_KEY,
                    state: on,
                    brightness,
                    color_mode: 3, // brightness
                    ..Default::default()
                })]
            }
            EspHomeMessage::SwitchCommandRequest(request) => {
                let on = request.state;
                commands
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .switches
                    .push(request);
                vec![EspHomeMessage::SwitchStateResponse(SwitchStateResponse {
                    key: SWITCH_KEY,
                    state: on,
                    ..Default::default()
                })]
            }
            EspHomeMessage::PingRequest(_) => vec![EspHomeMessage::PingResponse(
                esphome_client::types::PingResponse {},
            )],
            _ => Vec::new(),
        };
        answers
    }
}

fn device_info() -> DeviceInfoResponse {
    DeviceInfoResponse {
        name: "fake".to_owned(),
        friendly_name: "Fake device".to_owned(),
        mac_address: MAC.to_owned(),
        model: "esp32".to_owned(),
        manufacturer: "Espressif".to_owned(),
        esphome_version: "2026.8.2".to_owned(),
        ..Default::default()
    }
}

fn entities() -> Vec<EspHomeMessage> {
    vec![
        EspHomeMessage::ListEntitiesLightResponse(ListEntitiesLightResponse {
            key: LIGHT_KEY,
            name: "Desk lamp".to_owned(),
            // Brightness and colour temperature, 2000-6500 K expressed as mireds.
            supported_color_modes: vec![11],
            min_mireds: 153.0,
            max_mireds: 500.0,
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesSwitchResponse(ListEntitiesSwitchResponse {
            key: SWITCH_KEY,
            name: "Fan plug".to_owned(),
            device_class: "outlet".to_owned(),
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesSensorResponse(ListEntitiesSensorResponse {
            key: SENSOR_KEY,
            name: "Room temperature".to_owned(),
            unit_of_measurement: "°C".to_owned(),
            device_class: "temperature".to_owned(),
            state_class: 1,
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesBinarySensorResponse(ListEntitiesBinarySensorResponse {
            key: MOTION_KEY,
            name: "Hallway motion".to_owned(),
            device_class: "motion".to_owned(),
            ..Default::default()
        }),
        // Irori has no fan kind yet; the integration should skip it and keep the rest.
        EspHomeMessage::ListEntitiesFanResponse(esphome_client::types::ListEntitiesFanResponse {
            key: FAN_KEY,
            name: "Ceiling fan".to_owned(),
            ..Default::default()
        }),
        EspHomeMessage::ListEntitiesDoneResponse(ListEntitiesDoneResponse {}),
    ]
}

fn states() -> Vec<EspHomeMessage> {
    vec![
        EspHomeMessage::LightStateResponse(LightStateResponse {
            key: LIGHT_KEY,
            state: false,
            brightness: 0.5,
            color_mode: 11,
            color_temperature: 370.0,
            ..Default::default()
        }),
        EspHomeMessage::SwitchStateResponse(SwitchStateResponse {
            key: SWITCH_KEY,
            state: true,
            ..Default::default()
        }),
        EspHomeMessage::SensorStateResponse(SensorStateResponse {
            key: SENSOR_KEY,
            state: 21.5,
            missing_state: false,
            ..Default::default()
        }),
        EspHomeMessage::BinarySensorStateResponse(BinarySensorStateResponse {
            key: MOTION_KEY,
            state: true,
            ..Default::default()
        }),
    ]
}

/// Reads one framed message, or `None` when the connection ends.
async fn read(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<EspHomeMessage> {
    loop {
        if let Some(message) = take_frame(buffer) {
            return Some(message);
        }
        let mut chunk = [0_u8; 1024];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

/// `0x00`, the payload length and the message type as varints, then the payload.
fn take_frame(buffer: &mut Vec<u8>) -> Option<EspHomeMessage> {
    if buffer.first()? != &0x00 {
        return None;
    }
    let (length, next) = varint(buffer, 1)?;
    let (type_id, next) = varint(buffer, next)?;
    let length = usize::from(length);
    if buffer.len() < next + length {
        return None;
    }
    let payload: Vec<u8> = buffer.drain(..next + length).skip(next).collect();
    // The generated code reads a message as type and length, both big-endian, then the payload.
    let framed = [
        type_id.to_be_bytes().to_vec(),
        u16::try_from(length).ok()?.to_be_bytes().to_vec(),
        payload,
    ]
    .concat();
    EspHomeMessage::try_from(framed).ok()
}

async fn write(stream: &mut TcpStream, message: EspHomeMessage) -> Option<()> {
    let encoded: Vec<u8> = message.into();
    let type_id = u16::from_be_bytes([encoded[0], encoded[1]]);
    let length = u16::from_be_bytes([encoded[2], encoded[3]]);
    let frame = [
        vec![0x00],
        leb128(length),
        leb128(type_id),
        encoded[4..].to_vec(),
    ]
    .concat();
    stream.write_all(&frame).await.ok()
}

fn leb128(mut value: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = u8::try_from(value & 0x7F).unwrap_or(0);
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}

fn varint(buffer: &[u8], mut index: usize) -> Option<(u16, usize)> {
    let mut value: u32 = 0;
    let mut shift = 0;
    loop {
        let byte = *buffer.get(index)?;
        index += 1;
        value |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some((u16::try_from(value).ok()?, index));
        }
        shift += 7;
        if shift > 21 {
            return None;
        }
    }
}
