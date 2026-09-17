//! Sequential-engine rule documents. See `docs/specs/rules.md`.
//!
//! This is the first-party automation engine's JSON, not a core OS type. Layer 2 is shape and
//! field invariants; parse, AST allow-list, and registry type-check live in this crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use irori_types::{AttributeKey, Description, EntityId, InvariantError, Name, ObjectId, RuleId};

const MAX_TRIGGERS: usize = 16;
const MAX_CONDITIONS: usize = 32;
const MAX_ACTIONS: usize = 64;
const MAX_DEPTH: u8 = 8;
const MAX_EXPR_CHARS: usize = 512;
const MAX_EVENT_DATA_KEYS: usize = 16;
const MAX_QUEUED: u8 = 32;
const MAX_STOP_REASON: usize = 200;
const MIN_DURATION_MS: i64 = 1;
const MAX_DURATION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// One rule, as stored in `rules/<id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: RuleId,
    pub name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Description>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Mode::is_single")]
    pub mode: Mode,
    #[schemars(length(min = 1, max = 16))]
    pub triggers: Vec<Trigger>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 32))]
    pub conditions: Vec<Condition>,
    #[schemars(length(min = 1, max = 64))]
    pub actions: Vec<Action>,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: RuleId,
    name: Name,
    #[serde(default)]
    description: Option<Description>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    mode: Mode,
    triggers: Vec<Trigger>,
    #[serde(default)]
    conditions: Vec<Condition>,
    actions: Vec<Action>,
}

impl<'de> Deserialize<'de> for Rule {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawRule::deserialize(deserializer)?;
        let rule = Rule {
            id: raw.id,
            name: raw.name,
            description: raw.description,
            enabled: raw.enabled,
            mode: raw.mode,
            triggers: raw.triggers,
            conditions: raw.conditions,
            actions: raw.actions,
        };
        rule.validate().map_err(serde::de::Error::custom)?;
        Ok(rule)
    }
}

impl Rule {
    pub fn validate(&self) -> Result<(), InvariantError> {
        self.mode.validate()?;
        if self.triggers.is_empty() {
            return Err(inv("a rule needs at least one trigger"));
        }
        if self.triggers.len() > MAX_TRIGGERS {
            return Err(inv(format!(
                "a rule can have at most {MAX_TRIGGERS} triggers"
            )));
        }
        if self.conditions.len() > MAX_CONDITIONS {
            return Err(inv(format!(
                "a rule can have at most {MAX_CONDITIONS} top-level conditions"
            )));
        }
        if self.actions.is_empty() {
            return Err(inv("a rule needs at least one action"));
        }
        if self.actions.len() > MAX_ACTIONS {
            return Err(inv(format!(
                "an action list can have at most {MAX_ACTIONS} actions"
            )));
        }
        for trigger in &self.triggers {
            trigger.validate()?;
        }
        for condition in &self.conditions {
            condition.validate(1)?;
        }
        for action in &self.actions {
            action.validate(1)?;
        }
        Ok(())
    }
}

fn inv(message: impl Into<String>) -> InvariantError {
    InvariantError::new(message)
}

/// How a second trigger is handled while a run is in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Mode {
    Named(NamedMode),
    Limited(LimitedMode),
}

impl Default for Mode {
    fn default() -> Self {
        Self::Named(NamedMode::Single)
    }
}

impl Mode {
    fn is_single(&self) -> bool {
        matches!(self, Self::Named(NamedMode::Single))
    }

    fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::Named(_) => Ok(()),
            Self::Limited(LimitedMode::Queued { max } | LimitedMode::Parallel { max }) => {
                if *max < 1 || *max > MAX_QUEUED {
                    return Err(inv(format!(
                        "queued/parallel max must be from 1 to {MAX_QUEUED}"
                    )));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NamedMode {
    Single,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LimitedMode {
    Queued { max: u8 },
    Parallel { max: u8 },
}

/// An edge that starts a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    State {
        entity: EntityId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<TypedValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<TypedValue>,
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        hold: Option<CompactDuration>,
    },
    Time {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<CivilTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cron: Option<Cron>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weekday: Option<Vec<Weekday>>,
    },
    Sun {
        event: SunEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<CompactDuration>,
    },
    Event {
        event: EventName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<BTreeMap<AttributeKey, EventDatum>>,
    },
    Startup {},
}

impl Trigger {
    fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::State { hold, .. } => {
                if let Some(hold) = hold {
                    hold.require_positive("for")?;
                }
                Ok(())
            }
            Self::Time { at, cron, weekday } => match (at.is_some(), cron.is_some()) {
                (false, false) => Err(inv("a time trigger needs `at` or `cron`")),
                (true, true) => Err(inv("a time trigger takes `at` or `cron`, not both")),
                (false, true) if weekday.is_some() => {
                    Err(inv("`weekday` is only for `at`; put days in the cron"))
                }
                _ => {
                    if let Some(days) = weekday {
                        check_weekdays(days)?;
                    }
                    Ok(())
                }
            },
            Self::Sun { offset, .. } => {
                if let Some(offset) = offset {
                    offset.require_nonzero("offset")?;
                }
                Ok(())
            }
            Self::Event { data, .. } => check_event_data(data.as_ref()),
            Self::Startup {} => Ok(()),
        }
    }
}

/// A level check. The top-level array is an implicit `all`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    State {
        entity: EntityId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is: Option<TypedValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        availability: Option<AvailabilityWanted>,
    },
    Expr {
        expr: ExprString,
    },
    Time {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<CivilTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<CivilTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weekday: Option<Vec<Weekday>>,
    },
    Sun {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<SunEvent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<SunEvent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<CompactDuration>,
    },
    All {
        conditions: Vec<Condition>,
    },
    Any {
        conditions: Vec<Condition>,
    },
    Not {
        condition: Box<Condition>,
    },
}

impl Condition {
    fn validate(&self, depth: u8) -> Result<(), InvariantError> {
        if depth > MAX_DEPTH {
            return Err(inv(format!(
                "conditions and actions can nest at most {MAX_DEPTH} levels"
            )));
        }
        match self {
            Self::State {
                is, availability, ..
            } => {
                if is.is_none() && availability.is_none() {
                    return Err(inv(
                        "a state condition needs `is` or `availability` (or both)",
                    ));
                }
                Ok(())
            }
            Self::Expr { expr } => expr.validate(),
            Self::Time {
                after,
                before,
                weekday,
            } => {
                if after.is_none() && before.is_none() && weekday.is_none() {
                    return Err(inv("a time window needs `after`, `before`, or `weekday`"));
                }
                if let Some(days) = weekday {
                    check_weekdays(days)?;
                }
                Ok(())
            }
            Self::Sun {
                after,
                before,
                offset,
            } => {
                if after.is_none() && before.is_none() {
                    return Err(inv("a sun window needs `after` or `before`"));
                }
                if let Some(offset) = offset {
                    offset.require_nonzero("offset")?;
                }
                Ok(())
            }
            Self::All { conditions } | Self::Any { conditions } => {
                if conditions.is_empty() {
                    return Err(inv("`all`/`any` needs at least one condition"));
                }
                if conditions.len() > MAX_CONDITIONS {
                    return Err(inv(format!(
                        "a combinator can have at most {MAX_CONDITIONS} conditions"
                    )));
                }
                for child in conditions {
                    child.validate(depth + 1)?;
                }
                Ok(())
            }
            Self::Not { condition } => condition.validate(depth + 1),
        }
    }
}

/// One step of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Call {
        service: RuleService,
        target: Target,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<CallData>,
        #[serde(default, skip_serializing_if = "OnError::is_stop")]
        on_error: OnError,
    },
    Delay {
        #[serde(rename = "for")]
        hold: CompactDuration,
    },
    Wait {
        until: WaitUntil,
        timeout: CompactDuration,
        #[serde(default, skip_serializing_if = "OnTimeout::is_continue")]
        on_timeout: OnTimeout,
    },
    If {
        conditions: Vec<Condition>,
        then: Vec<Action>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        r#else: Vec<Action>,
    },
    Choose {
        options: Vec<ChooseOption>,
        /// JSON name is `default`; the Rust field isn't, so JSON Schema doesn't treat it as
        /// the `default` keyword.
        #[serde(default, rename = "default", skip_serializing_if = "Vec::is_empty")]
        otherwise: Vec<Action>,
    },
    Set {
        name: ObjectId,
        expr: ExprString,
        #[serde(default, skip_serializing_if = "OnError::is_stop")]
        on_error: OnError,
    },
    Event {
        event: EventName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<BTreeMap<AttributeKey, EventDatum>>,
    },
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StopReason>,
    },
}

