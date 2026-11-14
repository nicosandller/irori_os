//! The Tesla account as a device: what the API has cost this month, where the month is heading,
//! and the two controls that decide how much reading there is (the polling mode and the monthly
//! allowance). The cars and sites hang off it.
//!
//! Controls, not settings: an automation can turn polling to Responsive while a car charges and
//! back to Saving after, and a person can see and change it from the device's page.

use irori_types::{
    Capabilities, NumberCapabilities, NumberMode, SensorCapabilities, SensorClass, SensorValueType,
    State, StateClass,
};

use crate::entity::{self, Entity};
use crate::usage::Usage;
use crate::usage::pace::{Mode, Outlook, Plan};
use crate::usage::price::{self, MICROS, Micros};

pub use crate::setup::ACCOUNT_DEVICE;

/// The most a month's allowance can be set to, in the account's currency.
const MOST_ALLOWANCE: f64 = 1_000.0;

fn money(
    key: &'static str,
    name: &'static str,
    currency: &str,
    state_class: Option<StateClass>,
) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: Some(SensorClass::Monetary),
            unit: Some(currency.to_owned()),
            state_class,
            options: Vec::new(),
        }),
    )
}

fn count(key: &'static str, name: &'static str) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: None,
            unit: None,
            state_class: Some(StateClass::TotalIncreasing),
            options: Vec::new(),
        }),
    )
    .diagnostic()
}

pub fn entities(currency: &str) -> Vec<Entity> {
    let modes: Vec<&str> = Mode::ALL.iter().map(|mode| mode.label()).collect();
    vec![
        // Only goes up, and starts again on the 1st: a drop is a new month.
        money(
            "spent_irori",
            "Spent by Irori this month",
            currency,
            Some(StateClass::TotalIncreasing),
        ),
        money(
            "spent_account",
            "Spent this month, all apps",
            currency,
            // An estimate that a new dashboard figure can lower.
            Some(StateClass::Total),
        ),
        money("projected", "Projected for the month", currency, None),
        money("left", "Left of the allowance", currency, None),
        count("data_requests", "Data requests this month"),
        count("commands", "Commands this month"),
        count("wakes", "Wakes this month"),
        entity::select("polling", "Polling", &modes).config(),
        Entity::new(
            "allowance",
            "Monthly allowance",
            Capabilities::Number(NumberCapabilities {
                min: 0.0,
                max: MOST_ALLOWANCE,
                step: 0.5,
                unit: Some(currency.to_owned()),
                device_class: Some(SensorClass::Monetary),
                mode: NumberMode::Box,
            }),
        )
        .config(),
    ]
}

/// An amount in the currency's units, to the tenth of a cent.
fn units(micros: Micros) -> f64 {
    (micros as f64 / MICROS as f64 * 1_000.0).round() / 1_000.0
}

pub fn state(key: &str, usage: &Usage, outlook: &Outlook) -> Option<State> {
    let total = usage.month.total();
    let amount = |micros: Micros| entity::reading(Some(units(micros)));
    let counted = |count: u32| entity::reading(Some(f64::from(count)));
    match key {
        "spent_irori" => amount(outlook.irori),
        // Only once a dashboard figure says what the other apps spend.
        "spent_account" => outlook.others.and_then(|_| amount(outlook.account())),
        "projected" => amount(outlook.projected),
        "left" => amount(outlook.allowance.saturating_sub(outlook.account())),
        "data_requests" => counted(total.data),
        "commands" => counted(total.command),
        "wakes" => counted(total.wake),
        "polling" => entity::chosen(Some(usage.prefs.mode.label())),
        "allowance" => entity::set_to(Some(units(usage.prefs.allowance))),
        _ => None,
    }
}

/// Why the month needs a person's eye, if it does: heading past the allowance, or nothing left
/// to read with.
pub fn warning(outlook: &Outlook, plan: &Plan, currency: &str) -> Option<String> {
    let others = (outlook.others_per_day > 0).then(|| {
        format!(
            " Other apps on the account spend about {} a day.",
            price::money(outlook.others_per_day, currency)
        )
    });
    if plan.starved {
        return Some(format!(
            "Nothing is left of the {} allowance for reading this month: cars and sites are \
             read only on Refresh, and commands still go out.{}",
            price::money(outlook.allowance, currency),
            others.unwrap_or_default()
        ));
    }
    // In a month's first hours a few calls project to a frightening figure: pacing reacts at
    // once, but the warning waits for a day of the month to go by.
    let settled = outlook.now.saturating_sub(outlook.month_start) >= 86_400;
    (settled && outlook.over()).then(|| {
        format!(
            "At this rate the month ends at {}, over the {} allowance, so Irori reads less \
             often.{}",
            price::money(outlook.projected, currency),
            price::money(outlook.allowance, currency),
            others.unwrap_or_default()
        )
    })
}

