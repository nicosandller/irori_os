//! Watching cameras live (`docs/specs/protocols.md` §5.3): one stream from the protocol per
//! camera, quality and set of codecs, however many people watch it.
//!
//! The first viewer starts the stream. Later ones get the init segment it already sent, then
//! join at its next keyframe. When the last one leaves, the stream runs on for a short grace
//! period, so a page reloading or a drawer reopening doesn't make the camera start over; then
//! the protocol is told to stop by its feed closing.
//!
//! Nothing here waits on a viewer: each has its own small queue ([`LiveFeed`]), and one that
//! falls behind skips ahead to a keyframe on its own without holding up anyone else.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use irori_protocol::live::{LIVE_QUEUE, LiveChunk, LiveFeed};
use irori_types::{EntityId, LiveQuality};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// How long a stream nobody watches keeps running.
pub const LIVE_GRACE: Duration = Duration::from_secs(10);

/// How long a camera has to send its init segment once asked.
pub const LIVE_START_TIMEOUT: Duration = Duration::from_secs(15);

/// How many viewers can be waiting to join one stream at once.
const JOINING: usize = 16;

/// Which stream a viewer wants. Two pages that can play the same codecs share one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct StreamKey {
    pub entity: EntityId,
    pub quality: LiveQuality,
    /// Sorted, each once.
    pub accepts: Vec<String>,
}

impl StreamKey {
    pub fn new(entity: EntityId, quality: LiveQuality, mut accepts: Vec<String>) -> Self {
        accepts.sort();
        accepts.dedup();
        Self {
            entity,
            quality,
            accepts,
        }
    }
}

/// The streams running now, each reached through the channel its pump takes viewers on.
#[derive(Debug, Default)]
pub(crate) struct LiveHub {
    streams: Mutex<HashMap<StreamKey, mpsc::Sender<LiveFeed>>>,
}

/// What [`LiveHub::join`] did.
pub(crate) enum Joined {
    /// Joined a stream already running.
    Running,
    /// No stream yet: start one with this, which takes the viewer and new viewers from now on.
    Start(Starting),
}

/// A stream about to start: [`Starting::run`] it with the protocol's chunks.
pub(crate) struct Starting {
    key: StreamKey,
    first: LiveFeed,
    joins: mpsc::Receiver<LiveFeed>,
    hub: Arc<LiveHub>,
    entry: mpsc::Sender<LiveFeed>,
}

impl LiveHub {
    /// Adds a viewer to the stream for `key`, or says to start one. The viewer's chunks arrive
    /// on the receiver.
    pub fn join(self: &Arc<Self>, key: StreamKey) -> (Joined, mpsc::Receiver<LiveChunk>) {
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = streams.get(&key) {
            let (feed, chunks) = LiveFeed::joining(LIVE_QUEUE);
            match entry.try_send(feed) {
                Ok(()) => return (Joined::Running, chunks),
                // The pump has ended and is on its way out of the table; start afresh.
                Err(mpsc::error::TrySendError::Closed(_)) => {}
                Err(mpsc::error::TrySendError::Full(feed)) => {
                    feed.end(Some("too many people are starting to watch at once".into()));
                    return (Joined::Running, chunks);
                }
            }
        }
        let (entry, joins) = mpsc::channel(JOINING);
        streams.insert(key.clone(), entry.clone());
        let (first, chunks) = LiveFeed::channel(LIVE_QUEUE);
        (
            Joined::Start(Starting {
                key,
                first,
                joins,
                hub: Arc::clone(self),
                entry,
            }),
            chunks,
        )
    }

    /// How many streams are running: for tests.
    #[cfg(test)]
    pub fn running(&self) -> usize {
        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn leave(&self, key: &StreamKey, entry: &mpsc::Sender<LiveFeed>) {
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        if streams
            .get(key)
            .is_some_and(|running| running.same_channel(entry))
        {
            streams.remove(key);
        }
    }
}

impl Starting {
    /// The first viewer could not be started: says why, and frees the slot.
    pub fn fail(self, error: String) {
        self.hub.leave(&self.key, &self.entry);
        self.first.end(Some(error));
    }

