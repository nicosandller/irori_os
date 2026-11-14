//! Every camera's live video goes through this pass-through on loopback. The video library
//! answers whichever login a camera names first, Basic included, and Basic hands over the
//! password as it is. So everything the camera sends is read on the way through, for as long as
//! the stream lasts: offers of Basic are left out of every answer, so the library only ever sees
//! Digest; a camera that offers nothing else is hung up on; and anything that can't be read as
//! RTSP or video ends the connection rather than being passed on unread.
//!
//! What the library sends goes to the camera as it is. Its requests name the relay's address,
//! so the Digest login is worked out over that address, and the camera checks it against the
//! same.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// How long the library has to connect to the relay, and the relay to the camera.
const CONNECT_WITHIN: Duration = Duration::from_secs(10);
/// The longest a message's head can be.
const MAX_HEAD: usize = 64 * 1024;
/// The longest a message's body can be (a stream's description).
const MAX_BODY: usize = 256 * 1024;

/// A running relay. Dropping it closes both sides.
pub struct Relay {
    /// Where the library connects to.
    pub here: SocketAddr,
    task: JoinHandle<()>,
    why: Arc<Mutex<Option<String>>>,
}

impl Relay {
    /// Why the relay hung up on the camera, if it did.
    pub fn why(&self) -> Option<String> {
        self.why.lock().ok().and_then(|why| why.clone())
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A relay to `camera`, taking one connection.
pub async fn start(camera: SocketAddr) -> Result<Relay, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("the camera's video couldn't be relayed: {error}"))?;
    let here = listener
        .local_addr()
        .map_err(|error| format!("the camera's video couldn't be relayed: {error}"))?;
    let why = Arc::new(Mutex::new(None));
    let said = Arc::clone(&why);
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
        let down = filtered(&mut from_camera, &mut to_library);
        // Either side ending ends both.
        tokio::select! {
            _ = up => {}
            ended = down => {
                if let Err(Hangup(reason)) = ended
                    && let Ok(mut why) = said.lock()
                {
                    *why = Some(reason);
                }
            }
        }
    });
    Ok(Relay { here, task, why })
}

/// Why the relay hung up on the camera.
#[derive(Debug, PartialEq, Eq)]
struct Hangup(String);

/// Passes everything the camera sends on, read as RTSP: messages with their Basic offers left
/// out, and video frames as they are. Ends when either side does (`Ok`), or when the camera
/// sends what can't be passed on (`Err`).
async fn filtered(
    from_camera: &mut (impl tokio::io::AsyncRead + Unpin),
    to_library: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), Hangup> {
    let mut read: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut at = 0;
        loop {
            match next(&read[at..]) {
                Next::Frame(length) => {
                    out.extend_from_slice(&read[at..at + length]);
                    at += length;
                }
                Next::Message { head, body } => {
                    let Some(kept) = without_basic(&read[at..at + head]) else {
                        return Err(Hangup(
                            "the camera's video only takes the kind of login that sends the \
                             password as it is (Basic), so Irori won't send it"
                                .into(),
                        ));
                    };
                    out.extend_from_slice(&kept);
                    out.extend_from_slice(&read[at + head..at + head + body]);
                    at += head + body;
                }
                Next::More => break,
                Next::Unreadable => {
                    return Err(Hangup(
                        "the camera sent something that isn't RTSP, so its video was closed".into(),
                    ));
                }
            }
        }
        read.drain(..at);
        if !out.is_empty() {
            if to_library.write_all(&out).await.is_err() {
                return Ok(());
            }
            out.clear();
        }
        match from_camera.read(&mut chunk).await {
            Ok(0) | Err(_) => return Ok(()),
            Ok(n) => read.extend_from_slice(&chunk[..n]),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// A whole video frame (`$`, channel, length): its length with its four bytes in front.
    Frame(usize),
    /// A whole message: its head's length (with the blank line), and its body's.
    Message {
        head: usize,
        body: usize,
    },
    More,
    Unreadable,
}

/// What `read` starts with.
fn next(read: &[u8]) -> Next {
    let Some(&first) = read.first() else {
        return Next::More;
    };
    if first == b'$' {
        let Some(length) = read.get(2..4) else {
            return Next::More;
        };
        let length = 4 + usize::from(u16::from_be_bytes([length[0], length[1]]));
        return if read.len() < length {
            Next::More
        } else {
            Next::Frame(length)
        };
    }
    // An answer (`RTSP/1.0 200 OK`) or a request the camera makes (`GET_PARAMETER … RTSP/1.0`):
    // either way, capital letters first.
    if !first.is_ascii_uppercase() {
        return Next::Unreadable;
    }
    let Some(end) = read.windows(4).position(|window| window == b"\r\n\r\n") else {
        return if read.len() > MAX_HEAD {
            Next::Unreadable
        } else {
            Next::More
        };
    };
    if end > MAX_HEAD {
        return Next::Unreadable;
    }
    let head = end + 4;
    let Ok(text) = std::str::from_utf8(&read[..end]) else {
        return Next::Unreadable;
    };
    let mut lines = text.split("\r\n");
    let first_line = lines.next().unwrap_or_default();
    if !(first_line.starts_with("RTSP/1.") || first_line.ends_with(" RTSP/1.0")) {
        return Next::Unreadable;
    }
    let mut body = 0;
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        if name.trim().eq_ignore_ascii_case("content-length") {
            match value.trim().parse::<usize>() {
                Ok(length) if length <= MAX_BODY => body = length,
                _ => return Next::Unreadable,
            }
        }
    }
    if read.len() < head + body {
        return Next::More;
    }
    Next::Message { head, body }
}

