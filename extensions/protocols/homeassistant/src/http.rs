//! One plain `GET` to Home Assistant's HTTP API, for what its WebSocket API doesn't carry: a
//! camera's picture (`/api/camera_proxy/<entity_id>`), its live stream (`/api/hls/…`), and an
//! automation's setup (`/api/config/automation/config/<id>`).
//!
//! A few lines of HTTP/1.1 over a connection from `transport.rs` (TCP, or TLS with its certificate
//! checked) rather than a client library in the binary: one request per connection, the token in
//! `Authorization`, the answer read up to a limit, within a time limit.
//!
//! **Only reads** (rule 5 of milestone 11): the request is always a `GET`, and only to an address
//! under [`READ_PATHS`]. A `POST` to the same automation address would change it; there is no way
//! to send one.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::transport::{Stream, Transport};

/// What Home Assistant answered: its `Content-Type` (without parameters) and the body.
#[derive(Debug, PartialEq)]
pub struct Answer {
    pub content_type: String,
    pub body: Vec<u8>,
}

/// The only addresses a request goes to: each one, read with a `GET`, changes nothing.
pub const READ_PATHS: &[&str] = &[
    "/api/camera_proxy/",
    "/api/image_proxy/",
    "/api/hls/",
    "/api/config/automation/config/",
];

/// Whether `path` is one this extension may read: under [`READ_PATHS`], one path with nothing
/// that could step out of it or start another line.
pub fn readable(path: &str) -> bool {
    READ_PATHS.iter().any(|prefix| path.starts_with(prefix))
        && !path.split(['/', '?']).any(|part| part == "..")
        // Nothing encoded: an encoded `/` or `.` could step out after all.
        && !path.contains('%')
        && path.bytes().all(|b| b.is_ascii_graphic())
}

/// `GET {base}{path}` with `token`, the body at most `limit` bytes, all within `within`. Refused,
/// with nothing sent, unless the address is [`readable`].
pub async fn get(
    transport: &Transport,
    path: &str,
    token: &str,
    limit: usize,
    within: Duration,
) -> Result<Answer, String> {
    if !readable(path) {
        return Err(format!("`{path}` isn't an address this extension reads"));
    }
    tokio::time::timeout(within, fetch(transport, path, token, limit))
        .await
        .map_err(|_| "Home Assistant didn't answer in time".to_owned())?
}

async fn fetch(
    transport: &Transport,
    path: &str,
    token: &str,
    limit: usize,
) -> Result<Answer, String> {
    let authority = transport.authority();
    let mut stream = transport.open().await.map_err(|why| why.to_string())?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\n\
         Accept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|why| format!("couldn't ask Home Assistant: {why}"))?;

    // The head, then whatever of the body came with it.
    let mut received = Vec::new();
    let head_end = loop {
        if let Some(at) = find(&received, b"\r\n\r\n") {
            break at;
        }
        if received.len() > 64 * 1024 {
            return Err("Home Assistant's answer has no end to its headers".into());
        }
        if read_some(&mut stream, &mut received).await? == 0 {
            return Err("Home Assistant closed the connection without answering".into());
        }
    };
    let head = String::from_utf8_lossy(&received[..head_end]).into_owned();
    let mut body = received.split_off(head_end + 4);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("Home Assistant's answer isn't HTTP")?;
    let mut content_type = None;
    let mut length = None;
    let mut chunked = false;
    let mut gzipped = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-type" => {
                content_type = value
                    .split(';')
                    .next()
                    .map(|t| t.trim().to_ascii_lowercase());
            }
            "content-length" => length = value.parse::<usize>().ok(),
            "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
            "content-encoding" => gzipped = value.eq_ignore_ascii_case("gzip"),
            _ => {}
        }
    }
    match status {
        200 => {}
        401 | 403 => return Err("Home Assistant refused the token for this".into()),
        404 => return Err("Home Assistant has nothing at that address".into()),
        other => return Err(format!("Home Assistant answered {other}")),
    }
    if length.is_some_and(|length| length > limit) {
        return Err(format!(
            "the picture is bigger than {} MB",
            limit / 1_000_000
        ));
    }

    // The rest of the body: as long as it said, or until the connection closes.
    let most = if chunked { limit + 64 * 1024 } else { limit };
    while length.is_none_or(|length| body.len() < length) {
        if body.len() > most {
            return Err(format!(
                "the picture is bigger than {} MB",
                limit / 1_000_000
            ));
        }
        if read_some(&mut stream, &mut body).await? == 0 {
            break;
        }
    }
    if let Some(length) = length {
        if body.len() < length {
            return Err("Home Assistant's answer was cut short".into());
        }
        body.truncate(length);
    }
    if chunked {
        body = unchunk(&body)?;
    }
    if gzipped {
        body = gunzip(&body, limit)?;
    }
    if body.len() > limit {
        return Err(format!(
            "the picture is bigger than {} MB",
            limit / 1_000_000
        ));
    }
    Ok(Answer {
        content_type: content_type.unwrap_or_default(),
        body,
    })
}

