//! What the SwitchBot extension can be told: the account's API token and secret key, and how
//! much of SwitchBot's daily allowance of calls it may use.
//!
//! The token and secret are secrets (`writeOnly`): kept in `secrets.toml`, never sent back to a
//! page, never printed. Neither is required, so a home with only Bluetooth devices works without
//! an account; a token without its secret, or the other way round, is refused.

use irori_cloud::Secret;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// SwitchBot's limit: 10 000 calls a day for the whole account.
pub const ACCOUNT_DAILY_CALLS: u32 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct Settings {
    /// The account's API token. In the SwitchBot app: Profile → Preferences → About, tap App
    /// Version ten times, then Developer Options.
    #[schemars(title = "Token")]
    pub token: Option<Secret>,
    /// The secret key shown beside the token in Developer Options.
    #[schemars(title = "Secret key")]
    pub secret: Option<Secret>,
    /// How many of the account's 10 000 calls a day Irori may use. Home Assistant or another app
    /// signed in with the same token spends from the same 10 000, so leave them room.
    #[schemars(
        title = "Calls a day",
        default = "default_daily_calls",
        range(min = 100, max = 10_000)
    )]
    pub daily_calls: u32,
}

/// The token and secret key, together: there is no use for one without the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub token: Secret,
    pub secret: Secret,
}

impl Settings {
    pub fn account(&self) -> Option<Account> {
        Some(Account {
            token: self.token.clone()?,
            secret: self.secret.clone()?,
        })
    }
}

fn default_daily_calls() -> u32 {
    5_000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    token: Option<Secret>,
    #[serde(default)]
    secret: Option<Secret>,
    #[serde(default = "default_daily_calls")]
    daily_calls: u32,
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Raw::deserialize(deserializer)?;
        let blank = |secret: Option<Secret>| secret.filter(|secret| !secret.expose().is_empty());
        let (token, secret) = (blank(raw.token), blank(raw.secret));
        if token.is_some() != secret.is_some() {
            return Err(serde::de::Error::custom(
                "give both the token and the secret key, or neither",
            ));
        }
        if !(100..=ACCOUNT_DAILY_CALLS).contains(&raw.daily_calls) {
            return Err(serde::de::Error::custom(
                "calls a day has to be between 100 and 10000",
            ));
        }
        Ok(Settings {
            token,
            secret,
            daily_calls: raw.daily_calls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(value: serde_json::Value) -> Result<Settings, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    #[test]
    fn no_settings_is_a_bluetooth_only_home() {
        let settings = load(serde_json::json!({})).expect("valid");
        assert_eq!(settings.account(), None);
        assert_eq!(settings.daily_calls, 5_000);
    }

    #[test]
    fn a_token_needs_its_secret() {
        let error = load(serde_json::json!({ "token": "abc123" })).expect_err("half an account");
        assert!(error.contains("both"), "{error}");
        assert!(!error.contains("abc123"), "{error}");
        let settings = load(serde_json::json!({ "token": "abc", "secret": " def\n" }))
            .expect("a whole account");
        let account = settings.account().expect("an account");
        assert_eq!(account.secret.expose(), "def");
        assert!(!format!("{settings:?}").contains("abc"));
    }

    #[test]
    fn blank_credentials_are_none_at_all() {
        let settings = load(serde_json::json!({ "token": "", "secret": "  " })).expect("valid");
        assert_eq!(settings.account(), None);
    }

    #[test]
    fn calls_a_day_stay_within_the_account_limit() {
        assert!(load(serde_json::json!({ "daily_calls": 20_000 })).is_err());
        assert!(load(serde_json::json!({ "daily_calls": 10 })).is_err());
        assert_eq!(
            load(serde_json::json!({ "daily_calls": 10_000 }))
                .expect("valid")
                .daily_calls,
            10_000
        );
    }
}
