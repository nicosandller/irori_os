//! What a run leaves behind (`docs/specs/flows.md` §6): its record, step by step, and the
//! near-misses of firings that didn't become runs.

use irori_types::{Availability, ContextId, EntityId, RuleId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{NodeId, Port, Wire};

/// One entity as a step read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Read {
    pub entity_id: EntityId,
    /// Absent when there was no such entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,
    /// Its value, `null` if unknown.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// How a run was started by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
    /// For real, from the saved flow.
    Live,
    /// Calls recorded, not sent; time fast-forwarded; nothing changes meanwhile.
    Dry,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Every path ran to its end.
    Completed,
    /// A path ended at a failure nothing was wired to.
    Error { node: NodeId, message: String },
    /// A `stop` node ended it.
    Stopped {
        node: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A new firing started over (`restart`).
    Superseded,
    /// Cut short from outside.
    Aborted { reason: AbortReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    Disabled,
    Changed,
    Removed,
    Cancelled,
    Shutdown,
}

/// Where a path ended: the node, and the port it left by (none for a `stop`, or a path that
/// ended inside a node).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ending {
    pub node: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<Port>,
}

/// One node running for one token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub seq: u32,
    pub node: NodeId,
    /// Which path of the run: paths fork into new tokens.
    pub token: u32,
    /// The wire it came in on; absent for the trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Wire>,
    pub at: Timestamp,
    /// Absent while it's still going (a wait, a call).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// The port it left by; absent while going, or when the path ended here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<Read>,
    /// What the node did, in words: "num('sensor.lux') < 30 → false (lux 42)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// For a call: what was asked and what it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<CallDetail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallDetail {
    pub service: String,
    pub entity_id: EntityId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// `None` while waiting for the answer; `Some(Ok)`, or the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Result<(), String>>,
    /// Recorded instead of sent (a dry run or a backtest).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub simulated: bool,
}

/// A whole run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: ContextId,
    pub flow_id: RuleId,
    pub version: String,
    pub trigger: NodeId,
    /// The context of the change that fired it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<ContextId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestKind>,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// Absent while it's going.
    #[serde(default, skip_serializing_if = "Option::is_none", flatten)]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ended_at: Vec<Ending>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Said when a dry run had to assume something, e.g. that nothing changed during a wait.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

/// Why a firing didn't become a run, or a trigger couldn't fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NearMissKind {
    /// The mode refused it.
    Dropped,
    /// A `for` was counting and the value changed back.
    HoldReset,
    /// A watched entity became unavailable or unknown.
    Unavailable,
    /// Reported again with the value it already had.
    SameValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NearMiss {
    pub at: Timestamp,
    pub flow_id: RuleId,
    pub version: String,
    pub node: NodeId,
    pub kind: NearMissKind,
    /// In words: "motion went on, but a run was already going and the mode is single".
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<Read>,
}
