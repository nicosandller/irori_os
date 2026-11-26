//! The history a bridged system kept before Irori did (`docs/specs/protocols.md` §5.5).
//!
//! Only a protocol whose manifest says `history = true` is asked. The core asks for one entity
//! at a time, for the stretch before Irori's own record of it begins, and keeps nothing of the
//! answer on disk: it is read again from the bridged system when it's wanted.

use irori_types::{Availability, State, Timestamp, UniqueId};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// The most changes one answer may hold. More are cut to the newest, as Irori's own are.
pub const MAX_PAST_CHANGES: usize = 2_000;
/// The most hourly points one answer may hold: a year and a day.
pub const MAX_PAST_HOURS: usize = 366 * 24;

/// One change the bridged system recorded: when, and what the entity was then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PastChange {
    /// When it changed.
    pub at: Timestamp,
    /// `unavailable` when the system had lost the device then.
    #[serde(default = "available", skip_serializing_if = "is_available")]
    pub availability: Availability,
    /// The value, as the entity's kind has it; `null` when the system didn't know it.
    #[serde(deserialize_with = "Option::deserialize")]
    pub state: Option<State>,
}

fn available() -> Availability {
    Availability::Available
}

fn is_available(availability: &Availability) -> bool {
    *availability == Availability::Available
}

/// One hour of a sensor that measures or counts, as the bridged system summarized it: the
/// average for a measurement, how much a counter changed for one that counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PastHour {
    /// The start of the hour.
    pub start: Timestamp,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// What a protocol answers: changes, or hours when hours were asked for. Oldest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Past {
    pub changes: Vec<PastChange>,
    pub hours: Vec<PastHour>,
}

/// What the core asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastAsk {
    pub unique_id: UniqueId,
    /// From when, inclusive.
    pub since: Timestamp,
    /// Until when, exclusive: where Irori's own record of the entity begins, or now.
    pub until: Timestamp,
    /// Hourly summaries rather than changes.
    pub hourly: bool,
}

/// The core asking for an entity's history from before Irori recorded it. Only sent to a
/// protocol whose manifest says `history = true`. Reply exactly once; an empty answer is fine
/// when the system kept nothing. Dropping it without replying reports a failure, and the core
/// shows Irori's own history alone.
#[derive(Debug)]
pub struct IncomingHistory {
    /// The entity's `unique_id`, as this protocol described it.
    pub unique_id: UniqueId,
    pub since: Timestamp,
    pub until: Timestamp,
    /// Hourly summaries ([`Past::hours`]) rather than changes ([`Past::changes`]).
    pub hourly: bool,
    pub(crate) reply: oneshot::Sender<Result<Past, String>>,
}

impl IncomingHistory {
    pub fn reply(self, result: Result<Past, String>) {
        let _ = self.reply.send(result);
    }
}
