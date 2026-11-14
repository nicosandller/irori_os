//! Talking through a camera (`docs/specs/protocols.md` §5.4): ONVIF's audio backchannel.
//!
//! A camera with a speaker says so when its stream is asked for with
//! `Require: www.onvif.org/ver20/backchannel`: its description gains an audio track marked
//! `sendonly` (it sends nothing; it's sent to), in G.711, µ-law or A-law. Talking is that track
//! set up over the RTSP connection itself (interleaved, so no UDP port is needed), then
//! RTP packets of 20 ms each, until the page lets go.
//!
//! `retina`, which reads the video, only receives, so this is a small RTSP client of its own:
//! DESCRIBE, SETUP, PLAY, a keepalive, TEARDOWN. The login is RTSP's Digest, answered with
//! `http-auth`, the same as `retina` answers it.
//!
//! Asking whether a camera can be talked through never costs it its login: a 401 there, a 400,
//! or a description without the track all just mean it can't.

use std::net::IpAddr;
use std::time::Duration;

use irori_protocol::IncomingTalk;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use crate::settings::Login;

const REQUIRE: &str = "www.onvif.org/ver20/backchannel";

/// How long each step may take.
const STEP: Duration = Duration::from_secs(5);

/// How long the camera's speaker stays open with nothing to play.
const QUIET: Duration = Duration::from_secs(3);

/// How long the speaker is fed silence after the last sound. A Reolink holds a few seconds
/// before it plays, and plays only while sound keeps coming: stopping, or just going quiet on
/// the line, loses what it holds, and a talk under about 3 s was all lost. The silence pushes
/// the voice out; it's the silence that's lost.
#[cfg(not(test))]
const TAIL: Duration = Duration::from_millis(3000);
#[cfg(test)]
const TAIL: Duration = Duration::from_millis(50);

/// How often the session is kept alive while talking: well inside the usual 60 s timeout.
const KEEPALIVE: Duration = Duration::from_secs(25);

/// The most an answer to a request may be: a description is a kilobyte or two.
const MAX_ANSWER: usize = 64 * 1024;

/// Samples in one RTP packet: 20 ms at 8 kHz.
const PACKET: usize = 160;

/// How a camera's speaker is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TalkTrack {
    /// The address its stream was described at, sent to the camera's IP, with no login in it.
    pub stream: String,
    /// The speaker's track, as the address to SETUP.
    pub track: String,
    /// Where PLAY and TEARDOWN go: the description's base.
    pub base: String,
    pub law: Law,
}

/// Which G.711 the speaker takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Law {
    /// µ-law, RTP payload type 0 (`PCMU`).
    Mu,
    /// A-law, RTP payload type 8 (`PCMA`).
    A,
}

impl Law {
    fn payload_type(self) -> u8 {
        match self {
            Self::Mu => 0,
            Self::A => 8,
        }
    }

    fn encode(self, sample: i16) -> u8 {
        match self {
            Self::Mu => ulaw(sample),
            Self::A => alaw(sample),
        }
    }
}

/// Whether the camera has a speaker, from one description of its main stream. `None` for any
/// answer that isn't a description with a speaker's track in it.
pub async fn ask(stream: &str, ip: IpAddr, login: &Login) -> Option<TalkTrack> {
    let asked = async {
        let url = crate::live::address(stream, ip)?;
        let mut rtsp = Rtsp::open(&url, login).await?;
        let described = rtsp.request("DESCRIBE", url.as_str(), &[("Accept", "application/sdp")]);
        let described = described.await?;
        if described.status != 200 {
            return Err(format!("it answered {}", described.status));
        }
        let base = described
            .header("content-base")
            .unwrap_or(url.as_str())
            .to_owned();
        let (control, law) =
            speaker(&described.body).ok_or_else(|| "it has no speaker's track".to_owned())?;
        Ok::<_, String>(TalkTrack {
            stream: url.to_string(),
            track: join(&base, &control),
            base,
            law,
        })
    };
    match tokio::time::timeout(STEP * 2, asked).await {
        Ok(Ok(track)) => {
            tracing::info!(%ip, law = ?track.law, "a camera can be talked through");
            Some(track)
        }
        Ok(Err(why)) => {
            tracing::info!(%ip, "a camera can't be talked through: {why}");
            None
        }
        Err(_) => {
            tracing::info!(%ip, "a camera can't be talked through: it took too long to say");
            None
        }
    }
}

