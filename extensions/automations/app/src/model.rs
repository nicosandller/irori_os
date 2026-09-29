//! What the canvas draws: where nodes and ports sit, what each node says in words, and the
//! nodes the palette offers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use irori_flow_types::{
    CallData, Condition, Flow, JoinMode, Node, NodeId, Port, RuleService, Trigger, TypedValue,
    WaitUntil,
};
use irori_types::{EntityId, EntityKind, EntityState, SensorValue, State};
use leptos::prelude::WithUntracked;

use crate::Home;

pub const NODE_W: f64 = 216.0;
pub const HEAD_H: f64 = 30.0;
pub const TEXT_H: f64 = 44.0;
pub const PORT_H: f64 = 22.0;

/// How tall a node is: its head, its sentence, and a row per output port.
pub fn height(node: &Node) -> f64 {
    HEAD_H + TEXT_H + PORT_H * node.ports().len() as f64
}

/// Where a wire leaves: the port's circle on the node's right edge.
pub fn out_anchor(at: [f64; 2], node: &Node, port: Port) -> [f64; 2] {
    let index = node.ports().iter().position(|p| *p == port).unwrap_or(0) as f64;
    [
        at[0] + NODE_W,
        at[1] + HEAD_H + TEXT_H + index * PORT_H + PORT_H / 2.0,
    ]
}

/// Where a wire arrives: the input on the node's left edge.
pub fn in_anchor(at: [f64; 2]) -> [f64; 2] {
    [at[0], at[1] + HEAD_H / 2.0]
}

/// A wire's curve, from `a` to `b`.
pub fn curve(a: [f64; 2], b: [f64; 2]) -> String {
    let pull = ((b[0] - a[0]).abs() / 2.0).max(60.0);
    format!(
        "M {} {} C {} {}, {} {}, {} {}",
        a[0],
        a[1],
        a[0] + pull,
        a[1],
        b[0] - pull,
        b[1],
        b[0],
        b[1]
    )
}

/// Every node's position: its own, or one worked out from how far it is from a trigger.
pub fn positions(flow: &Flow) -> BTreeMap<NodeId, [f64; 2]> {
    let mut depth: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut queue: VecDeque<NodeId> = flow
        .nodes
        .iter()
        .filter(|(_, node)| node.is_trigger())
        .map(|(id, _)| id.clone())
        .collect();
    for id in &queue {
        depth.insert(id.clone(), 0);
    }
    let mut guard = 0;
    while let Some(id) = queue.pop_front() {
        guard += 1;
        if guard > 10_000 {
            break;
        }
        let here = depth.get(&id).copied().unwrap_or(0);
        for wire in flow.wires.iter().filter(|wire| wire.from.node == id) {
            let next = here + 1;
            if depth.get(&wire.to).is_none_or(|d| *d < next) && next < 64 {
                depth.insert(wire.to.clone(), next);
                queue.push_back(wire.to.clone());
            }
        }
    }
    let mut rows: BTreeMap<usize, usize> = BTreeMap::new();
    flow.nodes
        .keys()
        .map(|id| {
            if let Some(at) = flow.layout.get(id) {
                return (id.clone(), *at);
            }
            let column = depth.get(id).copied().unwrap_or(0);
            let row = rows.entry(column).or_default();
            let at = [40.0 + column as f64 * 280.0, 40.0 + *row as f64 * 170.0];
            *row += 1;
            (id.clone(), at)
        })
        .collect()
}

/// The node's colour family, for its stripe and the palette.
pub fn family(node: &Node) -> &'static str {
    match node {
        Node::Trigger { .. } => "var(--k-trigger)",
        Node::Gate { .. } | Node::Switch { .. } | Node::Set { .. } => "var(--k-logic)",
        Node::Call { .. } => "var(--k-act)",
        Node::Delay { .. } | Node::Wait { .. } => "var(--k-time)",
        Node::Join { .. } | Node::Stop { .. } => "var(--k-flow)",
    }
}

/// The node's head label.
pub fn label(node: &Node) -> &'static str {
    match node {
        Node::Trigger { .. } => "When",
        Node::Gate { .. } => "If",
        Node::Switch { .. } => "Choose",
        Node::Call { .. } => "Do",
        Node::Set { .. } => "Remember",
        Node::Delay { .. } => "Delay",
        Node::Wait { .. } => "Wait until",
        Node::Join { .. } => "Join",
        Node::Stop { .. } => "Stop",
    }
}

