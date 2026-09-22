//! MQTT protocol with Home Assistant MQTT Discovery.
//!
//! Empty shell created in M0.1; see `ROADMAP.md` §2.1 for what lands here. Installable so the
//! catalog is honest: it runs, contributes no devices yet.

use irori_protocol::{NoSettings, Protocol, ProtocolContext, ProtocolError};

/// The MQTT protocol.
#[derive(Debug)]
pub struct Mqtt;

impl Protocol for Mqtt {
    type Config = NoSettings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");

    async fn run(_config: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        while ctx.next_call().await.is_some() {}
        Ok(())
    }
}
