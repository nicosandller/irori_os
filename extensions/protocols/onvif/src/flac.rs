//! A camera's G.711 sound, as FLAC a browser can play.
//!
//! Browsers can't play G.711 (what Tapo and many other cameras send), but they all play FLAC in
//! MP4. G.711 is 8 kHz, one byte a sample, so it's decoded to 16-bit samples and written as
//! *verbatim* FLAC frames: the samples as they are, with FLAC's headers and checksums around
//! them. That's lossless, about 128 kbit/s for one channel, and needs no encoder.
//!
//! The frame layout is RFC 9639 §9; the MP4 boxes are the "Encapsulation of FLAC in ISO Base
//! Media File Format" spec (`fLaC` sample entry, `dfLa` box).

/// The most samples one frame holds; a longer packet is split.
pub const MAX_BLOCK: usize = 4096;

/// Which G.711.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Law {
    /// PCMU, North America and Japan.
    Mu,
    /// PCMA, everywhere else.
    A,
}

impl Law {
    pub fn decode(self, byte: u8) -> i16 {
        match self {
            Self::Mu => mu_law(byte),
            Self::A => a_law(byte),
        }
    }
}

fn mu_law(byte: u8) -> i16 {
    let u = !byte;
    let exponent = (u >> 4) & 0x07;
    let mantissa = i32::from(u & 0x0f);
    let magnitude = (((mantissa << 3) + 0x84) << exponent) - 0x84;
    let value = if u & 0x80 != 0 { -magnitude } else { magnitude };
    i16::try_from(value).unwrap_or(0)
}

fn a_law(byte: u8) -> i16 {
    let a = byte ^ 0x55;
    let exponent = (a >> 4) & 0x07;
    let mantissa = i32::from(a & 0x0f);
    let magnitude = if exponent == 0 {
        (mantissa << 4) + 8
    } else {
        ((mantissa << 4) + 0x108) << (exponent - 1)
    };
    let value = if a & 0x80 != 0 { magnitude } else { -magnitude };
    i16::try_from(value).unwrap_or(0)
}

/// Writes FLAC frames for one stream.
#[derive(Debug)]
pub struct Encoder {
    rate: u32,
    channels: u8,
    /// The first sample's number in the next frame.
    next_sample: u64,
}

impl Encoder {
    /// For `channels` channels (1 to 8) at `rate` Hz (1 to 65535).
    pub fn new(rate: u32, channels: u8) -> Option<Self> {
        ((1..=65_535).contains(&rate) && (1..=8).contains(&channels)).then_some(Self {
            rate,
            channels,
            next_sample: 0,
        })
    }

    /// The MP4 sample entry (`fLaC`, holding `dfLa`) for this stream.
    pub fn sample_entry(&self) -> Vec<u8> {
        let mut streaminfo = Vec::with_capacity(34);
        // The fewest and most samples a frame holds, both said as the most. A camera's frames
        // are shorter (160 samples at 8 kHz), and every decoder tried plays them, but Safari
        // turns the whole stream away when the fewest said is under 256 or so, video and all.
        let block = u16::try_from(MAX_BLOCK).unwrap_or(u16::MAX).to_be_bytes();
        streaminfo.extend_from_slice(&block);
        streaminfo.extend_from_slice(&block);
        // Smallest and largest frame: unknown.
        streaminfo.extend_from_slice(&[0; 6]);
        // 20 bits of rate, 3 of channels - 1, 5 of bits per sample - 1, then 36 of total
        // samples (unknown: 0).
        let packed: u64 =
            (u64::from(self.rate) << 44) | (u64::from(self.channels - 1) << 41) | (15u64 << 36);
        streaminfo.extend_from_slice(&packed.to_be_bytes());
        // The audio's MD5: unknown.
        streaminfo.extend_from_slice(&[0; 16]);

        let mut dfla = Vec::new();
        // Full box: version 0, no flags.
        dfla.extend_from_slice(&[0, 0, 0, 0]);
        // The last (and only) metadata block, STREAMINFO (type 0), 34 bytes long.
        dfla.extend_from_slice(&[0x80, 0, 0, 34]);
        dfla.extend_from_slice(&streaminfo);

        let mut entry = Vec::new();
        entry.extend_from_slice(&[0; 6]);
        // Data reference index.
        entry.extend_from_slice(&1u16.to_be_bytes());
        entry.extend_from_slice(&[0; 8]);
        entry.extend_from_slice(&u16::from(self.channels).to_be_bytes());
        // Sample size, then pre-defined and reserved.
        entry.extend_from_slice(&16u16.to_be_bytes());
        entry.extend_from_slice(&[0; 4]);
        entry.extend_from_slice(&(self.rate << 16).to_be_bytes());
        put_box(&mut entry, b"dfLa", &dfla);

        let mut out = Vec::new();
        put_box(&mut out, b"fLaC", &entry);
        out
    }

