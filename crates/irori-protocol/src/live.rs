//! Live video from a camera (`docs/specs/protocols.md` §5.3).
//!
//! A stream is fragmented MP4, the container browsers' Media Source Extensions play: an init
//! segment once, then fragments. The protocol makes it, so nothing between the camera and the
//! page knows about codecs.
//!
//! Video never waits for anyone. Every hop is a [`LiveFeed`]: a small queue that refuses rather
//! than blocks when it's full, and that then skips ahead to the next fragment starting at a
//! keyframe, because a decoder handed a fragment whose keyframe was dropped shows garbage until
//! the next one anyway. So a slow page, a busy pipe or a slow core costs frames, never the
//! protocol's state reports or anyone else's view.

use std::fmt;

use irori_types::{LiveQuality, UniqueId};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

/// The most an init segment may be: a codec's parameters, a few hundred bytes in practice.
pub const MAX_INIT: usize = 64 * 1024;

/// The most one fragment may be. A fragment is a frame or a few, and a keyframe of a 4K camera
/// is a few hundred kilobytes; it crosses the extension pipe as base64, a third bigger again.
pub const MAX_FRAGMENT: usize = 1024 * 1024;

/// How many chunks wait between two hops before the sender starts skipping: a second or so
/// of video at 25 frames a second.
pub const LIVE_QUEUE: usize = 32;

/// What a page can ask for in `accepts`: a codec family, as MP4 names them (`avc1`, `hvc1`,
/// `mp4a`, `flac`). At most this many.
pub const MAX_ACCEPTS: usize = 16;

/// One piece of a stream.
#[derive(Clone, PartialEq, Eq)]
pub enum LiveChunk {
    /// The init segment, and what it is: `video/mp4; codecs="avc1.4d401f"`. First, and once.
    Init { mime: String, data: Vec<u8> },
    /// Some video. `key` when it starts at a keyframe, where a new viewer can begin.
    Fragment { key: bool, data: Vec<u8> },
    /// The stream is over, and why when it's a failure.
    End { error: Option<String> },
}

impl fmt::Debug for LiveChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init { mime, data } => write!(f, "Init({mime}, {} bytes)", data.len()),
            Self::Fragment { key, data } => {
                write!(
                    f,
                    "Fragment({}{} bytes)",
                    if *key { "key, " } else { "" },
                    data.len()
                )
            }
            Self::End { error } => write!(f, "End({error:?})"),
        }
    }
}

impl LiveChunk {
    /// Whether the core passes it on: an MP4 type, and no bigger than [`MAX_INIT`] or
    /// [`MAX_FRAGMENT`].
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::Init { mime, data } => {
                if !mime.starts_with("video/mp4") || mime.len() > 256 {
                    return Err(format!("live video is `video/mp4`, not {mime:?}"));
                }
                if data.is_empty() || data.len() > MAX_INIT {
                    return Err(format!(
                        "an init segment is up to {MAX_INIT} bytes, and this one is {}",
                        data.len()
                    ));
                }
            }
            Self::Fragment { data, .. } => {
                if data.is_empty() || data.len() > MAX_FRAGMENT {
                    return Err(format!(
                        "a fragment is up to {MAX_FRAGMENT} bytes, and this one is {}",
                        data.len()
                    ));
                }
            }
            Self::End { .. } => {}
        }
        Ok(())
    }
}

/// The far end of a feed has gone: nobody is watching any more. Stop the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveClosed;

impl fmt::Display for LiveClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("nobody is watching any more")
    }
}

impl std::error::Error for LiveClosed {}

/// What happened to a fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Sent,
    /// Dropped: the queue was full, or it was waiting for a keyframe after one that was.
    Skipped,
}

/// Where a stream's chunks go. Never waits (see the module docs).
#[derive(Debug)]
pub struct LiveFeed {
    tx: mpsc::Sender<LiveChunk>,
    /// Dropping fragments until one starts at a keyframe.
    skipping: bool,
}

