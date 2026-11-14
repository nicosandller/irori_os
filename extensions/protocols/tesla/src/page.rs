//! What the usage page asks the extension, answered from the meter.
//!
//! The page only lays things out: the wording, the money and the names are worked out here,
//! where tests can read them. Amounts travel twice, as [`Micros`] for drawing and as text for
//! reading, so the page never formats money itself.
//!
//! The page's own types mirror these shapes; `tests::the_shapes_the_page_reads` pins the field
//! names so the two can't drift apart unnoticed.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::usage::pace::{Mode, Outlook, Plan};
use crate::usage::price::{self, Micros, Prices};
use crate::usage::{Call, Counts, Month, Usage, Why};

/// Where Tesla shows what the account spent.
pub const DASHBOARD: &str = "https://developer.tesla.com/en_US/dashboard";

/// What a ledger target is called on the page, and what kind of thing it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    /// `car`, `site` or `account`.
    pub kind: &'static str,
}

/// The names of the cars and sites, by the target their calls are counted under.
pub type Names = BTreeMap<String, Target>;

fn money(micros: Micros, currency: &str) -> Value {
    json!({ "micros": micros, "text": price::money(micros, currency) })
}

fn target<'a>(names: &'a Names, id: &str) -> (String, &'a str) {
    match names.get(id) {
        Some(found) => (found.name.clone(), found.kind),
        None if id == crate::api::ACCOUNT => ("The account".to_owned(), "account"),
        // A car or site that has gone from the account since: its id is all there is.
        None => (id.to_owned(), "gone"),
    }
}

/// What each mode does, as the page explains it.
fn about(mode: Mode) -> &'static str {
    match mode {
        Mode::Off => "Nothing is read on its own; only Refresh and commands.",
        Mode::Saving => "Cars are read only while charging or driving, every ten minutes.",
        Mode::Balanced => "As often as the allowance allows, from every two minutes.",
        Mode::Responsive => "From every minute while a car is doing something.",
    }
}

/// The month at a glance: `usage.summary`.
pub fn summary(
    usage: &Usage,
    outlook: &Outlook,
    plan: &Plan,
    warning: Option<String>,
    can_change: bool,
) -> Value {
    let currency = usage.currency.as_str();
    let total = usage.month.total();
    let seconds = |gap: Option<std::time::Duration>| gap.map(|gap| gap.as_secs());
    json!({
        "currency": currency,
        "month": usage.month.month,
        "now": outlook.now,
        "month_start": outlook.month_start,
        "month_end": outlook.month_end,
        "allowance": money(outlook.allowance, currency),
        "credit": money(usage.credit, currency),
        "irori": money(outlook.irori, currency),
        "irori_commands": money(outlook.irori_commands, currency),
        "others": outlook.others.map(|others| money(others, currency)),
        "others_per_day": money(outlook.others_per_day, currency),
        "account": money(outlook.account(), currency),
        "projected": money(outlook.projected, currency),
        "left": money(outlook.allowance.saturating_sub(outlook.account()), currency),
        "over": outlook.over(),
        "checked_at": outlook.checked_at,
        "runs_out_at": outlook.runs_out_at,
        "counts": counts(total),
        "prices": {
            "data": money(usage.prices.data, currency),
            "command": money(usage.prices.command, currency),
            "wake": money(usage.prices.wake, currency),
        },
        "mode": usage.prefs.mode.label(),
        "modes": Mode::ALL
            .iter()
            .map(|mode| json!({ "label": mode.label(), "about": about(*mode) }))
            .collect::<Vec<_>>(),
        "pace": {
            "states_every": seconds(plan.state),
            "car_every": seconds(plan.car),
            "site_every": seconds(plan.site),
            "starved": plan.starved,
        },
        "warning": warning,
        "can_change": can_change,
        "dashboard": DASHBOARD,
    })
}

fn counts(counts: Counts) -> Value {
    json!({ "data": counts.data, "command": counts.command, "wake": counts.wake })
}

