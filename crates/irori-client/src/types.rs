//! Wire shapes the server doesn't share as types. Registry documents come from `irori-types`.

use std::collections::BTreeMap;

use irori_types::{
    Area, Device, Entity, EntityId, EntityState, ExtensionId, Floor, Floorplan, Name, Role,
    Timestamp, TokenId, User, UserId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
    pub commit: String,
    pub built_at: String,
    pub boot_id: String,
    pub uptime_ms: u64,
    pub features: Vec<String>,
    pub sqlite: SqliteHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqliteHealth {
    pub version: String,
    pub journal_mode: String,
    pub ok: bool,
}

/// The fields the CLI prints. The rest of `/api/system` is left unread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct System {
    #[serde(default)]
    pub host: Option<String>,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu: String,
    pub memory_total: u64,
    pub memory_used: u64,
    pub uptime_secs: u64,
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub locked: bool,
    pub user: Option<User>,
    pub owner: bool,
    pub setup: Setup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Setup {
    pub owner: bool,
    pub place: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Home {
    pub devices: Vec<Device>,
    pub entities: Vec<Entity>,
    pub states: Vec<EntityState>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
    pub areas: Vec<Area>,
    #[serde(default)]
    pub floors: Vec<Floor>,
    #[serde(default)]
    pub held: Vec<HeldDevice>,
    #[serde(default)]
    pub floorplan: Option<Floorplan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldDevice {
    pub id: irori_types::DeviceId,
    pub protocol: irori_types::ProtocolId,
    pub name: Name,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provides: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: ExtensionId,
    pub name: String,
    pub category: String,
    pub description: String,
    pub version: String,
    pub official: bool,
    #[serde(default)]
    pub full_access: bool,
    pub installed: bool,
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct History {
    pub entity: EntityId,
    pub states: Vec<EntityState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub entity: EntityId,
    pub points: Vec<SummaryPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryPoint {
    pub start: Timestamp,
    pub value: f64,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Recorder {
    pub retain_days: u32,
    #[serde(default)]
    pub summary_days: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Lines {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRow {
    pub id: TokenId,
    pub name: String,
    pub user: UserId,
    pub scopes: Vec<irori_types::ApiScope>,
    #[serde(default)]
    pub extension: Option<ExtensionId>,
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedToken {
    #[serde(flatten)]
    pub token: TokenRow,
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRow {
    #[serde(flatten)]
    pub user: User,
    pub has_password: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewUser<'a> {
    pub name: &'a str,
    pub role: Role,
    pub password: &'a str,
}