/// How a talk ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The page let go, or went quiet.
    Stopped,
    /// The camera turned its login down.
    Refused,
    Failed(String),
}

/// Opens the camera's speaker, plays what the page sends until it stops, and closes it.
pub async fn run(track: &TalkTrack, ip: IpAddr, login: &Login, talk: IncomingTalk) -> Ended {
    let IncomingTalk {
        mut audio, line, ..
    } = talk;
    // One session at a time on a camera's speaker: a quick second press waits for the first
    // one's tail rather than asking a camera that's still busy.
    let speaker = speaker_of(ip);
    let _turn = speaker.lock().await;
    let ended = speak(track, ip, login, &mut audio, &line).await;
    match &ended {
        Ended::Stopped => line.end(None),
        Ended::Refused => line.end(Some("the camera refused its login".into())),
        Ended::Failed(why) => line.end(Some(why.clone())),
    }
    ended
}

async fn speak(
    track: &TalkTrack,
    ip: IpAddr,
    login: &Login,
    audio: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    line: &irori_protocol::TalkLine,
) -> Ended {
    let url = match crate::live::address(&track.stream, ip) {
        Ok(url) => url,
        Err(why) => return Ended::Failed(why),
    };
    let opened = async {
        let mut rtsp = Rtsp::open(&url, login).await?;
        // Some cameras only set a track up after describing it on the same connection.
        let described = rtsp
            .request("DESCRIBE", url.as_str(), &[("Accept", "application/sdp")])
            .await?;
        expect(&described, "describe")?;
        let setup = rtsp
            .request(
                "SETUP",
                &track.track,
                &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")],
            )
            .await?;
        expect(&setup, "set the speaker up")?;
        let session = setup
            .header("session")
            .and_then(|session| session.split(';').next())
            .map(str::trim)
            .filter(|session| !session.is_empty())
            .ok_or_else(|| "the camera gave no session".to_owned())?
            .to_owned();
        let play = rtsp
            .request(
                "PLAY",
                &track.base,
                &[("Session", &session), ("Range", "npt=0.000-")],
            )
            .await?;
        expect(&play, "open the speaker")?;
        Ok::<_, String>((rtsp, session))
    };
    let (mut rtsp, session) = match opened.await {
        Ok(opened) => opened,
        Err(why) if why == REFUSED => return Ended::Refused,
        Err(why) => return Ended::Failed(why),
    };
    line.ready();
    tracing::info!(%ip, "talking through a camera");

    let mut packets = Packets::new(track.law);
    let mut keepalive =
        tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
    let mut buffer = vec![0u8; 4096];
    // What was sent, for the log: how much, and how loud at its loudest.
    let (mut sent, mut loudest) = (0usize, 0u16);
    let ended = loop {
        tokio::select! {
            chunk = tokio::time::timeout(QUIET, audio.recv()) => match chunk {
                Ok(Some(chunk)) => {
                    sent += chunk.len() / 2;
                    loudest = irori_protocol::talk::samples(&chunk)
                        .map(i16::unsigned_abs)
                        .fold(loudest, u16::max);
                    let mut gone = false;
                    for packet in packets.push(&chunk) {
                        if rtsp.write(&packet).await.is_err() {
                            gone = true;
                            break;
                        }
                    }
                    if gone {
                        break Ended::Failed("the camera stopped listening".into());
                    }
                }
                Ok(None) | Err(_) => break Ended::Stopped,
            },
            _ = keepalive.tick() => {
                if rtsp.send("GET_PARAMETER", &track.base, &[("Session", &session)]).await.is_err() {
                    break Ended::Failed("the camera stopped listening".into());
                }
            }
            // What the camera sends meanwhile (its reports, keepalive answers) is read and let
            // go, so its side of the connection never fills up.
            read = rtsp.stream.read(&mut buffer) => match read {
                Ok(0) | Err(_) => break Ended::Failed("the camera hung up".into()),
                Ok(_) => {}
            },
        }
    };
    if ended == Ended::Stopped && sent > 0 {
        // Silence, a packet each 20 ms as speech would come, so the camera plays out what it
        // holds; what it says meanwhile is let go.
        let silence = vec![0u8; PACKET * 2];
        let mut pace = tokio::time::interval(Duration::from_millis(20));
        let tail = tokio::time::sleep(TAIL);
        tokio::pin!(tail);
        'tail: loop {
            tokio::select! {
                () = &mut tail => break,
                _ = pace.tick() => {
                    for packet in packets.push(&silence) {
                        if rtsp.write(&packet).await.is_err() {
                            break 'tail;
                        }
                    }
                }
                read = rtsp.stream.read(&mut buffer) => if matches!(read, Ok(0) | Err(_)) { break },
            }
        }
    }
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        rtsp.send("TEARDOWN", &track.base, &[("Session", &session)]),
    )
    .await;
    tracing::info!(
        %ip,
        seconds = sent as f64 / 8000.0,
        loudest = format!("{:.0}%", f64::from(loudest) * 100.0 / 32768.0),
        "stopped talking through a camera"
    );
    ended
}

