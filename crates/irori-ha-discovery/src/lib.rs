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
pub mod kinds;
pub mod map;
pub mod state;
pub mod template;
pub mod topic;
pub mod unmodeled;

/// The largest MQTT message, in bytes, either protocol takes by default: 1 MiB. rumqttc's own
/// default is 10 KiB, and it drops the connection on anything bigger (it can't skip one message).
/// Zigbee2MQTT 2.14 retains `bridge/info` (~48 KB, its settings schema included) and
/// `bridge/definitions` (~228 KB, the ZCL cluster list), and a 20 KB cap closed the connection
/// while Home Assistant discovery was still starting.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub use discovery::{AvailabilityTopic, EntityTopics, ParsedConfig, ParsedDevice};
pub use state::Publish;
pub use template::{CommandTemplate, ValueTemplate};
pub use topic::{Component, DiscoveryTopic};
