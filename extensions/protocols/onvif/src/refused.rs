//! Logins a camera has refused, remembered across restarts.
//!
//! Giving any camera its login restarts this extension, and cameras lock an account after a
//! few failures. So a refused login is remembered in the extension's store, as a fingerprint:
//! a SHA-256 of the camera's id, the username and the password, never the password itself.
//! While a camera's login still matches its fingerprint it isn't sent again. Typing a different
//! one changes the fingerprint, and that one is tried, once.

use std::collections::BTreeMap;

use irori_protocol::ProtocolContext;
use sha2::{Digest as _, Sha256};

use crate::settings::Login;

/// The key in the extension's store.
const KEY: &str = "refused_logins";

/// Refused fingerprints, by camera id.
#[derive(Debug, Default)]
pub struct Refusals(BTreeMap<String, String>);

impl Refusals {
    /// What the store has. A store that can't be read is treated as empty, with a warning: the
    /// worst case is one more attempt with a login that was refused before.
    pub async fn load(ctx: &ProtocolContext) -> Self {
        match ctx.load(KEY).await {
            Ok(Some(value)) => match serde_json::from_value(value) {
                Ok(map) => Self(map),
                Err(_) => {
                    tracing::warn!("the refused logins couldn't be read; starting afresh");
                    Self::default()
                }
            },
            Ok(None) => Self::default(),
            Err(_) => {
                tracing::warn!("the refused logins couldn't be loaded");
                Self::default()
            }
        }
    }

    /// Whether this is the login the camera last refused.
    pub fn refused(&self, id: &str, login: &Login) -> bool {
        self.0.get(id) == Some(&fingerprint(id, login))
    }

    /// The camera refused this login: remember it.
    pub async fn refuse(&mut self, ctx: &ProtocolContext, id: &str, login: &Login) {
        self.0.insert(id.to_owned(), fingerprint(id, login));
        self.save(ctx).await;
    }

    /// The camera took this login: whatever it refused before no longer matters.
    pub async fn accepted(&mut self, ctx: &ProtocolContext, id: &str, _login: &Login) {
        if self.0.remove(id).is_some() {
            self.save(ctx).await;
        }
    }

    async fn save(&self, ctx: &ProtocolContext) {
        let value = serde_json::to_value(&self.0).unwrap_or_default();
        if ctx.store(KEY, value).await.is_err() {
            tracing::warn!("a refused login couldn't be remembered");
        }
    }
}

fn fingerprint(id: &str, login: &Login) -> String {
    let mut sha = Sha256::new();
    for part in [id, &login.username, &login.password] {
        sha.update((part.len() as u64).to_be_bytes());
        sha.update(part.as_bytes());
    }
    crate::http::hex(&sha.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_changes_with_any_part_and_shows_none() {
        let login = Login {
            username: "admin".into(),
            password: "hunter2!".into(),
        };
        let print = fingerprint("cam", &login);
        assert_eq!(print.len(), 64);
        assert!(!print.contains("hunter2"));
        let other = Login {
            password: "hunter3!".into(),
            ..login.clone()
        };
        assert_ne!(print, fingerprint("cam", &other));
        assert_ne!(print, fingerprint("cam2", &login));
        // Lengths go in too, so moving a character between parts changes it.
        let shifted = Login {
            username: "admin".into(),
            password: "hunter2!".into(),
        };
        assert_ne!(
            fingerprint("cama", &shifted),
            fingerprint(
                "cam",
                &Login {
                    username: "aadmin".into(),
                    password: "hunter2!".into(),
                }
            )
        );
    }
}