/// Each camera's speaker, to take turns on.
fn speaker_of(ip: IpAddr) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static SPEAKERS: std::sync::LazyLock<
        std::sync::Mutex<
            std::collections::BTreeMap<IpAddr, std::sync::Arc<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::LazyLock::new(Default::default);
    let mut speakers = SPEAKERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    speakers.entry(ip).or_default().clone()
}

/// A refused login, as [`Rtsp::request`] says it.
const REFUSED: &str = "the camera refused its login";

fn expect(answer: &Answer, step: &str) -> Result<(), String> {
    match answer.status {
        200..=299 => Ok(()),
        401 => Err(REFUSED.to_owned()),
        status => Err(format!("the camera wouldn't {step} (it answered {status})")),
    }
}

/// The speaker's track in a description: its control and which law it takes.
fn speaker(sdp: &str) -> Option<(String, Law)> {
    for media in sdp.split("\nm=").skip(1) {
        let mut lines = media.lines().map(str::trim);
        let first = lines.next()?;
        if !first.starts_with("audio") {
            continue;
        }
        let attributes: Vec<&str> = media.lines().map(str::trim).collect();
        if !attributes.contains(&"a=sendonly") {
            continue;
        }
        let formats: Vec<&str> = first.split_whitespace().skip(3).collect();
        let named = |format: &str| {
            attributes.iter().find_map(|line| {
                let rest = line.strip_prefix("a=rtpmap:")?;
                let (number, name) = rest.split_once(' ')?;
                (number == format).then(|| name.to_ascii_uppercase())
            })
        };
        let law = formats
            .iter()
            .find_map(|format| match (*format, named(format).as_deref()) {
                (_, Some(name)) if name.starts_with("PCMU/8000") => Some(Law::Mu),
                (_, Some(name)) if name.starts_with("PCMA/8000") => Some(Law::A),
                ("0", None) => Some(Law::Mu),
                ("8", None) => Some(Law::A),
                _ => None,
            })?;
        let control = attributes
            .iter()
            .find_map(|line| line.strip_prefix("a=control:"))?
            .to_owned();
        return Some((control, law));
    }
    None
}

/// A track's address: as it is when absolute, else after the base.
fn join(base: &str, control: &str) -> String {
    if control.contains("://") {
        return control.to_owned();
    }
    if base.ends_with('/') {
        format!("{base}{control}")
    } else {
        format!("{base}/{control}")
    }
}

/// An answer to a request.
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// One RTSP connection to a camera, logged in once it has asked.
struct Rtsp {
    stream: TcpStream,
    login: Login,
    auth: Option<http_auth::PasswordClient>,
    cseq: u32,
    read: Vec<u8>,
}

