//! What each call costs: which of Tesla's billing categories an endpoint falls in, and what the
//! categories cost in the account's currency.
//!
//! Tesla lists each endpoint's category in its docs, behind a "+" on the endpoint. What could be
//! read is marked `confirmed`; the rest is counted as a data request until someone checks, so
//! the meter errs on the side of spending less. Checking one is a one-line change here.

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// Millionths of the account's currency: a data request at $0.002 is 2 000.
pub type Micros = u64;

pub const MICROS: Micros = 1_000_000;

/// Tesla's billing categories. Streaming signals come later, with Fleet Telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Data,
    Command,
    Wake,
    /// Not billed: signing in, and anything Tesla says is free.
    Free,
}

/// Every call the extension makes, by what it is to Tesla's bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    /// Signing in, refreshing a token, the app's own token for registering: Tesla's auth server,
    /// not the Fleet API.
    Token,
    RegisterPartner,
    PartnerPublicKey,
    Products,
    /// `GET /vehicles`: every car's online/asleep state in one call.
    Vehicles,
    FleetStatus,
    VehicleData,
    VehicleDataCached,
    WakeUp,
    SignedCommand,
    SiteInfo,
    LiveStatus,
    EnergyHistory,
    EnergyCommand,
}

impl Endpoint {
    #[cfg(test)]
    pub const ALL: [Endpoint; 14] = [
        Endpoint::Token,
        Endpoint::RegisterPartner,
        Endpoint::PartnerPublicKey,
        Endpoint::Products,
        Endpoint::Vehicles,
        Endpoint::FleetStatus,
        Endpoint::VehicleData,
        Endpoint::VehicleDataCached,
        Endpoint::WakeUp,
        Endpoint::SignedCommand,
        Endpoint::SiteInfo,
        Endpoint::LiveStatus,
        Endpoint::EnergyHistory,
        Endpoint::EnergyCommand,
    ];

    /// Its billing category, and whether that's read from Tesla's docs rather than assumed.
    pub fn category(self) -> (Category, bool) {
        use Category::*;
        match self {
            Endpoint::Token => (Free, true),
            Endpoint::VehicleData => (Data, true),
            Endpoint::WakeUp => (Wake, true),
            Endpoint::SignedCommand => (Command, true),
            Endpoint::EnergyCommand => (Command, false),
            // Not read from Tesla's docs yet: counted as a data request until they are.
            Endpoint::RegisterPartner
            | Endpoint::PartnerPublicKey
            | Endpoint::Products
            | Endpoint::Vehicles
            | Endpoint::FleetStatus
            | Endpoint::VehicleDataCached
            | Endpoint::SiteInfo
            | Endpoint::LiveStatus
            | Endpoint::EnergyHistory => (Data, false),
        }
    }

    /// What it is, for a person reading the usage page.
    pub fn label(self) -> &'static str {
        match self {
            Endpoint::Token => "Signing in",
            Endpoint::RegisterPartner => "Registering with Tesla",
            Endpoint::PartnerPublicKey => "Checking the public key",
            Endpoint::Products => "Listing cars and sites",
            Endpoint::Vehicles => "Checking which cars are awake",
            Endpoint::FleetStatus => "Checking the car's key",
            Endpoint::VehicleData => "Reading the car",
            Endpoint::VehicleDataCached => "Reading the car's last report",
            Endpoint::WakeUp => "Waking the car",
            Endpoint::SignedCommand => "Command",
            Endpoint::SiteInfo => "Reading the site's settings",
            Endpoint::LiveStatus => "Reading the site",
            Endpoint::EnergyHistory => "Reading the site's day",
            Endpoint::EnergyCommand => "Site command",
        }
    }
}

/// What one call in each category costs, in [`Micros`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Prices {
    pub data: Micros,
    pub command: Micros,
    pub wake: Micros,
}

impl Prices {
    pub fn from_settings(settings: &Settings) -> Self {
        let each = |per: u32| MICROS / Micros::from(per.max(1));
        Self {
            data: each(settings.data_per_unit),
            command: each(settings.commands_per_unit),
            wake: each(settings.wakes_per_unit),
        }
    }

