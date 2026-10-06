//! Layer 3 (`docs/specs/flows.md` §4): the graph, and the flow against the home.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use irori_flow_types::api::{Problem, Severity};
use irori_flow_types::{Condition, Flow, Node, NodeId, Port, Trigger, WaitUntil, Wire};
use irori_rules::{ExprString, RegistryView, VarKind};

/// Everything wrong with `flow`, errors first. Errors unarm it; warnings don't.
pub fn check(flow: &Flow, registry: &impl RegistryView) -> Vec<Problem> {
    let mut problems = Vec::new();
    let wires_ok = check_wires(flow, &mut problems);
    if !flow.nodes.values().any(Node::is_trigger) {
        problems.push(error(
            None,
            None,
            "a flow needs a trigger: nothing would ever start it",
        ));
    }
    // The rest walks the graph, which only makes sense once every wire joins real ports.
    if wires_ok {
        if let Some(order) = topological(flow) {
            check_reachable(flow, &mut problems);
            check_joins(flow, &mut problems);
            check_merges(flow, &mut problems);
            let vars = check_vars(flow, &order, registry, &mut problems);
            check_home(flow, registry, &vars, &mut problems);
        } else {
            problems.push(error(
                None,
                None,
                "the wires go round in a circle; a flow has to run one way, from its triggers",
            ));
        }
    }
    problems.sort_by_key(|problem| std::cmp::Reverse(problem.severity));
    problems
}

fn error(node: Option<&NodeId>, wire: Option<usize>, message: impl Into<String>) -> Problem {
    Problem {
        severity: Severity::Error,
        node: node.cloned(),
        wire,
        field: None,
        message: message.into(),
    }
}

fn warning(node: Option<&NodeId>, wire: Option<usize>, message: impl Into<String>) -> Problem {
    Problem {
        severity: Severity::Warning,
        ..error(node, wire, message)
    }
}

/// Every wire joins real nodes by real ports, once. `false` if any doesn't.
fn check_wires(flow: &Flow, problems: &mut Vec<Problem>) -> bool {
    let before = problems.len();
    let mut seen = BTreeSet::new();
    for (i, wire) in flow.wires.iter().enumerate() {
        let Some(from) = flow.nodes.get(&wire.from.node) else {
            problems.push(error(
                None,
                Some(i),
                format!("a wire starts at `{}`, which isn't a node", wire.from.node),
            ));
            continue;
        };
        if !from.ports().contains(&wire.from.port) {
            let ports: Vec<String> = from.ports().iter().map(ToString::to_string).collect();
            problems.push(error(
                Some(&wire.from.node),
                Some(i),
                if ports.is_empty() {
                    format!(
                        "a {} has no way out, so nothing can be wired from it",
                        from.kind()
                    )
                } else {
                    format!(
                        "a {} has no `{}` port; it has {}",
                        from.kind(),
                        wire.from.port,
                        ports.join(", ")
                    )
                },
            ));
        }
        match flow.nodes.get(&wire.to) {
            None => problems.push(error(
                None,
                Some(i),
                format!("a wire goes to `{}`, which isn't a node", wire.to),
            )),
            Some(to) if to.is_trigger() => problems.push(error(
                Some(&wire.to),
                Some(i),
                "nothing can be wired into a trigger: it only starts runs",
            )),
            Some(_) => {}
        }
        if !seen.insert(wire) {
            problems.push(error(
                Some(&wire.from.node),
                Some(i),
                format!("the wire {wire} is there twice"),
            ));
        }
    }
    problems.len() == before
}

/// The nodes in an order where every wire goes forward, or `None` if the wires make a circle.
pub fn topological(flow: &Flow) -> Option<Vec<NodeId>> {
    let mut incoming: BTreeMap<&NodeId, usize> = flow.nodes.keys().map(|id| (id, 0)).collect();
    for wire in &flow.wires {
        if let Some(count) = incoming.get_mut(&wire.to) {
            *count += 1;
        }
    }
    let mut ready: VecDeque<&NodeId> = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut order = Vec::new();
    while let Some(id) = ready.pop_front() {
        order.push(id.clone());
        for wire in flow.wires.iter().filter(|wire| &wire.from.node == id) {
            if let Some(count) = incoming.get_mut(&wire.to) {
                *count -= 1;
                if *count == 0 {
                    ready.push_back(&wire.to);
                }
            }
        }
    }
    (order.len() == flow.nodes.len()).then_some(order)
}

