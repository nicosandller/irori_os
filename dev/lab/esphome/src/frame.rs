//! Plaintext ESPHome native API framing: `0x00`, then length and type as LEB128, then the body.
//! The body layout is the one `esphome-client` 0.2.1 (`api-1-15`) encodes, so the lab devices
//! and the extension agree without the lab crate depending on the extension.

use esphome_client::types::EspHomeMessage;

pub fn encode(message: EspHomeMessage) -> Vec<u8> {
    let encoded: Vec<u8> = message.into();
    let type_id = u16::from_be_bytes([encoded[0], encoded[1]]);
    let length = u16::from_be_bytes([encoded[2], encoded[3]]);
    let mut frame = Vec::with_capacity(1 + 4 + length as usize);
    frame.push(0x00);
    frame.extend(leb128(length));
    frame.extend(leb128(type_id));
    frame.extend_from_slice(&encoded[4..]);
    frame
}

/// Pulls one message off the front of `buffer`. `None` when the bytes so far are not a full frame.
pub fn take(buffer: &mut Vec<u8>) -> Option<EspHomeMessage> {
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
    let framed = [
        type_id.to_be_bytes().to_vec(),
        u16::try_from(length).ok()?.to_be_bytes().to_vec(),
        payload,
    ]
    .concat();
    EspHomeMessage::try_from(framed).ok()
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
    while index < buffer.len() && shift < 21 {
        let byte = buffer[index];
        index += 1;
        value |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return u16::try_from(value).ok().map(|value| (value, index));
        }
        shift += 7;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use esphome_client::types::{HelloResponse, PingRequest};

    #[test]
    fn a_hello_roundtrips() {
        let message = EspHomeMessage::HelloResponse(HelloResponse {
            api_version_major: 1,
            api_version_minor: 15,
            server_info: "irori-lab".to_owned(),
            name: "lab".to_owned(),
        });
        let mut buffer = encode(message.clone());
        let back = take(&mut buffer).expect("a frame");
        assert!(buffer.is_empty());
        assert_eq!(back, message);
    }

    #[test]
    fn a_short_buffer_waits() {
        let mut buffer = encode(EspHomeMessage::PingRequest(PingRequest {}));
        let rest = buffer.split_off(2);
        assert!(take(&mut buffer).is_none());
        buffer.extend(rest);
        assert!(take(&mut buffer).is_some());
    }
}