impl Rtsp {
    async fn open(url: &url::Url, login: &Login) -> Result<Self, String> {
        let host = url.host_str().ok_or("the camera's address has no host")?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = url.port().unwrap_or(554);
        let stream = tokio::time::timeout(STEP, TcpStream::connect((host, port)))
            .await
            .map_err(|_| "the camera didn't answer".to_owned())?
            .map_err(|error| format!("the camera couldn't be reached: {error}"))?;
        let _ = stream.set_nodelay(true);
        Ok(Self {
            stream,
            login: login.clone(),
            auth: None,
            cseq: 0,
            read: Vec::new(),
        })
    }

    /// Sends a request and reads its answer, logging in first if the camera asks.
    async fn request(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> Result<Answer, String> {
        self.send(method, uri, headers).await?;
        let answer = self.answer().await?;
        if answer.status != 401 || self.auth.is_some() {
            return Ok(answer);
        }
        let challenge = answer
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("www-authenticate"))
            .map(|(_, value)| value.as_str())
            .find_map(|value| http_auth::PasswordClient::try_from(value).ok())
            // Basic sends the password as it is, to whoever asks: never answered.
            .filter(|client| !matches!(client, http_auth::PasswordClient::Basic(_)));
        let Some(challenge) = challenge else {
            return Ok(answer);
        };
        self.auth = Some(challenge);
        self.send(method, uri, headers).await?;
        self.answer().await
    }

    /// Sends a request without waiting for its answer.
    async fn send(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> Result<(), String> {
        self.cseq += 1;
        let mut text = format!(
            "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: Irori\r\nRequire: {REQUIRE}\r\n",
            self.cseq
        );
        for (key, value) in headers {
            text.push_str(&format!("{key}: {value}\r\n"));
        }
        if let Some(auth) = self.auth.as_mut() {
            let answer = auth
                .respond(&http_auth::PasswordParams {
                    username: &self.login.username,
                    password: &self.login.password,
                    uri,
                    method,
                    body: Some(&[]),
                })
                .map_err(|error| {
                    format!("the camera's login challenge can't be answered: {error}")
                })?;
            text.push_str(&format!("Authorization: {answer}\r\n"));
        }
        text.push_str("\r\n");
        self.write(text.as_bytes()).await
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let written = tokio::time::timeout(STEP, self.stream.write_all(bytes)).await;
        if !matches!(written, Ok(Ok(()))) {
            return Err("the camera stopped listening".into());
        }
        Ok(())
    }

    /// The next answer, skipping anything sent interleaved before it (a camera may send a
    /// report as soon as it plays).
    async fn answer(&mut self) -> Result<Answer, String> {
        let deadline = tokio::time::Instant::now() + STEP;
        loop {
            if let Some(answer) = take_answer(&mut self.read)? {
                return Ok(answer);
            }
            if self.read.len() > MAX_ANSWER {
                return Err("the camera's answer was too long".into());
            }
            let mut chunk = [0u8; 4096];
            let read = tokio::time::timeout_at(deadline, self.stream.read(&mut chunk))
                .await
                .map_err(|_| "the camera didn't answer".to_owned())?
                .map_err(|error| format!("the camera hung up: {error}"))?;
            if read == 0 {
                return Err("the camera hung up".into());
            }
            self.read.extend_from_slice(&chunk[..read]);
        }
    }
}

/// Takes one answer off the front of what's been read, if it's all there. Interleaved data
/// (`$`, channel, length) in front of it is dropped.
fn take_answer(read: &mut Vec<u8>) -> Result<Option<Answer>, String> {
    loop {
        if read.first() == Some(&b'$') {
            if read.len() < 4 {
                return Ok(None);
            }
            let length = usize::from(u16::from_be_bytes([read[2], read[3]]));
            if read.len() < 4 + length {
                return Ok(None);
            }
            read.drain(..4 + length);
            continue;
        }
        let Some(end) = read.windows(4).position(|window| window == b"\r\n\r\n") else {
            return Ok(None);
        };
        let head = String::from_utf8_lossy(&read[..end]).to_string();
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|line| line.strip_prefix("RTSP/1.0 "))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| "the camera's answer isn't RTSP".to_owned())?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                Some((key.trim().to_owned(), value.trim().to_owned()))
            })
            .collect();
        let length = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        if length > MAX_ANSWER {
            return Err("the camera's answer was too long".into());
        }
        if read.len() < end + 4 + length {
            return Ok(None);
        }
        let body = String::from_utf8_lossy(&read[end + 4..end + 4 + length]).to_string();
        read.drain(..end + 4 + length);
        return Ok(Some(Answer {
            status,
            headers,
            body,
        }));
    }
}