impl Action {
    fn validate(&self, depth: u8) -> Result<(), InvariantError> {
        if depth > MAX_DEPTH {
            return Err(inv(format!(
                "conditions and actions can nest at most {MAX_DEPTH} levels"
            )));
        }
        match self {
            Self::Call { service, data, .. } => service.validate_data(data.as_ref()),
            Self::Delay { hold } => hold.require_positive("for"),
            Self::Wait { until, timeout, .. } => {
                until.validate()?;
                timeout.require_positive("timeout")
            }
            Self::If {
                conditions,
                then,
                r#else,
            } => {
                if conditions.len() > MAX_CONDITIONS {
                    return Err(inv(format!(
                        "an if can have at most {MAX_CONDITIONS} conditions"
                    )));
                }
                for condition in conditions {
                    condition.validate(depth + 1)?;
                }
                check_action_list(then, depth + 1)?;
                check_action_list(r#else, depth + 1)
            }
            Self::Choose { options, otherwise } => {
                if options.is_empty() {
                    return Err(inv("`choose` needs at least one option"));
                }
                for option in options {
                    if option.conditions.len() > MAX_CONDITIONS {
                        return Err(inv(format!(
                            "a choose option can have at most {MAX_CONDITIONS} conditions"
                        )));
                    }
                    for condition in &option.conditions {
                        condition.validate(depth + 1)?;
                    }
                    check_action_list(&option.then, depth + 1)?;
                }
                check_action_list(otherwise, depth + 1)
            }
            Self::Set { expr, .. } => expr.validate(),
            Self::Event { data, .. } => check_event_data(data.as_ref()),
            Self::Stop { reason } => {
                if let Some(reason) = reason {
                    reason.validate()?;
                }
                Ok(())
            }
        }
    }
}

fn check_action_list(actions: &[Action], depth: u8) -> Result<(), InvariantError> {
    if actions.len() > MAX_ACTIONS {
        return Err(inv(format!(
            "an action list can have at most {MAX_ACTIONS} actions"
        )));
    }
    for action in actions {
        action.validate(depth)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChooseOption {
    pub conditions: Vec<Condition>,
    pub then: Vec<Action>,
}

/// Level matcher for a wait: `state` or `expr`, never a trigger `to`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WaitUntil {
    State {
        entity: EntityId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is: Option<TypedValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        availability: Option<AvailabilityWanted>,
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        hold: Option<CompactDuration>,
    },
    Expr {
        expr: ExprString,
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        hold: Option<CompactDuration>,
    },
}

impl WaitUntil {
    fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::State {
                is,
                availability,
                hold,
                ..
            } => {
                if is.is_none() && availability.is_none() {
                    return Err(inv(
                        "a wait state matcher needs `is` or `availability` (or both)",
                    ));
                }
                if let Some(hold) = hold {
                    hold.require_positive("for")?;
                }
                Ok(())
            }
            Self::Expr { expr, hold } => {
                expr.validate()?;
                if let Some(hold) = hold {
                    hold.require_positive("for")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub entity: EntityId,
}

/// Services a rule may name. Includes `toggle`, which the core resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RuleService {
    #[serde(rename = "light.turn_on")]
    LightTurnOn,
    #[serde(rename = "light.turn_off")]
    LightTurnOff,
    #[serde(rename = "light.toggle")]
    LightToggle,
    #[serde(rename = "switch.turn_on")]
    SwitchTurnOn,
    #[serde(rename = "switch.turn_off")]
    SwitchTurnOff,
    #[serde(rename = "switch.toggle")]
    SwitchToggle,
}

impl RuleService {
    /// The kind of entity this service acts on.
    pub fn kind(self) -> irori_types::EntityKind {
        match self {
            Self::LightTurnOn | Self::LightTurnOff | Self::LightToggle => {
                irori_types::EntityKind::Light
            }
            Self::SwitchTurnOn | Self::SwitchTurnOff | Self::SwitchToggle => {
                irori_types::EntityKind::Switch
            }
        }
    }

    fn validate_data(self, data: Option<&CallData>) -> Result<(), InvariantError> {
        match (self, data) {
            (Self::LightTurnOn | Self::LightToggle, Some(CallData::Light(light))) => {
                light.validate()
            }
            (Self::LightTurnOn | Self::LightToggle, None) => Ok(()),
            (
                Self::LightTurnOff | Self::SwitchTurnOn | Self::SwitchTurnOff | Self::SwitchToggle,
                Some(_),
            ) => Err(inv(format!("{self} does not take data"))),
            (
                Self::LightTurnOff | Self::SwitchTurnOn | Self::SwitchTurnOff | Self::SwitchToggle,
                None,
            ) => Ok(()),
        }
    }
}

impl fmt::Display for RuleService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LightTurnOn => "light.turn_on",
            Self::LightTurnOff => "light.turn_off",
            Self::LightToggle => "light.toggle",
            Self::SwitchTurnOn => "switch.turn_on",
            Self::SwitchTurnOff => "switch.turn_off",
            Self::SwitchToggle => "switch.toggle",
        })
    }
}