async fn read_some(stream: &mut Stream, into: &mut Vec<u8>) -> Result<usize, String> {
    let mut buffer = [0u8; 16 * 1024];
    let read = match stream.read(&mut buffer).await {
        Ok(read) => read,
        // A TLS peer closing without saying so. A body with a length is still checked for it.
        Err(why) if why.kind() == std::io::ErrorKind::UnexpectedEof => 0,
        Err(why) => return Err(format!("lost Home Assistant's answer: {why}")),
    };
    into.extend_from_slice(&buffer[..read]);
    Ok(read)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A gzipped body, at most `limit` bytes once unzipped (one more says it's too big).
fn gunzip(body: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(body)
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| "Home Assistant's answer isn't readable".to_owned())?;
    Ok(out)
}

/// A `Transfer-Encoding: chunked` body, put back together.
fn unchunk(mut raw: &[u8]) -> Result<Vec<u8>, String> {
    let cut = || "Home Assistant's answer was cut short".to_owned();
    let mut out = Vec::new();
    loop {
        let line_end = find(raw, b"\r\n").ok_or_else(cut)?;
        let size_text = String::from_utf8_lossy(&raw[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "Home Assistant's answer isn't readable".to_owned())?;
        raw = &raw[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if raw.len() < size + 2 {
            return Err(cut());
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A server that answers one request with `answer`, and hands back what it was asked.
    async fn serve(answer: Vec<u8>) -> (Transport, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let asked = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("a connection");
            let mut request = Vec::new();
            while find(&request, b"\r\n\r\n").is_none() {
                let mut buffer = [0u8; 1024];
                let read = stream.read(&mut buffer).await.expect("the request");
                request.extend_from_slice(&buffer[..read]);
            }
            stream.write_all(&answer).await.expect("the answer");
            String::from_utf8_lossy(&request).into_owned()
        });
        let settings: crate::settings::Settings =
            serde_json::from_value(serde_json::json!({ "url": base, "token": "t" }))
                .expect("settings");
        (Transport::of(&settings).expect("a transport"), asked)
    }

    #[tokio::test]
    async fn a_picture_is_fetched_with_the_token() {
        let (base, asked) = serve(
            b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg; charset=binary\r\nContent-Length: 4\r\n\r\nJPEG"
                .to_vec(),
        )
        .await;
        let answer = get(
            &base,
            "/api/camera_proxy/camera.front",
            "tok",
            1000,
            Duration::from_secs(5),
        )
        .await
        .expect("a picture");
        assert_eq!(
            answer,
            Answer {
                content_type: "image/jpeg".into(),
                body: b"JPEG".to_vec()
            }
        );
        let request = asked.await.expect("asked");
        assert!(request.starts_with("GET /api/camera_proxy/camera.front HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: Bearer tok\r\n"));
    }

    #[tokio::test]
    async fn a_chunked_answer_is_put_back_together() {
        let (base, _) = serve(
            b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nPN\r\n1\r\nG\r\n0\r\n\r\n"
                .to_vec(),
        )
        .await;
        let answer = get(&base, "/api/camera_proxy/camera.x", "t", 1000, Duration::from_secs(5))
            .await
            .expect("a picture");
        assert_eq!(answer.body, b"PNG");
    }

    #[tokio::test]
    async fn a_gzipped_answer_is_unzipped() {
        use std::io::Write as _;
        let mut zipped = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        zipped.write_all(b"#EXTM3U\n").expect("zip");
        let zipped = zipped.finish().expect("zipped");
        let mut answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            zipped.len()
        )
        .into_bytes();
        answer.extend_from_slice(&zipped);
        let (base, _) = serve(answer).await;
        let answer = get(&base, "/api/camera_proxy/camera.x", "t", 1000, Duration::from_secs(5))
            .await
            .expect("a playlist");
        assert_eq!(answer.body, b"#EXTM3U\n");
    }

    /// Rule 5: only reads leave. Every request is a `GET`, and only to an address that reads.
    #[tokio::test]
    async fn only_reads_are_sent() {
        for path in [
            "/api/camera_proxy/camera.front",
            "/api/image_proxy/image.door",
            "/api/hls/abc/master_playlist.m3u8",
            "/api/hls/abc/playlist.m3u8?_HLS_msn=3",
            "/api/config/automation/config/1700000000000",
        ] {
            assert!(readable(path), "{path}");
        }
        for path in [
            "/api/services/light/turn_on",
            "/api/config/automation/config",
            "/api/config/automation/config/../../services/light/turn_on",
            "/api/config/automation/config/..",
            "/api/config/automation/config/x%2F..%2Fy",
            "/api/config/automation/config/a b",
            "/api/config/automation/config/a\r\nX: y",
            "/api/config/script/config/x",
            "/api/events/x",
            "/",
        ] {
            assert!(!readable(path), "{path}");
        }

        // Refused before anything is sent: the server never hears from it.
        let (base, asked) =
            serve(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
        let why = get(&base, "/api/services/light/turn_on", "t", 1000, Duration::from_secs(5))
            .await
            .expect_err("refused");
        assert!(why.contains("isn't an address"), "{why}");
        asked.abort();

        // What does leave is a GET.
        let (base, asked) =
            serve(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
        get(&base, "/api/config/automation/config/17", "t", 1000, Duration::from_secs(5))
            .await
            .expect("an answer");
        let request = asked.await.expect("asked");
        assert!(
            request.starts_with("GET /api/config/automation/config/17 HTTP/1.1\r\n"),
            "{request}"
        );
        assert_eq!(request.matches("\r\n\r\n").count(), 1, "one request, no body");
    }

    #[tokio::test]
    async fn refusals_and_oversized_pictures_are_said() {
        let (base, _) =
            serve(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_vec()).await;
        let why = get(&base, "/api/camera_proxy/camera.x", "t", 1000, Duration::from_secs(5))
            .await
            .expect_err("refused");
        assert!(why.contains("token"), "{why}");

        let (base, _) = serve(
            b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 5000\r\n\r\n".to_vec(),
        )
        .await;
        let why = get(&base, "/api/camera_proxy/camera.x", "t", 1000, Duration::from_secs(5))
            .await
            .expect_err("too big");
        assert!(why.contains("bigger"), "{why}");
    }
}
