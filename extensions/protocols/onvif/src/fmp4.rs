//! Fragmented MP4, the container a browser's Media Source Extensions play
//! (`docs/specs/protocols.md` §5.3).
//!
//! An init segment says what the tracks are (`ftyp` + `moov`, with `mvex` so the samples come
//! later). Then each fragment is a `moof` + `mdat` carrying samples of one track. The codec's
//! own description (`avc1`/`hvc1`/`mp4a`… sample entry) comes from `retina`, already built;
//! this only puts the boxes around it.
//!
//! ISO/IEC 14496-12 is the reference; section numbers below are from it.

/// What a track carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    Video { width: u16, height: u16 },
    Audio,
}

/// One track in the init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// 1 and up.
    pub id: u32,
    /// Ticks per second of its timestamps: 90000 for video over RTP, the sample rate for audio.
    pub timescale: u32,
    pub media: Media,
    /// The codec's whole sample entry box (`avc1`, `hvc1`, `mp4a`, `fLaC`).
    pub sample_entry: Vec<u8>,
}

/// One sample in a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// In the track's timescale.
    pub duration: u32,
    /// Whether it can be decoded on its own (a keyframe; every audio sample).
    pub key: bool,
    pub data: Vec<u8>,
}

/// Sample flags (§8.8.3.1): a keyframe depends on nothing; anything else depends on others and
/// isn't a sync sample.
const KEY_FLAGS: u32 = 0x0200_0000;
const DELTA_FLAGS: u32 = 0x0101_0000;

/// The init segment for these tracks.
pub fn init(tracks: &[Track]) -> Vec<u8> {
    let mut out = Vec::new();
    // §4.3: iso5 is the first brand with `default-base-is-moof`, which fragments rely on.
    put_box(&mut out, b"ftyp", |b| {
        b.extend_from_slice(b"iso5");
        b.extend_from_slice(&512u32.to_be_bytes());
        for brand in [b"iso5", b"iso6", b"mp41"] {
            b.extend_from_slice(brand);
        }
    });
    put_box(&mut out, b"moov", |moov| {
        mvhd(
            moov,
            tracks.iter().map(|track| track.id).max().unwrap_or(0) + 1,
        );
        for track in tracks {
            trak(moov, track);
        }
        put_box(moov, b"mvex", |mvex| {
            for track in tracks {
                // §8.8.3: every default left to the fragments.
                full_box(mvex, b"trex", 0, 0, |b| {
                    b.extend_from_slice(&track.id.to_be_bytes());
                    b.extend_from_slice(&1u32.to_be_bytes());
                    b.extend_from_slice(&[0; 12]);
                });
            }
        });
    });
    out
}

