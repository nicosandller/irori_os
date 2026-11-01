//! A camera's live video, from its RTSP stream to fragmented MP4 (`docs/specs/protocols.md`
//! §5.3).
//!
//! `retina` reads the stream over the RTSP connection itself (TCP, so nothing needs a UDP port
//! opened) and hands over whole frames. Each frame becomes one fragment, sent once the next
//! frame says how long it lasted. The first fragment is always a keyframe, which is where a
//! browser can start.
//!
//! The camera's login goes to RTSP's own Digest challenge, never into the address.

use std::net::IpAddr;
use std::time::Duration;

use futures_util::StreamExt as _;
use irori_protocol::live::MAX_FRAGMENT;
use irori_protocol::{IncomingLive, LiveFeed};
use retina::client::{Credentials, PlayOptions, Session, SessionOptions, SetupOptions};
use retina::codec::{CodecItem, FrameFormat, ParametersRef};

use crate::fmp4::{self, Media, Sample, Track};
use crate::settings::Login;

/// How long the camera has to answer while the stream is being opened.
const OPENING: Duration = Duration::from_secs(10);
/// How long a running stream may go without a frame before it's taken to have stopped.
const SILENCE: Duration = Duration::from_secs(10);
/// The longest one frame may last: a camera that pauses doesn't get one frame lasting minutes.
const LONGEST_FRAME_SECS: i64 = 5;
/// The tracks in the MP4.
const VIDEO_TRACK: u32 = 1;
const AUDIO_TRACK: u32 = 2;

/// How a stream ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// Nobody is watching any more.
    Stopped,
    /// The camera refused the login.
    Refused,
    /// Why it couldn't go on, for whoever was watching.
    Failed(String),
}

/// Streams one camera to `live` until nobody watches or it can't go on. `stream` is the RTSP
/// address the camera gave; it's sent to `ip` whatever host it names.
pub async fn run(stream: &str, ip: IpAddr, login: &Login, live: IncomingLive) -> Ended {
    let IncomingLive {
        accepts, mut feed, ..
    } = live;
    let ended = stream_to(stream, ip, login, &accepts, &mut feed).await;
    match &ended {
        Ended::Failed(why) => feed.end(Some(why.clone())),
        Ended::Refused => feed.end(Some("the camera refused its login".into())),
        Ended::Stopped => feed.end(None),
    }
    ended
}

/// A frame waiting to learn how long it lasts.
struct Held {
    /// From the first keyframe, in the clock's ticks.
    at: i64,
    key: bool,
    data: Vec<u8>,
}

async fn stream_to(
    stream: &str,
    ip: IpAddr,
    login: &Login,
    accepts: &[String],
    feed: &mut LiveFeed,
) -> Ended {
    let url = match address(stream, ip) {
        Ok(url) => url,
        Err(why) => return Ended::Failed(why),
    };
    let options = SessionOptions::default()
        .creds(Some(Credentials {
            username: login.username.clone(),
            password: login.password.clone(),
        }))
        .user_agent("Irori".to_owned());
    let opened = tokio::select! {
        () = feed.closed() => return Ended::Stopped,
        opened = tokio::time::timeout(OPENING, open(url, options, accepts)) => opened,
    };
    let (mut demuxed, streams) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(ended)) => return ended,
        Err(_) => return Ended::Failed("the camera's video didn't start in time".into()),
    };

    let mut muxer: Option<Muxer> = None;
    loop {
        let next = tokio::select! {
            () = feed.closed() => return Ended::Stopped,
            next = tokio::time::timeout(SILENCE, demuxed.next()) => next,
        };
        let item = match next {
            Err(_) | Ok(None) => return Ended::Failed("the camera stopped sending video".into()),
            Ok(Some(Err(error))) => {
                tracing::debug!("a camera's stream failed: {error}");
                return Ended::Failed("the camera's video stream broke off".into());
            }
            Ok(Some(Ok(item))) => item,
        };
        let sent = match item {
            CodecItem::VideoFrame(frame) if frame.stream_id() == streams.video => {
                let Some(muxer) = muxer.as_mut() else {
                    // Start at a keyframe, with the codecs as the camera describes them now.
                    if !frame.is_random_access_point() {
                        continue;
                    }
                    match Muxer::start(&demuxed, &streams, accepts, &frame, feed) {
                        Ok(started) => muxer = Some(started),
                        Err(ended) => return ended,
                    }
                    continue;
                };
                if frame.has_new_parameters() {
                    // The init segment no longer describes it; a page has to start over.
                    return Ended::Failed(
                        "the camera changed its video settings; open it again".into(),
                    );
                }
                muxer.video(&frame, feed)
            }
            CodecItem::AudioFrame(frame) if Some(frame.stream_id()) == streams.audio => {
                match muxer.as_mut() {
                    Some(muxer) => muxer.audio(&frame, feed),
                    None => continue,
                }
            }
            _ => continue,
        };
        if let Err(ended) = sent {
            return ended;
        }
    }
}

