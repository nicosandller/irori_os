//! What the shell and an extension's page share (`docs/specs/automations.md` §B4).
//!
//! The page runs in a sandboxed frame: it can't call Irori's API or read the shell's storage.
//! Everything it needs goes through the bridge, as JSON strings passed with `postMessage`:
//!
//! - [`message`]: the messages, the same types on both sides.
//! - [`page`]: the page's end — ask the shell, hear its events, look like Irori.
//!
//! And what both are built from, so that the same thing is the same thing in either:
//!
//! - [`combo`]: a field that searches a list as you type, for choosing an entity.
//! - [`entity_icon`]: the icon an entity has until someone gives it another.
//! - [`json`]: JSON written for a person, and coloured as it's typed.
//! - [`color`]: a light's colour as the page and the light each say it.
//! - [`clipboard`]: copying, where the browser makes it awkward.

pub mod clipboard;
pub mod color;
pub mod combo;
pub mod entity_icon;
pub mod json;
pub mod message;
pub mod page;
