//! The flow engine's documents (`docs/specs/flows.md` §2): a flow is nodes joined by wires, drawn
//! freely on a canvas.
//!
//! Layer 1 (JSON Schema) and layer 2 (these types' own rules) live here; the graph and the home
//! are layer 3, in `irori-flows`. No CEL: the extension's page compiles this to wasm.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use irori_types::{Description, EntityId, InvariantError, Name, ObjectId, RuleId};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use irori_rules::{
    AvailabilityWanted, CallData, CompactDuration, Condition, ExprString, LightCallData,
    LimitedMode, Mode, NamedMode, RuleService, StopReason, Trigger, TypedValue, WaitUntil,
};

/// Most nodes in a flow.
pub const MAX_NODES: usize = 128;
/// Most wires in a flow.
pub const MAX_WIRES: usize = 512;
/// Most cases in a switch.
pub const MAX_CASES: usize = 16;

/// A node's name inside its flow, e.g. `motion`. Stable: traces point at it.
pub type NodeId = ObjectId;

/// One flow, as stored in `flows/<id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub id: RuleId,
    pub name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Description>,
    /// Off keeps the file and its history, but nothing runs. Not part of the version.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// What happens when a trigger fires while a run is going.
    #[serde(default, skip_serializing_if = "is_single")]
    pub mode: Mode,
    #[schemars(extend("minProperties" = 1, "maxProperties" = 128))]
    pub nodes: BTreeMap<NodeId, Node>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 512))]
    pub wires: Vec<Wire>,
    /// Where each node sits on the canvas. Not part of the version.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layout: BTreeMap<NodeId, [f64; 2]>,
}

fn yes() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if signature"
)]
fn is_true(b: &bool) -> bool {
    *b
}

fn is_single(mode: &Mode) -> bool {
    mode.is_single()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFlow {
    id: RuleId,
    name: Name,
    #[serde(default)]
    description: Option<Description>,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    mode: Mode,
    nodes: BTreeMap<NodeId, Node>,
    #[serde(default)]
    wires: Vec<Wire>,
    #[serde(default)]
    layout: BTreeMap<NodeId, [f64; 2]>,
}

impl<'de> Deserialize<'de> for Flow {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawFlow::deserialize(deserializer)?;
        let flow = Flow {
            id: raw.id,
            name: raw.name,
            description: raw.description,
            enabled: raw.enabled,
            mode: raw.mode,
            nodes: raw.nodes,
            wires: raw.wires,
            layout: raw.layout,
        };
        flow.validate().map_err(serde::de::Error::custom)?;
        Ok(flow)
    }
}

fn inv(message: impl Into<String>) -> InvariantError {
    InvariantError::new(message)
}

impl Flow {
    /// Layer 2: what the types can check without the graph or the home. Deserialization runs
    /// this; call it yourself when building a flow in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.nodes.is_empty() {
            return Err(inv("a flow needs at least one node"));
        }
        if self.nodes.len() > MAX_NODES {
            return Err(inv(format!("a flow has at most {MAX_NODES} nodes")));
        }
        if self.wires.len() > MAX_WIRES {
            return Err(inv(format!("a flow has at most {MAX_WIRES} wires")));
        }
        self.mode.validate()?;
        for (id, node) in &self.nodes {
            node.validate()
                .map_err(|e| inv(format!("nodes/{id}: {e}")))?;
        }
        for (id, [x, y]) in &self.layout {
            if !x.is_finite() || !y.is_finite() {
                return Err(inv(format!(
                    "layout/{id}: positions must be finite numbers"
                )));
            }
        }
        Ok(())
    }

    /// The canonical JSON the version is the hash of: no `enabled`, no `layout`, keys sorted,
    /// wires sorted, no whitespace (`docs/specs/flows.md` §5).
    pub fn canonical_json(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(map) = &mut value {
            map.remove("enabled");
            map.remove("layout");
            if let Some(serde_json::Value::Array(wires)) = map.get_mut("wires") {
                wires.sort_by_key(|wire| wire.to_string());
            }
        }
        let mut out = String::new();
        write_canonical(&value, &mut out);
        out
    }

    /// Which definition this is: lowercase hex SHA-256 of [`Self::canonical_json`].
    pub fn version(&self) -> String {
        let digest = Sha256::digest(self.canonical_json().as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The wires leaving `node`'s `port`, in the order they're written.
    pub fn wires_from<'a>(
        &'a self,
        node: &'a NodeId,
        port: &'a Port,
    ) -> impl Iterator<Item = &'a Wire> + 'a {
        self.wires
            .iter()
            .filter(move |wire| &wire.from.node == node && &wire.from.port == port)
    }

    /// The wires arriving at `node`.
    pub fn wires_into<'a>(&'a self, node: &'a NodeId) -> impl Iterator<Item = &'a Wire> + 'a {
        self.wires.iter().filter(move |wire| &wire.to == node)
    }
}

