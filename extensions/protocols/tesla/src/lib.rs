//! Tesla cars and Powerwall energy sites, through Tesla's Fleet API.

pub mod settings;
mod usage;

use irori_protocol::{Health, Protocol, ProtocolContext, ProtocolError};

use crate::settings::Settings;

/// The Tesla protocol.
#[derive(Debug)]
pub struct Tesla;

impl Protocol for Tesla {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(_settings: Settings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        ctx.set_health(Health::Running).await;
        ctx.stopped().await;
        Ok(())
    }
}
