//! First-party sequential automation engine (library only; not linked into the core).
//!
//! JSON documents, CEL expressions, and save-time type-check. The engine that waits and calls
//! services is later, and it will load as an extension, not as part of `irori serve`.
//!
//! Its building blocks — triggers, conditions, waits, services, durations, the expression
//! surface and the registry type-check — are also what the flow engine (`irori-flows`) is made
//! of. Without the `engine` feature this crate is only the document types, with no CEL, so a
//! wasm page can share them.

#[cfg(feature = "engine")]
pub mod eval;
#[cfg(feature = "engine")]
mod expr;
mod rule;
#[cfg(feature = "engine")]
mod validate;

#[cfg(feature = "engine")]
pub use expr::{Compiled, ExprError, Reading, StateView, compile, eval_bool};
pub use rule::{
    Action, AvailabilityWanted, BRIGHTNESS_PCT, CallData, ChooseOption, CivilTime, CompactDuration, Condition,
    Cron, EventDatum, EventName, ExprString, LimitedMode, Mode, NamedMode, NumberField, OnError,
    OnTimeout, Rule, RuleService, StopReason, SunEvent, Target, Trigger, TypedValue, Values,
    WaitUntil, Weekday,
};
#[cfg(feature = "engine")]
pub use validate::{
    Inspected, MapRegistry, Problem, RegistryView, VarKind, check_call, check_condition,
    check_trigger, check_wait, inspect, validate, vars_read,
};

/// JSON Schema for this engine's rule documents (`schemas/rule.schema.json`).
pub fn schemas() -> Vec<irori_types::SchemaDoc> {
    vec![irori_types::SchemaDoc::for_type::<Rule>("rule")]
}
