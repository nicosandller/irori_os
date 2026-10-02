//! Parse, AST allow-list, and registry type-check (layer 3). No scheduler.

use std::collections::{BTreeMap, BTreeSet};

use cel::Program;
use cel::common::ast::{CallExpr, Expr, IdedExpr, LiteralValue};
use irori_types::{Capabilities, Entity, EntityId, ValueShape};

use crate::{
    Action, CallData, Condition, ExprString, LightCallData, Rule, RuleService, Trigger, TypedValue,
    WaitUntil,
};

use crate::expr::compile;

const ENTITY_FNS: &[&str] = &[
    "num",
    "on",
    "text",
    "brightness",
    "available",
    "unknown",
    "attr",
];
const CLOCK_FNS: &[&str] = &["hour", "minute", "now_ts"];
/// Maths on numbers: `min(a, b)`, `max(a, b)`, `round(x)`, `clamp(x, low, high)`.
const MATH_FNS: &[&str] = &["min", "max", "round", "clamp"];
const OPS: &[&str] = &[
    "_&&_", "_||_", "!_", "_+_", "_-_", "_*_", "_/_", "_==_", "_!=_", "_>=_", "_<=_", "_>_", "_<_",
    "-_", "_?_:_",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

/// Registry facts used at save time. Not live values.
pub trait RegistryView {
    fn entity(&self, id: &EntityId) -> Option<&Entity>;
    fn has_timezone(&self) -> bool;
    fn has_location(&self) -> bool;
}

impl RegistryView for BTreeMap<EntityId, Entity> {
    fn entity(&self, id: &EntityId) -> Option<&Entity> {
        self.get(id)
    }

    fn has_timezone(&self) -> bool {
        false
    }

    fn has_location(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub struct MapRegistry {
    pub entities: BTreeMap<EntityId, Entity>,
    pub timezone: bool,
    pub location: bool,
}

impl RegistryView for MapRegistry {
    fn entity(&self, id: &EntityId) -> Option<&Entity> {
        self.entities.get(id)
    }

    fn has_timezone(&self) -> bool {
        self.timezone
    }

    fn has_location(&self) -> bool {
        self.location
    }
}

/// The type of an expression or a run variable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VarKind {
    Bool,
    Number,
    String,
    /// `attr()`: whatever the protocol put there.
    Scalar,
}

type ExprKind = VarKind;

/// Type-check a rule against the registry. Does not arm it.
pub fn validate(rule: &Rule, registry: &impl RegistryView) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut sets = BTreeMap::new();
    collect_sets("actions", &rule.actions, registry, &mut sets, &mut problems);
    walk_triggers("triggers", &rule.triggers, registry, &mut problems);
    walk_conditions(
        "conditions",
        &rule.conditions,
        registry,
        &sets,
        &mut problems,
    );
    walk_actions("actions", &rule.actions, registry, &sets, &mut problems);
    problems
}

fn walk_triggers(
    path: &str,
    triggers: &[Trigger],
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) {
    for (i, trigger) in triggers.iter().enumerate() {
        walk_triggers_one(&format!("{path}/{i}"), trigger, registry, problems);
    }
}

fn walk_triggers_one(
    here: &str,
    trigger: &Trigger,
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) {
    match trigger {
        Trigger::State {
            entity,
            from,
            to,
            above,
            below,
            ..
        } => {
            check_state_match(here, entity, from.as_ref(), None, None, registry, problems);
            for value in to.iter().flat_map(crate::Values::iter) {
                check_state_match(here, entity, None, Some(value), None, registry, problems);
            }
            if (above.is_some() || below.is_some())
                && let Some(found) = registry.entity(entity)
                && found.capabilities.primary_shape() != Some(ValueShape::Number)
            {
                problems.push(problem(
                    here,
                    format!(
                        "above and below are for a sensor with numbers, and {entity} is a {}",
                        found.capabilities.kind()
                    ),
                ));
            }
        }
        Trigger::Time { .. } => {
            if !registry.has_timezone() {
                problems.push(problem(
                    here,
                    "time triggers need a timezone; it isn't in irori.toml yet",
                ));
            }
        }
        Trigger::Sun { .. } => check_sun_gate(here, registry, problems),
        Trigger::Event { .. } | Trigger::Startup {} => {}
    }
}

fn check_sun_gate(path: &str, registry: &impl RegistryView, problems: &mut Vec<Problem>) {
    if !registry.has_timezone() {
        problems.push(problem(
            path,
            "sun triggers need a timezone; it isn't in irori.toml yet",
        ));
    }
    if !registry.has_location() {
        problems.push(problem(
            path,
            "sun windows need a location (lat/lon); it isn't in irori.toml yet",
        ));
    }
}

fn walk_conditions(
    path: &str,
    conditions: &[Condition],
    registry: &impl RegistryView,
    sets: &BTreeMap<String, ExprKind>,
    problems: &mut Vec<Problem>,
) {
    for (i, condition) in conditions.iter().enumerate() {
        walk_condition(&format!("{path}/{i}"), condition, registry, sets, problems);
    }
}

fn walk_condition(
    path: &str,
    condition: &Condition,
    registry: &impl RegistryView,
    sets: &BTreeMap<String, ExprKind>,
    problems: &mut Vec<Problem>,
) {
    match condition {
        Condition::Expr { expr } => {
            check_expr(path, expr, ExprRole::Bool, registry, sets, problems);
        }
        Condition::State { entity, is, .. } => check_state_match(
            path,
            entity,
            None,
            is.as_ref(),
            Some("is"),
            registry,
            problems,
        ),
        Condition::Time { .. } => {
            if !registry.has_timezone() {
                problems.push(problem(
                    path,
                    "time windows need a timezone; it isn't in irori.toml yet",
                ));
            }
        }
        Condition::Sun { .. } => check_sun_gate(path, registry, problems),
        Condition::All { conditions } | Condition::Any { conditions } => {
            walk_conditions(
                &format!("{path}/conditions"),
                conditions,
                registry,
                sets,
                problems,
            );
        }
        Condition::Not { condition } => {
            walk_condition(
                &format!("{path}/condition"),
                condition,
                registry,
                sets,
                problems,
            );
        }
    }
}

fn walk_actions(
    path: &str,
    actions: &[Action],
    registry: &impl RegistryView,
    sets: &BTreeMap<String, ExprKind>,
    problems: &mut Vec<Problem>,
) {
    for (i, action) in actions.iter().enumerate() {
        let here = format!("{path}/{i}");
        match action {
            Action::Call {
                service,
                target,
                data,
                ..
            } => check_call_inner(
                &here,
                *service,
                &target.entity,
                data.as_ref(),
                registry,
                problems,
            ),
            Action::Wait { until, .. } => match until {
                WaitUntil::State { entity, is, .. } => check_state_match(
                    &format!("{here}/until"),
                    entity,
                    None,
                    is.as_ref(),
                    Some("is"),
                    registry,
                    problems,
                ),
                WaitUntil::Expr { expr, .. } => {
                    check_expr(
                        &format!("{here}/until"),
                        expr,
                        ExprRole::Wait,
                        registry,
                        sets,
                        problems,
                    );
                }
            },
            Action::If {
                conditions,
                then,
                r#else,
            } => {
                walk_conditions(
                    &format!("{here}/conditions"),
                    conditions,
                    registry,
                    sets,
                    problems,
                );
                walk_actions(&format!("{here}/then"), then, registry, sets, problems);
                walk_actions(&format!("{here}/else"), r#else, registry, sets, problems);
            }
            Action::Choose { options, otherwise } => {
                for (j, option) in options.iter().enumerate() {
                    let opt = format!("{here}/options/{j}");
                    walk_conditions(
                        &format!("{opt}/conditions"),
                        &option.conditions,
                        registry,
                        sets,
                        problems,
                    );
                    walk_actions(
                        &format!("{opt}/then"),
                        &option.then,
                        registry,
                        sets,
                        problems,
                    );
                }
                walk_actions(
                    &format!("{here}/default"),
                    otherwise,
                    registry,
                    sets,
                    problems,
                );
            }
            Action::Set { expr, .. } => {
                check_expr(&here, expr, ExprRole::Value, registry, sets, problems);
            }
            Action::Delay { .. } | Action::Event { .. } | Action::Stop { .. } => {}
        }
    }
}

fn collect_sets(
    path: &str,
    actions: &[Action],
    registry: &impl RegistryView,
    sets: &mut BTreeMap<String, ExprKind>,
    problems: &mut Vec<Problem>,
) {
    for (i, action) in actions.iter().enumerate() {
        let here = format!("{path}/{i}");
        match action {
            Action::Set { name, expr, .. } => {
                if let Ok(inspected) = inspect_expr(expr, registry, sets) {
                    if let Some(previous) = sets.get(name.as_str())
                        && *previous != inspected.kind
                        && *previous != ExprKind::Scalar
                        && inspected.kind != ExprKind::Scalar
                    {
                        problems.push(problem(
                            &here,
                            format!(
                                "var {name:?} is set as both {previous:?} and {:?}",
                                inspected.kind
                            ),
                        ));
                    } else {
                        sets.insert(name.as_str().to_owned(), inspected.kind);
                    }
                }
            }
            Action::If { then, r#else, .. } => {
                collect_sets(&format!("{here}/then"), then, registry, sets, problems);
                collect_sets(&format!("{here}/else"), r#else, registry, sets, problems);
            }
            Action::Choose { options, otherwise } => {
                for (j, option) in options.iter().enumerate() {
                    collect_sets(
                        &format!("{here}/options/{j}/then"),
                        &option.then,
                        registry,
                        sets,
                        problems,
                    );
                }
                collect_sets(
                    &format!("{here}/default"),
                    otherwise,
                    registry,
                    sets,
                    problems,
                );
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
enum ExprRole {
    Bool,
    Wait,
    Value,
}

fn check_expr(
    path: &str,
    expr: &ExprString,
    role: ExprRole,
    registry: &impl RegistryView,
    sets: &BTreeMap<String, ExprKind>,
    problems: &mut Vec<Problem>,
) {
    let inspected = match inspect_expr(expr, registry, sets) {
        Ok(inspected) => inspected,
        Err(reason) => {
            problems.push(problem(path, reason));
            return;
        }
    };
    match role {
        ExprRole::Wait => {
            if inspected.uses_clock {
                problems.push(problem(
                    path,
                    "hour() cannot drive a wait; use a time trigger",
                ));
            }
            if inspected.ids.is_empty() {
                problems.push(problem(
                    path,
                    "a wait expression has to read at least one entity (num/on/…); use a delay or a time trigger otherwise",
                ));
            }
            if inspected.kind != ExprKind::Bool {
                problems.push(problem(path, "a wait expression must be a boolean"));
            }
        }
        ExprRole::Bool => {
            if inspected.uses_clock && !registry.has_timezone() {
                problems.push(problem(
                    path,
                    "time triggers need a timezone; it isn't in irori.toml yet",
                ));
            }
            if inspected.kind != ExprKind::Bool {
                problems.push(problem(path, "a condition expression must be a boolean"));
            }
        }
        ExprRole::Value => {
            if inspected.uses_clock && !registry.has_timezone() {
                problems.push(problem(
                    path,
                    "time triggers need a timezone; it isn't in irori.toml yet",
                ));
            }
        }
    }
}

/// What an expression is, found at save time: the entities it reads, whether it reads the clock,
/// and its type.
#[derive(Debug, Clone)]
pub struct Inspected {
    pub ids: BTreeSet<EntityId>,
    pub uses_clock: bool,
    pub kind: VarKind,
}

/// Parses an expression inside the allowed surface and type-checks it against the registry,
/// with `vars` the run variables it may read.
pub fn inspect(
    expr: &ExprString,
    registry: &impl RegistryView,
    vars: &BTreeMap<String, VarKind>,
) -> Result<Inspected, String> {
    inspect_expr(expr, registry, vars)
}

/// Checks one trigger against the home; problems are reported at `path`.
pub fn check_trigger(path: &str, trigger: &Trigger, registry: &impl RegistryView) -> Vec<Problem> {
    let mut problems = Vec::new();
    walk_triggers_one(path, trigger, registry, &mut problems);
    problems
}

/// Checks one condition (and its children) against the home.
pub fn check_condition(
    path: &str,
    condition: &Condition,
    registry: &impl RegistryView,
    vars: &BTreeMap<String, VarKind>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    walk_condition(path, condition, registry, vars, &mut problems);
    problems
}

/// Checks a wait matcher against the home.
pub fn check_wait(
    path: &str,
    until: &WaitUntil,
    registry: &impl RegistryView,
    vars: &BTreeMap<String, VarKind>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    match until {
        WaitUntil::State { entity, is, .. } => check_state_match(
            path,
            entity,
            None,
            is.as_ref(),
            Some("is"),
            registry,
            &mut problems,
        ),
        WaitUntil::Expr { expr, .. } => {
            check_expr(path, expr, ExprRole::Wait, registry, vars, &mut problems);
        }
    }
    problems
}

/// Checks a service call against the entity it names.
pub fn check_call(
    path: &str,
    service: RuleService,
    entity: &EntityId,
    data: Option<&CallData>,
    registry: &impl RegistryView,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    check_call_inner(path, service, entity, data, registry, &mut problems);
    problems
}

/// The run variables an expression reads (`var('name')`). Empty if it doesn't parse — that's
/// [`inspect`]'s to report.
pub fn vars_read(expr: &ExprString) -> BTreeSet<String> {
    fn walk(expr: &IdedExpr, names: &mut BTreeSet<String>) {
        if let Expr::Call(call) = &expr.expr {
            if crate::expr::author_name(call.func_name.as_str()) == "var"
                && let Some(name) = call.args.first().and_then(string_literal)
            {
                names.insert(name.to_owned());
            }
            for arg in &call.args {
                walk(arg, names);
            }
            if let Some(target) = &call.target {
                walk(target, names);
            }
        }
    }
    let mut names = BTreeSet::new();
    if let Ok(compiled) = compile(expr.as_str()) {
        walk(compiled.program().expression(), &mut names);
    }
    names
}

fn inspect_expr(
    expr: &ExprString,
    registry: &impl RegistryView,
    sets: &BTreeMap<String, ExprKind>,
) -> Result<Inspected, String> {
    let compiled = compile(expr.as_str()).map_err(|error| error.to_string())?;
    let ast = compiled.program().expression();
    let mut ids = BTreeSet::new();
    let mut uses_clock = false;
    walk_ast(ast, registry, sets, &mut ids, &mut uses_clock)?;
    let kind = infer_type(ast, sets)?;
    Ok(Inspected {
        ids,
        uses_clock,
        kind,
    })
}

fn walk_ast(
    expr: &IdedExpr,
    registry: &impl RegistryView,
    sets: &BTreeMap<String, ExprKind>,
    ids: &mut BTreeSet<EntityId>,
    uses_clock: &mut bool,
) -> Result<(), String> {
    match &expr.expr {
        Expr::Unspecified | Expr::Literal(_) => Ok(()),
        Expr::Ident(name) => Err(format!(
            "unknown name {name:?}; use num/on/… with a string literal entity id"
        )),
        Expr::List(_) | Expr::Map(_) | Expr::Struct(_) => {
            Err("lists, maps, and structs aren't allowed in rule expressions".into())
        }
        Expr::Select(_) => Err("field access isn't allowed; use num/on/text/…".into()),
        Expr::Comprehension(_) => Err("macros like map/filter/exists aren't allowed".into()),
        Expr::Call(call) => {
            let name = crate::expr::author_name(call.func_name.as_str());
            if name == "has" || name == "duration" || name == "timestamp" {
                return Err(format!("{name}() isn't allowed in rule expressions"));
            }
            if name == "map"
                || name == "filter"
                || name == "exists"
                || name == "exists_one"
                || name == "all"
            {
                return Err("macros like map/filter/exists aren't allowed".into());
            }
            if CLOCK_FNS.contains(&name) {
                *uses_clock = true;
            }
            check_arity(name, call.args.len())?;
            if name == "_==_" || name == "_!=_" {
                check_text_comparison(call, registry)?;
            }
            if ENTITY_FNS.contains(&name) {
                let Some(first) = call.args.first() else {
                    return Err(format!("{name}() needs a string literal entity id"));
                };
                let id = string_literal(first).ok_or_else(|| {
                    format!("{name}() needs a string literal entity id, not an expression")
                })?;
                if name != "attr" {
                    let entity_id: EntityId = id
                        .parse()
                        .map_err(|e: irori_types::IdError| e.to_string())?;
                    ids.insert(entity_id.clone());
                    check_fn_against_registry(name, &entity_id, registry)?;
                } else {
                    let entity_id: EntityId = id
                        .parse()
                        .map_err(|e: irori_types::IdError| e.to_string())?;
                    ids.insert(entity_id.clone());
                    if registry.entity(&entity_id).is_none() {
                        return Err(format!(
                            "{name}({id:?}): no such entity — check the entity id"
                        ));
                    }
                    let key = call.args.get(1).and_then(string_literal).ok_or_else(|| {
                        "attr() needs a string literal key, not an expression".to_owned()
                    })?;
                    key.parse::<irori_types::AttributeKey>()
                        .map_err(|e| e.to_string())?;
                }
            } else if name == "var" {
                let Some(first) = call.args.first() else {
                    return Err("var() needs a string literal name".into());
                };
                let Some(var_name) = string_literal(first) else {
                    return Err("var() needs a string literal name, not an expression".into());
                };
                var_name
                    .parse::<irori_types::ObjectId>()
                    .map_err(|e| e.to_string())?;
                if !sets.contains_key(var_name) {
                    return Err(format!(
                        "var({var_name:?}): no set for this name in the rule"
                    ));
                }
            } else if !OPS.contains(&name)
                && !CLOCK_FNS.contains(&name)
                && !MATH_FNS.contains(&name)
            {
                return Err(format!("unknown function {name}()"));
            }
            for arg in &call.args {
                walk_ast(arg, registry, sets, ids, uses_clock)?;
            }
            if let Some(target) = &call.target {
                walk_ast(target, registry, sets, ids, uses_clock)?;
            }
            Ok(())
        }
    }
}

fn check_arity(name: &str, n: usize) -> Result<(), String> {
    let expected = match name {
        "num" | "on" | "text" | "brightness" | "available" | "unknown" | "var" => Some(1),
        "attr" | "min" | "max" => Some(2),
        "round" => Some(1),
        "clamp" => Some(3),
        "hour" | "minute" | "now_ts" => Some(0),
        "!_" | "-_" => Some(1),
        "_&&_" | "_||_" | "_+_" | "_-_" | "_*_" | "_/_" | "_==_" | "_!=_" | "_>=_" | "_<=_"
        | "_>_" | "_<_" => Some(2),
        "_?_:_" => Some(3),
        _ => None,
    };
    if let Some(expected) = expected
        && n != expected
    {
        return Err(format!("{name}() takes {expected} argument(s), not {n}"));
    }
    Ok(())
}

fn string_literal(expr: &IdedExpr) -> Option<&str> {
    match &expr.expr {
        Expr::Literal(LiteralValue::String(s)) => Some(s.as_ref()),
        _ => None,
    }
}

fn check_fn_against_registry(
    name: &str,
    id: &EntityId,
    registry: &impl RegistryView,
) -> Result<(), String> {
    let Some(entity) = registry.entity(id) else {
        return Err(format!(
            "{name}(\"{id}\"): no such entity — check the entity id"
        ));
    };
    let Some(shape) = entity.capabilities.primary_shape() else {
        // `available()` and `unknown()` are about the entity, not a value, so they still apply.
        return match name {
            "num" | "text" | "on" | "brightness" => Err(format!(
                "{name}(\"{id}\"): entity is a {}, which has no value to read",
                entity.capabilities.kind()
            )),
            _ => Ok(()),
        };
    };
    match (name, &entity.capabilities) {
        ("num", _) if shape == ValueShape::Number => Ok(()),
        ("num", _) => {
            let instead = if shape == ValueShape::Text {
                "text"
            } else {
                "on"
            };
            Err(format!(
                "num(\"{id}\"): entity is {}, not a numeric sensor — use {instead}(\"{id}\")",
                entity.capabilities.kind()
            ))
        }
        ("text", _) if shape == ValueShape::Text => Ok(()),
        ("text", _) => Err(format!("text(\"{id}\"): entity is not a text sensor")),
        ("on", _) if shape == ValueShape::Bool => Ok(()),
        ("on", _) if shape == ValueShape::Text => Err(format!(
            "on(\"{id}\"): entity is text, not on/off — use text(\"{id}\")"
        )),
        ("on", _) => Err(format!(
            "on(\"{id}\"): entity is a number, not on/off — use num(\"{id}\")"
        )),
        ("brightness", Capabilities::Light(light)) if light.brightness => Ok(()),
        ("brightness", Capabilities::Light(_)) => {
            Err(format!("brightness(\"{id}\"): this light is not dimmable"))
        }
        ("brightness", _) => Err(format!("brightness(\"{id}\"): entity is not a light")),
        ("available" | "unknown" | "attr", _) => Ok(()),
        _ => Ok(()),
    }
}

fn infer_type(expr: &IdedExpr, sets: &BTreeMap<String, ExprKind>) -> Result<ExprKind, String> {
    match &expr.expr {
        Expr::Literal(LiteralValue::Boolean(_)) => Ok(ExprKind::Bool),
        Expr::Literal(LiteralValue::Int(_) | LiteralValue::UInt(_) | LiteralValue::Double(_)) => {
            Ok(ExprKind::Number)
        }
        Expr::Literal(LiteralValue::String(_)) => Ok(ExprKind::String),
        Expr::Literal(LiteralValue::Null | LiteralValue::Bytes(_)) => {
            Err("null and bytes literals aren't allowed in rule expressions".into())
        }
        Expr::Call(call) => {
            let name = crate::expr::author_name(call.func_name.as_str());
            Ok(match name {
                "num" | "brightness" | "hour" | "minute" | "now_ts" | "_+_" | "_-_" | "_*_"
                | "_/_" | "-_" => ExprKind::Number,
                "min" | "max" | "round" | "clamp" => {
                    for arg in &call.args {
                        if !matches!(infer_type(arg, sets)?, ExprKind::Number | ExprKind::Scalar) {
                            return Err(format!("{name}() takes numbers"));
                        }
                    }
                    ExprKind::Number
                }
                "on" | "available" | "unknown" | "_&&_" | "_||_" | "!_" | "_==_" | "_!=_"
                | "_>=_" | "_<=_" | "_>_" | "_<_" => ExprKind::Bool,
                "text" => ExprKind::String,
                "attr" => ExprKind::Scalar,
                "var" => {
                    let name = call
                        .args
                        .first()
                        .and_then(string_literal)
                        .ok_or("var() needs a string literal name")?;
                    *sets
                        .get(name)
                        .ok_or_else(|| format!("var({name:?}): no set for this name in the rule"))?
                }
                "_?_:_" => {
                    let left =
                        infer_type(call.args.get(1).ok_or("ternary needs two branches")?, sets)?;
                    let right =
                        infer_type(call.args.get(2).ok_or("ternary needs two branches")?, sets)?;
                    if left == right {
                        left
                    } else if left == ExprKind::Scalar {
                        right
                    } else if right == ExprKind::Scalar {
                        left
                    } else {
                        return Err("ternary branches must be the same type".into());
                    }
                }
                _ => {
                    // Operators already handled; unknown names are rejected in walk_ast.
                    infer_type(call.args.first().unwrap_or(expr), sets)?
                }
            })
        }
        Expr::Unspecified => Err("empty expression".into()),
        _ => Err("this expression shape isn't allowed".into()),
    }
}

fn check_state_match(
    path: &str,
    id: &EntityId,
    from: Option<&TypedValue>,
    to: Option<&TypedValue>,
    to_field: Option<&str>,
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) {
    let Some(entity) = registry.entity(id) else {
        problems.push(problem(
            path,
            format!("no such entity {id} — check the entity id"),
        ));
        return;
    };
    if let Some(value) = from
        && let Err(reason) = typed_value_fits(value, entity)
    {
        problems.push(problem(format!("{path}/from"), reason));
    }
    if let Some(value) = to
        && let Err(reason) = typed_value_fits(value, entity)
    {
        let field = to_field.unwrap_or("to");
        problems.push(problem(format!("{path}/{field}"), reason));
    }
}

fn typed_value_fits(value: &TypedValue, entity: &Entity) -> Result<(), String> {
    let (shape, what) = match value {
        TypedValue::Null => return Ok(()),
        TypedValue::Bool(_) => (ValueShape::Bool, "a boolean"),
        TypedValue::Number(_) => (ValueShape::Number, "a number"),
        TypedValue::Text(_) => (ValueShape::Text, "a string"),
    };
    let Some(has) = entity.capabilities.primary_shape() else {
        return Err(format!(
            "{} is a {}, which has no value to compare",
            entity.id,
            entity.capabilities.kind()
        ));
    };
    if has == shape {
        match value {
            TypedValue::Text(text) => text_can_be(entity, text),
            _ => Ok(()),
        }
    } else {
        Err(format!(
            "`is` is {what}, but {} is a {}",
            entity.id,
            entity.capabilities.kind()
        ))
    }
}

/// Whether `entity` can ever report `text`: always, unless it lists its options.
fn text_can_be(entity: &Entity, text: &str) -> Result<(), String> {
    match entity.capabilities.text_options() {
        Some(options) if !options.iter().any(|o| o == text) => Err(format!(
            "{} is never {text:?}; it is one of: {}",
            entity.id,
            options.join(", ")
        )),
        _ => Ok(()),
    }
}

/// `text('id') == 'x'` (either way round, or `!=`): `x` must be something the entity can say.
/// The entity id in `text('id')`, if `expr` is that call.
fn text_of(expr: &IdedExpr) -> Option<&str> {
    match &expr.expr {
        Expr::Call(inner) if crate::expr::author_name(inner.func_name.as_str()) == "text" => {
            inner.args.first().and_then(string_literal)
        }
        _ => None,
    }
}

fn check_text_comparison(call: &CallExpr, registry: &impl RegistryView) -> Result<(), String> {
    let [left, right] = call.args.as_slice() else {
        return Ok(());
    };
    let (id, literal) = match (
        text_of(left),
        string_literal(right),
        text_of(right),
        string_literal(left),
    ) {
        (Some(id), Some(literal), _, _) | (_, _, Some(id), Some(literal)) => (id, literal),
        _ => return Ok(()),
    };
    let Ok(entity_id) = id.parse::<EntityId>() else {
        return Ok(());
    };
    match registry.entity(&entity_id) {
        Some(entity) => text_can_be(entity, literal),
        None => Ok(()),
    }
}

fn check_call_inner(
    path: &str,
    service: RuleService,
    target: &EntityId,
    data: Option<&CallData>,
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) {
    let Some(entity) = registry.entity(target) else {
        problems.push(problem(
            path,
            format!("no such entity {target} — check the entity id"),
        ));
        return;
    };
    if entity.capabilities.kind() != service.kind() {
        problems.push(problem(
            path,
            format!("{service} acts on a {}, not {}", service.kind(), entity.id),
        ));
        return;
    }
    if let Some(CallData::Light(light)) = data {
        check_light_data(path, light, entity, problems);
    }
}

fn check_light_data(
    path: &str,
    light: &LightCallData,
    entity: &Entity,
    problems: &mut Vec<Problem>,
) {
    let Capabilities::Light(caps) = &entity.capabilities else {
        return;
    };
    if (light.brightness.is_some() || light.brightness_pct.is_some()) && !caps.brightness {
        problems.push(problem(path, format!("{} is not dimmable", entity.id)));
    }
    if light.color_temp_kelvin.is_some() && caps.color_temp_kelvin.is_none() {
        problems.push(problem(
            path,
            format!("{} does not support color temperature", entity.id),
        ));
    }
    if light.rgb.is_some() && !caps.rgb {
        problems.push(problem(path, format!("{} does not support RGB", entity.id)));
    }
}

fn problem(path: impl Into<String>, reason: impl Into<String>) -> Problem {
    Problem {
        path: path.into(),
        reason: reason.into(),
    }
}

impl crate::expr::Compiled {
    pub(crate) fn program(&self) -> &Program {
        self.program_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use irori_types::{
        BinarySensorCapabilities, LightCapabilities, Name, SensorCapabilities, SensorValueType,
        SwitchCapabilities, UniqueId,
    };

    fn entity(id: &str, capabilities: Capabilities) -> Entity {
        let id: EntityId = id.parse().unwrap();
        Entity {
            id: id.clone(),
            protocol: "demo".parse().unwrap(),
            unique_id: UniqueId::try_from(id.as_str().replace('.', "-")).unwrap(),
            name: Name::try_from("x").unwrap(),
            device_id: None,
            area_id: None,
            capabilities,
            entity_category: None,
        }
    }

    fn hallway_registry() -> MapRegistry {
        let mut entities = BTreeMap::new();
        for e in [
            entity(
                "binary_sensor.demo_movement_motion",
                Capabilities::BinarySensor(BinarySensorCapabilities { device_class: None }),
            ),
            entity(
                "sensor.demo_luminosity_illuminance",
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: Some("lx".into()),
                    state_class: None,
                    options: Vec::new(),
                }),
            ),
            entity(
                "binary_sensor.demo_mmwave_occupancy",
                Capabilities::BinarySensor(BinarySensorCapabilities { device_class: None }),
            ),
            entity(
                "light.demo_hall_light",
                Capabilities::Light(LightCapabilities {
                    brightness: true,
                    color_temp_kelvin: None,
                    rgb: false,
                }),
            ),
            entity(
                "switch.guests_over",
                Capabilities::Switch(SwitchCapabilities { device_class: None }),
            ),
        ] {
            entities.insert(e.id.clone(), e);
        }
        MapRegistry {
            entities,
            timezone: true,
            location: false,
        }
    }

    fn hallway_rule() -> Rule {
        serde_json::from_str(include_str!(
            "../../../fixtures/types/rule/valid/hallway_motion_light.json"
        ))
        .unwrap()
    }

    #[test]
    fn hallway_type_checks() {
        let problems = validate(&hallway_rule(), &hallway_registry());
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_text_sensor_is_pointed_at_text() {
        let washer = entity(
            "sensor.washer_program",
            Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Text,
                device_class: None,
                unit: None,
                state_class: None,
                options: Vec::new(),
            }),
        );
        let mut registry = hallway_registry();
        registry.entities.insert(washer.id.clone(), washer.clone());
        let check = |name| check_fn_against_registry(name, &washer.id, &registry);
        assert_eq!(
            check("num"),
            Err(r#"num("sensor.washer_program"): entity is sensor, not a numeric sensor — use text("sensor.washer_program")"#.to_owned())
        );
        assert_eq!(
            check("on"),
            Err(r#"on("sensor.washer_program"): entity is text, not on/off — use text("sensor.washer_program")"#.to_owned())
        );
        assert_eq!(check("text"), Ok(()));
    }

    #[test]
    fn a_button_has_no_value_to_read_or_compare() {
        let restart = entity(
            "button.board_restart",
            Capabilities::Button(irori_types::ButtonCapabilities { device_class: None }),
        );
        let mut registry = hallway_registry();
        registry
            .entities
            .insert(restart.id.clone(), restart.clone());
        assert_eq!(
            check_fn_against_registry("on", &restart.id, &registry),
            Err(
                r#"on("button.board_restart"): entity is a button, which has no value to read"#
                    .to_owned()
            )
        );
        assert_eq!(
            check_fn_against_registry("available", &restart.id, &registry),
            Ok(())
        );
        assert_eq!(
            typed_value_fits(&TypedValue::Bool(true), &restart),
            Err("button.board_restart is a button, which has no value to compare".to_owned())
        );
    }

    #[test]
    fn text_a_sensor_can_never_report_is_rejected() {
        let washer = entity(
            "sensor.washer_program",
            Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Text,
                device_class: None,
                unit: None,
                state_class: None,
                options: vec!["wash".into(), "rinse".into()],
            }),
        );
        let mut registry = hallway_registry();
        registry.entities.insert(washer.id.clone(), washer.clone());
        let problems_with = |expr: &str| {
            let mut rule = hallway_rule();
            if let Condition::Expr { expr: e } = &mut rule.conditions[0] {
                *e = serde_json::from_value(serde_json::json!(expr)).unwrap();
            }
            validate(&rule, &registry)
        };
        assert!(problems_with("text('sensor.washer_program') == 'rinse'").is_empty());
        for wrong in [
            "text('sensor.washer_program') == 'spin'",
            "'spin' != text('sensor.washer_program')",
        ] {
            let problems = problems_with(wrong);
            assert!(
                problems.iter().any(|p| p
                    .reason
                    .contains(r#"is never "spin"; it is one of: wash, rinse"#)),
                "{wrong}: {problems:?}"
            );
        }
        assert!(typed_value_fits(&TypedValue::Text("spin".into()), &washer).is_err());
        assert!(typed_value_fits(&TypedValue::Text("wash".into()), &washer).is_ok());
    }

    #[test]
    fn num_on_a_pir_is_rejected() {
        let mut rule = hallway_rule();
        if let Condition::Expr { expr } = &mut rule.conditions[0] {
            *expr = serde_json::from_str(r#""num('binary_sensor.demo_movement_motion') < 30""#)
                .unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems.iter().any(|p| p.reason.contains("use on(")),
            "{problems:?}"
        );
    }

    #[test]
    fn banned_macros_are_rejected() {
        let mut rule = hallway_rule();
        if let Condition::Expr { expr } = &mut rule.conditions[0] {
            *expr = serde_json::from_str(r#""[1,2].exists(x, x > 0)""#).unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(!problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn num_of_a_var_is_rejected() {
        let mut rule = hallway_rule();
        if let Condition::Expr { expr } = &mut rule.conditions[0] {
            *expr = serde_json::from_str(r#""num(var('id')) < 30""#).unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems
                .iter()
                .any(|p| p.reason.contains("string literal")
                    || p.reason.contains("reserved identifier")),
            "{problems:?}"
        );
    }

    #[test]
    fn condition_must_be_boolean() {
        let mut rule = hallway_rule();
        if let Condition::Expr { expr } = &mut rule.conditions[0] {
            *expr = serde_json::from_str(r#""num('sensor.demo_luminosity_illuminance')""#).unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems.iter().any(|p| p.reason.contains("boolean")),
            "{problems:?}"
        );
    }

    #[test]
    fn extra_arguments_are_rejected() {
        let mut rule = hallway_rule();
        if let Condition::Expr { expr } = &mut rule.conditions[0] {
            *expr = serde_json::from_str(r#""on('switch.guests_over', 'extra')""#).unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems.iter().any(|p| p.reason.contains("argument")),
            "{problems:?}"
        );
    }

    #[test]
    fn missing_trigger_entity_is_reported() {
        let mut rule = hallway_rule();
        if let crate::Trigger::State { entity, .. } = &mut rule.triggers[0] {
            *entity = "binary_sensor.does_not_exist".parse().unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems
                .iter()
                .any(|p| p.path.starts_with("triggers/") && p.reason.contains("does_not_exist")),
            "{problems:?}"
        );
    }

    #[test]
    fn string_is_on_a_sensor_is_rejected() {
        let mut rule = hallway_rule();
        rule.conditions = vec![
            serde_json::from_value(serde_json::json!({
                "type": "state",
                "entity": "sensor.demo_luminosity_illuminance",
                "is": "on"
            }))
            .unwrap(),
        ];
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems
                .iter()
                .any(|p| p.path.contains("is") && p.reason.contains("string")),
            "{problems:?}"
        );
    }

    #[test]
    fn light_service_on_a_switch_is_rejected() {
        let mut rule = hallway_rule();
        if let Action::Call { target, .. } = &mut rule.actions[0] {
            target.entity = "switch.guests_over".parse().unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems.iter().any(|p| p.reason.contains("light.turn_on")),
            "{problems:?}"
        );
    }

    #[test]
    fn undeclared_var_is_rejected() {
        let mut rule = hallway_rule();
        if let Condition::Expr { expr } = &mut rule.conditions[0] {
            *expr = serde_json::from_str(r#""var('nope')""#).unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems
                .iter()
                .any(|p| p.reason.contains("no set") || p.reason.contains("reserved identifier")),
            "{problems:?}"
        );
    }

    #[test]
    fn sun_needs_timezone_and_location() {
        let mut rule = hallway_rule();
        rule.triggers = vec![
            serde_json::from_value(serde_json::json!({
                "type": "sun",
                "event": "sunrise"
            }))
            .unwrap(),
        ];
        let mut registry = hallway_registry();
        registry.timezone = false;
        registry.location = true;
        let problems = validate(&rule, &registry);
        assert!(
            problems.iter().any(|p| p.reason.contains("timezone")),
            "{problems:?}"
        );
    }

    #[test]
    fn wait_with_hour_is_rejected() {
        let mut rule = hallway_rule();
        if let Action::Wait {
            until: WaitUntil::Expr { expr, .. },
            ..
        } = &mut rule.actions[1]
        {
            *expr = serde_json::from_str(r#""hour() == 22""#).unwrap();
        }
        let problems = validate(&rule, &hallway_registry());
        assert!(
            problems.iter().any(|p| p.reason.contains("hour()")),
            "{problems:?}"
        );
    }
}