/// Who asked for a command, as the usage page says it.
pub fn asked_by(origin: &irori_types::Origin) -> String {
    use irori_types::Origin;
    match origin {
        Origin::User { .. } => "you",
        Origin::Automation { .. } => "an automation",
        Origin::Api { .. } => "an API client",
        Origin::System => "Irori",
        Origin::Device { .. } => "a device",
    }
    .to_owned()
}

/// A polling mode from its label, as the select gives it.
pub fn mode(option: &str) -> Result<Mode, String> {
    Mode::from_label(option).ok_or_else(|| format!("no polling mode called {option}"))
}

/// An allowance as the number gives it, in [`Micros`].
pub fn allowance(value: f64) -> Result<Micros, String> {
    if !(0.0..=MOST_ALLOWANCE).contains(&value) {
        return Err(format!("an allowance is 0 to {MOST_ALLOWANCE}"));
    }
    Ok((value * MICROS as f64).round() as Micros)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Why;
    use crate::usage::price::Endpoint;
    use crate::usage::tests::{NOV_10, usage};

    #[test]
    fn the_month_shows_as_money_and_counts() {
        let mut usage = usage();
        for _ in 0..3 {
            usage.record(NOV_10, "car", Endpoint::VehicleData, 200, Why::Schedule);
        }
        usage.record(NOV_10, "car", Endpoint::WakeUp, 200, Why::Refresh);
        let outlook = usage.outlook(NOV_10);
        let value = |key| state(key, &usage, &outlook).and_then(|s| serde_json::to_value(s).ok());
        assert_eq!(value("spent_irori").expect("spent")["value"], 0.026);
        assert_eq!(value("data_requests").expect("count")["value"], 3.0);
        assert_eq!(value("wakes").expect("count")["value"], 1.0);
        assert_eq!(value("polling").expect("mode")["option"], "Balanced");
        assert_eq!(value("allowance").expect("allowance")["value"], 10.0);
        assert!(
            value("spent_account").is_none(),
            "unknown before a dashboard figure"
        );
        usage.calibrate(NOV_10, 2 * MICROS).expect("a figure");
        let outlook = usage.outlook(NOV_10);
        assert_eq!(
            state("spent_account", &usage, &outlook)
                .and_then(|s| serde_json::to_value(s).ok())
                .expect("known now")["value"],
            2.0
        );
    }

    #[test]
    fn every_entity_and_state_is_one_the_core_takes() {
        let manifest = irori_protocol::parse_manifest(include_str!("../irori-extension.toml"))
            .expect("a valid manifest");
        let kinds = &manifest.contributes.protocol[0].entity_kinds;
        let mut usage = usage();
        usage.calibrate(NOV_10, MICROS).expect("a figure");
        let outlook = usage.outlook(NOV_10);
        for entity in entities("USD") {
            let description = irori_types::EntityDescription {
                unique_id: entity::id(ACCOUNT_DEVICE, entity.key).expect("an id"),
                name: irori_types::Name::try_from(entity.name.as_str()).ok(),
                device_unique_id: None,
                suggested_object_id: None,
                capabilities: entity.capabilities.clone(),
                entity_category: entity.category,
            };
            let wire = serde_json::to_value(&description).expect("serialises");
            let back: irori_types::EntityDescription =
                serde_json::from_value(wire).expect("the core takes it");
            assert!(kinds.contains(&back.capabilities.kind()));
            let state = state(entity.key, &usage, &outlook)
                .unwrap_or_else(|| panic!("{} has a state", entity.key));
            let _: State =
                serde_json::from_value(serde_json::to_value(&state).expect("json")).expect("valid");
        }
    }

    #[test]
    fn a_month_heading_over_says_so_with_the_figures() {
        let mut usage = usage();
        usage.prefs.allowance = MICROS / 100;
        for _ in 0..50 {
            usage.record(NOV_10, "car", Endpoint::VehicleData, 200, Why::Schedule);
        }
        let outlook = usage.outlook(NOV_10);
        let plan = Plan {
            state: None,
            car: Some(std::time::Duration::from_secs(600)),
            site: None,
            starved: false,
        };
        let why = warning(&outlook, &plan, "USD").expect("over");
        assert!(why.starts_with("At this rate the month ends at $"), "{why}");
        assert!(why.contains("over the $0.01 allowance"), "{why}");
        let starved = Plan {
            starved: true,
            ..plan
        };
        assert!(
            warning(&outlook, &starved, "USD")
                .expect("starved")
                .starts_with("Nothing is left")
        );
        assert_eq!(allowance(12.5), Ok(12_500_000));
        assert!(allowance(-1.0).is_err());
        assert_eq!(mode("Saving"), Ok(Mode::Saving));
    }
}