    pub fn of(self, category: Category) -> Micros {
        match category {
            Category::Data => self.data,
            Category::Command => self.command,
            Category::Wake => self.wake,
            Category::Free => 0,
        }
    }

    pub fn call(self, endpoint: Endpoint) -> Micros {
        self.of(endpoint.category().0)
    }
}

/// Whether Tesla bills a call that got this status: everything below 500 is billed, a refused
/// or rate-limited one included (Tesla's billing page).
pub fn billed(status: u16) -> bool {
    status < 500
}

/// An amount as a person reads it: two decimals, three below a cent.
pub fn money(micros: Micros, currency: &str) -> String {
    let symbol = match currency {
        "USD" | "CAD" | "AUD" | "NZD" | "MXN" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" | "CNY" => "¥",
        _ => "",
    };
    let units = micros as f64 / MICROS as f64;
    let digits = if micros > 0 && micros < 10_000 { 3 } else { 2 };
    if symbol.is_empty() {
        format!("{units:.digits$} {currency}")
    } else {
        format!("{symbol}{units:.digits$}")
    }
}

/// An amount a person typed, like Tesla's dashboard shows it: `3.42`, `$3.42`, `3,42`.
pub fn parse_money(text: &str) -> Result<Micros, String> {
    let digits: String = text
        .trim()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    // One comma and no point is a decimal comma; otherwise commas group thousands.
    let digits = if digits.matches(',').count() == 1 && !digits.contains('.') {
        digits.replace(',', ".")
    } else {
        digits.replace(',', "")
    };
    let amount: f64 = digits
        .parse()
        .map_err(|_| format!("`{}` isn't an amount like 3.42", text.trim()))?;
    if !amount.is_finite() || !(0.0..=100_000.0).contains(&amount) {
        return Err("that amount is out of range".to_owned());
    }
    Ok((amount * MICROS as f64).round() as Micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_us_prices_are_teslas() {
        let prices = Prices::from_settings(&crate::settings::for_tests());
        assert_eq!(prices.data, 2_000, "$0.002");
        assert_eq!(prices.command, 1_000, "$0.001");
        assert_eq!(prices.wake, 20_000, "$0.02");
        assert_eq!(prices.call(Endpoint::Token), 0);
        assert_eq!(prices.call(Endpoint::VehicleData), 2_000);
    }

    #[test]
    fn what_is_not_read_from_teslas_docs_is_counted_as_billed() {
        for endpoint in Endpoint::ALL {
            let (category, confirmed) = endpoint.category();
            if !confirmed {
                assert_ne!(category, Category::Free, "{endpoint:?} is assumed billed");
            }
            assert!(!endpoint.label().is_empty());
        }
    }

    #[test]
    fn a_refusal_is_billed_and_a_server_error_is_not() {
        assert!(billed(200));
        assert!(billed(408), "an asleep car's answer is billed");
        assert!(billed(429), "so is being rate limited");
        assert!(!billed(500));
        assert!(!billed(503));
    }

    #[test]
    fn money_reads_like_the_dashboard() {
        assert_eq!(money(3_420_000, "USD"), "$3.42");
        assert_eq!(money(2_000, "USD"), "$0.002");
        assert_eq!(money(0, "USD"), "$0.00");
        assert_eq!(money(14_000_000, "CAD"), "$14.00");
        assert_eq!(money(1_500_000, "SEK"), "1.50 SEK");
    }

    #[test]
    fn an_amount_reads_as_typed() {
        assert_eq!(parse_money("3.42"), Ok(3_420_000));
        assert_eq!(parse_money(" $3.42 "), Ok(3_420_000));
        assert_eq!(parse_money("3,42 €"), Ok(3_420_000));
        assert_eq!(parse_money("1,234.50"), Ok(1_234_500_000));
        assert_eq!(parse_money("0"), Ok(0));
        assert!(parse_money("lots").is_err());
    }
}
