//! Parse, AST allow-list, and registry type-check (layer 3). No scheduler.

use std::collections::{BTreeMap, BTreeSet};

use cel::Program;
use cel::common::ast::{Expr, IdedExpr, LiteralValue};
use irori_types::{
    Action, Capabilities, Condition, Entity, EntityId, ExprString, Rule, SensorValueType, WaitUntil,
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

/// Type-check a rule against the registry. Does not arm it.
pub fn validate(rule: &Rule, registry: &impl RegistryView) -> Vec<Problem> {
    let mut problems = Vec::new();
    walk_conditions("conditions", &rule.conditions, registry, &mut problems);
    walk_actions("actions", &rule.actions, registry, &mut problems, false);
    problems
}

fn walk_conditions(
    path: &str,
    conditions: &[Condition],
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) {
    for (i, condition) in conditions.iter().enumerate() {
        walk_condition(&format!("{path}/{i}"), condition, registry, problems);
    }
}

fn walk_condition(
    path: &str,
    condition: &Condition,
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) {
    match condition {
        Condition::Expr { expr } => {
            check_expr(path, expr, ExprRole::Bool, registry, problems);
        }
        Condition::State { entity, .. } => check_entity_exists(path, entity, registry, problems),
        Condition::Time { .. } => {
            if !registry.has_timezone() {
                problems.push(problem(
                    path,
                    "time windows need a timezone; it isn't in irori.toml yet",
                ));
            }
        }
        Condition::Sun { .. } => {
            if !registry.has_location() {
                problems.push(problem(
                    path,
                    "sun windows need a location (lat/lon); it isn't in irori.toml yet",
                ));
            }
        }
        Condition::All { conditions } | Condition::Any { conditions } => {
            walk_conditions(&format!("{path}/conditions"), conditions, registry, problems);
        }
        Condition::Not { condition } => {
            walk_condition(&format!("{path}/condition"), condition, registry, problems);
        }
    }
}

fn walk_actions(
    path: &str,
    actions: &[Action],
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
    _in_wait: bool,
) {
    for (i, action) in actions.iter().enumerate() {
        let here = format!("{path}/{i}");
        match action {
            Action::Call { target, .. } => {
                check_entity_exists(&here, &target.entity, registry, problems);
            }
            Action::Wait { until, .. } => match until {
                WaitUntil::State { entity, .. } => {
                    check_entity_exists(&format!("{here}/until"), entity, registry, problems);
                }
                WaitUntil::Expr { expr, .. } => {
                    check_expr(
                        &format!("{here}/until"),
                        expr,
                        ExprRole::Wait,
                        registry,
                        problems,
                    );
                }
            },
            Action::If {
                conditions,
                then,
                r#else,
            } => {
                walk_conditions(&format!("{here}/conditions"), conditions, registry, problems);
                walk_actions(&format!("{here}/then"), then, registry, problems, false);
                walk_actions(&format!("{here}/else"), r#else, registry, problems, false);
            }
            Action::Choose {
                options,
                otherwise,
            } => {
                for (j, option) in options.iter().enumerate() {
                    let opt = format!("{here}/options/{j}");
                    walk_conditions(
                        &format!("{opt}/conditions"),
                        &option.conditions,
                        registry,
                        problems,
                    );
                    walk_actions(&format!("{opt}/then"), &option.then, registry, problems, false);
                }
                walk_actions(&format!("{here}/default"), otherwise, registry, problems, false);
            }
            Action::Set { expr, .. } => {
                check_expr(&here, expr, ExprRole::Value, registry, problems);
            }
            Action::Delay { .. } | Action::Event { .. } | Action::Stop { .. } => {}
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
    problems: &mut Vec<Problem>,
) {
    let compiled = match compile(expr.as_str()) {
        Ok(compiled) => compiled,
        Err(error) => {
            problems.push(problem(path, error.to_string()));
            return;
        }
    };
    let ast = compiled.program().expression();
    let mut ids = BTreeSet::new();
    let mut uses_clock = false;
    if let Err(reason) = walk_ast(ast, registry, &mut ids, &mut uses_clock) {
        problems.push(problem(path, reason));
        return;
    }
    match role {
        ExprRole::Wait => {
            if uses_clock {
                problems.push(problem(
                    path,
                    "hour() cannot drive a wait; use a time trigger",
                ));
            }
            if ids.is_empty() {
                problems.push(problem(
                    path,
                    "a wait expression has to read at least one entity (num/on/…); use a delay or a time trigger otherwise",
                ));
            }
        }
        ExprRole::Bool | ExprRole::Value => {
            if uses_clock && !registry.has_timezone() {
                problems.push(problem(
                    path,
                    "time triggers need a timezone; it isn't in irori.toml yet",
                ));
            }
        }
    }
}

fn walk_ast(
    expr: &IdedExpr,
    registry: &impl RegistryView,
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
            let name = call.func_name.as_str();
            if name == "has" || name == "duration" || name == "timestamp" {
                return Err(format!("{name}() isn't allowed in rule expressions"));
            }
            if name == "map" || name == "filter" || name == "exists" || name == "exists_one" || name == "all"
            {
                return Err("macros like map/filter/exists aren't allowed".into());
            }
            if CLOCK_FNS.contains(&name) {
                *uses_clock = true;
            }
            if ENTITY_FNS.contains(&name) {
                let Some(first) = call.args.first() else {
                    return Err(format!("{name}() needs a string literal entity id"));
                };
                let id = string_literal(first).ok_or_else(|| {
                    format!("{name}() needs a string literal entity id, not an expression")
                })?;
                if name == "var" {
                    id.parse::<irori_types::ObjectId>()
                        .map_err(|e| e.to_string())?;
                } else if name != "attr" {
                    let entity_id: EntityId = id.parse().map_err(|e: irori_types::IdError| e.to_string())?;
                    ids.insert(entity_id.clone());
                    check_fn_against_registry(name, &entity_id, registry)?;
                } else {
                    let entity_id: EntityId = id.parse().map_err(|e: irori_types::IdError| e.to_string())?;
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
                if string_literal(first).is_none() {
                    return Err("var() needs a string literal name, not an expression".into());
                }
            } else if !OPS.contains(&name) && !CLOCK_FNS.contains(&name) {
                return Err(format!("unknown function {name}()"));
            }
            for arg in &call.args {
                walk_ast(arg, registry, ids, uses_clock)?;
            }
            if let Some(target) = &call.target {
                walk_ast(target, registry, ids, uses_clock)?;
            }
            Ok(())
        }
    }
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
    match (name, &entity.capabilities) {
        ("num", Capabilities::Sensor(s)) if s.value_type == SensorValueType::Number => Ok(()),
        ("num", _) => Err(format!(
            "num(\"{id}\"): entity is {}, not a numeric sensor — use on(\"{id}\")",
            entity.capabilities.kind()
        )),
        ("text", Capabilities::Sensor(s)) if s.value_type == SensorValueType::Text => Ok(()),
        ("text", _) => Err(format!("text(\"{id}\"): entity is not a text sensor")),
        ("on", Capabilities::Light(_) | Capabilities::Switch(_) | Capabilities::BinarySensor(_)) => {
            Ok(())
        }
        ("on", _) => Err(format!(
            "on(\"{id}\"): entity is a number, not on/off — use num(\"{id}\")"
        )),
        ("brightness", Capabilities::Light(light)) if light.brightness => Ok(()),
        ("brightness", Capabilities::Light(_)) => Err(format!(
            "brightness(\"{id}\"): this light is not dimmable"
        )),
        ("brightness", _) => Err(format!("brightness(\"{id}\"): entity is not a light")),
        ("available" | "unknown" | "attr", _) => Ok(()),
        _ => Ok(()),
    }
}

fn check_entity_exists(
    path: &str,
    id: &EntityId,
    registry: &impl RegistryView,
    problems: &mut Vec<Problem>,
) {
    if registry.entity(id).is_none() {
        problems.push(problem(
            path,
            format!("no such entity {id:?} — check the entity id"),
        ));
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
        BinarySensorCapabilities, LightCapabilities, Name, SensorCapabilities, SwitchCapabilities,
        UniqueId,
    };

    fn entity(id: &str, capabilities: Capabilities) -> Entity {
        let id: EntityId = id.parse().unwrap();
        Entity {
            id: id.clone(),
            integration: "demo".parse().unwrap(),
            unique_id: UniqueId::try_from(id.as_str().replace('.', "-")).unwrap(),
            name: Name::try_from("x").unwrap(),
            device_id: None,
            area_id: None,
            capabilities,
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
            problems.iter().any(|p| p.reason.contains("string literal")
                || p.reason.contains("reserved identifier")),
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
