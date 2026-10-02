//! The Cast v2 frame: a 4-byte big-endian length and one protobuf `CastMessage`.
//!
//! Only the fields a session needs are written, and only a text payload is read. There is no
//! `protoc` step.

use tokio::io::{AsyncRead, AsyncReadExt};

/// The largest frame accepted. A status is a few kilobytes; anything larger is not a Cast message.
const MAX_FRAME: usize = 1024 * 1024;

pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

pub const RECEIVER: &str = "receiver-0";
pub const SENDER: &str = "sender-0";

/// One text message on a Cast namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastMessage {
    pub source_id: String,
    pub destination_id: String,
    pub namespace: String,
    pub payload: String,
}

impl CastMessage {
    pub fn new(
        destination_id: impl Into<String>,
        namespace: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        Self {
            source_id: SENDER.to_owned(),
            destination_id: destination_id.into(),
            namespace: namespace.into(),
            payload: payload.into(),
        }
    }
}

/// The length prefix and the protobuf body.
pub fn encode(message: &CastMessage) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint_field(&mut body, 1, 0);
    write_string(&mut body, 2, &message.source_id);
    write_string(&mut body, 3, &message.destination_id);
    write_string(&mut body, 4, &message.namespace);
    write_varint_field(&mut body, 5, 0);
    write_string(&mut body, 6, &message.payload);
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(u32::try_from(body.len()).unwrap_or(u32::MAX)).to_be_bytes());
    frame.extend(body);
    frame
}

/// The next frame. `Err` is a closed socket or a frame that isn't a text Cast message.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<CastMessage, String> {
    let mut len_buf = [0; 4];
    reader
        .read_exact(&mut len_buf)
        .await
        .map_err(|_| "the Cast device closed the connection".to_owned())?;
    let len = usize::try_from(u32::from_be_bytes(len_buf)).unwrap_or(usize::MAX);
    if len == 0 || len > MAX_FRAME {
        return Err("a Cast message was the wrong size".into());
    }
    let mut body = vec![0; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|_| "the Cast device closed the connection".to_owned())?;
    decode(&body)
}

fn decode(mut data: &[u8]) -> Result<CastMessage, String> {
    let mut source = None;
    let mut destination = None;
    let mut namespace = None;
    let mut payload = None;
    let mut payload_type = 0u64;
    while !data.is_empty() {
        let key = read_varint(&mut data)?;
        let field = key >> 3;
        let wire = key & 7;
        match (field, wire) {
            (1, 0) => {
                let _version = read_varint(&mut data)?;
            }
            (5, 0) => payload_type = read_varint(&mut data)?,
            (2 | 3 | 4 | 6, 2) => {
                let text = read_bytes(&mut data)?;
                let text = String::from_utf8(text).map_err(|_| "a Cast field wasn't text")?;
                match field {
                    2 => source = Some(text),
                    3 => destination = Some(text),
                    4 => namespace = Some(text),
                    _ => payload = Some(text),
                }
            }
            (_, 0) => {
                let _ = read_varint(&mut data)?;
            }
            (_, 1) => skip(&mut data, 8)?,
            (_, 2) => {
                let _ = read_bytes(&mut data)?;
            }
            (_, 5) => skip(&mut data, 4)?,
            _ => return Err("a Cast message had a field this reader doesn't know".into()),
        }
    }
    if payload_type != 0 {
        return Err("a Cast message wasn't text".into());
    }
    Ok(CastMessage {
        source_id: source.unwrap_or_default(),
        destination_id: destination.unwrap_or_default(),
        namespace: namespace.ok_or("a Cast message had no namespace")?,
        payload: payload.unwrap_or_default(),
    })
}

fn write_varint_field(out: &mut Vec<u8>, field: u32, value: u64) {
    write_varint(out, u64::from(field) << 3);
    write_varint(out, value);
}

fn write_string(out: &mut Vec<u8>, field: u32, value: &str) {
    write_varint(out, (u64::from(field) << 3) | 2);
    write_varint(out, value.len() as u64);
    out.extend(value.as_bytes());
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn read_varint(data: &mut &[u8]) -> Result<u64, String> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = data.first().copied().ok_or("a Cast message ended early")?;
        *data = &data[1..];
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("a Cast message had a number that didn't end".into())
}

fn read_bytes(data: &mut &[u8]) -> Result<Vec<u8>, String> {
    let len = usize::try_from(read_varint(data)?).unwrap_or(usize::MAX);
    if data.len() < len {
        return Err("a Cast message ended early".into());
    }
    let (head, tail) = data.split_at(len);
    *data = tail;
    Ok(head.to_vec())
}

fn skip(data: &mut &[u8], len: usize) -> Result<(), String> {
    if data.len() < len {
        return Err("a Cast message ended early".into());
    }
    *data = &data[len..];
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_round_trips() {
        let message = CastMessage::new(RECEIVER, NS_RECEIVER, r#"{"type":"GET_STATUS"}"#);
        let frame = encode(&message);
        let len = u32::from_be_bytes(frame[..4].try_into().expect("length"));
        let decoded = decode(&frame[4..]).expect("decodes");
        assert_eq!(len as usize, frame.len() - 4);
        assert_eq!(decoded, message);
    }
}
