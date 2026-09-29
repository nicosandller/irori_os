//! What the extension's page and its engine say to each other (`docs/specs/flows.md` §8).

use irori_types::{ContextId, EntityId, EntityState, Name, RuleId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::trace::{NearMiss, RunRecord};
use crate::{Flow, NodeId};

/// How bad a problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing; the flow still runs.
    Warning,
    /// The flow can't run until it's fixed.
    Error,
}

/// Something wrong with a flow, pinned to where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    /// Index into `wires`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<usize>,
    /// Which part of the node, e.g. `trigger/to`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub message: String,
}

impl Problem {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Whether a flow is running, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Armed {
    Armed,
    Disabled,
    /// It has problems that stop it.
    Unarmed {
        reason: String,
    },
}

/// A row of the flow list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowSummary {
    pub id: RuleId,
    pub name: Name,
    pub enabled: bool,
    #[serde(flatten)]
    pub armed: Armed,
    pub problems: usize,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunSummary>,
    pub near_misses: usize,
    pub active_runs: usize,
    /// Nodes by kind, for the row's little picture.
    pub triggers: usize,
    pub nodes: usize,
}

/// A run in a list: enough to pick it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: ContextId,
    pub version: String,
    pub trigger: NodeId,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// `completed`, `error`, … or `running`.
    pub outcome: String,
    /// "ended at dark → no".
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<crate::trace::TestKind>,
}

/// A flow as the editor opens it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowDetail {
    pub flow: Flow,
    pub version: String,
    #[serde(flatten)]
    pub armed: Armed,
    pub problems: Vec<Problem>,
}

/// What saving answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub version: String,
    pub problems: Vec<Problem>,
}

/// One saved definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionEntry {
    pub version: String,
    pub saved_at: Timestamp,
    /// The definition as saved, layout included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
}

/// Where a run in progress is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveRun {
    pub record: RunRecord,
    pub at: Vec<TokenAt>,
}

/// One path of a run in progress: at which node, doing what, until when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenAt {
    pub node: NodeId,
    /// `waiting`, `delaying`, `calling`, `joining`.
    pub doing: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Timestamp>,
    /// For a wait with `for`: since when the level has held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holding_since: Option<Timestamp>,
}

/// What `test` asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRequest {
    /// The draft to test; the saved flow when absent (and always for a live test).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
    pub id: RuleId,
    pub trigger: NodeId,
    pub dry: bool,
    /// Values to pretend, for a dry run: `{ "sensor.lux": 20 }`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub overrides: std::collections::BTreeMap<EntityId, serde_json::Value>,
}

/// What `backtest` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backtest {
    /// The window it really covered: history is capped per entity.
    pub from: Timestamp,
    pub to: Timestamp,
    /// The runs the draft would have made.
    pub would: Vec<RunRecord>,
    /// The runs the saved flow actually made in the window.
    pub did: Vec<RunSummary>,
    /// How many state changes it replayed.
    pub changes: usize,
}

/// What `timeline` answers: everything around a moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub from: Timestamp,
    pub to: Timestamp,
    pub changes: Vec<EntityState>,
    pub near_misses: Vec<NearMiss>,
    pub runs: Vec<RunSummary>,
}

impl RunRecord {
    /// The record as a list row.
    pub fn summary(&self) -> RunSummary {
        use crate::trace::Outcome;
        let outcome = match &self.outcome {
            None => "running",
            Some(Outcome::Completed) => "completed",
            Some(Outcome::Error { .. }) => "error",
            Some(Outcome::Stopped { .. }) => "stopped",
            Some(Outcome::Superseded) => "superseded",
            Some(Outcome::Aborted { .. }) => "aborted",
        };
        let ends: Vec<String> = self
            .ended_at
            .iter()
            .map(|end| match end.port {
                Some(port) => format!("{} → {port}", end.node),
                None => end.node.to_string(),
            })
            .collect();
        let summary = match &self.outcome {
            Some(Outcome::Error { node, message }) => format!("failed at {node}: {message}"),
            Some(Outcome::Stopped { node, .. }) => format!("stopped at {node}"),
            Some(Outcome::Superseded) => "started over by a new firing".to_owned(),
            Some(Outcome::Aborted { reason }) => format!("aborted: {reason:?}").to_lowercase(),
            _ if ends.is_empty() => String::new(),
            _ => format!("ended at {}", ends.join(", ")),
        };
        RunSummary {
            run_id: self.run_id.clone(),
            version: self.version.clone(),
            trigger: self.trigger.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            outcome: outcome.to_owned(),
            summary,
            test: self.test,
        }
    }
}
