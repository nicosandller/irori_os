//! Deterministic rules engine: pure, with injected Clock and StateView.
//!
//! The engine itself is M1.4. This crate currently holds the M0.8 CEL spike (ROADMAP D41):
//! compile and evaluate the expressions a rule would write, against a fake state view.

mod expr;
mod validate;

pub use expr::{Compiled, ExprError, Reading, StateView, compile, eval_bool};
pub use validate::{MapRegistry, Problem, RegistryView, validate};