impl LiveFeed {
    /// A feed and the receiving end its chunks arrive on.
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<LiveChunk>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                skipping: false,
            },
            rx,
        )
    }

    /// A feed that starts by skipping to a keyframe: for a viewer who joins a stream already
    /// running.
    pub fn joining(capacity: usize) -> (Self, mpsc::Receiver<LiveChunk>) {
        let (mut feed, rx) = Self::channel(capacity);
        feed.skipping = true;
        (feed, rx)
    }

    /// Sends the init segment. It's first, so there's room; a feed that can't take even that is
    /// treated as gone.
    pub fn init(&mut self, mime: String, data: Vec<u8>) -> Result<(), LiveClosed> {
        self.tx
            .try_send(LiveChunk::Init { mime, data })
            .map_err(|_| LiveClosed)
    }

    /// Sends a fragment, or skips it (see [`Sent::Skipped`]).
    pub fn fragment(&mut self, key: bool, data: Vec<u8>) -> Result<Sent, LiveClosed> {
        if self.skipping && !key {
            return if self.tx.is_closed() {
                Err(LiveClosed)
            } else {
                Ok(Sent::Skipped)
            };
        }
        match self.tx.try_send(LiveChunk::Fragment { key, data }) {
            Ok(()) => {
                self.skipping = false;
                Ok(Sent::Sent)
            }
            Err(TrySendError::Full(_)) => {
                self.skipping = true;
                Ok(Sent::Skipped)
            }
            Err(TrySendError::Closed(_)) => Err(LiveClosed),
        }
    }

    /// Passes a chunk on as it is: [`LiveFeed::init`], [`LiveFeed::fragment`] or
    /// [`LiveFeed::end`].
    pub fn forward(&mut self, chunk: LiveChunk) -> Result<Sent, LiveClosed> {
        match chunk {
            LiveChunk::Init { mime, data } => self.init(mime, data).map(|()| Sent::Sent),
            LiveChunk::Fragment { key, data } => self.fragment(key, data),
            LiveChunk::End { error } => {
                self.end_ref(error);
                Ok(Sent::Sent)
            }
        }
    }

    /// Ends the stream, with why when it failed. If there's no room for the word, dropping the
    /// feed says it ended all the same.
    pub fn end(mut self, error: Option<String>) {
        self.end_ref(error);
    }

    fn end_ref(&mut self, error: Option<String>) {
        let _ = self.tx.try_send(LiveChunk::End { error });
    }

    /// Whether the far end has gone.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Resolves once the far end has gone: stop the stream then.
    pub async fn closed(&self) {
        self.tx.closed().await;
    }
}

/// A page wants to watch one of this protocol's cameras live (`docs/specs/protocols.md` §5.3).
/// Only sent for a `camera` that says it's `live`, in one of its `qualities`.
///
/// Send the init segment, then fragments, through `feed`, until [`LiveFeed::closed`] resolves
/// (nobody watches any more) or the camera stops; then [`LiveFeed::end`]. Dropping the feed
/// ends the stream too. Answer quickly: the page waits about ten seconds for the init segment.
#[derive(Debug)]
pub struct IncomingLive {
    /// The camera entity's `unique_id`, as this protocol described it.
    pub unique_id: UniqueId,
    pub quality: LiveQuality,
    /// The codec families the page can play, as MP4 names them: `avc1`, `hvc1`, `mp4a`,
    /// `flac`. Send video it can play, and sound only in one it can; if there's no video it can
    /// play, end the stream saying so.
    pub accepts: Vec<String>,
    pub feed: LiveFeed,
}

impl IncomingLive {
    /// Whether the page can play this codec family.
    pub fn accepts(&self, family: &str) -> bool {
        self.accepts.iter().any(|accepted| accepted == family)
    }
}