/// Every node reachable from `start`, `start` included.
fn reach(flow: &Flow, start: &NodeId) -> BTreeSet<NodeId> {
    let mut seen = BTreeSet::from([start.clone()]);
    let mut queue = VecDeque::from([start.clone()]);
    while let Some(id) = queue.pop_front() {
        for wire in flow.wires.iter().filter(|wire| wire.from.node == id) {
            if seen.insert(wire.to.clone()) {
                queue.push_back(wire.to.clone());
            }
        }
    }
    seen
}

fn check_reachable(flow: &Flow, problems: &mut Vec<Problem>) {
    let mut reached = BTreeSet::new();
    for (id, node) in &flow.nodes {
        if node.is_trigger() {
            reached.extend(reach(flow, id));
        }
    }
    for id in flow.nodes.keys() {
        if !reached.contains(id) {
            problems.push(warning(
                Some(id),
                None,
                "no trigger leads here, so this never runs",
            ));
        }
    }
}

fn check_joins(flow: &Flow, problems: &mut Vec<Problem>) {
    for (id, node) in &flow.nodes {
        if matches!(node, Node::Join { .. }) && flow.wires_into(id).count() < 2 {
            problems.push(warning(
                Some(id),
                None,
                "a join brings paths together, but fewer than two come in here",
            ));
        }
    }
}

/// F4: two paths of one run that forked meet again at a node other than a join. Paths that part
/// at different ports of one node never both run, and paths from different triggers are
/// different runs; neither counts.
fn check_merges(flow: &Flow, problems: &mut Vec<Problem>) {
    let reaches: BTreeMap<&NodeId, BTreeSet<NodeId>> =
        flow.nodes.keys().map(|id| (id, reach(flow, id))).collect();
    // Whether a token on `lead` can go on to travel `wire`.
    let leads_to = |lead: &Wire, wire: &Wire| {
        lead == wire
            || reaches
                .get(&lead.to)
                .is_some_and(|reached| reached.contains(&wire.from.node))
    };
    // Every port with more than one wire: a fork.
    let mut forks: BTreeMap<(&NodeId, Port), Vec<&Wire>> = BTreeMap::new();
    for wire in &flow.wires {
        forks
            .entry((&wire.from.node, wire.from.port))
            .or_default()
            .push(wire);
    }
    forks.retain(|_, wires| wires.len() > 1);

    for (id, node) in &flow.nodes {
        if matches!(node, Node::Join { .. }) {
            continue;
        }
        let incoming: Vec<&Wire> = flow.wires_into(id).collect();
        let merged = incoming.iter().enumerate().any(|(i, a)| {
            incoming[i + 1..].iter().any(|b| {
                forks.values().any(|fork| {
                    fork.iter().enumerate().any(|(x, left)| {
                        fork.iter()
                            .enumerate()
                            .any(|(y, right)| x != y && leads_to(left, a) && leads_to(right, b))
                    })
                })
            })
        });
        if merged {
            problems.push(warning(
                Some(id),
                None,
                "paths that split earlier meet again here, so this can run more than once per \
                 run; put a join in front of it to run it once",
            ));
        }
    }
}