/// Closed `data` object for a call. Unknown keys are rejected by the inner structs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum CallData {
    Light(LightCallData),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightCallData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 255))]
    pub brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100))]
    pub brightness_pct: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1000, max = 20000))]
    pub color_temp_kelvin: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
}

impl LightCallData {
    fn validate(&self) -> Result<(), InvariantError> {
        if self.brightness.is_some() && self.brightness_pct.is_some() {
            return Err(inv("not both `brightness` and `brightness_pct`; pick one"));
        }
        if let Some(brightness) = self.brightness
            && !(1..=255).contains(&brightness)
        {
            return Err(inv(
                "brightness must be from 1 to 255; off is light.turn_off",
            ));
        }
        if let Some(pct) = self.brightness_pct
            && !(1..=100).contains(&pct)
        {
            return Err(inv("brightness_pct must be from 1 to 100"));
        }
        if let Some(kelvin) = self.color_temp_kelvin
            && !(1000..=20000).contains(&kelvin)
        {
            return Err(inv("color_temp_kelvin must be from 1000 to 20000"));
        }
        if self.color_temp_kelvin.is_some() && self.rgb.is_some() {
            return Err(inv(
                "not both `color_temp_kelvin` and `rgb`; pick one color setting",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    #[default]
    Stop,
    Continue,
}

impl OnError {
    fn is_stop(&self) -> bool {
        matches!(self, Self::Stop)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnTimeout {
    #[default]
    Continue,
    Stop,
}

impl OnTimeout {
    fn is_continue(&self) -> bool {
        matches!(self, Self::Continue)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityWanted {
    Available,
    Unavailable,
}

/// Bool, finite number, string, or JSON `null` (unknown).
#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Bool(bool),
    Number(f64),
    Text(String),
    Null,
}

impl JsonSchema for TypedValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TypedValue".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A typed state value: bool, finite number, string, or null (unknown).",
            "anyOf": [
                { "type": "boolean" },
                { "type": "number" },
                { "type": "string" },
                { "type": "null" },
            ],
        })
    }
}

impl Serialize for TypedValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Number(value) => serializer.serialize_f64(*value),
            Self::Text(value) => serializer.serialize_str(value),
            Self::Null => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for TypedValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Bool(value) => Ok(Self::Bool(value)),
            serde_json::Value::Number(number) => {
                let value = number
                    .as_f64()
                    .ok_or_else(|| serde::de::Error::custom("typed values need a finite number"))?;
                if !value.is_finite() {
                    return Err(serde::de::Error::custom(
                        "typed values need a finite number",
                    ));
                }
                Ok(Self::Number(value))
            }
            serde_json::Value::String(value) => Ok(Self::Text(value)),
            serde_json::Value::Null => Ok(Self::Null),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => Err(
                serde::de::Error::custom("typed values are bool, number, string, or null"),
            ),
        }
    }
}

/// Compact duration: `2m`, `1h30m`, `-30m`. Elapsed time, not civil.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompactDuration(String);

impl JsonSchema for CompactDuration {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CompactDuration".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Compact elapsed duration, e.g. 2m, 10m, 1h30m, 500ms, -30m.",
            "pattern": r"^[+-]?([0-9]+d)?([0-9]+h)?([0-9]+m)?([0-9]+s)?([0-9]+ms)?$",
            "minLength": 2,
        })
    }
}