/// A message's head with any offer of Basic left out. `None` when Basic was all it offered.
fn without_basic(head: &[u8]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(head);
    let mut out = String::with_capacity(text.len());
    let (mut left_out, mut kept) = (false, false);
    for line in text.split_inclusive("\r\n") {
        let offer = line.split_once(':').filter(|(name, _)| {
            let name = name.trim();
            name.eq_ignore_ascii_case("www-authenticate")
                || name.eq_ignore_ascii_case("proxy-authenticate")
        });
        match offer {
            Some((_, value)) if is_basic(value) => left_out = true,
            Some(_) => {
                kept = true;
                out.push_str(line);
            }
            None => out.push_str(line),
        }
    }
    if left_out && !kept {
        return None;
    }
    Some(out.into_bytes())
}

/// Whether a `WWW-Authenticate` value offers Basic. One header may offer several logins; any
/// mention of Basic as a scheme counts, rather than trying to split them.
fn is_basic(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|word| word == "basic")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHALLENGE: &str = "RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\n\
        WWW-Authenticate: Basic realm=\"cam\"\r\n\
        WWW-Authenticate: Digest realm=\"cam\", nonce=\"n\"\r\n\r\n";

    #[test]
    fn basic_is_left_out_and_digest_kept() {
        let out = String::from_utf8(without_basic(CHALLENGE.as_bytes()).expect("Digest is left"))
            .expect("text");
        assert!(!out.contains("Basic"), "{out}");
        assert!(out.contains("WWW-Authenticate: Digest realm=\"cam\""));
        assert!(out.starts_with("RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\n"));
        assert!(out.ends_with("\r\n\r\n"));
        let only_basic =
            b"RTSP/1.0 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\n\r\n";
        assert_eq!(without_basic(only_basic), None, "nothing else to answer");
        let both_in_one = b"RTSP/1.0 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"x\", nonce=\"n\", Basic realm=\"x\"\r\n\r\n";
        assert_eq!(
            without_basic(both_in_one),
            None,
            "a header naming Basic anywhere is left out"
        );
        let plain = b"RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n";
        assert_eq!(without_basic(plain).as_deref(), Some(&plain[..]));
    }

    #[test]
    fn what_the_camera_sends_is_told_apart() {
        assert_eq!(next(b""), Next::More);
        assert_eq!(next(b"RTSP/1.0 200 OK\r\nCSeq"), Next::More);
        assert_eq!(next(b"$\x00"), Next::More);
        assert_eq!(next(b"$\x00\x00\x02a"), Next::More);
        assert_eq!(next(b"$\x00\x00\x02ab$"), Next::Frame(6));
        let described = b"RTSP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nv=0";
        assert_eq!(
            next(described),
            Next::Message {
                head: described.len() - 3,
                body: 3
            }
        );
        assert_eq!(next(&described[..described.len() - 1]), Next::More);
        let asked = b"GET_PARAMETER rtsp://x/ RTSP/1.0\r\nCSeq: 9\r\n\r\n";
        assert_eq!(
            next(asked),
            Next::Message {
                head: asked.len(),
                body: 0
            },
            "a request from the camera"
        );
        assert_eq!(next(b"hello\r\n\r\n"), Next::Unreadable);
        assert_eq!(next(b"HTTP/1.1 200 OK\r\n\r\n"), Next::Unreadable);
        assert_eq!(
            next(b"RTSP/1.0 200 OK\r\nContent-Length: 999999999\r\n\r\n"),
            Next::Unreadable
        );
        assert_eq!(
            next(&[b'A'; MAX_HEAD + 1]),
            Next::Unreadable,
            "a head that never ends"
        );
    }

    async fn read_until(stream: &mut TcpStream, want: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        while got.len() < want {
            let n = stream.read(&mut buf).await.expect("a read");
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        got
    }

    /// Basic is left out of every login request, before and after video has started.
    #[tokio::test]
    async fn basic_never_reaches_the_library() {
        let camera = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
        let at = camera.local_addr().expect("an address");
        let late = "RTSP/1.0 401 Unauthorized\r\nCSeq: 3\r\n\
            WWW-Authenticate: Basic realm=\"cam\"\r\n\
            WWW-Authenticate: Digest realm=\"cam\", nonce=\"m\"\r\n\r\n";
        tokio::spawn(async move {
            let (mut stream, _) = camera.accept().await.expect("the relay connects");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            stream.write_all(CHALLENGE.as_bytes()).await.expect("sent");
            let _ = stream.read(&mut buf).await;
            stream
                .write_all(b"RTSP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nv=0$\x00\x00\x02ab")
                .await
                .expect("sent");
            // Logged in, then asked again, with Basic first.
            stream.write_all(late.as_bytes()).await.expect("sent");
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let relay = start(at).await.expect("a relay");
        let mut library = TcpStream::connect(relay.here).await.expect("connects");
        library
            .write_all(b"DESCRIBE rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n")
            .await
            .expect("sent");
        let first = String::from_utf8_lossy(&read_until(&mut library, 60).await).into_owned();
        assert!(!first.contains("Basic"), "{first}");
        assert!(first.contains("Digest"), "{first}");
        library
            .write_all(b"DESCRIBE rtsp://x/ RTSP/1.0\r\nCSeq: 2\r\nAuthorization: Digest x\r\n\r\n")
            .await
            .expect("sent");
        let video = b"RTSP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nv=0$\x00\x00\x02ab";
        let want = video.len() + late.len() - "WWW-Authenticate: Basic realm=\"cam\"\r\n".len();
        let rest = read_until(&mut library, want).await;
        let rest_text = String::from_utf8_lossy(&rest).into_owned();
        assert!(rest.starts_with(video), "{rest_text:?}");
        assert!(!rest_text.contains("Basic"), "{rest_text:?}");
        assert!(rest_text.contains("nonce=\"m\""), "{rest_text:?}");
        assert_eq!(relay.why(), None);
    }

    /// A camera that only offers Basic, or sends what isn't RTSP, is hung up on, and says why.
    #[tokio::test]
    async fn what_cant_be_passed_on_ends_the_connection() {
        let cases: [(&'static [u8], &str); 2] = [
            (
                b"RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\nWWW-Authenticate: Basic realm=\"cam\"\r\n\r\n",
                "Basic",
            ),
            (b"\x00\x01garbage", "isn't RTSP"),
        ];
        for (sent, why) in cases {
            let camera = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
            let at = camera.local_addr().expect("an address");
            tokio::spawn(async move {
                let (mut stream, _) = camera.accept().await.expect("the relay connects");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(sent).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            });
            let relay = start(at).await.expect("a relay");
            let mut library = TcpStream::connect(relay.here).await.expect("connects");
            library
                .write_all(b"DESCRIBE rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\n\r\n")
                .await
                .expect("sent");
            let got = read_until(&mut library, 1).await;
            assert!(got.is_empty(), "nothing passed on: {got:?}");
            let said = relay.why().expect("a reason");
            assert!(said.contains(why), "{said}");
        }
    }
}
