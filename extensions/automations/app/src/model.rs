//! What the canvas draws: where nodes and ports sit, what each node says in words, and the
//! nodes the palette offers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use irori_flow_types::{
    Amount, Condition, Flow, JoinMode, Node, NodeId, Port, Trigger, TypedValue, Values, WaitUntil,
};
use irori_types::{
    BinarySensorClass, Capabilities, EntityId, EntityKind, EntityState, SensorValue, State,
    ValueShape,
};
use leptos::prelude::WithUntracked;

use crate::Home;

pub const NODE_W: f64 = 216.0;
pub const HEAD_H: f64 = 30.0;
pub const TEXT_H: f64 = 44.0;
pub const PORT_H: f64 = 22.0;

/// A check's line, on a condition with several.
pub const CHECK_H: f64 = 20.0;

/// How tall a node is: its head, its sentence, and a row per output port.
pub fn height(node: &Node) -> f64 {
    HEAD_H + text_height(node) + PORT_H * node.ports().len() as f64
}

/// A condition's checks, when it has several: each gets its own line on the node.
pub fn checks_of(node: &Node) -> Option<crate::checks::Checks> {
    let Node::Gate { condition } = node else {
        return None;
    };
    let value = serde_json::to_value(condition).ok()?;
    crate::checks::Checks::from_condition(&value).filter(|checks| checks.clauses.len() > 1)
}

/// How tall the part between a node's head and its ports is: its sentence, or its checks.
pub fn text_height(node: &Node) -> f64 {
    checks_of(node).map_or(TEXT_H, |checks| {
        (10.0 + CHECK_H * checks.clauses.len() as f64).max(TEXT_H)
    })
}