/// The expressions a node holds, for variables and the home check.
fn node_exprs(node: &Node) -> Vec<&ExprString> {
    fn walk<'a>(condition: &'a Condition, out: &mut Vec<&'a ExprString>) {
        match condition {
            Condition::Expr { expr } => out.push(expr),
            Condition::All { conditions } | Condition::Any { conditions } => {
                for child in conditions {
                    walk(child, out);
                }
            }
            Condition::Not { condition } => walk(condition, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    match node {
        Node::Gate { condition } => walk(condition, &mut out),
        Node::Switch { cases } => {
            for case in cases {
                walk(case, &mut out);
            }
        }
        Node::Set { expr, .. } => out.push(expr),
        Node::Wait {
            until: WaitUntil::Expr { expr, .. },
            ..
        } => out.push(expr),
        Node::Call {
            data: Some(data), ..
        } => out.extend(data.exprs().into_iter().map(|(_, expr)| expr)),
        _ => {}
    }
    out
}

/// Types every variable from its `set`s, and checks each `var()` is set on every path that
/// reaches it. Answers the types, for the home check.
fn check_vars(
    flow: &Flow,
    order: &[NodeId],
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) -> BTreeMap<String, VarKind> {
    // Types, in an order where each set's own expression can use the variables set before it.
    let mut kinds: BTreeMap<String, VarKind> = BTreeMap::new();
    for id in order {
        if let Some(Node::Set { name, expr }) = flow.nodes.get(id)
            && let Ok(inspected) = irori_rules::inspect(expr, registry, &kinds)
        {
            match kinds.get(name.as_str()) {
                Some(kind)
                    if *kind != inspected.kind
                        && *kind != VarKind::Scalar
                        && inspected.kind != VarKind::Scalar =>
                {
                    problems.push(error(
                        Some(id),
                        None,
                        format!(
                            "`{name}` is set as {} here but as {} elsewhere; a variable has one type",
                            kind_word(inspected.kind),
                            kind_word(*kind)
                        ),
                    ));
                }
                _ => {
                    kinds.insert(name.as_str().to_owned(), inspected.kind);
                }
            }
        }
    }
    // What's set on every path to each node: the intersection over the paths coming in.
    let mut set_before: BTreeMap<&NodeId, BTreeSet<String>> = BTreeMap::new();
    for id in order {
        let mut incoming = flow.wires_into(id).map(|wire| {
            let mut set = set_before.get(&wire.from.node).cloned().unwrap_or_default();
            if let Some(Node::Set { name, .. }) = flow.nodes.get(&wire.from.node) {
                set.insert(name.as_str().to_owned());
            }
            set
        });
        let first = incoming.next().unwrap_or_default();
        let before = incoming.fold(first, |all, set| all.intersection(&set).cloned().collect());
        set_before.insert(id, before);
    }
    for id in order {
        let Some(node) = flow.nodes.get(id) else {
            continue;
        };
        let before = set_before.get(id).cloned().unwrap_or_default();
        for expr in node_exprs(node) {
            for name in irori_rules::vars_read(expr) {
                if !before.contains(&name) {
                    problems.push(error(
                        Some(id),
                        None,
                        if kinds.contains_key(&name) {
                            format!(
                                "var('{name}') isn't set on every path that reaches here; set it \
                                 earlier on each of them"
                            )
                        } else {
                            format!("var('{name}') is never set in this flow")
                        },
                    ));
                }
            }
        }
    }
    kinds
}

fn kind_word(kind: VarKind) -> &'static str {
    match kind {
        VarKind::Bool => "true/false",
        VarKind::Number => "a number",
        VarKind::String => "text",
        VarKind::Scalar => "an attribute",
    }
}

/// Each node against the home: entities, kinds, services, capabilities, expressions.
fn check_home(
    flow: &Flow,
    registry: &impl RegistryView,
    vars: &BTreeMap<String, VarKind>,
    problems: &mut Vec<Problem>,
) {
    for (id, node) in &flow.nodes {
        let found = match node {
            Node::Trigger { trigger } => {
                if matches!(trigger, Trigger::Event { .. }) {
                    problems.push(warning(
                        Some(id),
                        None,
                        "nothing sends events yet, so this trigger won't fire until protocols do",
                    ));
                }
                irori_rules::check_trigger("trigger", trigger, registry)
            }
            Node::Gate { condition } => {
                irori_rules::check_condition("condition", condition, registry, vars)
            }
            Node::Switch { cases } => cases
                .iter()
                .enumerate()
                .flat_map(|(i, case)| {
                    irori_rules::check_condition(&format!("cases/{i}"), case, registry, vars)
                })
                .collect(),
            Node::Call {
                service,
                entity,
                data,
            } => {
                // A worked-out number stands in as three different ones from its field's
                // range. The entity's own range can be narrower (this lamp's colour
                // temperatures against any lamp's), and a stand-in refused for where it
                // happens to be says nothing about the flow: that refusal names the number, so
                // it differs between the three. What all three are told alike is real.
                let mut found = match data {
                    None => irori_rules::check_call("call", *service, entity, None, registry),
                    Some(data) => {
                        let [low, middle, high] = data.shapes(*service).map(|shape| {
                            irori_rules::check_call(
                                "call",
                                *service,
                                entity,
                                Some(&shape),
                                registry,
                            )
                        });
                        low.into_iter()
                            .filter(|problem| middle.contains(problem) && high.contains(problem))
                            .collect()
                    }
                };
                for (field, expr) in data.iter().flat_map(irori_flow_types::FlowCallData::exprs) {
                    let reason = match irori_rules::inspect(expr, registry, vars) {
                        Ok(inspected)
                            if matches!(inspected.kind, VarKind::Number | VarKind::Scalar) =>
                        {
                            continue;
                        }
                        Ok(inspected) => format!(
                            "{field} has to work out to a number, but this gives {}",
                            kind_word(inspected.kind)
                        ),
                        Err(reason) => reason,
                    };
                    found.push(irori_rules::Problem {
                        path: format!("data/{field}"),
                        reason,
                    });
                }
                found
            }
            Node::Set { expr, .. } => match irori_rules::inspect(expr, registry, vars) {
                Ok(_) => Vec::new(),
                Err(reason) => vec![irori_rules::Problem {
                    path: "expr".into(),
                    reason,
                }],
            },
            Node::Wait { until, .. } => irori_rules::check_wait("until", until, registry, vars),
            Node::Delay { .. } | Node::Join { .. } | Node::Stop { .. } => Vec::new(),
        };
        for problem in found {
            // Variables are checked above, with the paths; don't say it twice.
            if problem.reason.starts_with("var(") {
                continue;
            }
            problems.push(Problem {
                severity: Severity::Error,
                node: Some(id.clone()),
                wire: None,
                field: Some(problem.path),
                message: problem.reason,
            });
        }
    }
}

/// Every entity the flow reads or acts on: its triggers', conditions', waits' and calls'
/// entities, and what its expressions read. For the "why didn't it fire?" timeline and backtests.
pub fn watched(flow: &Flow, registry: &impl RegistryView) -> BTreeSet<irori_types::EntityId> {
    let mut out = BTreeSet::new();
    for node in flow.nodes.values() {
        match node {
            Node::Trigger {
                trigger: Trigger::State { entity, .. },
            }
            | Node::Call { entity, .. } => {
                out.insert(entity.clone());
            }
            Node::Gate { condition } => watched_condition(condition, &mut out),
            Node::Switch { cases } => {
                for case in cases {
                    watched_condition(case, &mut out);
                }
            }
            Node::Wait {
                until: WaitUntil::State { entity, .. },
                ..
            } => {
                out.insert(entity.clone());
            }
            _ => {}
        }
        for expr in node_exprs(node) {
            if let Ok(inspected) = irori_rules::inspect(expr, registry, &every_var(flow)) {
                out.extend(inspected.ids);
            }
        }
    }
    out
}

fn watched_condition(condition: &Condition, out: &mut BTreeSet<irori_types::EntityId>) {
    match condition {
        Condition::State { entity, .. } => {
            out.insert(entity.clone());
        }
        Condition::All { conditions } | Condition::Any { conditions } => {
            for child in conditions {
                watched_condition(child, out);
            }
        }
        Condition::Not { condition } => watched_condition(condition, out),
        _ => {}
    }
}

/// Every variable the flow sets, loosely typed: enough to read its expressions' entities.
fn every_var(flow: &Flow) -> BTreeMap<String, VarKind> {
    flow.nodes
        .values()
        .filter_map(|node| match node {
            Node::Set { name, .. } => Some((name.as_str().to_owned(), VarKind::Scalar)),
            _ => None,
        })
        .collect()
}