/// Turns the page's sound into RTP packets of 20 ms, interleaved on channel 0.
struct Packets {
    law: Law,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    first: bool,
    /// Encoded samples waiting to make up a whole packet.
    waiting: Vec<u8>,
}

impl Packets {
    fn new(law: Law) -> Self {
        let mut random = [0u8; 6];
        // A failed draw leaves zeros, which are as good a start as any.
        let _ = getrandom::fill(&mut random);
        Self {
            law,
            ssrc: u32::from_be_bytes([random[0], random[1], random[2], random[3]]),
            sequence: u16::from_be_bytes([random[4], random[5]]),
            timestamp: 0,
            first: true,
            waiting: Vec::with_capacity(PACKET * 2),
        }
    }

    /// The packets a chunk of 16-bit little-endian PCM completes.
    fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.waiting
            .extend(irori_protocol::talk::samples(chunk).map(|sample| self.law.encode(sample)));
        let mut packets = Vec::new();
        while self.waiting.len() >= PACKET {
            let payload: Vec<u8> = self.waiting.drain(..PACKET).collect();
            let mut packet = Vec::with_capacity(4 + 12 + PACKET);
            let length = u16::try_from(12 + PACKET).unwrap_or(u16::MAX);
            packet.extend_from_slice(&[b'$', 0]);
            packet.extend_from_slice(&length.to_be_bytes());
            packet.push(0x80);
            // The marker starts a talkspurt.
            let marker = if self.first { 0x80 } else { 0 };
            packet.push(marker | self.law.payload_type());
            packet.extend_from_slice(&self.sequence.to_be_bytes());
            packet.extend_from_slice(&self.timestamp.to_be_bytes());
            packet.extend_from_slice(&self.ssrc.to_be_bytes());
            packet.extend_from_slice(&payload);
            packets.push(packet);
            self.first = false;
            self.sequence = self.sequence.wrapping_add(1);
            self.timestamp = self.timestamp.wrapping_add(PACKET as u32);
        }
        packets
    }
}

/// Where each µ-law segment ends, on the 14-bit scale.
const ULAW_ENDS: [i32; 8] = [0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF];

/// Where each A-law segment ends, on the 13-bit scale.
const ALAW_ENDS: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];

fn segment(value: i32, ends: &[i32; 8]) -> usize {
    ends.iter().position(|end| value <= *end).unwrap_or(8)
}

/// G.711 µ-law, as ITU-T's reference does it.
fn ulaw(sample: i16) -> u8 {
    let mut value = i32::from(sample) >> 2;
    let mask = if value < 0 {
        value = -value;
        0x7F
    } else {
        0xFF
    };
    value = value.min(8159) + 33;
    let segment = segment(value, &ULAW_ENDS);
    if segment >= 8 {
        return 0x7F ^ mask;
    }
    let mantissa = (value >> (segment + 1)) & 0x0F;
    ((segment as i32) << 4 | mantissa) as u8 ^ mask
}

