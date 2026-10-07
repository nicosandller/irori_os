//! What the shell and an extension's page share (`docs/specs/automations.md` §B4).
//!
//! The page runs in a sandboxed frame: it can't call Irori's API or read the shell's storage.
//! Everything it needs goes through the bridge, as JSON strings passed with `postMessage`:
//!
//! - [`message`]: the messages, the same types on both sides.
//! - [`page`]: the page's end — ask the shell, hear its events, look like Irori.
//!
//! And what both draw the same way: [`entity_icon`], the icon an entity has until someone gives
//! it another.

pub mod entity_icon;
pub mod message;
pub mod page;
