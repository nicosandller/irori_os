//! Watching a camera live through Home Assistant (`docs/specs/protocols.md` §5.3).
//!
//! Home Assistant's `camera/stream` answers with an HLS playlist whose segments are fragmented
//! MP4: an init segment (`#EXT-X-MAP`), then segments that each start at a keyframe. That is the
//! container Irori's live view plays, so nothing is decoded or re-packed here: the init segment
//! goes out once, then each new segment as a fragment, until nobody watches any more. The
//! playlist's own address carries Home Assistant's access for it, and reading it is what keeps
//! the stream running there.

use std::collections::BTreeSet;
use std::time::Duration;

use irori_protocol::IncomingLive;

use crate::client::{Client, Outgoing};
use crate::http;
use crate::transport::Transport;

/// How long asking for the stream, and fetching a playlist, may take.
const ASK_WITHIN: Duration = Duration::from_secs(10);
/// How long a playlist may take while the camera starts: Home Assistant answers once it has the
/// first segment, which a camera reached over RTSP can take a while to send.
const STARTING_WITHIN: Duration = Duration::from_secs(30);
/// How long one segment may take to fetch.
const SEGMENT_WITHIN: Duration = Duration::from_secs(10);
/// The biggest init segment and fragment Irori takes.
const INIT_MOST: usize = 64 * 1024;
const FRAGMENT_MOST: usize = 4 << 20;
/// How often the playlist is read for new segments.
const POLL: Duration = Duration::from_millis(500);
/// A playlist that hasn't grown in this long has stopped.
const STALLED: Duration = Duration::from_secs(20);

/// A media playlist, as far as watching needs it.
#[derive(Debug, Default, PartialEq)]
pub struct Media {
    /// The init segment's address.
    pub init: Option<String>,
    /// Each whole segment, oldest first, by its sequence number.
    pub segments: Vec<(u64, String)>,
}

/// What a master playlist points at: the media playlist and its codecs.
#[derive(Debug, PartialEq)]
pub struct Variant {
    pub uri: String,
    pub codecs: Option<String>,
}

/// The first variant of a master playlist, or `None` for a media playlist.
pub fn variant(playlist: &str) -> Option<Variant> {
    let mut lines = playlist.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let codecs = attribute(attributes, "CODECS");
            let uri = lines.find(|l| !l.is_empty() && !l.starts_with('#'))?;
            return Some(Variant {
                uri: uri.to_owned(),
                codecs,
            });
        }
    }
    None
}

/// A media playlist's init segment and whole segments (low-latency parts are left aside).
pub fn media(playlist: &str) -> Media {
    let mut media = Media::default();
    let mut sequence = 0u64;
    let mut segment = false;
    for line in playlist.lines().map(str::trim) {
        if let Some(number) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = number.trim().parse().unwrap_or(0);
        } else if let Some(attributes) = line.strip_prefix("#EXT-X-MAP:") {
            media.init = attribute(attributes, "URI");
        } else if line.starts_with("#EXTINF:") {
            segment = true;
        } else if !line.is_empty() && !line.starts_with('#') && segment {
            media.segments.push((sequence, line.to_owned()));
            sequence += 1;
            segment = false;
        }
    }
    media
}

/// An attribute of an HLS tag: `KEY="value"` or `KEY=value`.
fn attribute(attributes: &str, key: &str) -> Option<String> {
    let mut rest = attributes;
    while !rest.is_empty() {
        let (name, after) = rest.split_once('=')?;
        let (value, after) = if let Some(quoted) = after.strip_prefix('"') {
            let (value, after) = quoted.split_once('"')?;
            (value, after.strip_prefix(',').unwrap_or(after))
        } else {
            after.split_once(',').unwrap_or((after, ""))
        };
        if name.trim() == key {
            return Some(value.to_owned());
        }
        rest = after;
    }
    None
}