/// Where a wire leaves: the port's circle on the node's right edge.
pub fn out_anchor(at: [f64; 2], node: &Node, port: Port) -> [f64; 2] {
    let index = node.ports().iter().position(|p| *p == port).unwrap_or(0) as f64;
    [
        at[0] + NODE_W,
        at[1] + HEAD_H + text_height(node) + index * PORT_H + PORT_H / 2.0,
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
        Node::Trigger { .. } => "Trigger",
        Node::Gate { .. } => "Condition",
        Node::Switch { .. } => "Case switch",
        Node::Call { .. } => "Do",
        Node::Set { .. } => "Calculate",
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

/// What a flag's `true` and `false` mean for this kind of thing: open and closed for a door,
/// detected and clear for presence. Switches and lights, and sensors that don't say what they
/// sense, are on and off.
pub fn flag_words(class: Option<BinarySensorClass>) -> (&'static str, &'static str) {
    use BinarySensorClass::*;
    match class {
        Some(Motion | Occupancy | Presence) => ("detected", "clear"),
        Some(Vibration) => ("shaking", "still"),
        Some(Door | GarageDoor | Window | Opening) => ("open", "closed"),
        Some(Moisture) => ("wet", "dry"),
        Some(Smoke) => ("smoke", "clear"),
        Some(Gas) => ("gas", "clear"),
        Some(CarbonMonoxide) => ("carbon monoxide", "clear"),
        Some(GlassBreak) => ("glass broken", "clear"),
        Some(Sound) => ("sound", "quiet"),
        Some(Tamper) => ("tampered", "clear"),
        Some(Plug) => ("plugged in", "unplugged"),
        Some(Power) => ("powered", "without power"),
        Some(Connectivity) => ("connected", "disconnected"),
        Some(Battery) => ("low", "ok"),
        Some(BatteryCharging) => ("charging", "not charging"),
        Some(Cold) => ("cold", "normal"),
        Some(Heat) => ("hot", "normal"),
        Some(Light) => ("light", "dark"),
        Some(Lock) => ("unlocked", "locked"),
        Some(Moving) => ("moving", "stopped"),
        Some(Running) => ("running", "stopped"),
        Some(Problem) => ("a problem", "ok"),
        Some(Safety) => ("unsafe", "safe"),
        Some(Update) => ("an update", "up to date"),
        None => ("on", "off"),
    }
}

impl Home {
    /// The binary sensor's class, if `entity` is one and says.
    pub fn flag_class(&self, entity: &EntityId) -> Option<BinarySensorClass> {
        self.entities.with_untracked(|entities| {
            entities
                .iter()
                .find(|e| &e.id == entity)
                .and_then(|e| match &e.capabilities {
                    Capabilities::BinarySensor(b) => b.device_class,
                    _ => None,
                })
        })
    }

    /// What kind of value `entity` has: on/off, a number, or text. On/off when it isn't known,
    /// since that's what a person picking a fresh entity most often means.
    pub fn value_shape(&self, entity: &str) -> ValueShape {
        self.entities.with_untracked(|entities| {
            entities
                .iter()
                .find(|e| e.id.as_str() == entity)
                .and_then(|e| e.capabilities.primary_shape())
                .unwrap_or(ValueShape::Bool)
        })
    }

    /// What on and off mean for `entity`.
    pub fn flag_words(&self, entity: &EntityId) -> (&'static str, &'static str) {
        flag_words(self.flag_class(entity))
    }
}

fn value_words(value: &TypedValue, entity: &EntityId, home: &Home) -> String {
    let (on, off) = home.flag_words(entity);
    match value {
        TypedValue::Bool(true) => on.into(),
        TypedValue::Bool(false) => off.into(),
        TypedValue::Number(n) => format!("{n}"),
        TypedValue::Text(text) => format!("“{text}”"),
        TypedValue::Null => "unknown".into(),
    }
}

/// Values as a person lists them: "“paused”, “idle” or “off”".
fn one_of(values: &[TypedValue], entity: &EntityId, home: &Home) -> String {
    let words: Vec<String> = values
        .iter()
        .map(|v| value_words(v, entity, home))
        .collect();
    match words.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}

/// A condition as a sentence.
pub fn condition_words(condition: &Condition, home: &Home) -> String {
    if let Some(checks) = serde_json::to_value(condition)
        .ok()
        .and_then(|value| crate::checks::Checks::from_condition(&value))
    {
        return checks.words(home);
    }
    match condition {
        Condition::State {
            entity,
            is,
            availability,
        } => match (is, availability) {
            (Some(is), _) => format!("{} is {}", home.name(entity), value_words(is, entity, home)),
            (None, Some(availability)) => {
                format!(
                    "{} is {}",
                    home.name(entity),
                    format!("{availability:?}").to_lowercase()
                )
            }
            (None, None) => home.name(entity),
        },
        Condition::Expr { expr } => crate::checks::expr_words(expr.as_str(), home)
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
                above,
                below,
                hold,
            } => {
                let name = home.name(entity);
                let values: Vec<TypedValue> = to.iter().flat_map(Values::iter).cloned().collect();
                let from: Vec<TypedValue> = from.iter().flat_map(Values::iter).cloned().collect();
                let flag = home.flag_class(entity).is_some();
                // Something that happens (a button's press) is said as what it reports.
                if entity.kind().counts_every_report() {
                    return match values.as_slice() {
                        [] => format!("{name} reports anything"),
                        values => format!("{name}: {}", one_of(values, entity, home)),
                    };
                }
                let mut text = match (above, below, from.as_slice(), values.as_slice()) {
                    (Some(above), Some(below), ..) => {
                        format!("{name} goes between {above} and {below}")
                    }
                    (None, Some(below), ..) => format!("{name} goes below {below}"),
                    (Some(above), None, ..) => format!("{name} goes above {above}"),
                    (_, _, [], [TypedValue::Bool(on)]) if flag => {
                        format!(
                            "{name} becomes {}",
                            value_words(&TypedValue::Bool(*on), entity, home)
                        )
                    }
                    (_, _, [], [TypedValue::Bool(true)]) => format!("{name} turns on"),
                    (_, _, [], [TypedValue::Bool(false)]) => format!("{name} turns off"),
                    (_, _, [], []) => format!("{name} changes"),
                    (_, _, [], values) => {
                        format!("{name} becomes {}", one_of(values, entity, home))
                    }
                    (_, _, from, []) => {
                        format!("{name} stops being {}", one_of(from, entity, home))
                    }
                    (_, _, from, values) => format!(
                        "{name} goes {} → {}",
                        one_of(from, entity, home),
                        one_of(values, entity, home)
                    ),
                };
                if let Some(hold) = hold {
                    text.push_str(&format!(" for {}", hold.as_str()));
                }
                text
            }
            Trigger::Startup {} => "Irori starts up".into(),
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
            let verb = crate::inspector::action_words(service.action());
            let Some(settings) = data else {
                return format!("{verb} {name}");
            };
            let mut text = format!("{verb} {name}");
            // A light's level reads as a percentage, however it was written.
            let mut said: Vec<&str> = Vec::new();
            if entity.kind() == irori_types::EntityKind::Light {
                said.extend(["brightness_pct", "brightness", "color_temp_kelvin"]);
                match (settings.get("brightness_pct"), settings.get("brightness")) {
                    (Some(Amount::Fixed(pct)), _) => text.push_str(&format!(" at {pct}%")),
                    (Some(Amount::Worked(w)), _) | (None, Some(Amount::Worked(w))) => {
                        text.push_str(&format!(" at {}", worked_words(w.expr.as_str())));
                    }
                    (None, Some(Amount::Fixed(b))) => {
                        let level = b.as_u64().unwrap_or(255) * 100 / 255;
                        text.push_str(&format!(" at {level}%"));
                    }
                    (None, None) => {}
                }
                match settings.get("color_temp_kelvin") {
                    Some(Amount::Fixed(k)) => text.push_str(&format!(", {k} K")),
                    Some(Amount::Worked(w)) => {
                        text.push_str(&format!(", {} K", worked_words(w.expr.as_str())));
                    }
                    None => {}
                }
            }
            // Every other setting, as its own name and value: "position 50", "hvac mode heat".
            for (field, amount) in &settings.0 {
                if said.contains(&field.as_str()) {
                    continue;
                }
                let value = match amount {
                    Amount::Fixed(serde_json::Value::String(word)) => word.replace('_', " "),
                    Amount::Fixed(value) => value.to_string(),
                    Amount::Worked(w) => worked_words(w.expr.as_str()),
                };
                text.push_str(&format!(", {} {value}", field.replace('_', " ")));
            }
            text
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
                        (Some(is), _) => {
                            format!("{} is {}", home.name(entity), value_words(is, entity, home))
                        }
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

/// A worked-out setting as it reads on a node: the calculation's name, or the expression.
fn worked_words(expr: &str) -> String {
    let name = expr
        .trim()
        .strip_prefix("var(")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(|inner| inner.trim().trim_matches(['\'', '"']));
    match name {
        Some(name) => format!("the calculated {name}"),
        None => format!("“{expr}”"),
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

/// Every entity the flow names: in its nodes' fields, and inside their expressions.
pub fn entities_used(flow: &Flow) -> BTreeSet<EntityId> {
    fn walk(value: &serde_json::Value, found: &mut BTreeSet<EntityId>) {
        match value {
            serde_json::Value::String(text) => {
                if let Ok(id) = text.parse() {
                    found.insert(id);
                }
                found.extend(
                    text.split(['\'', '"'])
                        .skip(1)
                        .step_by(2)
                        .filter_map(|literal| literal.parse().ok()),
                );
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, found)),
            serde_json::Value::Object(fields) => fields.values().for_each(|v| walk(v, found)),
            _ => {}
        }
    }
    let mut found = BTreeSet::new();
    for node in flow.nodes.values() {
        if let Ok(value) = serde_json::to_value(node) {
            walk(&value, &mut found);
        }
    }
    found
}

/// The devices a run from `trigger` looks at on its way: what the nodes after it read — their
/// checks, waits and expressions — but not what they only switch. The ones worth pretending
/// about in a dry run.
pub fn entities_after(flow: &Flow, trigger: &NodeId) -> Vec<EntityId> {
    let mut reached = BTreeSet::from([trigger.clone()]);
    let mut queue = VecDeque::from([trigger.clone()]);
    while let Some(id) = queue.pop_front() {
        for wire in flow.wires.iter().filter(|w| w.from.node == id) {
            if reached.insert(wire.to.clone()) {
                queue.push_back(wire.to.clone());
            }
        }
    }
    let mut reads = flow.clone();
    reads.nodes.retain(|id, node| {
        reached.contains(id) && id != trigger && !matches!(node, Node::Call { .. })
    });
    let mut worked: BTreeSet<EntityId> = BTreeSet::new();
    for (id, node) in &flow.nodes {
        // A call's own light is switched, not read; what its worked-out settings read is.
        if let Node::Call {
            data: Some(data), ..
        } = node
            && reached.contains(id)
        {
            for (_, expr) in data.exprs() {
                worked.extend(
                    expr.as_str()
                        .split(['\'', '"'])
                        .skip(1)
                        .step_by(2)
                        .filter_map(|literal| literal.parse().ok()),
                );
            }
        }
    }
    let mut found: Vec<EntityId> = entities_used(&reads).into_iter().chain(worked).collect();
    found.sort_by_key(|id| id.to_string());
    found.dedup();
    found
}

/// The first `'kind.object'` string literal in an expression.
fn first_entity_in(expr: &str) -> Option<EntityId> {
    expr.split(['\'', '"'])
        .skip(1)
        .step_by(2)
        .find_map(|literal| literal.parse().ok())
}

/// A state's value the way a person says it.
pub fn state_words(state: &EntityState, home: &Home) -> String {
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
        Some(State::BinarySensor(s)) => {
            let (on, off) = home.flag_words(&state.entity_id);
            if s.on { on } else { off }.into()
        }
        Some(State::Select(s)) => s.option.clone(),
        Some(State::Text(t)) => t.value.clone(),
        Some(State::Event(e)) => e.event_type.clone(),
        Some(State::Lock(l)) => l.state.as_str().to_owned(),
        Some(State::Valve(v)) => v.state.as_str().to_owned(),
        Some(State::Siren(s)) => if s.on { "sounding" } else { "quiet" }.into(),
        // The canvas line. A trigger compares `state` on its own (`playing`), not the title.
        Some(State::MediaPlayer(player)) => match player.title.as_deref() {
            Some(title) if !title.is_empty() => format!("{} · {title}", player.state.as_str()),
            _ => player.state.as_str().to_owned(),
        },
        Some(State::Humidifier(h)) => match (h.on, h.target_humidity) {
            (true, Some(target)) => format!("on {target}%"),
            (true, None) => "on".into(),
            (false, _) => "off".into(),
        },
        Some(State::WaterHeater(h)) => match h.target_temperature {
            Some(target) => format!("{} {target}°", h.operation_mode.as_str()),
            None => h.operation_mode.as_str().to_owned(),
        },
        Some(State::Climate(c)) => match c.target_temperature {
            Some(target) => format!("{} {target}°", c.hvac_mode.as_str()),
            None => c.hvac_mode.as_str().to_owned(),
        },
        Some(State::Fan(f)) => match (f.on, f.percentage) {
            (true, Some(p)) => format!("on {p}%"),
            (true, None) => "on".into(),
            (false, _) => "off".into(),
        },
        Some(State::Cover(c)) => match c.position {
            Some(position) => format!("{} {position}%", c.state.as_str()),
            None => c.state.as_str().to_owned(),
        },
        Some(State::Number(n)) => {
            if n.value.fract() == 0.0 {
                format!("{:.0}", n.value)
            } else {
                format!("{:.1}", n.value)
            }
        }
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
            .find(|entity| entity.capabilities.primary_shape() == Some(ValueShape::Number))
            .map(|entity| entity.id.to_string())
            .unwrap_or_else(|| "sensor.choose_one".into())
    })
}

pub const TEMPLATES: &[Template] = &[
    Template {
        group: "Triggers",
        label: "When something changes",
        base: "when",
        make: |home| {
            serde_json::json!({ "type": "trigger", "trigger": {
            "type": "state", "entity": pick(home, &[EntityKind::BinarySensor, EntityKind::Switch, EntityKind::Light]), "to": true } })
        },
    },
    Template {
        group: "Decide",
        label: "Condition",
        base: "check",
        make: |home| {
            serde_json::json!({ "type": "gate",
                "condition": crate::checks::Checks::starter(home).render() })
        },
    },
    Template {
        group: "Decide",
        label: "Case switch",
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
        label: "Calculate a value",
        base: "calculate",
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

/// A branch of the canvas: nodes wired together, whichever way the wires go, and the parts it
/// splits into.
#[derive(Debug, Clone, PartialEq)]
pub struct Branch {
    /// What its name is kept under: its first trigger, or its first node if it has none.
    pub key: NodeId,
    pub nodes: Vec<NodeId>,
    /// The runs of nodes it splits into, each one after another with no way off in between.
    /// Only when there are at least two: one run is just the branch.
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    /// What its name is kept under: its first node.
    pub key: NodeId,
    pub nodes: Vec<NodeId>,
}

/// The flow's branches, with at least two nodes each, in the order their keys sort.
pub fn branches(flow: &Flow) -> Vec<Branch> {
    // Which branch each node is in: joined by any wire, either way.
    let ids: Vec<&NodeId> = flow.nodes.keys().collect();
    let index = |id: &NodeId| ids.iter().position(|i| *i == id);
    let mut root: Vec<usize> = (0..ids.len()).collect();
    fn find(root: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    for wire in &flow.wires {
        if let (Some(a), Some(b)) = (index(&wire.from.node), index(&wire.to)) {
            let (a, b) = (find(&mut root, a), find(&mut root, b));
            root[a] = b;
        }
    }
    let mut groups: BTreeMap<usize, Vec<NodeId>> = BTreeMap::new();
    for (i, id) in ids.iter().enumerate() {
        let r = find(&mut root, i);
        groups.entry(r).or_default().push((*id).clone());
    }

    let incoming = |id: &NodeId| {
        flow.wires
            .iter()
            .filter(|w| &w.to == id)
            .collect::<Vec<_>>()
    };
    let outgoing = |id: &NodeId| flow.wires.iter().filter(|w| &w.from.node == id).count();
    let is_trigger = |id: &NodeId| flow.nodes.get(id).is_some_and(Node::is_trigger);

    let mut out: Vec<Branch> = groups
        .into_values()
        .filter(|nodes| nodes.len() > 1)
        .map(|nodes| {
            let key = nodes
                .iter()
                .find(|id| is_trigger(id))
                .unwrap_or(&nodes[0])
                .clone();
            // Runs: a node carries on its one way in's run when that comes from a node with
            // only one way out that isn't a trigger; anything else starts a run of its own.
            let mut part_of: BTreeMap<NodeId, NodeId> = BTreeMap::new();
            // How far along its run each node is, to list a run in the order it goes.
            let mut step: BTreeMap<NodeId, usize> = BTreeMap::new();
            let mut pending: Vec<NodeId> =
                nodes.iter().filter(|id| !is_trigger(id)).cloned().collect();
            let mut guard = 0;
            while !pending.is_empty() && guard <= nodes.len() {
                guard += 1;
                pending.retain(|id| {
                    let into = incoming(id);
                    let carries = match into.as_slice() {
                        [one] if !is_trigger(&one.from.node) && outgoing(&one.from.node) == 1 => {
                            Some(&one.from.node)
                        }
                        _ => None,
                    };
                    match carries {
                        None => {
                            part_of.insert(id.clone(), id.clone());
                            step.insert(id.clone(), 0);
                            false
                        }
                        Some(from) => match part_of.get(from).cloned() {
                            Some(start) => {
                                let at = step.get(from).copied().unwrap_or(0) + 1;
                                part_of.insert(id.clone(), start);
                                step.insert(id.clone(), at);
                                false
                            }
                            None => true,
                        },
                    }
                });
            }
            // Whatever's left sits in a circle; each is its own run.
            for id in pending {
                part_of.insert(id.clone(), id);
            }
            let mut parts: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
            for id in &nodes {
                if let Some(start) = part_of.get(id) {
                    parts.entry(start.clone()).or_default().push(id.clone());
                }
            }
            let parts: Vec<Part> = if parts.len() >= 2 {
                parts
                    .into_iter()
                    .map(|(key, mut nodes)| {
                        nodes.sort_by_key(|id| step.get(id).copied().unwrap_or(0));
                        Part { key, nodes }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Branch { key, nodes, parts }
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// A node id as words: `lights_off` → "Lights off".
pub fn words_of(id: &NodeId) -> String {
    let text = id.as_str().replace('_', " ");
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// A branch's name: the one it was given, or its triggers' names.
pub fn branch_name(flow: &Flow, branch: &Branch) -> String {
    if let Some(name) = flow.groups.get(&branch.key) {
        return name.to_string();
    }
    let triggers: Vec<String> = branch
        .nodes
        .iter()
        .filter(|id| flow.nodes.get(*id).is_some_and(Node::is_trigger))
        .map(words_of)
        .collect();
    if triggers.is_empty() {
        words_of(&branch.key)
    } else {
        triggers.join(" · ")
    }
}

/// A part's name: the one it was given, or its last node's — usually what it ends up doing.
pub fn part_name(flow: &Flow, part: &Part) -> String {
    flow.groups
        .get(&part.key)
        .map(ToString::to_string)
        .unwrap_or_else(|| words_of(part.nodes.last().unwrap_or(&part.key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(nodes: &[(&str, &str)], wires: &[[&str; 2]]) -> Flow {
        let nodes: serde_json::Map<String, serde_json::Value> = nodes
            .iter()
            .map(|(id, kind)| {
                let node = match *kind {
                    "trigger" => serde_json::json!({ "type": "trigger", "trigger": { "type": "startup" } }),
                    "gate" => serde_json::json!({ "type": "gate", "condition": { "type": "expr", "expr": "true" } }),
                    _ => serde_json::json!({ "type": "delay", "for": "1s" }),
                };
                ((*id).to_owned(), node)
            })
            .collect();
        serde_json::from_value(
            serde_json::json!({ "id": "f", "name": "F", "nodes": nodes, "wires": wires }),
        )
        .expect("a flow")
    }

    #[test]
    fn a_trigger_that_forks_three_ways_is_one_branch_of_three_parts() {
        // The shape of the TV flow: playing forks into three checks, each with its action; the
        // room emptying is a straight line; four triggers meet at one check.
        let f = flow(
            &[
                ("playing", "trigger"),
                ("lights_on_now", "gate"),
                ("lights_off", "act"),
                ("moon_is_on", "gate"),
                ("moon_off", "act"),
                ("dining_is_off", "gate"),
                ("dining_dim", "act"),
                ("area_cleared", "trigger"),
                ("cleared_bias_on", "gate"),
                ("cleared_bias_off", "act"),
                ("paused", "trigger"),
                ("idle", "trigger"),
                ("presence", "trigger"),
                ("light_needed", "gate"),
                ("turn_on", "act"),
                ("loose", "act"),
            ],
            &[
                ["playing", "lights_on_now"],
                ["playing", "moon_is_on"],
                ["playing", "dining_is_off"],
                ["lights_on_now:yes", "lights_off"],
                ["moon_is_on:yes", "moon_off"],
                ["dining_is_off:yes", "dining_dim"],
                ["area_cleared", "cleared_bias_on"],
                ["cleared_bias_on:yes", "cleared_bias_off"],
                ["paused", "light_needed"],
                ["idle", "light_needed"],
                ["presence", "light_needed"],
                ["light_needed:yes", "turn_on"],
            ],
        );
        let found = branches(&f);
        let keys: Vec<&str> = found.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(
            keys,
            ["area_cleared", "idle", "playing"],
            "a lone node isn't a branch"
        );
        let playing = &found[2];
        let parts: Vec<Vec<&str>> = playing
            .parts
            .iter()
            .map(|p| p.nodes.iter().map(NodeId::as_str).collect())
            .collect();
        assert_eq!(
            parts,
            [
                vec!["dining_is_off", "dining_dim"],
                vec!["lights_on_now", "lights_off"],
                vec!["moon_is_on", "moon_off"]
            ]
        );
        assert!(
            found[0].parts.is_empty(),
            "a straight line is just the branch"
        );
        assert!(
            found[1].parts.is_empty(),
            "triggers meeting at one line are just the branch"
        );
        assert_eq!(branch_name(&f, &found[1]), "Idle · Paused · Presence");
        assert_eq!(part_name(&f, &playing.parts[0]), "Dining dim");
    }
}
