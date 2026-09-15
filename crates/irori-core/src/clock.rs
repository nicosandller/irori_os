//! Time, injected (ROADMAP D10): the core never reads the wall clock directly, so tests and, later,
//! rule replays control what "now" is.

use std::fmt;

use irori_types::Timestamp;

pub trait Clock: Send + Sync + fmt::Debug + 'static {
    fn now(&self) -> Timestamp;
}

/// The real wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_jiff(jiff::Timestamp::now())
    }
}
