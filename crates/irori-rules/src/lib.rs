//! First-party sequential automation engine (library only; not linked into the core).
//!
//! JSON documents, CEL expressions, and save-time type-check. The engine that waits and calls
//! services is later, and it will load as an extension, not as part of `irori serve`.

mod expr;
mod rule;
mod validate;

pub use expr::{Compiled, ExprError, Reading, StateView, compile, eval_bool};
pub use rule::{
    Action, AvailabilityWanted, CallData, ChooseOption, CivilTime, CompactDuration, Condition,
    Cron, EventDatum, EventName, ExprString, LightCallData, LimitedMode, Mode, NamedMode, OnError,
    OnTimeout, Rule, RuleService, StopReason, SunEvent, Target, Trigger, TypedValue, WaitUntil,
    Weekday,
};
pub use validate::{MapRegistry, Problem, RegistryView, validate};

/// JSON Schema for this engine's rule documents (`schemas/rule.schema.json`).
pub fn schemas() -> Vec<irori_types::SchemaDoc> {
    vec![irori_types::SchemaDoc::for_type::<Rule>("rule")]
}