/// One fragment: `samples` of track `track_id`, the first decoded at `decode_time`.
/// `sequence` goes up by one with every fragment of the stream, whatever the track.
pub fn fragment(sequence: u32, track_id: u32, decode_time: u64, samples: &[Sample]) -> Vec<u8> {
    let mut moof = Vec::new();
    put_box(&mut moof, b"moof", |moof| {
        full_box(moof, b"mfhd", 0, 0, |b| {
            b.extend_from_slice(&sequence.to_be_bytes());
        });
        put_box(moof, b"traf", |traf| {
            // default-base-is-moof: data offsets count from the start of this `moof`.
            full_box(traf, b"tfhd", 0, 0x02_0000, |b| {
                b.extend_from_slice(&track_id.to_be_bytes());
            });
            full_box(traf, b"tfdt", 1, 0, |b| {
                b.extend_from_slice(&decode_time.to_be_bytes());
            });
            // data offset, and each sample's duration, size and flags.
            full_box(traf, b"trun", 0, 0x00_0701, |b| {
                b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
                b.extend_from_slice(&0i32.to_be_bytes());
                for sample in samples {
                    b.extend_from_slice(&sample.duration.to_be_bytes());
                    b.extend_from_slice(&(sample.data.len() as u32).to_be_bytes());
                    let flags = if sample.key { KEY_FLAGS } else { DELTA_FLAGS };
                    b.extend_from_slice(&flags.to_be_bytes());
                }
            });
        });
    });
    let data_offset = (moof.len() + 8) as i32;
    moof[TRUN_DATA_OFFSET..TRUN_DATA_OFFSET + 4].copy_from_slice(&data_offset.to_be_bytes());

    let size: usize = samples.iter().map(|sample| sample.data.len()).sum();
    let mut out = moof;
    out.reserve(size + 8);
    out.extend_from_slice(&((size + 8) as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for sample in samples {
        out.extend_from_slice(&sample.data);
    }
    out
}

/// Where the trun's data offset is in a `moof` written by [`fragment`]: past the moof's header
/// (8), mfhd (16), traf's header (8), tfhd (16) and tfdt (20), then the trun's header (8),
/// version and flags (4) and sample count (4).
const TRUN_DATA_OFFSET: usize = 8 + 16 + 8 + 16 + 20 + 8 + 4 + 4;

/// The `codecs` a page passes to `MediaSource.isTypeSupported`.
pub fn mime(codecs: &[&str]) -> String {
    format!("video/mp4; codecs=\"{}\"", codecs.join(", "))
}

fn mvhd(out: &mut Vec<u8>, next_track: u32) {
    // §8.2.2
    full_box(out, b"mvhd", 0, 0, |b| {
        b.extend_from_slice(&[0; 8]); // creation and modification time
        b.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        b.extend_from_slice(&0u32.to_be_bytes()); // duration: unknown, it's live
        b.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        b.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        b.extend_from_slice(&[0; 10]);
        matrix(b);
        b.extend_from_slice(&[0; 24]); // pre_defined
        b.extend_from_slice(&next_track.to_be_bytes());
    });
}

fn trak(out: &mut Vec<u8>, track: &Track) {
    put_box(out, b"trak", |trak| {
        // §8.3.2: enabled and in the movie.
        full_box(trak, b"tkhd", 0, 0x00_0003, |b| {
            b.extend_from_slice(&[0; 8]);
            b.extend_from_slice(&track.id.to_be_bytes());
            b.extend_from_slice(&[0; 4]);
            b.extend_from_slice(&0u32.to_be_bytes()); // duration
            b.extend_from_slice(&[0; 8]);
            b.extend_from_slice(&0u16.to_be_bytes()); // layer
            b.extend_from_slice(&0u16.to_be_bytes()); // alternate group
            let volume: u16 = match track.media {
                Media::Audio => 0x0100,
                Media::Video { .. } => 0,
            };
            b.extend_from_slice(&volume.to_be_bytes());
            b.extend_from_slice(&[0; 2]);
            matrix(b);
            let (width, height) = match track.media {
                Media::Video { width, height } => (width, height),
                Media::Audio => (0, 0),
            };
            b.extend_from_slice(&(u32::from(width) << 16).to_be_bytes());
            b.extend_from_slice(&(u32::from(height) << 16).to_be_bytes());
        });
        put_box(trak, b"mdia", |mdia| {
            full_box(mdia, b"mdhd", 0, 0, |b| {
                b.extend_from_slice(&[0; 8]);
                b.extend_from_slice(&track.timescale.to_be_bytes());
                b.extend_from_slice(&0u32.to_be_bytes());
                b.extend_from_slice(&0x55c4u16.to_be_bytes()); // "und"
                b.extend_from_slice(&[0; 2]);
            });
            let (handler, name): (&[u8; 4], &[u8]) = match track.media {
                Media::Video { .. } => (b"vide", b"Video\0"),
                Media::Audio => (b"soun", b"Sound\0"),
            };
            full_box(mdia, b"hdlr", 0, 0, |b| {
                b.extend_from_slice(&[0; 4]);
                b.extend_from_slice(handler);
                b.extend_from_slice(&[0; 12]);
                b.extend_from_slice(name);
            });
            put_box(mdia, b"minf", |minf| {
                match track.media {
                    Media::Video { .. } => full_box(minf, b"vmhd", 0, 1, |b| {
                        b.extend_from_slice(&[0; 8]);
                    }),
                    Media::Audio => full_box(minf, b"smhd", 0, 0, |b| {
                        b.extend_from_slice(&[0; 4]);
                    }),
                }
                put_box(minf, b"dinf", |dinf| {
                    full_box(dinf, b"dref", 0, 0, |b| {
                        b.extend_from_slice(&1u32.to_be_bytes());
                        // The media is in this file.
                        full_box(b, b"url ", 0, 1, |_| {});
                    });
                });
                put_box(minf, b"stbl", |stbl| {
                    full_box(stbl, b"stsd", 0, 0, |b| {
                        b.extend_from_slice(&1u32.to_be_bytes());
                        b.extend_from_slice(&track.sample_entry);
                    });
                    // No samples here: they come in fragments.
                    for kind in [b"stts", b"stsc", b"stco"] {
                        full_box(stbl, kind, 0, 0, |b| b.extend_from_slice(&[0; 4]));
                    }
                    full_box(stbl, b"stsz", 0, 0, |b| b.extend_from_slice(&[0; 8]));
                });
            });
        });
    });
}

fn matrix(out: &mut Vec<u8>) {
    for value in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn put_box(out: &mut Vec<u8>, kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(kind);
    body(out);
    let size = (out.len() - start) as u32;
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

fn full_box(
    out: &mut Vec<u8>,
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    body: impl FnOnce(&mut Vec<u8>),
) {
    put_box(out, kind, |b| {
        b.push(version);
        b.extend_from_slice(&flags.to_be_bytes()[1..]);
        body(b);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boxes at the top of `data`, by kind and size, each read from its header.
    fn boxes(data: &[u8]) -> Vec<(String, usize)> {
        let mut at = 0;
        let mut out = Vec::new();
        while at + 8 <= data.len() {
            let size = u32::from_be_bytes(data[at..at + 4].try_into().expect("4")) as usize;
            let kind = String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned();
            assert!(size >= 8 && at + size <= data.len(), "{kind} overruns");
            out.push((kind, size));
            at += size;
        }
        assert_eq!(at, data.len(), "nothing left over");
        out
    }

    /// The boxes inside the box at the start of `data`, skipping `skip` bytes of its own fields.
    fn inside(data: &[u8], skip: usize) -> Vec<(String, usize)> {
        let size = u32::from_be_bytes(data[..4].try_into().expect("4")) as usize;
        boxes(&data[8 + skip..size])
    }

    fn video() -> Track {
        Track {
            id: 1,
            timescale: 90_000,
            media: Media::Video {
                width: 896,
                height: 336,
            },
            sample_entry: {
                let mut entry = Vec::new();
                put_box(&mut entry, b"avc1", |b| b.extend_from_slice(&[0; 78]));
                entry
            },
        }
    }

    #[test]
    fn an_init_segment_is_ftyp_then_moov_with_its_tracks() {
        let audio = Track {
            id: 2,
            timescale: 16_000,
            media: Media::Audio,
            sample_entry: {
                let mut entry = Vec::new();
                put_box(&mut entry, b"mp4a", |b| b.extend_from_slice(&[0; 28]));
                entry
            },
        };
        let init = init(&[video(), audio]);
        let top = boxes(&init);
        assert_eq!(
            top.iter()
                .map(|(kind, _)| kind.as_str())
                .collect::<Vec<_>>(),
            ["ftyp", "moov"]
        );
        let moov = &init[top[0].1..];
        let kinds: Vec<String> = inside(moov, 0).into_iter().map(|(kind, _)| kind).collect();
        assert_eq!(kinds, ["mvhd", "trak", "trak", "mvex"]);
        // The sample entry went in whole, under stsd.
        assert!(init.windows(4).any(|window| window == b"avc1"));
        assert!(init.windows(4).any(|window| window == b"mp4a"));
        assert!(init.windows(4).any(|window| window == b"smhd"));
    }

    #[test]
    fn a_fragment_points_its_samples_at_its_mdat() {
        let samples = [
            Sample {
                duration: 3000,
                key: true,
                data: vec![1, 2, 3],
            },
            Sample {
                duration: 3000,
                key: false,
                data: vec![4, 5],
            },
        ];
        let fragment = fragment(7, 1, 123_456, &samples);
        let top = boxes(&fragment);
        assert_eq!(
            top.iter()
                .map(|(kind, _)| kind.as_str())
                .collect::<Vec<_>>(),
            ["moof", "mdat"]
        );
        let moof_size = top[0].1;
        let at = TRUN_DATA_OFFSET;
        assert_eq!(&fragment[at - 12..at - 8], b"trun");
        let offset = i32::from_be_bytes(fragment[at..at + 4].try_into().expect("4")) as usize;
        assert_eq!(offset, moof_size + 8, "right after mdat's header");
        assert_eq!(&fragment[offset..], &[1, 2, 3, 4, 5]);
        // The decode time, as tfdt version 1 writes it.
        let tfdt = fragment
            .windows(4)
            .position(|window| window == b"tfdt")
            .expect("tfdt");
        assert_eq!(fragment[tfdt + 4], 1, "version 1");
        let time = u64::from_be_bytes(fragment[tfdt + 8..tfdt + 16].try_into().expect("8"));
        assert_eq!(time, 123_456);
        // The second sample isn't a sync sample.
        let flags_at = at + 4 + 4 + 4;
        assert_eq!(&fragment[flags_at..flags_at + 4], &KEY_FLAGS.to_be_bytes());
        assert_eq!(
            &fragment[flags_at + 12..flags_at + 16],
            &DELTA_FLAGS.to_be_bytes()
        );
    }

    #[test]
    fn the_mime_lists_every_codec() {
        assert_eq!(
            mime(&["avc1.640033", "mp4a.40.2"]),
            "video/mp4; codecs=\"avc1.640033, mp4a.40.2\""
        );
    }
}
