//! A managed Node.js, for a protocol that wraps a program written for it: Zigbee's Zigbee2MQTT,
//! Matter's Open Home Foundation Matter Server. No Rust stack exists for either that does what
//! those already do in a great many homes, so the protocol runs the real thing beside it.
//!
//! - [`install`] downloads Node.js once into the extension's own package directory, checked
//!   against nodejs.org's published checksums, and installs one npm package next to it with
//!   Node's own `corepack pnpm`. Nothing touches a Node.js the machine already has.
//! - [`child`] runs that package as a child process that dies with the extension, with its log
//!   lines forwarded into Irori's and the last error kept, so a failure can say why.
//!
//! Both need the extension's `host_shell` permission: full access to this machine, which the page
//! says in those words before it installs.

pub mod child;
pub mod install;
