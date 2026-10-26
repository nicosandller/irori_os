//! The one HTTP exchange KLAP needs: a POST of bytes, answered with a status, maybe a session
//! cookie, and bytes. One connection per exchange, so nothing waits on a half-closed socket.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Connect, send and read, all within this.
const EXCHANGE: Duration = Duration::from_secs(5);
/// A strip's child list is the largest answer, a few KB. Anything past this isn't a plug.
const RESPONSE_CAP: usize = 256 * 1024;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    /// `TP_SESSIONID=…`, without its attributes.
    pub session: Option<String>,
    pub body: Vec<u8>,
}

pub async fn post(
    address: SocketAddr,
    path: &str,
    cookie: Option<&str>,
    body: &[u8],
) -> io::Result<Response> {
    tokio::time::timeout(EXCHANGE, exchange(address, path, cookie, body))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the device didn't answer in time"))?
}

async fn exchange(
    address: SocketAddr,
    path: &str,
    cookie: Option<&str>,
    body: &[u8],
) -> io::Result<Response> {
    let mut stream = TcpStream::connect(address).await?;
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\n\
         Content-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        address.ip(),
        body.len()
    );
    if let Some(cookie) = cookie {
        request.push_str("Cookie: ");
        request.push_str(cookie);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        if raw.len() > RESPONSE_CAP {
            return Err(invalid("the device's answer is too large"));
        }
        // Stop as soon as a Content-Length body is complete: some firmware keeps the socket open.
        if let Some(expected) = complete_length(&raw)
            && raw.len() >= expected
        {
            break;
        }
    }
    parse(&raw)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

/// The whole response's length, when the headers are in and give a Content-Length.
fn complete_length(raw: &[u8]) -> Option<usize> {
    let end = header_end(raw)?;
    let head = std::str::from_utf8(&raw[..end]).ok()?;
    let length = head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    })?;
    Some(end + 4 + length)
}

pub(crate) fn parse(raw: &[u8]) -> io::Result<Response> {
    let end = header_end(raw).ok_or_else(|| invalid("the device's answer has no headers"))?;
    let head = std::str::from_utf8(&raw[..end])
        .map_err(|_| invalid("the device's headers aren't text"))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| invalid("the device's answer has no status"))?;
    let mut session = None;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("set-cookie") {
            for part in value.split(';') {
                let part = part.trim();
                if part.starts_with("TP_SESSIONID=") {
                    session = Some(part.to_owned());
                }
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
        }
    }
    let rest = &raw[end + 4..];
    let body = if chunked {
        dechunk(rest)?
    } else {
        match length {
            Some(length) if length <= rest.len() => rest[..length].to_vec(),
            Some(_) => return Err(invalid("the device's answer was cut short")),
            None => rest.to_vec(),
        }
    };
    Ok(Response {
        status,
        session,
        body,
    })
}

fn dechunk(mut rest: &[u8]) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| invalid("the device's answer was cut short"))?;
        let size = std::str::from_utf8(&rest[..line_end])
            .ok()
            .and_then(|text| usize::from_str_radix(text.split(';').next()?.trim(), 16).ok())
            .ok_or_else(|| invalid("the device's answer has a bad chunk"))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        if size > RESPONSE_CAP {
            return Err(invalid("the device's answer is too large"));
        }
        let Some(next) = size.checked_add(2).filter(|next| *next <= rest.len()) else {
            return Err(invalid("the device's answer was cut short"));
        };
        body.extend_from_slice(&rest[..size]);
        rest = &rest[next..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_status_session_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
            Set-Cookie: TP_SESSIONID=ABC123;TIMEOUT=86400\r\nContent-Length: 3\r\n\r\nxyz";
        let response = parse(raw).expect("valid");
        assert_eq!(response.status, 200);
        assert_eq!(response.session.as_deref(), Some("TP_SESSIONID=ABC123"));
        assert_eq!(response.body, b"xyz");
    }

    #[test]
    fn reads_a_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse(raw).expect("valid").body, b"abcde");
    }

    #[test]
    fn a_huge_chunk_size_is_an_error_not_a_panic() {
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nabc\r\n";
        assert!(parse(raw).is_err());
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nfffffffffffffffe\r\nabc\r\n";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn a_short_body_is_an_error() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc";
        assert!(parse(raw).is_err());
    }
}