/// JSON with object keys sorted and no whitespace, whatever order the map kept them in.
fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// One step of a flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /// Starts a run.
    Trigger { trigger: Trigger },
    /// Checks once: `yes` or `no`.
    Gate { condition: Condition },
    /// The first case that holds, or `else`.
    Switch {
        #[schemars(length(min = 1, max = 16))]
        cases: Vec<Condition>,
    },
    /// Calls a service: `out`, or `error` if it failed.
    Call {
        service: RuleService,
        entity: EntityId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<CallData>,
    },
    /// Sets a run variable, read with `var('name')`.
    Set { name: ObjectId, expr: ExprString },
    /// Waits this long.
    Delay {
        #[serde(rename = "for")]
        hold: CompactDuration,
    },
    /// Waits for a level to hold: `matched`, or `timeout`.
    Wait {
        until: WaitUntil,
        timeout: CompactDuration,
    },
    /// Brings paths of one run back together.
    Join {
        mode: JoinMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<CompactDuration>,
    },
    /// Ends the whole run.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StopReason>,
    },
}

/// How a join brings paths together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JoinMode {
    /// Wait until every incoming wire has brought a token.
    All,
    /// Let the first through; the rest end here.
    First,
}

impl Node {
    fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::Trigger { trigger } => trigger.validate(),
            Self::Gate { condition } => condition.validate(0),
            Self::Switch { cases } => {
                if cases.is_empty() || cases.len() > MAX_CASES {
                    return Err(inv(format!("a switch has 1 to {MAX_CASES} cases")));
                }
                cases.iter().try_for_each(|case| case.validate(0))
            }
            Self::Call { service, data, .. } => service.validate_data(data.as_ref()),
            Self::Set { expr, .. } => expr.validate(),
            Self::Delay { hold } => hold.require_positive("for"),
            Self::Wait { until, timeout } => {
                until.validate()?;
                timeout.require_positive("timeout")
            }
            Self::Join { mode, timeout } => {
                if let Some(timeout) = timeout {
                    timeout.require_positive("timeout")?;
                }
                if *mode == JoinMode::All && timeout.is_none() {
                    return Err(inv("a join that waits for all needs a timeout"));
                }
                Ok(())
            }
            Self::Stop { reason } => reason.as_ref().map_or(Ok(()), StopReason::validate),
        }
    }

    /// Its output ports, in the order they're drawn.
    pub fn ports(&self) -> Vec<Port> {
        match self {
            Self::Trigger { .. } | Self::Set { .. } | Self::Delay { .. } => vec![Port::Out],
            Self::Gate { .. } => vec![Port::Yes, Port::No],
            Self::Switch { cases } => (1..=cases.len())
                .filter_map(|n| u8::try_from(n).ok())
                .map(Port::Case)
                .chain([Port::Else])
                .collect(),
            Self::Call { .. } => vec![Port::Out, Port::Error],
            Self::Wait { .. } => vec![Port::Matched, Port::Timeout],
            Self::Join { mode, .. } => match mode {
                JoinMode::All => vec![Port::Out, Port::Timeout],
                JoinMode::First => vec![Port::Out],
            },
            Self::Stop { .. } => Vec::new(),
        }
    }

    /// Whether at most one of its ports carries a given token: paths that part at such a node
    /// never both run, so they may meet again without a join (`docs/specs/flows.md` F4).
    pub fn is_exclusive(&self) -> bool {
        !matches!(self, Self::Trigger { .. })
    }

    pub fn is_trigger(&self) -> bool {
        matches!(self, Self::Trigger { .. })
    }

    /// The kind, as written in `type`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Trigger { .. } => "trigger",
            Self::Gate { .. } => "gate",
            Self::Switch { .. } => "switch",
            Self::Call { .. } => "call",
            Self::Set { .. } => "set",
            Self::Delay { .. } => "delay",
            Self::Wait { .. } => "wait",
            Self::Join { .. } => "join",
            Self::Stop { .. } => "stop",
        }
    }
}