    /// Encodes interleaved samples into frames of at most [`MAX_BLOCK`] samples each, with how
    /// many samples each holds.
    pub fn frames(&mut self, samples: &[i16]) -> Vec<(u32, Vec<u8>)> {
        let channels = usize::from(self.channels);
        let usable = samples.len() - samples.len() % channels;
        samples[..usable]
            .chunks(MAX_BLOCK * channels)
            .map(|chunk| {
                let block = chunk.len() / channels;
                let frame = self.frame(chunk, block);
                (u32::try_from(block).unwrap_or(0), frame)
            })
            .collect()
    }

    fn frame(&mut self, samples: &[i16], block: usize) -> Vec<u8> {
        let channels = usize::from(self.channels);
        let mut out = Vec::with_capacity(16 + samples.len() * 2 + channels);
        // Sync code and variable block size, since packets differ in length.
        out.extend_from_slice(&[0xff, 0xf9]);
        // Block size: 16 bits of block - 1 follow. Sample rate: from the table, or 16 bits of
        // Hz follow.
        let rate_code: u8 = match self.rate {
            8_000 => 0b0100,
            16_000 => 0b0101,
            22_050 => 0b0110,
            24_000 => 0b0111,
            32_000 => 0b1000,
            44_100 => 0b1001,
            48_000 => 0b1010,
            _ => 0b1101,
        };
        out.push((0b0111 << 4) | rate_code);
        // Independent channels; 16 bits a sample (0b100); reserved bit.
        out.push(((self.channels - 1) << 4) | (0b100 << 1));
        coded_number(&mut out, self.next_sample);
        let block_bytes = u16::try_from(block - 1).unwrap_or(u16::MAX).to_be_bytes();
        out.extend_from_slice(&block_bytes);
        if rate_code == 0b1101 {
            out.extend_from_slice(&u16::try_from(self.rate).unwrap_or(0).to_be_bytes());
        }
        let header_crc = crc8(&out);
        out.push(header_crc);
        for channel in 0..channels {
            // Verbatim subframe, no wasted bits.
            out.push(0b0000_0010);
            for sample in samples.iter().skip(channel).step_by(channels) {
                out.extend_from_slice(&sample.to_be_bytes());
            }
        }
        // Every subframe ends on a byte, so there's no padding before the footer.
        let footer = crc16(&out);
        out.extend_from_slice(&footer.to_be_bytes());
        self.next_sample += u64::try_from(block).unwrap_or(0);
        out
    }
}

/// A number in FLAC's UTF-8-like coding (up to 36 bits).
fn coded_number(out: &mut Vec<u8>, number: u64) {
    let number = number & ((1 << 36) - 1);
    if number < 0x80 {
        out.push(u8::try_from(number).unwrap_or(0));
        return;
    }
    // How many continuation bytes: each holds 6 bits.
    let more = match number {
        n if n < 1 << 11 => 1,
        n if n < 1 << 16 => 2,
        n if n < 1 << 21 => 3,
        n if n < 1 << 26 => 4,
        n if n < 1 << 31 => 5,
        _ => 6,
    };
    let lead_mark: u8 = !(0xffu8 >> (more + 1));
    let lead_bits = u8::try_from(number >> (6 * more)).unwrap_or(0);
    out.push(lead_mark | lead_bits);
    for index in (0..more).rev() {
        out.push(0x80 | u8::try_from((number >> (6 * index)) & 0x3f).unwrap_or(0));
    }
}