/// `reference` read against the playlist at `base` (an absolute path).
pub fn resolve(base: &str, reference: &str) -> String {
    if reference.starts_with('/') {
        return reference.to_owned();
    }
    let directory = base.split('?').next().unwrap_or(base);
    let directory = &directory[..=directory.rfind('/').unwrap_or(0)];
    // Home Assistant names them `./segment/7.m4s`; it serves them without the `./`.
    let reference = reference.trim_start_matches("./");
    format!("{directory}{reference}")
}

/// The codec families in `codecs` (`avc1.640028,mp4a.40.2` → `avc1`, `mp4a`).
fn families(codecs: &str) -> Vec<String> {
    codecs
        .split(',')
        .map(|codec| {
            let family = codec.trim().split('.').next().unwrap_or("");
            // Both are HEVC; a page names it by the one MP4 uses for its sample entry.
            if family == "hev1" { "hvc1" } else { family }.to_owned()
        })
        .filter(|family| !family.is_empty())
        .collect()
}

/// Streams `entity_id` to `live` until nobody watches, Home Assistant stops, or it fails.
pub async fn watch(
    client: Client,
    transport: Transport,
    token: String,
    entity_id: String,
    live: IncomingLive,
) {
    let IncomingLive { accepts, feed, .. } = live;
    let mut feed = feed;
    let ended = stream(&client, &transport, &token, &entity_id, &accepts, &mut feed).await;
    feed.end(ended.err());
}

