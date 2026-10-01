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
