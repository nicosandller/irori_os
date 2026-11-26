//! The engine's methods, typed (`docs/specs/flows.md` §8).

use irori_flow_types::Flow;
use irori_flow_types::api::{
    ActiveRun, Backtest, FlowDetail, FlowSummary, Live, Problem, RunSummary, Saved, TestRequest,
    Timeline, VersionEntry,
};
use irori_flow_types::trace::{NearMiss, RunRecord};
use irori_types::Timestamp;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::bridge;

async fn call<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, String> {
    bridge().rpc(method, params).await
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FileProblem {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Listing {
    pub flows: Vec<FlowSummary>,
    #[serde(default)]
    pub file_problems: Vec<FileProblem>,
    /// Whether Home Assistant's devices are in this home, so flows say they are Irori's.
    #[serde(default)]
    pub home_assistant: bool,
}

/// One of Home Assistant's own automations, as the engine lists them for bringing over.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HaAutomation {
    pub entity_id: String,
    pub name: String,
    /// Whether it's switched on in Home Assistant.
    pub on: bool,
    /// Whether Home Assistant keeps a setup of it that can be read (one written in YAML without
    /// an `id` has none).
    #[serde(default)]
    pub readable: bool,
    /// The flow brought over from it, if it was.
    #[serde(default)]
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct HaAutomations {
    automations: Vec<HaAutomation>,
}

/// Home Assistant's own automations. Reading them changes nothing in Home Assistant.
pub async fn ha_automations() -> Result<Vec<HaAutomation>, String> {
    call::<HaAutomations>("home_assistant.automations", json!({}))
        .await
        .map(|found| found.automations)
}

/// What bringing one over did: the flow it made (or had made before), or why it can't come
/// over, and then nothing was made.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BroughtOver {
    #[serde(default)]
    pub flow: Option<String>,
    #[serde(default)]
    pub already: bool,
    #[serde(default)]
    pub untranslatable: Vec<String>,
}

pub async fn bring_over(entity_id: &str) -> Result<BroughtOver, String> {
    call(
        "home_assistant.bring_over",
        json!({ "entity_id": entity_id }),
    )
    .await
}

pub async fn list() -> Result<Listing, String> {
    call("flows.list", json!({})).await
}

pub async fn get(id: &str) -> Result<FlowDetail, String> {
    call("flows.get", json!({ "id": id })).await
}

pub async fn validate(flow: &Flow) -> Result<Vec<Problem>, String> {
    call("flows.validate", json!({ "flow": flow })).await
}

pub async fn save(flow: &Flow) -> Result<Saved, String> {
    call("flows.save", json!({ "flow": flow })).await
}

pub async fn enable(id: &str, enabled: bool) -> Result<Value, String> {
    call("flows.enable", json!({ "id": id, "enabled": enabled })).await
}

pub async fn delete(id: &str) -> Result<Value, String> {
    call("flows.delete", json!({ "id": id })).await
}

pub async fn versions(id: &str) -> Result<Vec<VersionEntry>, String> {
    call("versions.list", json!({ "id": id })).await
}

pub async fn version(id: &str, version: &str) -> Result<VersionEntry, String> {
    call("versions.get", json!({ "id": id, "version": version })).await
}

pub async fn restore(id: &str, version: &str) -> Result<Saved, String> {
    call("versions.restore", json!({ "id": id, "version": version })).await
}

pub async fn runs(id: &str) -> Result<Vec<RunSummary>, String> {
    call("runs.list", json!({ "id": id })).await
}

pub async fn run(id: &str, run_id: &str) -> Result<RunRecord, String> {
    call("runs.get", json!({ "id": id, "run_id": run_id })).await
}

pub async fn active(id: &str) -> Result<Vec<ActiveRun>, String> {
    call("runs.active", json!({ "id": id })).await
}

pub async fn cancel(run_id: &str) -> Result<bool, String> {
    call("runs.cancel", json!({ "run_id": run_id })).await
}

pub async fn near_misses(id: &str) -> Result<Vec<NearMiss>, String> {
    call("nearmiss.list", json!({ "id": id })).await
}

pub async fn timeline(id: &str, from: Timestamp, to: Timestamp) -> Result<Timeline, String> {
    call("timeline", json!({ "id": id, "from": from, "to": to })).await
}

pub async fn test(request: &TestRequest) -> Result<RunRecord, String> {
    call(
        "test",
        serde_json::to_value(request).map_err(|e| e.to_string())?,
    )
    .await
}

/// How a test of this flow was last set up, as the page left it (`null` if never).
pub async fn test_settings(id: &str) -> Result<Value, String> {
    call("tests.get", json!({ "id": id })).await
}

pub async fn save_test_settings(id: &str, settings: &Value) -> Result<Value, String> {
    call("tests.save", json!({ "id": id, "settings": settings })).await
}

pub async fn backtest(id: &str, draft: Option<&Flow>) -> Result<Backtest, String> {
    call("backtest", json!({ "id": id, "flow": draft })).await
}

/// What happened in the flow since `after`, for the canvas to play (`None`: just start watching).
pub async fn live(id: &str, after: Option<Timestamp>) -> Result<Live, String> {
    call("live", json!({ "id": id, "after": after })).await
}