/// A port's name as a person reads it.
pub fn port_label(node: &Node, port: Port) -> String {
    match (node, port) {
        (Node::Call { .. }, Port::Out) => "done".into(),
        (Node::Call { .. }, Port::Error) => "failed".into(),
        (Node::Join { .. }, Port::Timeout) => "gave up".into(),
        (Node::Wait { .. }, Port::Timeout) => "gave up".into(),
        (Node::Switch { .. }, Port::Case(n)) => format!("case {n}"),
        (Node::Switch { .. }, Port::Else) => "none".into(),
        (_, port) => port.to_string(),
    }
}

fn value_words(value: &TypedValue) -> String {
    match value {
        TypedValue::Bool(true) => "on".into(),
        TypedValue::Bool(false) => "off".into(),
        TypedValue::Number(n) => format!("{n}"),
        TypedValue::Text(text) => format!("“{text}”"),
        TypedValue::Null => "unknown".into(),
    }
}

/// A condition as a sentence.
pub fn condition_words(condition: &Condition, home: &Home) -> String {
    match condition {
        Condition::State {
            entity,
            is,
            availability,
        } => match (is, availability) {
            (Some(is), _) => format!("{} is {}", home.name(entity), value_words(is)),
            (None, Some(availability)) => {
                format!(
                    "{} is {}",
                    home.name(entity),
                    format!("{availability:?}").to_lowercase()
                )
            }
            (None, None) => home.name(entity),
        },
        Condition::Expr { expr } => crate::inspector::expr_words(expr.as_str(), home)
            .unwrap_or_else(|| expr.as_str().to_owned()),
        Condition::Time { .. } => "in the time window".into(),
        Condition::Sun { .. } => "the sun is where it should be".into(),
        Condition::All { conditions } => conditions
            .iter()
            .map(|c| condition_words(c, home))
            .collect::<Vec<_>>()
            .join(" and "),
        Condition::Any { conditions } => conditions
            .iter()
            .map(|c| condition_words(c, home))
            .collect::<Vec<_>>()
            .join(" or "),
        Condition::Not { condition } => format!("not {}", condition_words(condition, home)),
    }
}

/// What the node does, in a sentence.
pub fn sentence(node: &Node, home: &Home) -> String {
    match node {
        Node::Trigger { trigger } => match trigger {
            Trigger::State {
                entity,
                from,
                to,
                hold,
            } => {
                let name = home.name(entity);
                let mut text = match (from, to) {
                    (None, Some(TypedValue::Bool(true))) => format!("{name} turns on"),
                    (None, Some(TypedValue::Bool(false))) => format!("{name} turns off"),
                    (None, Some(to)) => format!("{name} becomes {}", value_words(to)),
                    (Some(from), Some(to)) => {
                        format!("{name} goes {} → {}", value_words(from), value_words(to))
                    }
                    (Some(from), None) => format!("{name} stops being {}", value_words(from)),
                    (None, None) => format!("{name} changes"),
                };
                if let Some(hold) = hold {
                    text.push_str(&format!(" for {}", hold.as_str()));
                }
                text
            }
            Trigger::Startup {} => "Irori starts".into(),
            Trigger::Time { at, cron, .. } => at
                .as_ref()
                .map(|at| format!("it's {}", at.as_str()))
                .or_else(|| {
                    cron.as_ref()
                        .map(|cron| format!("the schedule {}", cron.as_str()))
                })
                .unwrap_or_else(|| "a time comes".into()),
            Trigger::Sun { event, .. } => format!("{event:?}").to_lowercase(),
            Trigger::Event { event, .. } => format!("the event {} happens", event.as_str()),
        },
        Node::Gate { condition } => condition_words(condition, home),
        Node::Switch { cases } => cases
            .iter()
            .enumerate()
            .map(|(i, case)| format!("{}: {}", i + 1, condition_words(case, home)))
            .collect::<Vec<_>>()
            .join(" · "),
        Node::Call {
            service,
            entity,
            data,
        } => {
            let name = home.name(entity);
            let verb = match service {
                RuleService::LightTurnOn | RuleService::SwitchTurnOn => "Turn on",
                RuleService::LightTurnOff | RuleService::SwitchTurnOff => "Turn off",
                RuleService::LightToggle | RuleService::SwitchToggle => "Toggle",
            };
            match data {
                Some(CallData::Light(light)) => {
                    let mut text = format!("{verb} {name}");
                    if let Some(pct) = light.brightness_pct {
                        text.push_str(&format!(" at {pct}%"));
                    } else if let Some(b) = light.brightness {
                        text.push_str(&format!(" at {}%", u16::from(b) * 100 / 255));
                    }
                    if let Some(k) = light.color_temp_kelvin {
                        text.push_str(&format!(", {k} K"));
                    }
                    text
                }
                None => format!("{verb} {name}"),
            }
        }
        Node::Set { name, expr } => format!("{name} = {}", expr.as_str()),
        Node::Delay { hold } => format!("wait {}", hold.as_str()),
        Node::Wait { until, timeout } => {
            let what = match until {
                WaitUntil::State {
                    entity,
                    is,
                    availability,
                    hold,
                } => {
                    let mut text = match (is, availability) {
                        (Some(is), _) => format!("{} is {}", home.name(entity), value_words(is)),
                        (None, Some(a)) => {
                            format!(
                                "{} is {}",
                                home.name(entity),
                                format!("{a:?}").to_lowercase()
                            )
                        }
                        (None, None) => home.name(entity),
                    };
                    if let Some(hold) = hold {
                        text.push_str(&format!(" for {}", hold.as_str()));
                    }
                    text
                }
                WaitUntil::Expr { expr, hold } => match hold {
                    Some(hold) => format!("{} for {}", expr.as_str(), hold.as_str()),
                    None => expr.as_str().to_owned(),
                },
            };
            format!("{what} (at most {})", timeout.as_str())
        }
        Node::Join { mode, timeout } => match mode {
            JoinMode::All => format!(
                "every path arrives{}",
                timeout
                    .as_ref()
                    .map(|t| format!(" (at most {})", t.as_str()))
                    .unwrap_or_default()
            ),
            JoinMode::First => "the first path goes on".into(),
        },
        Node::Stop { reason } => reason
            .as_ref()
            .map(|r| r.as_str().to_owned())
            .unwrap_or_else(|| "end the whole run".into()),
    }
}

