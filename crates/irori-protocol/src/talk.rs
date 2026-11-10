//! Talking through a camera (`docs/specs/protocols.md` §5.4): a page's microphone, out of the
//! camera's speaker.
//!
//! The sound is the plainest there is, so nothing between the page and the protocol needs to
//! know about codecs: 16-bit signed little-endian PCM, one channel, 8000 samples a second, the
//! rate every camera speaker takes. The protocol turns it into whatever its camera wants.
//!
//! Like live video, it never waits for anyone. A full queue drops a chunk rather than holding up
//! the page, the pipe or the core: a missing 20 ms of speech is a click, a queue that backs up
//! is a voice arriving seconds late.

use std::fmt;

use irori_types::UniqueId;
use tokio::sync::mpsc;

/// Samples a second.
pub const TALK_RATE: u32 = 8000;

/// The most one chunk of sound may be, in bytes: a quarter of a second. A page sends 20 ms
/// (320 bytes) at a time.
pub const MAX_TALK_CHUNK: usize = 4096;

/// How many chunks wait between two hops before new ones are dropped: a second of 20 ms
/// chunks.
pub const TALK_QUEUE: usize = 50;

/// What the protocol says about a talk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TalkWord {
    /// The camera is listening: what's sent now comes out of its speaker.
    Ready,
    /// The talk is over, and why when it's a failure.
    End { error: Option<String> },
}

/// The protocol's side of a talk: how it tells the page the camera is listening, or that it
/// stopped.
#[derive(Debug)]
pub struct TalkLine {
    tx: mpsc::Sender<TalkWord>,
}

impl TalkLine {
    /// A line and the receiving end its words arrive on.
    pub fn channel() -> (Self, mpsc::Receiver<TalkWord>) {
        // Two words at most: ready, then the end.
        let (tx, rx) = mpsc::channel(2);
        (Self { tx }, rx)
    }

    /// The camera is listening.
    pub fn ready(&self) {
        let _ = self.tx.try_send(TalkWord::Ready);
    }

    /// The talk is over, with why when it failed. Dropping the line says it ended too.
    pub fn end(self, error: Option<String>) {
        let _ = self.tx.try_send(TalkWord::End { error });
    }

    /// Whether the far end has gone.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Passes a word on as it is.
    pub fn forward(&self, word: TalkWord) {
        let _ = self.tx.try_send(word);
    }
}

/// A page wants to talk through one of this protocol's cameras (`docs/specs/protocols.md` §5.4).
/// Only sent for a `camera` that says it can `talk`, and only one at a time per camera.
///
/// Open the camera's speaker, say [`TalkLine::ready`], then play each chunk from `audio` as it
/// comes (see the module docs for its format). When `audio` ends, or nothing has arrived for a
/// few seconds, close the speaker and [`TalkLine::end`]. Answer quickly: the page waits about
/// ten seconds for ready.
pub struct IncomingTalk {
    /// The camera entity's `unique_id`, as this protocol described it.
    pub unique_id: UniqueId,
    pub audio: mpsc::Receiver<Vec<u8>>,
    pub line: TalkLine,
}

impl fmt::Debug for IncomingTalk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IncomingTalk")
            .field("unique_id", &self.unique_id)
            .finish_non_exhaustive()
    }
}

/// Whether a chunk of sound may be passed on: not empty, whole samples, and no bigger than
/// [`MAX_TALK_CHUNK`].
pub fn check(chunk: &[u8]) -> Result<(), String> {
    if chunk.is_empty() || chunk.len() > MAX_TALK_CHUNK {
        return Err(format!(
            "a chunk of sound is 1 to {MAX_TALK_CHUNK} bytes, and this one is {}",
            chunk.len()
        ));
    }
    if chunk.len() % 2 != 0 {
        return Err("sound comes in whole 16-bit samples".into());
    }
    Ok(())
}

/// Sound off the extension pipe: base64, checked for size before decoding so an oversized line
/// costs nothing.
pub fn decode(data: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    if data.len() > MAX_TALK_CHUNK.div_ceil(3) * 4 {
        return Err(format!("a chunk of sound is up to {MAX_TALK_CHUNK} bytes"));
    }
    let chunk = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("sound that isn't base64: {e}"))?;
    check(&chunk)?;
    Ok(chunk)
}

/// Sound for the extension pipe: base64.
pub fn encode(chunk: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(chunk)
}

/// The samples in a chunk.
pub fn samples(chunk: &[u8]) -> impl Iterator<Item = i16> + '_ {
    chunk
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_is_whole_samples_and_not_too_much() {
        assert!(check(&[0; 320]).is_ok());
        assert!(check(&[]).is_err());
        assert!(check(&[0; 321]).is_err(), "half a sample");
        assert!(check(&vec![0; MAX_TALK_CHUNK + 2]).is_err());
    }

    #[test]
    fn sound_off_the_pipe_is_checked_before_it_is_decoded() {
        use base64::Engine as _;
        let ok = base64::engine::general_purpose::STANDARD.encode([1u8, 0, 255, 255]);
        assert_eq!(decode(&ok), Ok(vec![1, 0, 255, 255]));
        assert_eq!(
            samples(&decode(&ok).expect("decodes")).collect::<Vec<_>>(),
            vec![1, -1]
        );
        assert!(decode(&"A".repeat(MAX_TALK_CHUNK * 2)).is_err());
        assert!(decode("not base64!").is_err());
    }

    #[test]
    fn a_line_says_ready_then_ends() {
        let (line, mut words) = TalkLine::channel();
        line.ready();
        line.end(Some("the camera hung up".into()));
        assert_eq!(words.try_recv(), Ok(TalkWord::Ready));
        assert_eq!(
            words.try_recv(),
            Ok(TalkWord::End {
                error: Some("the camera hung up".into())
            })
        );
    }
}