/// Which of the session's streams are being played.
struct Streams {
    video: usize,
    /// AAC, when the camera sends it and the page can play it.
    audio: Option<usize>,
}

/// Turns frames into fragments, once the init segment is out.
struct Muxer {
    sequence: u32,
    /// The video clock's reading at the first keyframe.
    video_zero: i64,
    video_clock: i64,
    held: Option<Held>,
    /// Where the latest video frame is, from the first keyframe.
    video_at: i64,
    audio: Option<AudioTrack>,
}

struct AudioTrack {
    clock: i64,
    /// The audio clock's reading that lines up with the first keyframe; set by the first
    /// audio frame, from where the video is when it comes.
    zero: Option<i64>,
}

impl Muxer {
    /// Sends the init segment and holds the first keyframe.
    fn start(
        demuxed: &retina::client::Demuxed,
        streams: &Streams,
        accepts: &[String],
        frame: &retina::codec::VideoFrame,
        feed: &mut LiveFeed,
    ) -> Result<Self, Ended> {
        let Some(ParametersRef::Video(parameters)) = demuxed.streams()[streams.video].parameters()
        else {
            return Err(Ended::Failed("the camera didn't describe its video".into()));
        };
        let codec = parameters.rfc6381_codec().to_owned();
        let family = codec.get(..4).unwrap_or_default();
        if !accepts.iter().any(|accepted| accepted == family) {
            return Err(Ended::Failed(cant_play(family)));
        }
        let sample_entry = parameters
            .mp4_sample_entry()
            .build()
            .map_err(|_| Ended::Failed("the camera's video can't be put in MP4".into()))?;
        let (width, height) = parameters.pixel_dimensions();
        let video_clock = i64::from(frame.timestamp().clock_rate().get());
        let mut tracks = vec![Track {
            id: VIDEO_TRACK,
            timescale: u32::try_from(video_clock).unwrap_or(90_000),
            media: Media::Video {
                width: u16::try_from(width).unwrap_or(u16::MAX),
                height: u16::try_from(height).unwrap_or(u16::MAX),
            },
            sample_entry,
        }];
        let mut codecs = vec![codec];
        let mut audio = None;
        if let Some(index) = streams.audio
            && let Some(ParametersRef::Audio(parameters)) = demuxed.streams()[index].parameters()
            && let Some(codec) = parameters.rfc6381_codec()
            && let Ok(sample_entry) = parameters.mp4_sample_entry().build()
        {
            let clock = parameters.clock_rate();
            tracks.push(Track {
                id: AUDIO_TRACK,
                timescale: clock,
                media: Media::Audio,
                sample_entry,
            });
            codecs.push(codec.to_owned());
            audio = Some(AudioTrack {
                clock: i64::from(clock),
                zero: None,
            });
        }
        let codecs: Vec<&str> = codecs.iter().map(String::as_str).collect();
        if feed.init(fmp4::mime(&codecs), fmp4::init(&tracks)).is_err() {
            return Err(Ended::Stopped);
        }
        Ok(Self {
            sequence: 1,
            video_zero: frame.timestamp().timestamp(),
            video_clock,
            held: Some(Held {
                at: 0,
                key: true,
                data: frame.data().to_vec(),
            }),
            video_at: 0,
            audio,
        })
    }

    /// Holds this frame, and sends the one before now that it's known how long it lasted.
    fn video(
        &mut self,
        frame: &retina::codec::VideoFrame,
        feed: &mut LiveFeed,
    ) -> Result<(), Ended> {
        let at = frame.timestamp().timestamp() - self.video_zero;
        self.video_at = at;
        let Some(previous) = self.held.replace(Held {
            at,
            key: frame.is_random_access_point(),
            data: frame.data().to_vec(),
        }) else {
            return Ok(());
        };
        let duration = (at - previous.at).clamp(1, LONGEST_FRAME_SECS * self.video_clock);
        let fragment = fmp4::fragment(
            self.next_sequence(),
            VIDEO_TRACK,
            u64::try_from(previous.at).unwrap_or(0),
            &[Sample {
                duration: u32::try_from(duration).unwrap_or(1),
                key: previous.key,
                data: previous.data,
            }],
        );
        if fragment.len() > MAX_FRAGMENT {
            return Err(Ended::Failed(
                "a frame from the camera is bigger than live video can carry; try the smaller \
                 stream"
                    .into(),
            ));
        }
        feed.fragment(previous.key, fragment)
            .map(|_| ())
            .map_err(|_| Ended::Stopped)
    }