/// One month, day by day (every day of it, quiet ones too) and target by target.
fn month(month: &Month, prices: Prices, currency: &str, names: &Names) -> Value {
    let by_day = month.by_day();
    let days: Vec<Value> = (1..=crate::usage::days_in(&month.month))
        .map(|day| {
            let counts_then = by_day.get(&day).copied().unwrap_or_default();
            json!({
                "day": day,
                "counts": counts(counts_then),
                "data": Micros::from(counts_then.data) * prices.data,
                "command": Micros::from(counts_then.command) * prices.command,
                "wake": Micros::from(counts_then.wake) * prices.wake,
                "cost": money(counts_then.cost(prices), currency),
            })
        })
        .collect();
    let mut targets: Vec<(Micros, Value)> = month
        .by_target()
        .into_iter()
        .map(|(id, counts_then)| {
            let (name, kind) = target(names, &id);
            let cost = counts_then.cost(prices);
            (
                cost,
                json!({
                    "id": id,
                    "name": name,
                    "kind": kind,
                    "counts": counts(counts_then),
                    "cost": money(cost, currency),
                }),
            )
        })
        .filter(|(cost, _)| *cost > 0)
        .collect();
    targets.sort_by_key(|(cost, _)| std::cmp::Reverse(*cost));
    json!({
        "month": month.month,
        "days": days,
        "targets": targets.into_iter().map(|(_, value)| value).collect::<Vec<_>>(),
        "cost": money(month.total().cost(prices), currency),
    })
}

/// This month and last, by day and by target: `usage.days`. Last month is costed at today's
/// prices, which are the ones the person set.
pub fn days(usage: &Usage, names: &Names) -> Value {
    let currency = usage.currency.as_str();
    let last = (!usage.last_month.month.is_empty())
        .then(|| month(&usage.last_month, usage.prices, currency, names));
    json!({
        "this": month(&usage.month, usage.prices, currency, names),
        "last": last,
    })
}

/// Why a call was made, in a few words.
pub fn why(why: &Why) -> String {
    match why {
        Why::Schedule => "On the schedule".to_owned(),
        Why::Refresh => "Refresh".to_owned(),
        Why::Command { what, by } => format!("{what}, asked by {by}"),
        Why::WakeForCommand { what } => format!("Waking it for {what}"),
        Why::Setup => "Setting up".to_owned(),
        Why::Start => "Starting up".to_owned(),
    }
}

/// What a billed call's status says, when it isn't plain success.
fn flag(status: u16) -> Option<String> {
    match status {
        200..=299 => None,
        408 => Some("billed, car asleep".to_owned()),
        429 => Some("billed, rate limited".to_owned()),
        other => Some(format!("billed, refused ({other})")),
    }
}

fn call(call: &Call, currency: &str, names: &Names) -> Value {
    let (name, kind) = target(names, &call.target);
    json!({
        "at": call.at,
        "target": name,
        "kind": kind,
        "what": call.endpoint.label(),
        "why": why(&call.why),
        "status": call.status,
        "flag": flag(call.status),
        "cost": money(call.cost, currency),
    })
}

/// The billed calls, newest first: `usage.calls`.
pub fn calls(usage: &Usage, names: &Names) -> Value {
    Value::Array(
        usage
            .calls
            .iter()
            .rev()
            .map(|one| call(one, &usage.currency, names))
            .collect(),
    )
}

/// What `usage.set` asks to change: the polling mode by its label, the allowance in the
/// account's currency.
pub fn change(params: &Value) -> Result<(Option<Mode>, Option<Micros>), String> {
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .map(crate::account::mode)
        .transpose()?;
    let allowance = match params.get("allowance") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => Some(crate::account::allowance(
            number.as_f64().unwrap_or(f64::NAN),
        )?),
        Some(Value::String(text)) => {
            let micros = price::parse_money(text)?;
            Some(crate::account::allowance(
                micros as f64 / price::MICROS as f64,
            )?)
        }
        Some(_) => return Err("the allowance is an amount like 10".to_owned()),
    };
    if mode.is_none() && allowance.is_none() {
        return Err("say what to change: the mode or the allowance".to_owned());
    }
    Ok((mode, allowance))
}

