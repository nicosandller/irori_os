//! The one HTTP exchange a reading needs: a GET, answered with a status and a body. One
//! connection per reading, closed after it, so nothing waits on a half-closed socket.
//!
//! Everything a monitor sends is bounded: the whole exchange in time, the answer in size. A
//! redirect is never followed, so a monitor can't point this at anything else.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Connect, send and read, all within this.
pub const EXCHANGE: Duration = Duration::from_secs(10);
/// A reading is under 1 KB. Anything past this isn't one.
pub const RESPONSE_CAP: usize = 64 * 1024;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Asks `address` for `path`, giving up after `limit`. `host` is what the monitor is called in
/// the settings, sent as the `Host` header.
pub async fn get(
    address: SocketAddr,
    host: &str,
    path: &str,
    limit: Duration,
) -> io::Result<Response> {
    tokio::time::timeout(limit, exchange(address, host, path))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the monitor didn't answer in time"))?
}

async fn exchange(address: SocketAddr, host: &str, path: &str) -> io::Result<Response> {
    let mut stream = TcpStream::connect(address).await?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        if raw.len() > RESPONSE_CAP {
            return Err(invalid("the monitor's answer is too large"));
        }
        // Stop as soon as a Content-Length body is complete: firmware may keep the socket open.
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
    end.checked_add(4)?.checked_add(length)
}

pub(crate) fn parse(raw: &[u8]) -> io::Result<Response> {
    let end = header_end(raw).ok_or_else(|| invalid("the monitor's answer has no headers"))?;
    let head = std::str::from_utf8(&raw[..end])
        .map_err(|_| invalid("the monitor's headers aren't text"))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|line| line.starts_with("HTTP/1."))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| invalid("the monitor's answer has no status"))?;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
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
            Some(_) => return Err(invalid("the monitor's answer was cut short")),
            None => rest.to_vec(),
        }
    };
    Ok(Response { status, body })
}

fn dechunk(mut rest: &[u8]) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| invalid("the monitor's answer was cut short"))?;
        let size = std::str::from_utf8(&rest[..line_end])
            .ok()
            .and_then(|text| usize::from_str_radix(text.split(';').next()?.trim(), 16).ok())
            .ok_or_else(|| invalid("the monitor's answer has a bad chunk"))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        let Some(next) = size.checked_add(2).filter(|next| *next <= rest.len()) else {
            return Err(invalid("the monitor's answer was cut short"));
        };
        body.extend_from_slice(&rest[..size]);
        rest = &rest[next..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_status_and_body() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let response = parse(raw).expect("valid");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn reads_a_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse(raw).expect("valid").body, b"abcde");
    }

    #[test]
    fn reads_a_body_that_ends_with_the_connection() {
        let raw = b"HTTP/1.0 200 OK\r\n\r\n{\"a\":1}";
        assert_eq!(parse(raw).expect("valid").body, b"{\"a\":1}");
    }

    #[test]
    fn a_huge_chunk_size_is_an_error_not_a_panic() {
        for size in ["ffffffffffffffff", "fffffffffffffffe"] {
            let raw =
                format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{size}\r\nabc\r\n");
            assert!(parse(raw.as_bytes()).is_err());
        }
    }

    #[test]
    fn a_huge_content_length_is_an_error_not_a_panic() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\nabc";
        assert_eq!(complete_length(raw), None);
        assert!(parse(raw).is_err());
    }

    #[test]
    fn a_short_body_is_an_error() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn something_that_isnt_http_is_an_error() {
        assert!(parse(b"SSH-2.0-server\r\n\r\n").is_err());
        assert!(parse(b"{\"serialno\":\"x\"}").is_err());
    }
}