/// The entity a node is mostly about, for its live value.
pub fn primary_entity(node: &Node) -> Option<EntityId> {
    match node {
        Node::Trigger {
            trigger: Trigger::State { entity, .. },
        }
        | Node::Call { entity, .. }
        | Node::Gate {
            condition: Condition::State { entity, .. },
        }
        | Node::Wait {
            until: WaitUntil::State { entity, .. },
            ..
        } => Some(entity.clone()),
        Node::Gate {
            condition: Condition::Expr { expr },
        } => first_entity_in(expr.as_str()),
        Node::Wait {
            until: WaitUntil::Expr { expr, .. },
            ..
        } => first_entity_in(expr.as_str()),
        _ => None,
    }
}

/// The first `'kind.object'` string literal in an expression.
fn first_entity_in(expr: &str) -> Option<EntityId> {
    expr.split(['\'', '"'])
        .skip(1)
        .step_by(2)
        .find_map(|literal| literal.parse().ok())
}

/// A state's value the way a person says it.
pub fn state_words(state: &EntityState) -> String {
    if state.availability == irori_types::Availability::Unavailable {
        return "unavailable".into();
    }
    match &state.state {
        None => "unknown".into(),
        Some(State::Light(light)) => match (light.on, light.brightness) {
            (true, Some(b)) => format!("on {}%", u16::from(b) * 100 / 255),
            (true, None) => "on".into(),
            (false, _) => "off".into(),
        },
        Some(State::Switch(s)) => if s.on { "on" } else { "off" }.into(),
        Some(State::BinarySensor(s)) => if s.on { "on" } else { "off" }.into(),
        Some(State::Sensor(s)) => match &s.value {
            SensorValue::Number(n) => {
                if n.fract() == 0.0 {
                    format!("{n:.0}")
                } else {
                    format!("{n:.1}")
                }
            }
            SensorValue::Text(text) => text.clone(),
        },
    }
}

/// A fresh node id: `base`, or `base_2`, … whichever isn't taken.
pub fn fresh_id(flow: &Flow, base: &str) -> NodeId {
    let taken: BTreeSet<String> = flow.nodes.keys().map(ToString::to_string).collect();
    let base = if base.is_empty() { "node" } else { base };
    let name = if taken.contains(base) {
        (2..)
            .map(|n| format!("{base}_{n}"))
            .find(|name| !taken.contains(name))
            .unwrap_or_else(|| base.to_owned())
    } else {
        base.to_owned()
    };
    name.parse()
        .unwrap_or_else(|_| "node".parse().expect("a valid id"))
}

/// One of the palette's entries.
#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub group: &'static str,
    pub label: &'static str,
    pub base: &'static str,
    pub make: fn(&Home) -> serde_json::Value,
}

/// The first entity of `kinds`, or a placeholder the check will point at.
fn pick(home: &Home, kinds: &[EntityKind]) -> String {
    home.entities.with_untracked(|entities| {
        entities
            .iter()
            .find(|entity| kinds.contains(&entity.id.kind()))
            .map(|entity| entity.id.to_string())
            .unwrap_or_else(|| format!("{}.choose_one", kinds[0]))
    })
}

