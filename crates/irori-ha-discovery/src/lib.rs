//! Home Assistant MQTT Discovery: parsing, device/entity mapping, and payload-state translation.
//! Shared by `irori-protocol-mqtt` and `irori-protocol-zigbee`, so this logic is written and
//! tested once (`docs/specs/protocols.md` §11: "discovery is inside each protocol; the contract
//! only sees the resulting descriptions" — this crate is that "inside," reused across two).
//!
//! Stateless and broker-agnostic on purpose: it never opens a connection itself (each protocol's
//! own `broker.rs` does that) and keeps no state between calls — every protocol keeps its own
//! `unique_id -> topics` map and calls into these pure functions.

pub mod bridge;
pub mod discovery;
pub mod map;
pub mod state;
pub mod template;
pub mod topic;

pub use discovery::{AvailabilityTopic, EntityTopics, ParsedConfig, ParsedDevice};
pub use state::Publish;
pub use template::ValueTemplate;
pub use topic::{Component, DiscoveryTopic};
