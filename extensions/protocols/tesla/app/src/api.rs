//! What the page asks the Tesla extension, typed. The shapes are the extension's
//! (`extensions/protocols/tesla/src/page.rs`), whose test pins their field names.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::bridge;

async fn call<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, String> {
    bridge().rpc(method, params).await
}

/// An amount, for drawing (`micros`, millionths of the currency) and for reading (`text`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Money {
    pub micros: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct Counts {
    pub data: u32,
    pub command: u32,
    pub wake: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModeChoice {
    pub label: String,
    pub about: String,
}

/// How often reading happens now, in seconds. `None`: not on a schedule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct Pace {
    pub states_every: Option<u64>,
    pub car_every: Option<u64>,
    pub site_every: Option<u64>,
    pub starved: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Prices {
    pub data: Money,
    pub command: Money,
    pub wake: Money,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Summary {
    pub currency: String,
    pub month: String,
    pub now: u64,
    pub month_start: u64,
    pub month_end: u64,
    pub allowance: Money,
    pub credit: Money,
    pub irori: Money,
    pub irori_commands: Money,
    pub others: Option<Money>,
    pub others_per_day: Money,
    pub account: Money,
    pub projected: Money,
    pub left: Money,
    pub over: bool,
    pub checked_at: Option<u64>,
    pub runs_out_at: Option<u64>,
    pub counts: Counts,
    pub prices: Prices,
    pub mode: String,
    pub modes: Vec<ModeChoice>,
    pub pace: Pace,
    pub warning: Option<String>,
    pub can_change: bool,
    pub dashboard: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Day {
    pub day: u8,
    pub counts: Counts,
    /// What each category cost that day, in micros.
    pub data: u64,
    pub command: u64,
    pub wake: u64,
    pub cost: Money,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Target {
    pub id: String,
    pub name: String,
    /// `car`, `site`, `account`, or `gone`.
    pub kind: String,
    pub counts: Counts,
    pub cost: Money,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Month {
    /// `2026-11`.
    pub month: String,
    pub days: Vec<Day>,
    pub targets: Vec<Target>,
    pub cost: Money,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Days {
    pub this: Month,
    pub last: Option<Month>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Call {
    pub at: u64,
    pub target: String,
    pub kind: String,
    pub what: String,
    pub why: String,
    pub status: u16,
    pub flag: Option<String>,
    pub cost: Money,
}

pub async fn summary() -> Result<Summary, String> {
    call("usage.summary", json!({})).await
}

pub async fn days() -> Result<Days, String> {
    call("usage.days", json!({})).await
}

pub async fn calls() -> Result<Vec<Call>, String> {
    call("usage.calls", json!({})).await
}

/// Changes the polling mode, the allowance, or both. Answers the month as it now stands.
pub async fn set(mode: Option<&str>, allowance: Option<&str>) -> Result<Summary, String> {
    call("usage.set", json!({ "mode": mode, "allowance": allowance })).await
}

/// What Tesla's dashboard says the account spent so far this month.
pub async fn calibrate(spent: &str) -> Result<Summary, String> {
    call("usage.calibrate", json!({ "spent": spent })).await
}
