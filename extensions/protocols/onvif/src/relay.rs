//! A pass-through on loopback for a camera whose video names the Basic login before Digest (a
//! Tapo C210 does). The video library answers the first login a camera names, and Basic hands
//! over the password as it is, so the camera's answers have their Basic offers left out on the
//! way through until it has let the library in. From then on the bytes go both ways untouched.
//!
//! The library's requests go to the camera as they are, so they name the relay's address: the
//! login's Digest is worked out over that address, and the camera checks it against the same.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// How long the library has to connect to the relay.
const CONNECT_WITHIN: Duration = Duration::from_secs(10);
/// The longest an answer's head can be.
const MAX_HEAD: usize = 64 * 1024;
/// The longest an answer's body can be (a stream's description).
const MAX_BODY: usize = 256 * 1024;

/// A relay to `camera`, taking one connection. Its address on loopback, and the task, which
/// ends when either side hangs up; aborting it closes both.
pub async fn start(camera: SocketAddr) -> Result<(SocketAddr, JoinHandle<()>), String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("the camera's video couldn't be relayed: {error}"))?;
    let here = listener
        .local_addr()
        .map_err(|error| format!("the camera's video couldn't be relayed: {error}"))?;
    let task = tokio::spawn(async move {
        let Ok(Ok((library, _))) = tokio::time::timeout(CONNECT_WITHIN, listener.accept()).await
        else {
            return;
        };
        drop(listener);
        let Ok(Ok(camera)) = tokio::time::timeout(CONNECT_WITHIN, TcpStream::connect(camera)).await
        else {
            return;
        };
        let _ = library.set_nodelay(true);
        let _ = camera.set_nodelay(true);
        let (mut from_library, mut to_library) = library.into_split();
        let (mut from_camera, mut to_camera) = camera.into_split();
        let up = async { tokio::io::copy(&mut from_library, &mut to_camera).await };
        let down = async {
            let rest = until_let_in(&mut from_camera, &mut to_library).await?;
            to_library.write_all(&rest).await?;
            tokio::io::copy(&mut from_camera, &mut to_library).await
        };
        // Either side ending ends both.
        tokio::select! {
            _ = up => {}
            _ = down => {}
        }
    });
    Ok((here, task))
}