/// An output port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Port {
    Out,
    Yes,
    No,
    Error,
    Matched,
    Timeout,
    /// A switch's case, from 1.
    Case(u8),
    Else,
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Out => f.write_str("out"),
            Self::Yes => f.write_str("yes"),
            Self::No => f.write_str("no"),
            Self::Error => f.write_str("error"),
            Self::Matched => f.write_str("matched"),
            Self::Timeout => f.write_str("timeout"),
            Self::Case(n) => write!(f, "case_{n}"),
            Self::Else => f.write_str("else"),
        }
    }
}

impl FromStr for Port {
    type Err = InvariantError;
    fn from_str(text: &str) -> Result<Self, InvariantError> {
        Ok(match text {
            "out" => Self::Out,
            "yes" => Self::Yes,
            "no" => Self::No,
            "error" => Self::Error,
            "matched" => Self::Matched,
            "timeout" => Self::Timeout,
            "else" => Self::Else,
            other => match other
                .strip_prefix("case_")
                .and_then(|n| n.parse::<u8>().ok())
            {
                Some(n) if n >= 1 => Self::Case(n),
                _ => return Err(inv(format!("`{text}` isn't a port"))),
            },
        })
    }
}

impl Serialize for Port {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Port {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Where a wire starts: a node and one of its ports. Written `node` for `out`, else
/// `node:port`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortRef {
    pub node: NodeId,
    pub port: Port,
}

impl fmt::Display for PortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.port == Port::Out {
            write!(f, "{}", self.node)
        } else {
            write!(f, "{}:{}", self.node, self.port)
        }
    }
}

impl FromStr for PortRef {
    type Err = InvariantError;
    fn from_str(text: &str) -> Result<Self, InvariantError> {
        let (node, port) = match text.split_once(':') {
            Some((node, port)) => (node, port.parse()?),
            None => (text, Port::Out),
        };
        Ok(Self {
            node: node
                .parse()
                .map_err(|e: irori_types::IdError| inv(e.to_string()))?,
            port,
        })
    }
}

/// A wire: from a node's port to a node. Written `["from", "to"]`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Wire {
    pub from: PortRef,
    pub to: NodeId,
}

impl fmt::Display for Wire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} → {}", self.from, self.to)
    }
}

impl Serialize for Wire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.from.to_string(), self.to.as_str()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Wire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (from, to) = <(String, String)>::deserialize(deserializer)?;
        Ok(Self {
            from: from.parse().map_err(serde::de::Error::custom)?,
            to: to
                .parse()
                .map_err(|e: irori_types::IdError| serde::de::Error::custom(e))?,
        })
    }
}

impl JsonSchema for Wire {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Wire".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "array",
            "description": "From a node (`node` for its `out` port, or `node:port`) to a node.",
            "prefixItems": [
                {
                    "type": "string",
                    "pattern": r"^[a-z0-9]+(_[a-z0-9]+)*(:(out|yes|no|error|matched|timeout|else|case_[1-9][0-9]*))?$",
                },
                { "type": "string", "pattern": r"^[a-z0-9]+(_[a-z0-9]+)*$" },
            ],
            "minItems": 2,
            "maxItems": 2,
        })
    }
}

/// JSON Schema for flow documents (`schemas/flow.schema.json`).
pub fn schemas() -> Vec<irori_types::SchemaDoc> {
    vec![irori_types::SchemaDoc::for_type::<Flow>("flow")]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const HALLWAY: &str = include_str!("../../../fixtures/types/flow/valid/hallway.json");

    #[test]
    fn the_version_ignores_layout_enabled_and_order() {
        let flow: Flow = serde_json::from_str(HALLWAY).unwrap();
        let mut moved = flow.clone();
        moved.layout.clear();
        moved.enabled = false;
        moved.wires.reverse();
        assert_eq!(flow.version(), moved.version());
        assert_eq!(flow.version().len(), 64);

        let mut changed = flow.clone();
        changed.name = Name::try_from("Hall light").unwrap();
        assert_ne!(flow.version(), changed.version());
    }

    #[test]
    fn ports_are_written_the_short_way() {
        let wire: Wire = serde_json::from_str(r#"["dark:yes", "on"]"#).unwrap();
        assert_eq!(wire.from.port, Port::Yes);
        let plain: Wire = serde_json::from_str(r#"["motion", "dark"]"#).unwrap();
        assert_eq!(plain.from.port, Port::Out);
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"["motion","dark"]"#
        );
        assert!(serde_json::from_str::<Wire>(r#"["dark:maybe", "on"]"#).is_err());
        assert_eq!("case_3".parse::<Port>().unwrap(), Port::Case(3));
    }
}

pub mod api;
pub mod trace;