/// CRC-8, polynomial x^8 + x^2 + x + 1, starting at 0.
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16, polynomial x^16 + x^15 + x^2 + 1, starting at 0.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn put_box(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    let size = u32::try_from(body.len() + 8).unwrap_or(u32::MAX);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_decodes_to_the_reference_values() {
        // From ITU-T G.711's tables, scaled to 16 bits.
        assert_eq!(Law::Mu.decode(0xff), 0);
        assert_eq!(Law::Mu.decode(0x7f), 0);
        assert_eq!(Law::Mu.decode(0x80), 32_124);
        assert_eq!(Law::Mu.decode(0x00), -32_124);
        assert_eq!(Law::Mu.decode(0xfe), 8);
        assert_eq!(Law::A.decode(0xd5), 8);
        assert_eq!(Law::A.decode(0x55), -8);
        assert_eq!(Law::A.decode(0xaa), 32_256);
        assert_eq!(Law::A.decode(0x2a), -32_256);
    }

    #[test]
    fn the_checksums_match_their_check_values() {
        assert_eq!(crc8(b"123456789"), 0xf4);
        assert_eq!(crc16(b"123456789"), 0xfee8);
    }

    #[test]
    fn numbers_are_coded_like_utf8() {
        let coded = |number| {
            let mut out = Vec::new();
            coded_number(&mut out, number);
            out
        };
        assert_eq!(coded(0x7f), [0x7f]);
        assert_eq!(coded(0x80), [0xc2, 0x80]);
        assert_eq!(coded(0x20ac), "€".as_bytes());
        assert_eq!(coded(0x1_f600), "😀".as_bytes());
        assert_eq!(coded(1 << 31), [0xfe, 0x82, 0x80, 0x80, 0x80, 0x80, 0x80]);
    }

    #[test]
    fn a_long_packet_is_split_and_samples_are_counted() {
        let mut encoder = Encoder::new(8_000, 1).expect("an encoder");
        let frames = encoder.frames(&vec![0; MAX_BLOCK + 10]);
        let lengths: Vec<u32> = frames.iter().map(|(length, _)| *length).collect();
        assert_eq!(lengths, [4096, 10]);
        // Header, 2 bytes a sample, the subframe's byte and the footer.
        assert_eq!(frames[1].1.len(), 2 + 2 + 3 + 2 + 1 + 1 + 20 + 2);
        assert_eq!(encoder.next_sample, 4106);
        assert!(Encoder::new(0, 1).is_none());
        assert!(Encoder::new(8_000, 9).is_none());
    }

    #[test]
    fn the_sample_entry_describes_the_stream() {
        let encoder = Encoder::new(8_000, 1).expect("an encoder");
        let entry = encoder.sample_entry();
        assert_eq!(&entry[4..8], b"fLaC");
        assert_eq!(
            usize::try_from(u32::from_be_bytes(entry[..4].try_into().expect("4"))).expect("size"),
            entry.len()
        );
        let dfla = entry
            .windows(4)
            .position(|window| window == b"dfLa")
            .expect("a dfLa box");
        let streaminfo = &entry[dfla + 4 + 4 + 4..];
        assert_eq!(streaminfo.len(), 34);
        // 8000 Hz in the first 20 bits after the frame sizes.
        let packed = u64::from_be_bytes(streaminfo[10..18].try_into().expect("8"));
        assert_eq!(packed >> 44, 8_000);
        assert_eq!((packed >> 41) & 0x7, 0, "one channel");
        assert_eq!((packed >> 36) & 0x1f, 15, "16 bits a sample");
        // Safari turns away a stream whose fewest samples a frame is said to be under 256.
        let fewest = u16::from_be_bytes(streaminfo[..2].try_into().expect("2"));
        assert!(fewest >= 256, "Safari plays it: fewest {fewest}");
    }

    /// The real decoder agrees, when it's installed: `flac -t` on a file of these frames.
    #[test]
    fn the_flac_tool_accepts_the_frames() {
        let Ok(output) = std::process::Command::new("flac").arg("--version").output() else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let mut encoder = Encoder::new(8_000, 1).expect("an encoder");
        let entry = encoder.sample_entry();
        let dfla = entry
            .windows(4)
            .position(|window| window == b"dfLa")
            .expect("a dfLa box");
        let mut file = b"fLaC".to_vec();
        file.extend_from_slice(&entry[dfla + 8..]);
        // Packets of different lengths, as a camera sends them: a 440 Hz tone.
        let mut at = 0.0f64;
        for length in [160, 320, 160, 5000, 17] {
            let bytes: Vec<i16> = (0..length)
                .map(|_| {
                    at += 1.0;
                    // Rounded to an i16 well inside its range.
                    (f64::sin(at * 440.0 * std::f64::consts::TAU / 8_000.0) * 10_000.0) as i16
                })
                .collect();
            for (_, frame) in encoder.frames(&bytes) {
                file.extend_from_slice(&frame);
            }
        }
        let dir = std::env::temp_dir().join(format!("irori-flac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let path = dir.join("tone.flac");
        std::fs::write(&path, &file).expect("written");
        let tested = std::process::Command::new("flac")
            .args(["-t", "-s"])
            .arg(&path)
            .output()
            .expect("flac ran");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            tested.status.success(),
            "flac -t: {}",
            String::from_utf8_lossy(&tested.stderr)
        );
    }

    /// ffmpeg plays these frames in the fragmented MP4 a page gets, when it's installed.
    #[test]
    fn ffmpeg_plays_flac_in_the_live_mp4() {
        use crate::fmp4::{self, Media, Sample, Track};
        let Ok(output) = std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let mut encoder = Encoder::new(8_000, 1).expect("an encoder");
        let mut file = fmp4::init(&[Track {
            id: 2,
            timescale: 8_000,
            media: Media::Audio,
            sample_entry: encoder.sample_entry(),
        }]);
        let mut at = 0u64;
        for sequence in 1..=50u32 {
            let packet: Vec<i16> = (0..160u32)
                .map(|n| Law::Mu.decode(u8::try_from((n * 7) % 256).unwrap_or(0)) / 4)
                .collect();
            let samples: Vec<Sample> = encoder
                .frames(&packet)
                .into_iter()
                .map(|(duration, data)| Sample {
                    duration,
                    key: true,
                    data,
                })
                .collect();
            file.extend_from_slice(&fmp4::fragment(sequence, 2, at, &samples));
            at += 160;
        }
        let dir = std::env::temp_dir().join(format!("irori-flac-mp4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let path = dir.join("sound.mp4");
        std::fs::write(&path, &file).expect("written");
        let played = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg ran");
        let _ = std::fs::remove_dir_all(&dir);
        let errors = String::from_utf8_lossy(&played.stderr);
        assert!(
            played.status.success() && errors.trim().is_empty(),
            "ffmpeg: {errors}"
        );
    }
}
