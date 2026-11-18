//! What the Tesla extension is told: the developer app it signs in as, the website Tesla finds
//! its public key on, and what Tesla charges for the API.
//!
//! The client secret and the private key are secrets (`writeOnly`): kept in `secrets.toml`, never
//! sent back to a page, never printed. Home Assistant's Tesla Fleet setup made all of these
//! already; reusing its app, domain and key means the car already trusts Irori.
//!
//! How often to read the cars and how much a month to spend aren't settings: they're controls
//! on the Tesla account device and the usage page, so an automation can change them too.

use irori_cloud::Secret;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, PartialEq, JsonSchema)]
pub struct Settings {
    /// The Client ID of your app on developer.tesla.com. Home Assistant's Tesla Fleet setup
    /// already has one: reuse it.
    #[schemars(title = "Client ID")]
    pub client_id: String,
    /// The Client Secret shown beside it.
    #[schemars(title = "Client secret")]
    pub client_secret: Secret,
    /// The website Tesla finds your public key on, without https://: the app's Allowed Origin,
    /// e.g. tesla.example.com.
    #[schemars(title = "Domain")]
    pub domain: String,
    /// The private key Tesla already knows, to reuse it: the contents of tesla_fleet.key in
    /// Home Assistant's config folder. Leave blank and Irori makes a key of its own, which then
    /// has to go on your website and on the car.
    #[schemars(title = "Private key")]
    pub private_key: Option<Secret>,
    /// Where Tesla sends you after signing in. It has to be one of the app's Allowed Redirect
    /// URIs; the page there doesn't have to exist. Blank: https://{domain}/irori/tesla.
    #[schemars(title = "Redirect address", extend("x-irori-group" = "Advanced"))]
    pub redirect_uri: Option<String>,
    /// What Tesla takes off each month's bill. Tesla gives every account the same, in its
    /// currency: $10 in the US.
    #[schemars(
        title = "Monthly credit",
        default = "default_credit",
        range(min = 0.0, max = 10_000.0),
        extend("x-irori-group" = "Billing")
    )]
    pub monthly_credit: f64,
    /// The currency Tesla bills the account in, as Tesla's dashboard shows it.
    #[schemars(
        title = "Currency",
        default = "default_currency",
        extend("x-irori-group" = "Billing")
    )]
    pub currency: String,
    /// How many requests for a car's data one unit of the currency buys, from Tesla's pricing.
    #[schemars(
        title = "Data requests per 1",
        default = "default_data",
        range(min = 1, max = 1_000_000),
        extend("x-irori-group" = "Billing")
    )]
    pub data_per_unit: u32,
    /// How many commands one unit of the currency buys.
    #[schemars(
        title = "Commands per 1",
        default = "default_commands",
        range(min = 1, max = 1_000_000),
        extend("x-irori-group" = "Billing")
    )]
    pub commands_per_unit: u32,
    /// How many wakes one unit of the currency buys.
    #[schemars(
        title = "Wakes per 1",
        default = "default_wakes",
        range(min = 1, max = 1_000_000),
        extend("x-irori-group" = "Billing")
    )]
    pub wakes_per_unit: u32,
}

impl Settings {
    /// Where Tesla sends the person after they sign in.
    pub fn redirect_uri(&self) -> String {
        self.redirect_uri
            .clone()
            .unwrap_or_else(|| format!("https://{}/irori/tesla", self.domain))
    }
}

fn default_credit() -> f64 {
    10.0
}

fn default_currency() -> String {
    "USD".to_owned()
}

fn default_data() -> u32 {
    500
}

fn default_commands() -> u32 {
    1_000
}

fn default_wakes() -> u32 {
    50
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    client_id: String,
    client_secret: Secret,
    domain: String,
    #[serde(default)]
    private_key: Option<Secret>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default = "default_credit")]
    monthly_credit: f64,
    #[serde(default = "default_currency")]
    currency: String,
    #[serde(default = "default_data")]
    data_per_unit: u32,
    #[serde(default = "default_commands")]
    commands_per_unit: u32,
    #[serde(default = "default_wakes")]
    wakes_per_unit: u32,
}

/// A domain as a person might paste it, as the bare host Tesla wants: no scheme, no path.
fn bare_host(text: &str) -> Result<String, String> {
    let text = text.trim();
    let text = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    let host = text
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let fine = !host.is_empty()
        && host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if fine {
        Ok(host)
    } else {
        Err(format!(
            "the domain is a website's name like tesla.example.com, not `{text}`"
        ))
    }
}