/// G.711 A-law, as ITU-T's reference does it.
fn alaw(sample: i16) -> u8 {
    let mut value = i32::from(sample) >> 3;
    let mask = if value >= 0 {
        0xD5
    } else {
        value = -value - 1;
        0x55
    };
    let segment = segment(value, &ALAW_ENDS);
    if segment >= 8 {
        return 0x7F ^ mask;
    }
    let shift = if segment < 2 { 1 } else { segment };
    let mantissa = (value >> shift) & 0x0F;
    ((segment as i32) << 4 | mantissa) as u8 ^ mask
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn g711_matches_the_reference_at_its_edges() {
        assert_eq!(ulaw(0), 0xFF);
        assert_eq!(ulaw(i16::MAX), 0x80);
        assert_eq!(ulaw(i16::MIN), 0x00);
        // A level below zero is its mirror above, with the sign bit cleared.
        for level in (4..=32_000).step_by(4) {
            assert_eq!(ulaw(-level), ulaw(level) ^ 0x80, "{level}");
        }
        assert_eq!(alaw(0), 0xD5);
        assert_eq!(alaw(i16::MAX), 0xAA);
        assert_eq!(alaw(i16::MIN), 0x2A);
        // Louder is never quieter: µ-law codes fall as the level rises, on either side.
        let mut last = 0xFF;
        for level in (0..=i16::MAX).step_by(64) {
            let code = ulaw(level);
            assert!(code <= last, "{level}");
            last = code;
        }
    }

    #[test]
    fn the_speaker_is_the_audio_track_the_camera_is_sent_to() {
        // The Reolink Duo 2V's answer, cut down.
        let reolink = "v=0\r\ns=Session streamed by \"preview\"\r\na=control:*\r\n\
            m=video 0 RTP/AVP 96\r\na=rtpmap:96 H265/90000\r\na=recvonly\r\na=control:track1\r\n\
            m=audio 0 RTP/AVP 97\r\na=rtpmap:97 MPEG4-GENERIC/16000\r\na=recvonly\r\na=control:track2\r\n\
            m=audio 0 RTP/AVP 0\r\na=control:track3\r\na=rtpmap:0 PCMU/8000\r\na=sendonly";
        assert_eq!(speaker(reolink), Some(("track3".into(), Law::Mu)));
        // A Tapo C200's: it has sound, but only coming out of it.
        let tapo = "v=0\r\nm=video 0 RTP/AVP 96\r\na=control:track1\r\n\
            m=audio 0 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=control:track2\r\n";
        assert_eq!(speaker(tapo), None);
        let alaw = "v=0\r\nm=audio 0 RTP/AVP 8\r\na=sendonly\r\na=control:rtsp://10.0.0.9/talk\r\n";
        assert_eq!(speaker(alaw), Some(("rtsp://10.0.0.9/talk".into(), Law::A)));
        assert_eq!(join("rtsp://h/main/", "track3"), "rtsp://h/main/track3");
        assert_eq!(join("rtsp://h/main", "track3"), "rtsp://h/main/track3");
    }

    #[test]
    fn answers_are_read_past_whatever_came_interleaved_first() {
        let mut read = b"$\x01\x00\x02ab".to_vec();
        read.extend_from_slice(
            b"RTSP/1.0 200 OK\r\nCSeq: 5\r\nContent-Length: 2\r\n\r\nhiRTSP/1.0",
        );
        let answer = take_answer(&mut read).expect("readable").expect("whole");
        assert_eq!((answer.status, answer.body.as_str()), (200, "hi"));
        assert_eq!(answer.header("cseq"), Some("5"));
        assert_eq!(read, b"RTSP/1.0", "the next answer is left to be read");
        assert!(
            take_answer(&mut read).expect("readable").is_none(),
            "not whole yet"
        );
    }

    /// How the fake camera answers a description asked for with its speaker.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Has {
        Speaker,
        NoSpeaker,
        BadRequest,
    }

    /// What the fake camera saw.
    #[derive(Debug, Default)]
    struct Heard {
        methods: Vec<String>,
        refused: u32,
        /// Each RTP packet's payload type, sequence number, timestamp and payload length.
        packets: Vec<(u8, u16, u32, usize)>,
    }

    /// A camera on a loopback port that speaks just enough RTSP: Digest logins, a description
    /// with or without a speaker, SETUP, PLAY (with a report sent before its answer), and the
    /// RTP it's sent.
    async fn fake_camera(has: Has) -> (u16, Arc<Mutex<Heard>>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let heard = Arc::new(Mutex::new(Heard::default()));
        let seen = Arc::clone(&heard);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, has, Arc::clone(&seen)));
            }
        });
        (port, heard)
    }

    fn md5_hex(text: &str) -> String {
        use md5::{Digest as _, Md5};
        Md5::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn digest_ok(authorization: &str, method: &str) -> bool {
        let field = |name: &str| {
            crate::http::params(authorization.trim_start_matches("Digest "))
                .into_iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
                .unwrap_or_default()
        };
        let ha1 = md5_hex(&format!("{}:fake:secret", field("username")));
        let ha2 = md5_hex(&format!("{method}:{}", field("uri")));
        field("response") == md5_hex(&format!("{ha1}:abc123:{ha2}"))
    }

    async fn serve(mut stream: tokio::net::TcpStream, has: Has, heard: Arc<Mutex<Heard>>) {
        let mut read = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            // RTP from the page, interleaved.
            if read.first() == Some(&b'$') {
                if read.len() < 4 {
                } else {
                    let length = usize::from(u16::from_be_bytes([read[2], read[3]]));
                    if read.len() >= 4 + length {
                        let packet: Vec<u8> = read.drain(..4 + length).skip(4).collect();
                        heard.lock().expect("heard").packets.push((
                            packet[1] & 0x7F,
                            u16::from_be_bytes([packet[2], packet[3]]),
                            u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
                            packet.len() - 12,
                        ));
                        continue;
                    }
                }
            } else if let Some(end) = read.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&read[..end]).to_string();
                read.drain(..end + 4);
                let method = head
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let header = |name: &str| {
                    head.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case(name)
                            .then(|| value.trim().to_owned())
                    })
                };
                let cseq = header("cseq").unwrap_or_default();
                heard.lock().expect("heard").methods.push(method.clone());
                let answer = match header("authorization") {
                    None => format!(
                        "RTSP/1.0 401 Unauthorized\r\nCSeq: {cseq}\r\n\
                         WWW-Authenticate: Digest realm=\"fake\", nonce=\"abc123\"\r\n\r\n"
                    ),
                    Some(authorization) if !digest_ok(&authorization, &method) => {
                        heard.lock().expect("heard").refused += 1;
                        format!("RTSP/1.0 401 Unauthorized\r\nCSeq: {cseq}\r\n\r\n")
                    }
                    Some(_) => match method.as_str() {
                        "DESCRIBE" if has == Has::BadRequest => {
                            format!("RTSP/1.0 400 Bad Request\r\nCSeq: {cseq}\r\n\r\n")
                        }
                        "DESCRIBE" => {
                            let mut sdp =
                                "v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n\
                                a=recvonly\r\na=control:track1\r\n"
                                    .to_owned();
                            if has == Has::Speaker && header("require").is_some() {
                                sdp.push_str(
                                    "m=audio 0 RTP/AVP 0\r\na=control:track3\r\n\
                                     a=rtpmap:0 PCMU/8000\r\na=sendonly\r\n",
                                );
                            }
                            format!(
                                "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n\
                                 Content-Base: rtsp://127.0.0.1/main/\r\n\
                                 Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",
                                sdp.len()
                            )
                        }
                        "SETUP" => format!(
                            "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: 4242;timeout=60\r\n\r\n"
                        ),
                        // A report first, the way a Reolink sends one.
                        "PLAY" => {
                            format!("$\x01\x00\x04abcdRTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n\r\n")
                        }
                        _ => format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n\r\n"),
                    },
                };
                if stream.write_all(answer.as_bytes()).await.is_err() {
                    return;
                }
                continue;
            }
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read_now) => read.extend_from_slice(&chunk[..read_now]),
            }
        }
    }

    fn login(password: &str) -> Login {
        Login {
            username: "admin".into(),
            password: password.into(),
        }
    }

    fn here() -> IpAddr {
        IpAddr::from([127, 0, 0, 1])
    }

    #[tokio::test]
    async fn a_camera_with_a_speaker_can_be_talked_through() {
        let (port, heard) = fake_camera(Has::Speaker).await;
        // The address names another host: it's sent to the camera's IP all the same.
        let stream = format!("rtsp://192.168.68.200:{port}/main");
        let track = ask(&stream, here(), &login("secret"))
            .await
            .expect("a speaker");
        assert_eq!(track.law, Law::Mu);
        assert_eq!(track.track, "rtsp://127.0.0.1/main/track3");
        assert_eq!(heard.lock().expect("heard").refused, 0);
    }

    #[tokio::test]
    async fn a_camera_without_one_is_just_one_that_cant_talk() {
        for has in [Has::NoSpeaker, Has::BadRequest] {
            let (port, heard) = fake_camera(has).await;
            let stream = format!("rtsp://127.0.0.1:{port}/main");
            assert_eq!(
                ask(&stream, here(), &login("secret")).await,
                None,
                "{has:?}"
            );
            assert_eq!(heard.lock().expect("heard").refused, 0);
        }
        // A login it turns down is tried once, and is only a camera that can't talk.
        let (port, heard) = fake_camera(Has::Speaker).await;
        let stream = format!("rtsp://127.0.0.1:{port}/main");
        assert_eq!(ask(&stream, here(), &login("wrong")).await, None);
        assert_eq!(heard.lock().expect("heard").refused, 1);
        // Nothing listening at all.
        assert_eq!(
            ask("rtsp://127.0.0.1:1/main", here(), &login("secret")).await,
            None
        );
    }

    #[tokio::test]
    async fn talking_plays_the_pages_sound_then_silence_then_closes_the_speaker() {
        let (port, heard) = fake_camera(Has::Speaker).await;
        let stream = format!("rtsp://127.0.0.1:{port}/main");
        let track = ask(&stream, here(), &login("secret"))
            .await
            .expect("a speaker");
        let unique_id = irori_types::UniqueId::try_from("camera:porch").expect("valid");
        let (incoming, audio, mut words) = irori_protocol::host::incoming_talk(unique_id);
        let talk = incoming.into_talk().expect("a talk");
        let running =
            tokio::spawn(async move { run(&track, here(), &login("secret"), talk).await });

        assert_eq!(words.recv().await, Some(irori_protocol::TalkWord::Ready));
        // 60 ms of sound, in the 20 ms chunks a page sends.
        for _ in 0..3 {
            audio.send(vec![0u8; 320]).await.expect("listening");
        }
        drop(audio);
        assert_eq!(running.await.expect("ran"), Ended::Stopped);
        assert_eq!(
            words.recv().await,
            Some(irori_protocol::TalkWord::End { error: None })
        );

        // The camera reads at its own pace: wait until it has heard the goodbye.
        for _ in 0..100 {
            if heard
                .lock()
                .expect("heard")
                .methods
                .iter()
                .any(|m| m == "TEARDOWN")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let heard = heard.lock().expect("heard");
        let packets = &heard.packets;
        // The 3 sent, then the silence that pushes them out of the camera's buffer, carrying
        // on the same sequence and clock.
        assert!(packets.len() > 3, "{packets:?}");
        for (n, (payload_type, sequence, timestamp, length)) in packets.iter().enumerate() {
            assert_eq!((*payload_type, *length), (0, 160));
            assert_eq!(*sequence, packets[0].1.wrapping_add(n as u16));
            assert_eq!(*timestamp, 160 * n as u32);
        }
        let methods: Vec<&str> = heard.methods.iter().map(String::as_str).collect();
        assert!(
            methods.ends_with(&["SETUP", "PLAY", "TEARDOWN"]),
            "{methods:?}"
        );
        assert_eq!(heard.refused, 0);
    }

    #[test]
    fn sound_goes_out_in_20_ms_packets() {
        let mut packets = Packets::new(Law::Mu);
        let start = packets.sequence;
        // 250 samples: one packet now, 90 samples waiting.
        let chunk: Vec<u8> = std::iter::repeat_n([0u8, 0], 250).flatten().collect();
        let sent = packets.push(&chunk);
        assert_eq!(sent.len(), 1);
        let first = &sent[0];
        assert_eq!(&first[..4], &[b'$', 0, 0, 172], "interleaved on channel 0");
        assert_eq!(first[5], 0x80, "the first packet is marked, payload type 0");
        assert_eq!(first.len(), 4 + 12 + 160);
        assert!(first[16..].iter().all(|code| *code == 0xFF), "silence");
        let sent = packets.push(&chunk);
        assert_eq!(sent.len(), 2, "90 + 250 makes two more");
        let second = &sent[0];
        assert_eq!(second[5], 0, "only the first is marked");
        assert_eq!(
            u16::from_be_bytes([second[6], second[7]]),
            start.wrapping_add(1)
        );
        assert_eq!(
            u32::from_be_bytes([second[8], second[9], second[10], second[11]]),
            160
        );
    }
}
