//! Where each camera was last found, kept in the extension's store.
//!
//! Multicast over Wi-Fi gets lost: on the user's network a Tapo answered only some probes, so a
//! camera could go unfound for minutes after a restart. A probe sent straight to a camera's
//! address is answered every time, so each probe also goes to every address remembered here.

use std::collections::BTreeMap;
use std::net::IpAddr;

use irori_protocol::ProtocolContext;

const KEY: &str = "addresses";
/// The most cameras remembered.
const MOST: usize = 64;

/// Camera id to the address it last answered from.
#[derive(Debug, Default)]
pub struct Addresses(BTreeMap<String, IpAddr>);

impl Addresses {
    pub async fn load(ctx: &ProtocolContext) -> Self {
        match ctx.load(KEY).await {
            Ok(Some(value)) => Self(serde_json::from_value(value).unwrap_or_default()),
            _ => Self::default(),
        }
    }

    pub fn all(&self) -> Vec<IpAddr> {
        self.0.values().copied().collect()
    }

    /// Remembers where a camera answered from, if that's news.
    pub async fn saw(&mut self, ctx: &ProtocolContext, id: &str, ip: IpAddr) {
        if self.0.get(id) == Some(&ip) || (!self.0.contains_key(id) && self.0.len() >= MOST) {
            return;
        }
        self.0.insert(id.to_owned(), ip);
        let value = serde_json::to_value(&self.0).unwrap_or_default();
        if ctx.store(KEY, value).await.is_err() {
            tracing::warn!("a camera's address couldn't be remembered");
        }
    }
}