    /// Passes the protocol's chunks to everyone watching until the stream ends, or nobody has
    /// watched for [`LIVE_GRACE`]. Returning drops `chunks`, which tells the protocol to stop.
    pub async fn run(self, mut chunks: mpsc::Receiver<LiveChunk>) {
        let Self {
            key,
            first,
            mut joins,
            hub,
            entry,
        } = self;
        let mut viewers = vec![first];
        let mut init: Option<(String, Vec<u8>)> = None;
        let started = Instant::now();
        let mut idle_since: Option<Instant> = None;
        let error: Option<String> = loop {
            let deadline = match (&init, idle_since) {
                (None, _) => started + LIVE_START_TIMEOUT,
                (Some(_), Some(idle)) => idle + LIVE_GRACE,
                (Some(_), None) => Instant::now() + Duration::from_secs(1),
            };
            tokio::select! {
                chunk = chunks.recv() => {
                    let Some(chunk) = chunk else {
                        break Some("the camera's stream stopped".into());
                    };
                    if let Err(why) = chunk.check() {
                        break Some(why);
                    }
                    match chunk {
                        LiveChunk::Init { mime, data } => {
                            if init.is_some() {
                                break Some("the camera sent a second init segment".into());
                            }
                            viewers.retain_mut(|viewer| viewer.init(mime.clone(), data.clone()).is_ok());
                            init = Some((mime, data));
                        }
                        LiveChunk::Fragment { key: at_key, data } => {
                            if init.is_none() {
                                break Some("the camera sent video before its init segment".into());
                            }
                            viewers.retain_mut(|viewer| viewer.fragment(at_key, data.clone()).is_ok());
                        }
                        LiveChunk::End { error } => break error,
                    }
                }
                Some(mut viewer) = joins.recv() => {
                    let joined = match &init {
                        Some((mime, data)) => viewer.init(mime.clone(), data.clone()).is_ok(),
                        None => true,
                    };
                    if joined {
                        viewers.push(viewer);
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    if init.is_none() {
                        break Some("the camera didn't start its video in time".into());
                    }
                }
            }
            viewers.retain(|viewer| !viewer.is_closed());
            if viewers.is_empty() {
                let idle = *idle_since.get_or_insert_with(Instant::now);
                if init.is_some() && Instant::now() >= idle + LIVE_GRACE {
                    break None;
                }
            } else {
                idle_since = None;
            }
        };
        // Out of the table first, so nobody else joins; then whoever slipped in hears why.
        hub.leave(&key, &entry);
        drop(entry);
        joins.close();
        while let Ok(viewer) = joins.try_recv() {
            viewers.push(viewer);
        }
        let error = error.or_else(|| Some("the stream ended".into()));
        for viewer in viewers {
            viewer.end(error.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> StreamKey {
        StreamKey::new(
            EntityId::try_from("camera.driveway").expect("valid"),
            LiveQuality::Low,
            vec!["avc1".into(), "mp4a".into(), "avc1".into()],
        )
    }

    fn start(hub: &Arc<LiveHub>) -> (Starting, mpsc::Receiver<LiveChunk>) {
        match hub.join(key()) {
            (Joined::Start(starting), chunks) => (starting, chunks),
            (Joined::Running, _) => panic!("nothing was running"),
        }
    }

    fn init() -> LiveChunk {
        LiveChunk::Init {
            mime: "video/mp4".into(),
            data: vec![0],
        }
    }

    fn fragment(key: bool) -> LiveChunk {
        LiveChunk::Fragment {
            key,
            data: vec![u8::from(key)],
        }
    }

    /// Two viewers, one stream: the second gets the init the camera already sent, and its
    /// first fragment is a keyframe.
    #[tokio::test]
    async fn a_second_viewer_shares_the_stream_and_starts_at_a_keyframe() {
        let hub = Arc::new(LiveHub::default());
        let (starting, mut first) = start(&hub);
        let (mut camera, from_camera) = LiveFeed::channel(LIVE_QUEUE);
        let pump = tokio::spawn(starting.run(from_camera));
        camera.init("video/mp4".into(), vec![0]).expect("room");
        camera.fragment(true, vec![1]).expect("room");
        assert_eq!(first.recv().await, Some(init()));
        assert_eq!(first.recv().await, Some(fragment(true)));

        let (Joined::Running, mut second) = hub.join(key()) else {
            panic!("the stream is running");
        };
        assert_eq!(hub.running(), 1);
        tokio::task::yield_now().await;
        camera.fragment(false, vec![0]).expect("room");
        camera.fragment(true, vec![1]).expect("room");
        assert_eq!(second.recv().await, Some(init()));
        assert_eq!(
            second.recv().await,
            Some(fragment(true)),
            "the fragment before the keyframe isn't for it"
        );
        assert_eq!(first.recv().await, Some(fragment(false)));
        assert_eq!(first.recv().await, Some(fragment(true)));

        camera.end(Some("unplugged".into()));
        pump.await.expect("the pump ends");
        assert_eq!(
            first.recv().await,
            Some(LiveChunk::End {
                error: Some("unplugged".into())
            })
        );
        assert_eq!(hub.running(), 0);
    }

    /// Nobody watching: the stream runs on through the grace period, then stops the camera.
    #[tokio::test(start_paused = true)]
    async fn a_stream_nobody_watches_stops_after_the_grace_period() {
        let hub = Arc::new(LiveHub::default());
        let (starting, viewer) = start(&hub);
        let (mut camera, from_camera) = LiveFeed::channel(LIVE_QUEUE);
        let pump = tokio::spawn(starting.run(from_camera));
        camera.init("video/mp4".into(), vec![0]).expect("room");
        tokio::task::yield_now().await;
        drop(viewer);

        tokio::time::sleep(LIVE_GRACE / 2).await;
        assert!(!camera.is_closed(), "still in its grace period");
        let (Joined::Running, mut back) = hub.join(key()) else {
            panic!("a page coming back rejoins the same stream");
        };
        tokio::time::sleep(LIVE_GRACE * 2).await;
        assert!(!camera.is_closed(), "watched again");
        assert_eq!(back.recv().await, Some(init()));

        drop(back);
        tokio::time::sleep(LIVE_GRACE + Duration::from_secs(2)).await;
        pump.await.expect("the pump ends");
        assert!(camera.is_closed(), "the camera is told to stop");
        assert_eq!(hub.running(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_camera_that_never_starts_is_given_up_on() {
        let hub = Arc::new(LiveHub::default());
        let (starting, mut viewer) = start(&hub);
        let (_camera, from_camera) = LiveFeed::channel(LIVE_QUEUE);
        tokio::spawn(starting.run(from_camera));
        let end = viewer.recv().await.expect("an end");
        assert_eq!(
            end,
            LiveChunk::End {
                error: Some("the camera didn't start its video in time".into())
            }
        );
    }

    #[test]
    fn pages_that_play_the_same_codecs_share_a_stream() {
        let entity = EntityId::try_from("camera.driveway").expect("valid");
        assert_eq!(
            StreamKey::new(
                entity.clone(),
                LiveQuality::High,
                vec!["mp4a".into(), "avc1".into()]
            ),
            StreamKey::new(
                entity,
                LiveQuality::High,
                vec!["avc1".into(), "mp4a".into()]
            ),
        );
    }
}