impl CompactDuration {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn millis(&self) -> i64 {
        parse_millis(&self.0).unwrap_or(0)
    }

    fn require_positive(&self, field: &str) -> Result<(), InvariantError> {
        let ms = parse_millis(&self.0)?;
        if ms <= 0 {
            return Err(inv(format!(
                "{field} must be a positive duration (got {:?})",
                self.0
            )));
        }
        Ok(())
    }

    fn require_nonzero(&self, field: &str) -> Result<(), InvariantError> {
        let ms = parse_millis(&self.0)?;
        if ms == 0 {
            return Err(inv(format!("{field} must not be zero (got {:?})", self.0)));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CompactDuration {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_millis(&text).map_err(serde::de::Error::custom)?;
        Ok(Self(text))
    }
}

fn parse_millis(text: &str) -> Result<i64, InvariantError> {
    let (sign, rest) = if let Some(rest) = text.strip_prefix('-') {
        (-1_i64, rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        (1, rest)
    } else {
        (1, text)
    };
    if rest.is_empty() {
        return Err(inv(format!(
            "invalid duration {text:?}: needs at least one unit (e.g. 2m, 10m, 500ms)"
        )));
    }
    let mut remaining = rest;
    let mut total: i64 = 0;
    let mut seen = [false; 5];
    // Order: d, h, m, s, ms. Each at most once, in that order.
    let units: [(&str, i64, usize); 5] = [
        ("d", 86_400_000, 0),
        ("h", 3_600_000, 1),
        ("m", 60_000, 2),
        ("ms", 1, 4),
        ("s", 1_000, 3),
    ];
    // Prefer `ms` over `m`+`s` by checking `ms` before `s` after `m` is handled via two-pass:
    // scan left to right, try longest unit that matches next.
    let mut last_index = -1_i32;
    while !remaining.is_empty() {
        let digits = remaining.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(inv(format!(
                "invalid duration {text:?}: compact form, no spaces, units d/h/m/s/ms in that order"
            )));
        }
        let (number, after) = remaining.split_at(digits);
        let value: i64 = number
            .parse()
            .map_err(|_| inv(format!("invalid duration {text:?}: number too large")))?;
        let (unit_ms, index, consumed) = if after.starts_with("ms") {
            (1_i64, 4, 2)
        } else if after.starts_with('d') {
            (86_400_000, 0, 1)
        } else if after.starts_with('h') {
            (3_600_000, 1, 1)
        } else if after.starts_with('m') {
            (60_000, 2, 1)
        } else if after.starts_with('s') {
            (1_000, 3, 1)
        } else {
            let _ = units;
            return Err(inv(format!(
                "invalid duration {text:?}: compact form, no spaces, units d/h/m/s/ms in that order"
            )));
        };
        if index as i32 <= last_index || seen[index] {
            return Err(inv(format!(
                "invalid duration {text:?}: units must appear at most once, in order d, h, m, s, ms"
            )));
        }
        seen[index] = true;
        last_index = index as i32;
        total = total.saturating_add(value.saturating_mul(unit_ms));
        remaining = &after[consumed..];
    }
    let signed = sign.saturating_mul(total);
    let abs = signed.unsigned_abs() as i64;
    if !(MIN_DURATION_MS..=MAX_DURATION_MS).contains(&abs) {
        return Err(inv(format!(
            "duration {text:?} must be from 1ms to 7d (got {signed}ms)"
        )));
    }
    Ok(signed)
}

/// `HH:MM` or `HH:MM:SS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CivilTime(String);

impl JsonSchema for CivilTime {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CivilTime".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Civil time of day, HH:MM or HH:MM:SS.",
            "pattern": r"^([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9])?$",
        })
    }
}

impl CivilTime {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CivilTime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_civil(&text).map_err(serde::de::Error::custom)?;
        Ok(Self(text))
    }
}

