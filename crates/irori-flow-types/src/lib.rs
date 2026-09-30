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
        data: Option<FlowCallData>,
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

/// A light's settings in a call: each number either written down, or worked out from an
/// expression when the call runs (`{"expr": "var('level')"}`), rounded and brought into range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowCallData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<Amount<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness_pct: Option<Amount<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_temp_kelvin: Option<Amount<u16>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
}

/// A number in a call's settings: fixed, or worked out when the call runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Amount<T> {
    Fixed(T),
    Worked(Worked),
}

/// An expression giving a number, e.g. `var('level')` or `round(num('sensor.lux') / 10)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Worked {
    pub expr: ExprString,
}

impl From<LightCallData> for FlowCallData {
    fn from(light: LightCallData) -> Self {
        Self {
            brightness: light.brightness.map(Amount::Fixed),
            brightness_pct: light.brightness_pct.map(Amount::Fixed),
            color_temp_kelvin: light.color_temp_kelvin.map(Amount::Fixed),
            rgb: light.rgb,
        }
    }
}

impl FlowCallData {
    /// The worked-out settings, by field name.
    pub fn exprs(&self) -> Vec<(&'static str, &ExprString)> {
        fn worked<'a, T>(
            field: &'static str,
            amount: Option<&'a Amount<T>>,
        ) -> Option<(&'static str, &'a ExprString)> {
            match amount {
                Some(Amount::Worked(w)) => Some((field, &w.expr)),
                _ => None,
            }
        }
        [
            worked("brightness", self.brightness.as_ref()),
            worked("brightness_pct", self.brightness_pct.as_ref()),
            worked("color_temp_kelvin", self.color_temp_kelvin.as_ref()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The settings as the service sees them, with any worked-out number standing in as one in
    /// range: enough to check what's asked of the light before anything runs.
    pub fn shape(&self) -> CallData {
        fn fixed<T: Copy>(amount: Option<&Amount<T>>, stand_in: T) -> Option<T> {
            amount.map(|amount| match amount {
                Amount::Fixed(n) => *n,
                Amount::Worked(_) => stand_in,
            })
        }
        CallData::Light(LightCallData {
            brightness: fixed(self.brightness.as_ref(), 255),
            brightness_pct: fixed(self.brightness_pct.as_ref(), 100),
            color_temp_kelvin: fixed(self.color_temp_kelvin.as_ref(), 2700),
            rgb: self.rgb,
        })
    }

    /// The settings with every expression worked out by `eval`, rounded and brought into each
    /// field's range, and a note for each worked-out one ("brightness_pct 57.5 → 58").
    pub fn resolve(
        &self,
        mut eval: impl FnMut(&ExprString) -> Result<f64, String>,
    ) -> Result<(CallData, Vec<String>), String> {
        let mut notes = Vec::new();
        let mut work = |field, amount, low, high| {
            worked_out(field, amount, (low, high), &mut eval, &mut notes)
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // rounded, in range
        let light = LightCallData {
            brightness: work(
                "brightness",
                self.brightness.as_ref().map(Amount::widen),
                1.0,
                255.0,
            )?
            .map(|n| n as u8),
            brightness_pct: work(
                "brightness_pct",
                self.brightness_pct.as_ref().map(Amount::widen),
                1.0,
                100.0,
            )?
            .map(|n| n as u8),
            color_temp_kelvin: work(
                "color_temp_kelvin",
                self.color_temp_kelvin.as_ref().map(Amount::widen),
                1000.0,
                20000.0,
            )?
            .map(|n| n as u16),
            rgb: self.rgb,
        };
        Ok((CallData::Light(light), notes))
    }
}

impl<T: Copy + Into<f64>> Amount<T> {
    fn widen(&self) -> Amount<f64> {
        match self {
            Self::Fixed(n) => Amount::Fixed((*n).into()),
            Self::Worked(w) => Amount::Worked(w.clone()),
        }
    }
}

/// One setting's number: as written, or worked out, rounded and brought into `range`.
fn worked_out(
    field: &str,
    amount: Option<Amount<f64>>,
    (low, high): (f64, f64),
    eval: &mut impl FnMut(&ExprString) -> Result<f64, String>,
    notes: &mut Vec<String>,
) -> Result<Option<f64>, String> {
    let Some(amount) = amount else {
        return Ok(None);
    };
    let w = match amount {
        Amount::Fixed(n) => return Ok(Some(n)),
        Amount::Worked(w) => w,
    };
    let value = eval(&w.expr).map_err(|e| format!("{field}: {e}"))?;
    if !value.is_finite() {
        return Err(format!("{field}: `{}` isn't a number", w.expr.as_str()));
    }
    let kept = value.round().clamp(low, high);
    notes.push(format!("{field} {} → {}", trim(value), trim(kept)));
    Ok(Some(kept))
}

/// A number as a person writes it: `58`, `57.5`.
fn trim(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{n:.0}")
    } else {
        format!("{}", (n * 100.0).round() / 100.0)
    }
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
            Self::Call { service, data, .. } => {
                for (field, expr) in data.iter().flat_map(FlowCallData::exprs) {
                    expr.validate().map_err(|e| inv(format!("{field}: {e}")))?;
                }
                service.validate_data(data.as_ref().map(FlowCallData::shape).as_ref())
            }
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
    fn a_fixed_number_is_written_as_it_always_was() {
        // Settings that may be worked out came later; a flow saved before keeps its version.
        let flow: Flow = serde_json::from_str(HALLWAY).unwrap();
        assert_eq!(
            flow.version(),
            "11423d5ea455c56d54763b624d5015cb8fb6d8dfca4608eac0489aaf30d88c09"
        );
    }

    #[test]
    fn a_setting_can_be_worked_out_when_the_call_runs() {
        let data: FlowCallData = serde_json::from_value(serde_json::json!({
            "brightness_pct": { "expr": "var('level')" }
        }))
        .unwrap();
        assert_eq!(data.exprs().len(), 1);
        let (resolved, notes) = data.resolve(|_| Ok(57.5)).unwrap();
        let CallData::Light(light) = resolved;
        assert_eq!(light.brightness_pct, Some(58));
        assert_eq!(notes, ["brightness_pct 57.5 → 58"]);
        // Out of range is brought into it, and says so.
        let (resolved, notes) = data.resolve(|_| Ok(140.0)).unwrap();
        let CallData::Light(light) = resolved;
        assert_eq!(light.brightness_pct, Some(100));
        assert_eq!(notes, ["brightness_pct 140 → 100"]);
        assert!(data.resolve(|_| Ok(f64::NAN)).is_err());
        assert!(
            serde_json::from_value::<FlowCallData>(serde_json::json!({
                "brightness_pct": { "expr": "1", "extra": true }
            }))
            .is_err()
        );
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
