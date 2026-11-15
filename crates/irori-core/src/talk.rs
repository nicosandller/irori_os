//! Talking through cameras (`docs/specs/protocols.md` §5.4): one person at a time per camera.
//!
//! A camera's speaker plays one voice. A second person who tries while someone is talking is
//! told so, rather than both voices being mixed or one cutting the other off.

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use irori_protocol::TalkWord;
use irori_types::EntityId;
use tokio::sync::mpsc;

/// How long a camera has to say it's listening once asked.
pub const TALK_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The cameras someone is talking through now.
#[derive(Debug, Default)]
pub(crate) struct TalkSlots {
    taken: Mutex<HashSet<EntityId>>,
}

impl TalkSlots {
    /// Takes the camera's slot, or `None` when someone else has it.
    pub fn take(self: &Arc<Self>, camera: &EntityId) -> Option<Slot> {
        let mut taken = self.taken.lock().unwrap_or_else(PoisonError::into_inner);
        taken.insert(camera.clone()).then(|| Slot {
            slots: Arc::clone(self),
            camera: camera.clone(),
        })
    }
}

/// A camera's slot, given back when dropped.
#[derive(Debug)]
pub(crate) struct Slot {
    slots: Arc<TalkSlots>,
    camera: EntityId,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.slots
            .taken
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.camera);
    }
}

/// A talk under way: sound goes into `audio` (16-bit little-endian PCM at 8 kHz, one channel,
/// [`irori_protocol::talk::MAX_TALK_CHUNK`] at most per chunk), and the protocol's words come
/// back on `words`. Dropping it ends the talk and frees the camera for someone else.
#[derive(Debug)]
pub struct Talking {
    pub audio: mpsc::Sender<Vec<u8>>,
    pub words: mpsc::Receiver<TalkWord>,
    _slot: Slot,
}

impl Talking {
    pub(crate) fn new(
        audio: mpsc::Sender<Vec<u8>>,
        words: mpsc::Receiver<TalkWord>,
        slot: Slot,
    ) -> Self {
        Self {
            audio,
            words,
            _slot: slot,
        }
    }
}

/// Why a talk couldn't start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TalkError {
    /// No such camera, or it has no speaker.
    Cant(String),
    /// Someone else is talking through it.
    Busy,
    /// Its extension isn't running, or is too busy to take it.
    NotRunning(String),
}

impl fmt::Display for TalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cant(why) | Self::NotRunning(why) => f.write_str(why),
            Self::Busy => f.write_str("someone else is talking through this camera"),
        }
    }
}

impl std::error::Error for TalkError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_person_talks_through_a_camera_at_a_time() {
        let slots = Arc::new(TalkSlots::default());
        let porch = EntityId::try_from("camera.porch").expect("valid");
        let gate = EntityId::try_from("camera.gate").expect("valid");
        let first = slots.take(&porch).expect("free");
        assert!(slots.take(&porch).is_none(), "taken");
        assert!(slots.take(&gate).is_some(), "another camera is its own");
        drop(first);
        assert!(slots.take(&porch).is_some(), "given back");
    }
}