async fn stream(
    client: &Client,
    transport: &Transport,
    token: &str,
    entity_id: &str,
    accepts: &[String],
    feed: &mut irori_protocol::LiveFeed,
) -> Result<(), String> {
    let answer = client
        .request(
            Outgoing::CameraStream {
                entity_id: entity_id.to_owned(),
            },
            ASK_WITHIN,
        )
        .await
        .map_err(|why| format!("Home Assistant wouldn't stream it: {why}"))?;
    let master = answer
        .get("url")
        .and_then(|url| url.as_str())
        .ok_or("Home Assistant didn't say where its stream is")?
        .to_owned();
    let text = |path: String, within: Duration| async move {
        let answer = http::get(transport, &path, token, 1 << 20, within).await?;
        let kind = answer.content_type.clone();
        String::from_utf8(answer.body).map_err(|bad| {
            let head: Vec<u8> = bad.as_bytes().iter().take(16).copied().collect();
            tracing::warn!(content_type = %kind, length = bad.as_bytes().len(), ?head, "a playlist that isn't text");
            "Home Assistant's playlist isn't text".to_owned()
        })
    };

    tracing::info!(
        entity_id,
        "Home Assistant gave a stream; reading its playlist"
    );
    // Home Assistant answers the first read once the camera has sent its first segment.
    let top = text(master.clone(), STARTING_WITHIN).await?;
    tracing::info!(entity_id, "read the stream's playlist");
    let (playlist, codecs) = match variant(&top) {
        Some(variant) => (resolve(&master, &variant.uri), variant.codecs),
        None => (master.clone(), None),
    };
    let codecs = codecs.ok_or("Home Assistant's stream doesn't say its codecs")?;
    let have = families(&codecs);
    let video = have
        .iter()
        .find(|family| ["avc1", "hvc1", "av01", "vp09"].contains(&family.as_str()))
        .ok_or("the stream has no video")?;
    if !accepts.iter().any(|family| family == video) {
        return Err(format!("this page can't play the camera's {video} video"));
    }
    if let Some(sound) = have.iter().find(|family| *family != video)
        && !accepts.iter().any(|family| family == sound)
    {
        return Err(format!("this page can't play the camera's {sound} sound"));
    }

    let mut sent_init = false;
    let mut after: Option<u64> = None;
    let mut grew_at = tokio::time::Instant::now();
    let mut fetched: BTreeSet<u64> = BTreeSet::new();
    loop {
        let media = media(&text(playlist.clone(), STARTING_WITHIN).await?);
        if !sent_init {
            tracing::debug!(
                segments = media.segments.len(),
                init = media.init.is_some(),
                "the stream's media playlist"
            );
            let init = media
                .init
                .as_deref()
                .ok_or("Home Assistant's stream has no init segment")?;
            let init = http::get(
                transport,
                &resolve(&playlist, init),
                token,
                INIT_MOST,
                SEGMENT_WITHIN,
            )
            .await?;
            feed.init(format!("video/mp4; codecs=\"{codecs}\""), init.body)
                .map_err(|_| "nobody is watching".to_owned())?;
            sent_init = true;
            // Join at the newest whole segment: the live edge.
            after = media.segments.iter().rev().nth(1).map(|(n, _)| *n);
        }
        let new: Vec<&(u64, String)> = media
            .segments
            .iter()
            .filter(|(n, _)| after.is_none_or(|after| *n > after))
            .collect();
        for (number, uri) in new {
            if !fetched.insert(*number) {
                continue;
            }
            match http::get(
                transport,
                &resolve(&playlist, uri),
                token,
                FRAGMENT_MOST,
                SEGMENT_WITHIN,
            )
            .await
            {
                Ok(segment) => {
                    if feed.fragment(true, segment.body).is_err() {
                        return Ok(());
                    }
                }
                // Too big, or gone already: the next one starts at a keyframe too.
                Err(why) => tracing::debug!(%why, "skipped a segment"),
            }
            after = Some(*number);
            grew_at = tokio::time::Instant::now();
        }
        while fetched.len() > 64 {
            fetched.pop_first();
        }
        if grew_at.elapsed() > STALLED {
            return Err("the camera stopped sending".into());
        }
        // Stops as soon as nobody watches, not at the next read.
        tokio::select! {
            () = tokio::time::sleep(POLL) => {}
            () = feed.closed() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"avc1.640028,mp4a.40.2\"\nplaylist.m3u8\n";
    const MEDIA: &str = "#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-PROGRAM-DATE-TIME:2026-11-19T03:00:00.000+00:00\n#EXTINF:2.000,\nsegment/7.m4s\n#EXT-X-PART:DURATION=1.000,URI=\"segment/8.0.m4s\",INDEPENDENT=YES\n#EXTINF:2.000,\nsegment/8.m4s\n#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"segment/9.0.m4s\"\n";

    #[test]
    fn a_master_playlist_names_its_variant_and_codecs() {
        assert_eq!(
            variant(MASTER),
            Some(Variant {
                uri: "playlist.m3u8".into(),
                codecs: Some("avc1.640028,mp4a.40.2".into()),
            })
        );
        assert_eq!(variant(MEDIA), None);
        assert_eq!(families("avc1.640028,mp4a.40.2"), ["avc1", "mp4a"]);
        assert_eq!(families("hev1.1.6.L93.B0"), ["hvc1"]);
    }

    #[test]
    fn a_media_playlist_gives_its_init_and_whole_segments() {
        assert_eq!(
            media(MEDIA),
            Media {
                init: Some("init.mp4".into()),
                segments: vec![(7, "segment/7.m4s".into()), (8, "segment/8.m4s".into())],
            }
        );
    }

    #[test]
    fn addresses_are_read_against_the_playlist() {
        let master = "/api/hls/abc/master_playlist.m3u8";
        assert_eq!(
            resolve(master, "playlist.m3u8"),
            "/api/hls/abc/playlist.m3u8"
        );
        assert_eq!(
            resolve("/api/hls/abc/playlist.m3u8?_HLS_msn=3", "segment/7.m4s"),
            "/api/hls/abc/segment/7.m4s"
        );
        assert_eq!(resolve(master, "/other/init.mp4"), "/other/init.mp4");
        assert_eq!(
            resolve("/api/hls/abc/playlist.m3u8", "./segment/7.m4s"),
            "/api/hls/abc/segment/7.m4s"
        );
    }
}