    /// Sends an audio frame straight away: its length is in the frame.
    fn audio(
        &mut self,
        frame: &retina::codec::AudioFrame,
        feed: &mut LiveFeed,
    ) -> Result<(), Ended> {
        let video_clock = self.video_clock;
        let video_at = self.video_at;
        let Some(audio) = self.audio.as_mut() else {
            return Ok(());
        };
        let reading = frame.timestamp().timestamp();
        // Lined up with the video by when it arrives: close enough to watch, without waiting
        // for the camera's clock reports.
        let zero = *audio
            .zero
            .get_or_insert_with(|| reading - video_at * audio.clock / video_clock.max(1));
        let Ok(at) = u64::try_from(reading - zero) else {
            return Ok(());
        };
        let fragment = fmp4::fragment(
            self.next_sequence(),
            AUDIO_TRACK,
            at,
            &[Sample {
                duration: frame.frame_length().get(),
                key: true,
                data: frame.data().to_vec(),
            }],
        );
        // Not a place to start watching from: that's a video keyframe.
        feed.fragment(false, fragment)
            .map(|_| ())
            .map_err(|_| Ended::Stopped)
    }

    fn next_sequence(&mut self) -> u32 {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        sequence
    }
}

/// Opens the stream and starts it playing: the frames, and which streams they come from.
async fn open(
    url: url::Url,
    options: SessionOptions,
    accepts: &[String],
) -> Result<(retina::client::Demuxed, Streams), Ended> {
    let mut session = Session::describe(url, options).await.map_err(failed)?;
    let Some(video) = session.streams().iter().position(|stream| {
        stream.media() == "video"
            && (stream.encoding_name().eq_ignore_ascii_case("h264")
                || stream.encoding_name().eq_ignore_ascii_case("h265"))
    }) else {
        return Err(Ended::Failed(
            "the camera's stream has no H.264 or H.265 video".into(),
        ));
    };
    let aac = accepts.iter().any(|accepted| accepted == "mp4a");
    let audio = session.streams().iter().position(|stream| {
        aac && stream.media() == "audio"
            && stream.encoding_name().eq_ignore_ascii_case("mpeg4-generic")
    });
    let setup = || SetupOptions::default().frame_format(FrameFormat::MP4);
    session.setup(video, setup()).await.map_err(failed)?;
    let audio = match audio {
        // Sound is a bonus: a camera that won't send it still shows its video.
        Some(index) => match session.setup(index, setup()).await {
            Ok(()) => Some(index),
            Err(error) if error.status_code() == Some(401) => return Err(Ended::Refused),
            Err(_) => None,
        },
        None => None,
    };
    let playing = session.play(PlayOptions::default()).await.map_err(failed)?;
    let demuxed = playing.demuxed().map_err(failed)?;
    Ok((demuxed, Streams { video, audio }))
}

fn failed(error: retina::Error) -> Ended {
    if error.status_code() == Some(401) {
        return Ended::Refused;
    }
    // The error can name the address; it goes to the log, and the page gets a plain reason.
    tracing::debug!("a camera's stream couldn't be opened: {error}");
    Ended::Failed("the camera's video couldn't be opened".into())
}

fn cant_play(family: &str) -> String {
    match family {
        "hvc1" | "hev1" => "this browser can't play H.265, which this camera sends; switch the \
                            camera to H.264, or watch the smaller stream"
            .into(),
        _ => "this browser can't play the camera's video".into(),
    }
}

/// The camera's RTSP address, sent to `ip` and with no login in it.
pub fn address(stream: &str, ip: IpAddr) -> Result<url::Url, String> {
    let mut url = url::Url::parse(stream.trim())
        .map_err(|_| "the camera gave a video address that can't be read".to_owned())?;
    if !url.scheme().eq_ignore_ascii_case("rtsp") {
        return Err("the camera gave a video address that isn't RTSP".into());
    }
    let host = match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        v4 => v4.to_string(),
    };
    url.set_host(Some(&host))
        .map_err(|_| "the camera's video address can't be used".to_owned())?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_video_address_goes_to_the_camera_with_no_login_in_it() {
        let ip: IpAddr = "192.168.68.64".parse().expect("ip");
        let url = address("rtsp://admin:secret@10.9.9.9:554/stream1?x=1", ip).expect("an address");
        assert_eq!(url.as_str(), "rtsp://192.168.68.64:554/stream1?x=1");
        let url = address("rtsp://192.168.68.63/h264Preview_01_sub", ip).expect("an address");
        assert_eq!(url.as_str(), "rtsp://192.168.68.64/h264Preview_01_sub");
        assert!(address("http://cam/stream", ip).is_err());
        assert!(address("not an address", ip).is_err());
    }
}