/// Live video off the extension pipe: base64, and never more than the biggest chunk there can
/// be, checked before decoding so an oversized line costs nothing.
pub fn decode(data: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    if data.len() > MAX_FRAGMENT.div_ceil(3) * 4 {
        return Err(format!("a fragment is up to {MAX_FRAGMENT} bytes"));
    }
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("live video that isn't base64: {e}"))
}

/// What the core checks of `accepts` before asking a protocol: a few short codec names.
pub fn check_accepts(accepts: &[String]) -> Result<(), String> {
    if accepts.len() > MAX_ACCEPTS {
        return Err(format!(
            "at most {MAX_ACCEPTS} codecs can be offered, not {}",
            accepts.len()
        ));
    }
    if let Some(odd) = accepts.iter().find(|codec| {
        codec.is_empty() || codec.len() > 8 || !codec.bytes().all(|b| b.is_ascii_alphanumeric())
    }) {
        return Err(format!("`{odd}` isn't a codec name like `avc1`"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(key: bool) -> LiveChunk {
        LiveChunk::Fragment {
            key,
            data: vec![u8::from(key)],
        }
    }

    /// A full queue drops what doesn't fit and everything after it up to the next keyframe, so
    /// what arrives always decodes.
    #[tokio::test]
    async fn a_full_feed_skips_to_the_next_keyframe() {
        let (mut feed, mut rx) = LiveFeed::channel(2);
        assert_eq!(feed.fragment(true, vec![1]), Ok(Sent::Sent));
        assert_eq!(feed.fragment(false, vec![0]), Ok(Sent::Sent));
        assert_eq!(feed.fragment(false, vec![0]), Ok(Sent::Skipped), "full");
        assert_eq!(rx.recv().await, Some(fragment(true)));
        assert_eq!(
            feed.fragment(false, vec![0]),
            Ok(Sent::Skipped),
            "room again, but its keyframe was dropped"
        );
        assert_eq!(feed.fragment(true, vec![1]), Ok(Sent::Sent));
        assert_eq!(rx.recv().await, Some(fragment(false)));
        assert_eq!(rx.recv().await, Some(fragment(true)));
    }

    #[tokio::test]
    async fn a_viewer_who_joins_late_starts_at_a_keyframe() {
        let (mut feed, mut rx) = LiveFeed::joining(4);
        feed.init("video/mp4".into(), vec![9]).expect("room");
        assert_eq!(feed.fragment(false, vec![0]), Ok(Sent::Skipped));
        assert_eq!(feed.fragment(true, vec![1]), Ok(Sent::Sent));
        assert!(matches!(rx.recv().await, Some(LiveChunk::Init { .. })));
        assert_eq!(rx.recv().await, Some(fragment(true)));
    }

    #[tokio::test]
    async fn a_feed_knows_when_nobody_watches() {
        let (mut feed, rx) = LiveFeed::channel(2);
        drop(rx);
        assert!(feed.is_closed());
        feed.closed().await;
        assert_eq!(feed.fragment(true, vec![1]), Err(LiveClosed));
        assert_eq!(feed.fragment(false, vec![1]), Err(LiveClosed));
    }

    #[test]
    fn only_mp4_of_a_sensible_size_is_passed_on() {
        let init = |mime: &str, size| LiveChunk::Init {
            mime: mime.into(),
            data: vec![0; size],
        };
        assert!(
            init("video/mp4; codecs=\"avc1.4d401f\"", 600)
                .check()
                .is_ok()
        );
        assert!(init("text/html", 600).check().is_err());
        assert!(init("video/mp4", MAX_INIT + 1).check().is_err());
        let big = LiveChunk::Fragment {
            key: true,
            data: vec![0; MAX_FRAGMENT + 1],
        };
        assert!(big.check().is_err());
        assert!(check_accepts(&["avc1".into(), "mp4a".into()]).is_ok());
        assert!(check_accepts(&["avc1; drop".into()]).is_err());
    }
}