fn parse_civil(text: &str) -> Result<(), InvariantError> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return Err(inv(format!("invalid time {text:?}: use HH:MM or HH:MM:SS")));
    }
    let parse = |piece: &str, max: u32, what: &str| -> Result<u32, InvariantError> {
        if piece.len() != 2 || !piece.bytes().all(|b| b.is_ascii_digit()) {
            return Err(inv(format!(
                "invalid time {text:?}: {what} must be two digits"
            )));
        }
        let value: u32 = piece.parse().unwrap_or(u32::MAX);
        if value > max {
            return Err(inv(format!("invalid time {text:?}: {what} out of range")));
        }
        Ok(value)
    };
    parse(parts[0], 23, "hour")?;
    parse(parts[1], 59, "minute")?;
    if parts.len() == 3 {
        parse(parts[2], 59, "second")?;
    }
    Ok(())
}

/// 5-field cron: `minute hour day-of-month month day-of-week`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Cron(String);

impl Cron {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Cron {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.split_whitespace().count() != 5 {
            return Err(serde::de::Error::custom(
                "cron needs 5 fields: minute hour day-of-month month day-of-week",
            ));
        }
        Ok(Self(text))
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

fn check_weekdays(days: &[Weekday]) -> Result<(), InvariantError> {
    if days.is_empty() {
        return Err(inv("`weekday` needs at least one day"));
    }
    let mut seen = BTreeSet::new();
    for day in days {
        if !seen.insert(*day) {
            return Err(inv("weekday list has a duplicate day"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SunEvent {
    Sunrise,
    Sunset,
    Dawn,
    Dusk,
    Noon,
    Midnight,
}

/// Slug, or `integration.event` (one dot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EventName(String);

impl EventName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EventName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_event_name(&text).map_err(serde::de::Error::custom)?;
        Ok(Self(text))
    }
}

fn parse_event_name(text: &str) -> Result<(), InvariantError> {
    match text.split_once('.') {
        None => text
            .parse::<ObjectId>()
            .map(|_| ())
            .map_err(|e| inv(e.to_string())),
        Some((left, right)) => {
            if right.contains('.') {
                return Err(inv(format!(
                    "invalid event name {text:?}: at most one dot (`integration.event`)"
                )));
            }
            left.parse::<ObjectId>().map_err(|e| inv(e.to_string()))?;
            right.parse::<ObjectId>().map_err(|e| inv(e.to_string()))?;
            Ok(())
        }
    }
}

/// Scalar JSON for event `data` match: bool, number, or string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EventDatum {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl JsonSchema for EventDatum {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EventDatum".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Event data values are bool, number, or string.",
            "anyOf": [
                { "type": "boolean" },
                { "type": "number" },
                { "type": "string" },
            ],
        })
    }
}

fn check_event_data(
    data: Option<&BTreeMap<AttributeKey, EventDatum>>,
) -> Result<(), InvariantError> {
    let Some(data) = data else {
        return Ok(());
    };
    if data.len() > MAX_EVENT_DATA_KEYS {
        return Err(inv(format!(
            "event data can have at most {MAX_EVENT_DATA_KEYS} keys"
        )));
    }
    Ok(())
}

/// Expression source. Length only; parse and type-check are `irori-rules`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExprString(String);

impl JsonSchema for ExprString {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ExprString".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A CEL expression using the Irori function surface (num, on, available, …).",
            "minLength": 1,
            "maxLength": MAX_EXPR_CHARS,
        })
    }
}

impl ExprString {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), InvariantError> {
        if self.0.is_empty() {
            return Err(inv("an expression must not be empty"));
        }
        if self.0.len() > MAX_EXPR_CHARS {
            return Err(inv(format!(
                "an expression can be at most {MAX_EXPR_CHARS} characters"
            )));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ExprString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let expr = Self(text);
        expr.validate().map_err(serde::de::Error::custom)?;
        Ok(expr)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StopReason(String);

impl JsonSchema for StopReason {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "StopReason".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Optional reason stored on a stop action's trace.",
            "minLength": 1,
            "maxLength": MAX_STOP_REASON,
        })
    }
}

impl StopReason {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), InvariantError> {
        if self.0.is_empty() {
            return Err(inv("stop reason must not be empty"));
        }
        if self.0.len() > MAX_STOP_REASON {
            return Err(inv(format!(
                "stop reason can be at most {MAX_STOP_REASON} characters"
            )));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for StopReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let reason = Self(text);
        reason.validate().map_err(serde::de::Error::custom)?;
        Ok(reason)
    }
}