/// A refusal, in the words a person reads under the settings form.
fn fail<E: serde::de::Error>(why: impl std::fmt::Display) -> E {
    E::custom(why)
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Raw::deserialize(deserializer)?;
        let client_id = raw.client_id.trim().to_owned();
        if client_id.is_empty() {
            return Err(fail("the Client ID is empty"));
        }
        if raw.client_secret.expose().is_empty() {
            return Err(fail("the client secret is empty"));
        }
        let domain = bare_host(&raw.domain).map_err(fail)?;
        let redirect_uri = raw
            .redirect_uri
            .map(|uri| uri.trim().to_owned())
            .filter(|uri| !uri.is_empty());
        if let Some(uri) = &redirect_uri
            && !uri.starts_with("https://")
        {
            return Err(fail("the redirect address starts with https://"));
        }
        if !raw.monthly_credit.is_finite() || !(0.0..=10_000.0).contains(&raw.monthly_credit) {
            return Err(fail("the monthly credit is between 0 and 10000"));
        }
        let currency = raw.currency.trim().to_ascii_uppercase();
        if currency.is_empty() || currency.len() > 8 {
            return Err(fail("the currency is a short code like USD"));
        }
        for (what, per) in [
            ("data requests", raw.data_per_unit),
            ("commands", raw.commands_per_unit),
            ("wakes", raw.wakes_per_unit),
        ] {
            if per == 0 {
                return Err(fail(format!("{what} per 1 can't be 0")));
            }
        }
        Ok(Settings {
            client_id,
            client_secret: raw.client_secret,
            domain,
            private_key: raw.private_key.filter(|key| !key.expose().is_empty()),
            redirect_uri,
            monthly_credit: raw.monthly_credit,
            currency,
            data_per_unit: raw.data_per_unit,
            commands_per_unit: raw.commands_per_unit,
            wakes_per_unit: raw.wakes_per_unit,
        })
    }
}

#[cfg(test)]
pub(crate) fn for_tests() -> Settings {
    Settings {
        client_id: "the-client".to_owned(),
        client_secret: Secret::from("the-secret".to_owned()),
        domain: "tesla.example.com".to_owned(),
        private_key: None,
        redirect_uri: None,
        monthly_credit: 10.0,
        currency: "USD".to_owned(),
        data_per_unit: 500,
        commands_per_unit: 1_000,
        wakes_per_unit: 50,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(value: serde_json::Value) -> Result<Settings, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    #[test]
    fn the_app_and_its_website_are_required_and_the_rest_has_defaults() {
        let schema = serde_json::to_value(schemars::schema_for!(Settings)).expect("a schema");
        let mut required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        required.sort_unstable();
        assert_eq!(required, ["client_id", "client_secret", "domain"]);
        let settings = load(serde_json::json!({
            "client_id": " abc ",
            "client_secret": "s",
            "domain": "https://Tesla.Example.com/",
        }))
        .expect("valid");
        assert_eq!(settings.client_id, "abc");
        assert_eq!(settings.domain, "tesla.example.com");
        assert_eq!(
            settings.redirect_uri(),
            "https://tesla.example.com/irori/tesla"
        );
        assert_eq!(settings.monthly_credit, 10.0);
        assert_eq!(settings.currency, "USD");
        assert_eq!(
            (
                settings.data_per_unit,
                settings.commands_per_unit,
                settings.wakes_per_unit
            ),
            (500, 1_000, 50)
        );
        assert_eq!(settings.private_key, None);
    }

    #[test]
    fn a_secret_never_prints() {
        let settings = for_tests();
        assert!(!format!("{settings:?}").contains("the-secret"));
    }

    #[test]
    fn what_is_wrong_is_said_plainly() {
        let base = serde_json::json!({ "client_id": "a", "client_secret": "s", "domain": "x.y" });
        let with = |key: &str, value: serde_json::Value| {
            let mut settings = base.clone();
            settings[key] = value;
            load(settings).expect_err("refused")
        };
        assert!(with("domain", "localhost".into()).contains("website's name"));
        assert!(with("domain", "a b.com".into()).contains("website's name"));
        assert!(with("redirect_uri", "http://x.y/back".into()).contains("https://"));
        assert!(with("monthly_credit", (-1.0).into()).contains("between 0"));
        assert!(with("wakes_per_unit", 0.into()).contains("can't be 0"));
        assert!(with("client_id", " ".into()).contains("Client ID"));
    }
}
