//! The flow engine (`docs/specs/flows.md`): checks flows against the home, and runs them.
//!
//! - [`validate::check`]: layer 3, the graph and the home.
//! - [`Engine`]: runs armed flows. Sans-IO and deterministic: it's told what happened and when,
//!   and answers with calls to make and records to keep.
//! - [`sim`]: dry runs and backtests, the same engine on a virtual clock.

pub mod engine;
pub mod sim;
pub mod validate;

pub use engine::{Arm, CountingIds, Effect, Engine, IdGen, ulid};