/// The dashboard figure `usage.calibrate` was given.
pub fn spent(params: &Value) -> Result<Micros, String> {
    match params.get("spent") {
        Some(Value::String(text)) => price::parse_money(text),
        Some(Value::Number(number)) => price::parse_money(&number.to_string()),
        _ => Err("say what Tesla's dashboard shows".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::price::Endpoint;
    use crate::usage::tests::{NOV_10, usage};

    fn names() -> Names {
        Names::from([(
            "VIN1".to_owned(),
            Target {
                name: "Blue".to_owned(),
                kind: "car",
            },
        )])
    }

    fn plan() -> Plan {
        Plan {
            state: Some(std::time::Duration::from_secs(900)),
            car: Some(std::time::Duration::from_secs(120)),
            site: None,
            starved: false,
        }
    }

    fn keys(value: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn the_shapes_the_page_reads() {
        let mut usage = usage();
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 408, Why::Schedule);
        usage.record(NOV_10, "gone", Endpoint::WakeUp, 200, Why::Refresh);
        usage
            .calibrate(NOV_10, 3 * price::MICROS)
            .expect("a figure");
        let outlook = usage.outlook(NOV_10);

        let summary = summary(&usage, &outlook, &plan(), None, true);
        assert_eq!(
            keys(&summary),
            [
                "account",
                "allowance",
                "can_change",
                "checked_at",
                "counts",
                "credit",
                "currency",
                "dashboard",
                "irori",
                "irori_commands",
                "left",
                "mode",
                "modes",
                "month",
                "month_end",
                "month_start",
                "now",
                "others",
                "others_per_day",
                "over",
                "pace",
                "prices",
                "projected",
                "runs_out_at",
                "warning",
            ]
        );
        assert_eq!(keys(&summary["irori"]), ["micros", "text"]);
        assert_eq!(
            keys(&summary["pace"]),
            ["car_every", "site_every", "starved", "states_every"]
        );
        assert_eq!(keys(&summary["modes"][0]), ["about", "label"]);
        assert_eq!(keys(&summary["counts"]), ["command", "data", "wake"]);
        assert_eq!(summary["irori"]["text"], "$0.02");
        assert_eq!(summary["pace"]["car_every"], 120);

        let days = days(&usage, &names());
        assert_eq!(keys(&days), ["last", "this"]);
        assert_eq!(keys(&days["this"]), ["cost", "days", "month", "targets"]);
        assert_eq!(
            keys(&days["this"]["days"][0]),
            ["command", "cost", "counts", "data", "day", "wake"]
        );
        assert_eq!(
            keys(&days["this"]["targets"][0]),
            ["cost", "counts", "id", "kind", "name"]
        );

        let calls = calls(&usage, &names());
        assert_eq!(
            keys(&calls[0]),
            [
                "at", "cost", "flag", "kind", "status", "target", "what", "why"
            ]
        );
    }

    #[test]
    fn every_day_of_the_month_is_there_and_targets_are_named_dearest_first() {
        let mut usage = usage();
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        usage.record(NOV_10, "VIN2", Endpoint::WakeUp, 200, Why::Refresh);
        let days = days(&usage, &names());
        let this = &days["this"];
        assert_eq!(this["days"].as_array().expect("days").len(), 30);
        assert_eq!(this["days"][9]["day"], 10);
        assert_eq!(this["days"][9]["data"], 2_000);
        assert_eq!(this["days"][9]["wake"], 20_000);
        assert_eq!(this["days"][0]["cost"]["micros"], 0);
        assert_eq!(this["targets"][0]["name"], "VIN2", "gone, so its VIN");
        assert_eq!(this["targets"][0]["kind"], "gone");
        assert_eq!(this["targets"][1]["name"], "Blue");
        assert!(days["last"].is_null(), "no last month yet");
    }

    #[test]
    fn calls_read_newest_first_and_say_what_a_refusal_cost() {
        let mut usage = usage();
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        usage.record(
            NOV_10 + 60,
            "VIN1",
            Endpoint::WakeUp,
            200,
            Why::WakeForCommand {
                what: "Climate on".into(),
            },
        );
        usage.record(
            NOV_10 + 120,
            "VIN1",
            Endpoint::VehicleData,
            408,
            Why::Refresh,
        );
        usage.record(
            NOV_10 + 180,
            "VIN1",
            Endpoint::SignedCommand,
            429,
            Why::Command {
                what: "Lock".into(),
                by: "an automation".into(),
            },
        );
        let calls = calls(&usage, &names());
        assert_eq!(calls[0]["why"], "Lock, asked by an automation");
        assert_eq!(calls[0]["flag"], "billed, rate limited");
        assert_eq!(calls[1]["flag"], "billed, car asleep");
        assert_eq!(calls[1]["target"], "Blue");
        assert_eq!(calls[2]["why"], "Waking it for Climate on");
        assert_eq!(calls[2]["cost"]["text"], "$0.02");
        assert!(calls[3]["flag"].is_null());
        assert_eq!(calls[3]["what"], "Reading the car");
    }

    #[test]
    fn changes_and_figures_are_read_as_a_person_types_them() {
        assert_eq!(
            change(&json!({ "mode": "Saving" })),
            Ok((Some(Mode::Saving), None))
        );
        assert_eq!(
            change(&json!({ "allowance": "12.50" })),
            Ok((None, Some(12_500_000)))
        );
        assert_eq!(
            change(&json!({ "allowance": 20 })),
            Ok((None, Some(20_000_000)))
        );
        assert!(change(&json!({ "mode": "Fast" })).is_err());
        assert!(change(&json!({ "allowance": 5000 })).is_err());
        assert!(change(&json!({})).is_err());
        assert_eq!(spent(&json!({ "spent": "$3,42" })), Ok(3_420_000));
        assert_eq!(spent(&json!({ "spent": 1.5 })), Ok(1_500_000));
        assert!(spent(&json!({})).is_err());
    }
}