fn first_number_sensor(home: &Home) -> String {
    home.entities.with_untracked(|entities| {
        entities
            .iter()
            .find(|entity| {
                matches!(&entity.capabilities, irori_types::Capabilities::Sensor(s)
                    if s.value_type == irori_types::SensorValueType::Number)
            })
            .map(|entity| entity.id.to_string())
            .unwrap_or_else(|| "sensor.choose_one".into())
    })
}

pub const TEMPLATES: &[Template] = &[
    Template {
        group: "Start",
        label: "When something changes",
        base: "when",
        make: |home| {
            serde_json::json!({ "type": "trigger", "trigger": {
            "type": "state", "entity": pick(home, &[EntityKind::BinarySensor, EntityKind::Switch, EntityKind::Light]), "to": true } })
        },
    },
    Template {
        group: "Start",
        label: "When Irori starts",
        base: "startup",
        make: |_| serde_json::json!({ "type": "trigger", "trigger": { "type": "startup" } }),
    },
    Template {
        group: "Decide",
        label: "If something is…",
        base: "check",
        make: |home| {
            serde_json::json!({ "type": "gate", "condition": {
            "type": "state", "entity": pick(home, &[EntityKind::Switch, EntityKind::BinarySensor, EntityKind::Light]), "is": true } })
        },
    },
    Template {
        group: "Decide",
        label: "If a number…",
        base: "compare",
        make: |home| {
            serde_json::json!({ "type": "gate", "condition": {
            "type": "expr", "expr": format!("num('{}') < 30", first_number_sensor(home)) } })
        },
    },
    Template {
        group: "Decide",
        label: "Choose one of…",
        base: "choose",
        make: |home| {
            let sensor = first_number_sensor(home);
            serde_json::json!({ "type": "switch", "cases": [
                { "type": "expr", "expr": format!("num('{sensor}') < 30") },
                { "type": "expr", "expr": format!("num('{sensor}') < 200") }
            ] })
        },
    },
    Template {
        group: "Do",
        label: "Turn on",
        base: "turn_on",
        make: |home| {
            let entity = pick(home, &[EntityKind::Light, EntityKind::Switch]);
            let service = if entity.starts_with("switch.") {
                "switch.turn_on"
            } else {
                "light.turn_on"
            };
            serde_json::json!({ "type": "call", "service": service, "entity": entity })
        },
    },
    Template {
        group: "Do",
        label: "Turn off",
        base: "turn_off",
        make: |home| {
            let entity = pick(home, &[EntityKind::Light, EntityKind::Switch]);
            let service = if entity.starts_with("switch.") {
                "switch.turn_off"
            } else {
                "light.turn_off"
            };
            serde_json::json!({ "type": "call", "service": service, "entity": entity })
        },
    },
    Template {
        group: "Do",
        label: "Remember a value",
        base: "remember",
        make: |_| serde_json::json!({ "type": "set", "name": "level", "expr": "50" }),
    },
    Template {
        group: "Time",
        label: "Wait until…",
        base: "wait",
        make: |home| {
            serde_json::json!({ "type": "wait", "until": {
            "type": "state", "entity": pick(home, &[EntityKind::BinarySensor, EntityKind::Switch]), "is": false, "for": "2m" },
            "timeout": "10m" })
        },
    },
    Template {
        group: "Time",
        label: "Delay",
        base: "delay",
        make: |_| serde_json::json!({ "type": "delay", "for": "30s" }),
    },
    Template {
        group: "Paths",
        label: "Join: wait for all",
        base: "join",
        make: |_| serde_json::json!({ "type": "join", "mode": "all", "timeout": "1m" }),
    },
    Template {
        group: "Paths",
        label: "Join: first one wins",
        base: "first",
        make: |_| serde_json::json!({ "type": "join", "mode": "first" }),
    },
    Template {
        group: "Paths",
        label: "Stop the run",
        base: "stop",
        make: |_| serde_json::json!({ "type": "stop" }),
    },
];

/// A new, empty flow with one trigger to start from.
pub fn blank(home: &Home) -> Flow {
    let trigger = (TEMPLATES[0].make)(home);
    serde_json::from_value(serde_json::json!({
        "id": "new_flow",
        "name": "New flow",
        "nodes": { "when": trigger },
        "layout": { "when": [60, 80] }
    }))
    .unwrap_or_else(|_| {
        serde_json::from_value(serde_json::json!({
            "id": "new_flow", "name": "New flow",
            "nodes": { "startup": { "type": "trigger", "trigger": { "type": "startup" } } }
        }))
        .expect("the fallback flow is valid")
    })
}

/// A slug made from a name, for a new flow's id.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_owned();
    if out.is_empty() {
        "flow".into()
    } else {
        out.chars().take(60).collect()
    }
}