/// Passes the camera's answers on with their Basic offers left out, up to and including the
/// first that isn't a request to log in. Returns what was read past it.
async fn until_let_in(
    from_camera: &mut (impl tokio::io::AsyncRead + Unpin),
    to_library: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> std::io::Result<Vec<u8>> {
    let mut read = Vec::new();
    loop {
        match next_answer(&read) {
            Next::Answer { head, body, status } => {
                let mut out = without_basic(&read[..head]);
                out.extend_from_slice(&read[head..head + body]);
                to_library.write_all(&out).await?;
                read.drain(..head + body);
                if status != 401 {
                    return Ok(read);
                }
            }
            // Anything that isn't an answer (video sent interleaved) means it's past logging in.
            Next::NotAnAnswer => return Ok(read),
            Next::More => {
                if read.len() > MAX_HEAD + MAX_BODY {
                    return Err(std::io::Error::other("the camera's answer was too long"));
                }
                let mut chunk = [0u8; 4096];
                let n = from_camera.read(&mut chunk).await?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                read.extend_from_slice(&chunk[..n]);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// A whole answer: its head's length (with the blank line), its body's, and its status.
    Answer {
        head: usize,
        body: usize,
        status: u16,
    },
    NotAnAnswer,
    More,
}

fn next_answer(read: &[u8]) -> Next {
    if read.is_empty() {
        return Next::More;
    }
    if !b"RTSP/".starts_with(&read[..read.len().min(5)]) {
        return Next::NotAnAnswer;
    }
    let Some(end) = read.windows(4).position(|window| window == b"\r\n\r\n") else {
        return if read.len() > MAX_HEAD {
            Next::NotAnAnswer
        } else {
            Next::More
        };
    };
    let head = end + 4;
    let text = String::from_utf8_lossy(&read[..end]);
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if body > MAX_BODY {
        return Next::NotAnAnswer;
    }
    if read.len() < head + body {
        return Next::More;
    }
    Next::Answer { head, body, status }
}

/// An answer's head with any `WWW-Authenticate: Basic …` line left out.
fn without_basic(head: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(head);
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive("\r\n") {
        let basic = line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("www-authenticate")
                && value
                    .trim_start()
                    .get(..5)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("basic"))
        });
        if !basic {
            out.push_str(line);
        }
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHALLENGE: &str = "RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\n\
        WWW-Authenticate: Basic realm=\"cam\"\r\n\
        WWW-Authenticate: Digest realm=\"cam\", nonce=\"n\"\r\n\r\n";

    #[test]
    fn basic_is_left_out_of_a_login_request_and_digest_kept() {
        let out = String::from_utf8(without_basic(CHALLENGE.as_bytes())).expect("text");
        assert!(!out.contains("Basic"), "{out}");
        assert!(out.contains("WWW-Authenticate: Digest realm=\"cam\""));
        assert!(out.starts_with("RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\n"));
        assert!(out.ends_with("\r\n\r\n"));
    }

    #[test]
    fn answers_are_told_apart_by_their_length() {
        assert_eq!(next_answer(b""), Next::More);
        assert_eq!(next_answer(b"RTSP/1.0 200 OK\r\nCSeq"), Next::More);
        assert_eq!(next_answer(b"$\x00\x00\x10"), Next::NotAnAnswer);
        let described = b"RTSP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nv=0";
        assert_eq!(
            next_answer(described),
            Next::Answer {
                head: described.len() - 3,
                body: 3,
                status: 200
            }
        );
        assert_eq!(next_answer(&described[..described.len() - 1]), Next::More);
    }

    /// The library sees Digest only, logs in, and everything after goes through as it is.
    #[tokio::test]
    async fn a_camera_naming_basic_first_is_answered_with_digest() {
        let camera = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
        let at = camera.local_addr().expect("an address");
        tokio::spawn(async move {
            let (mut stream, _) = camera.accept().await.expect("the relay connects");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            stream.write_all(CHALLENGE.as_bytes()).await.expect("sent");
            let n = stream.read(&mut buf).await.expect("the retry");
            assert!(String::from_utf8_lossy(&buf[..n]).contains("Authorization: Digest"));
            stream
                .write_all(b"RTSP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nv=0$\x00\x00\x02ab")
                .await
                .expect("sent");
            // Afterwards, a Basic line passes untouched: it's only data by then.
            stream
                .write_all(b"WWW-Authenticate: Basic x\r\n")
                .await
                .expect("sent");
        });
        let (relay, _task) = start(at).await.expect("a relay");
        let mut library = TcpStream::connect(relay).await.expect("connects");
        library
            .write_all(b"DESCRIBE rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n")
            .await
            .expect("sent");
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = library.read(&mut buf).await.expect("an answer");
            got.extend_from_slice(&buf[..n]);
        }
        let challenge = String::from_utf8_lossy(&got).into_owned();
        assert!(!challenge.contains("Basic"), "{challenge}");
        assert!(challenge.contains("Digest"), "{challenge:?}");
        library
            .write_all(b"DESCRIBE rtsp://x/ RTSP/1.0\r\nCSeq: 2\r\nAuthorization: Digest x\r\n\r\n")
            .await
            .expect("sent");
        let want: &[u8] =
            b"RTSP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nv=0$\x00\x00\x02abWWW-Authenticate: Basic x\r\n";
        let mut got = Vec::new();
        while got.len() < want.len() {
            let n = library.read(&mut buf).await.expect("the rest");
            assert!(n > 0, "hung up early: {:?}", String::from_utf8_lossy(&got));
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, want);
    }
}
