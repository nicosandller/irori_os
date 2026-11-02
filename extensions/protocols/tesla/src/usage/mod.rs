//! The meter: what the extension spent on Tesla's API, what the whole account is likely to
//! spend this month, and how often the cars may be read so the month ends inside the allowance.
//!
//! Tesla has no API that says what an account spent, and other apps on the same developer
//! account (Home Assistant's Tesla Fleet, often) spend from the same credit. So the meter has
//! two halves: what Irori counts itself, call by call, and what the person reads off Tesla's
//! dashboard now and then ("Update from Tesla's dashboard"), which says what everyone else spent.
//! Pacing uses both.
//!
//! Months are calendar months in UTC, like Tesla's billing cycle.

pub mod pace;
pub mod price;

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

pub use pace::{Mode, Outlook};
pub use price::{Category, Endpoint, Micros, Prices};

/// How many billed calls the usage page can list. Each is about a hundred bytes stored, well
/// inside a stored value's 64 KB.
pub const CALLS_KEPT: usize = 250;

/// How many dashboard readings a month keeps.
const POINTS_KEPT: usize = 31;

/// One day's calls for one car, site, or the account, by category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub data: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub command: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wake: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub free: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Counts {
    fn add(&mut self, category: Category) {
        let slot = match category {
            Category::Data => &mut self.data,
            Category::Command => &mut self.command,
            Category::Wake => &mut self.wake,
            Category::Free => &mut self.free,
        };
        *slot = slot.saturating_add(1);
    }

    pub fn plus(self, other: Counts) -> Counts {
        Counts {
            data: self.data.saturating_add(other.data),
            command: self.command.saturating_add(other.command),
            wake: self.wake.saturating_add(other.wake),
            free: self.free.saturating_add(other.free),
        }
    }

    pub fn cost(self, prices: Prices) -> Micros {
        Micros::from(self.data) * prices.data
            + Micros::from(self.command) * prices.command
            + Micros::from(self.wake) * prices.wake
    }
}

/// A month's calls: by day of the month, then by what they were for (a car's VIN, `site:<id>`,
/// or `account`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Month {
    /// `2026-11`.
    pub month: String,
    pub days: BTreeMap<u8, BTreeMap<String, Counts>>,
}

impl Month {
    fn new(month: String) -> Self {
        Self {
            month,
            days: BTreeMap::new(),
        }
    }

    pub fn total(&self) -> Counts {
        self.days
            .values()
            .flat_map(BTreeMap::values)
            .fold(Counts::default(), |sum, counts| sum.plus(*counts))
    }

    /// Each day's counts, all targets together.
    pub fn by_day(&self) -> BTreeMap<u8, Counts> {
        self.days
            .iter()
            .map(|(day, targets)| {
                (
                    *day,
                    targets
                        .values()
                        .fold(Counts::default(), |sum, counts| sum.plus(*counts)),
                )
            })
            .collect()
    }

    /// Each target's counts, the whole month.
    pub fn by_target(&self) -> BTreeMap<String, Counts> {
        let mut out: BTreeMap<String, Counts> = BTreeMap::new();
        for targets in self.days.values() {
            for (target, counts) in targets {
                let sum = out.entry(target.clone()).or_default();
                *sum = sum.plus(*counts);
            }
        }
        out
    }
}

/// Why a call was made, as the usage page says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Why {
    /// Reading on the schedule the polling mode and the allowance allow.
    Schedule,
    /// Someone pressed Refresh.
    Refresh,
    /// A command, and who asked for it ("you", "an automation").
    Command { what: String, by: String },
    /// Waking the car so a command can reach it.
    WakeForCommand { what: String },
    /// Setting up: signing in, registering, checking keys and cars.
    Setup,
    /// Finding the cars and sites after a start.
    Start,
}

/// One billed call, for "Where it went".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    /// Seconds since 1970.
    pub at: u64,
    pub target: String,
    pub endpoint: Endpoint,
    pub status: u16,
    pub why: Why,
    /// What it cost at the prices when it was made.
    pub cost: Micros,
}

/// What Tesla's dashboard said the whole account had spent, and what Irori had counted itself
/// at that moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub at: u64,
    pub account: Micros,
    pub irori: Micros,
}

/// The person's choices: how often to read, and how much a month to spend. Controls, not
/// settings, so an automation can change them ("read often while charging").
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    pub mode: Mode,
    /// The most the whole account should spend in a month, in [`Micros`]: the credit, unless
    /// the person is happy to pay more (or wants to keep some for later).
    pub allowance: Micros,
}

/// The meter itself.
#[derive(Debug, Clone)]
pub struct Usage {
    pub prices: Prices,
    pub currency: String,
    /// What Tesla takes off each month's bill.
    pub credit: Micros,
    pub month: Month,
    /// The month before, for the page's comparison. Empty in the first month.
    pub last_month: Month,
    pub calls: VecDeque<Call>,
    /// This month's dashboard readings, oldest first.
    pub points: Vec<Point>,
    pub prefs: Prefs,
    /// Whether anything changed since it was last stored.
    pub dirty: bool,
}

/// The keys it's stored under (`ctx.store`).
pub mod keys {
    pub const CALLS: &str = "usage/calls";
    pub const POINTS: &str = "usage/dashboard";
    pub const PREFS: &str = "prefs";

    pub fn month(month: &str) -> String {
        format!("usage/month/{month}")
    }
}

impl Usage {
    pub fn new(prices: Prices, currency: String, credit: Micros, now: u64) -> Self {
        Self {
            prices,
            currency,
            credit,
            month: Month::new(month_of(now)),
            last_month: Month::default(),
            calls: VecDeque::new(),
            points: Vec::new(),
            prefs: Prefs {
                mode: Mode::Balanced,
                allowance: credit,
            },
            dirty: false,
        }
    }

    /// Starts a new month's count when `now` is in a later month. The month that ended becomes
    /// last month; its dashboard readings are let go.
    pub fn roll(&mut self, now: u64) {
        let current = month_of(now);
        if self.month.month != current {
            let ended = std::mem::replace(&mut self.month, Month::new(current));
            self.last_month = if ended.month == previous_month(&self.month.month) {
                ended
            } else {
                Month::default()
            };
            self.points.clear();
            self.dirty = true;
        }
    }

    /// Counts a call that Tesla answered with `status`. Calls Tesla doesn't bill (a server error)
    /// aren't counted at all.
    pub fn record(&mut self, now: u64, target: &str, endpoint: Endpoint, status: u16, why: Why) {
        if !price::billed(status) {
            return;
        }
        self.roll(now);
        let (category, _) = endpoint.category();
        let day = day_of(now);
        self.month
            .days
            .entry(day)
            .or_default()
            .entry(target.to_owned())
            .or_default()
            .add(category);
        if category != Category::Free {
            if self.calls.len() >= CALLS_KEPT {
                self.calls.pop_front();
            }
            self.calls.push_back(Call {
                at: now,
                target: target.to_owned(),
                endpoint,
                status,
                why,
                cost: self.prices.of(category),
            });
        }
        self.dirty = true;
    }

    /// What Irori spent this month.
    pub fn spent(&self) -> Micros {
        self.month.total().cost(self.prices)
    }

    /// The person read the account's total off Tesla's dashboard.
    pub fn calibrate(&mut self, now: u64, account: Micros) -> Result<(), String> {
        self.roll(now);
        let irori = self.spent();
        if let Some(last) = self.points.last()
            && account < last.account
        {
            return Err(format!(
                "That's less than the {} it said before. Tesla's figure only goes up during a \
                 month; check it's this month's.",
                price::money(last.account, &self.currency)
            ));
        }
        if self.points.len() >= POINTS_KEPT {
            self.points.remove(0);
        }
        self.points.push(Point {
            at: now,
            account,
            irori,
        });
        self.dirty = true;
        Ok(())
    }

    /// Everything pacing and the page need to know about the month, as of `now`.
    pub fn outlook(&self, now: u64) -> Outlook {
        Outlook::new(
            now,
            &self.month,
            self.prices,
            &self.points,
            self.prefs.allowance,
        )
    }
}

/// What an earlier run stored, to carry the month's count over a restart.
#[derive(Debug, Default)]
pub struct Stored {
    pub month: Option<Month>,
    pub last_month: Option<Month>,
    pub calls: Option<VecDeque<Call>>,
    pub points: Option<Vec<Point>>,
    pub prefs: Option<Prefs>,
}

impl Usage {
    /// The meter as an earlier run left it. A month that has ended since is last month.
    pub fn restore(mut self, now: u64, stored: Stored) -> Self {
        let this = month_of(now);
        let last = previous_month(&this);
        for month in [stored.month, stored.last_month].into_iter().flatten() {
            if month.month == this {
                self.month = month;
            } else if month.month == last && self.last_month.days.is_empty() {
                self.last_month = month;
            }
        }
        if let Some(calls) = stored.calls {
            self.calls = calls;
            while self.calls.len() > CALLS_KEPT {
                self.calls.pop_front();
            }
        }
        let (start, _) = month_bounds(now);
        self.points = stored
            .points
            .unwrap_or_default()
            .into_iter()
            .filter(|point| point.at >= start)
            .collect();
        if let Some(prefs) = stored.prefs {
            self.prefs = prefs;
        }
        self
    }

    /// What to store, by key.
    pub fn to_store(&self) -> Vec<(String, serde_json::Value)> {
        let mut out = vec![
            (
                keys::month(&self.month.month),
                serde_json::to_value(&self.month).unwrap_or_default(),
            ),
            (
                keys::CALLS.to_owned(),
                serde_json::to_value(&self.calls).unwrap_or_default(),
            ),
            (
                keys::POINTS.to_owned(),
                serde_json::to_value(&self.points).unwrap_or_default(),
            ),
            (
                keys::PREFS.to_owned(),
                serde_json::to_value(self.prefs).unwrap_or_default(),
            ),
        ];
        if !self.last_month.month.is_empty() {
            out.push((
                keys::month(&self.last_month.month),
                serde_json::to_value(&self.last_month).unwrap_or_default(),
            ));
        }
        out
    }
}

/// `2026-11` for a time in November 2026, UTC.
pub fn month_of(now: u64) -> String {
    let date = date_of(now);
    format!("{:04}-{:02}", date.year(), date.month())
}

/// The day of the month, 1 to 31, UTC.
pub fn day_of(now: u64) -> u8 {
    u8::try_from(date_of(now).day()).unwrap_or(1)
}

fn date_of(now: u64) -> jiff::civil::Date {
    let seconds = i64::try_from(now).unwrap_or(i64::MAX);
    jiff::Timestamp::from_second(seconds)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
}

/// When the month `now` is in began and when it ends, in seconds since 1970.
pub fn month_bounds(now: u64) -> (u64, u64) {
    let date = date_of(now);
    let first = date.first_of_month();
    let next = first
        .checked_add(jiff::Span::new().months(1))
        .unwrap_or(first);
    let at = |day: jiff::civil::Date| {
        day.to_zoned(jiff::tz::TimeZone::UTC)
            .map(|zoned| u64::try_from(zoned.timestamp().as_second()).unwrap_or(0))
            .unwrap_or(0)
    };
    (at(first), at(next))
}

/// The month before the one `now` is in, as `2026-10`.
pub fn previous_month_of(now: u64) -> String {
    previous_month(&month_of(now))
}

/// The first day of next month, as a person reads it: "December 1".
pub fn next_month_label(now: u64) -> String {
    let (_, end) = month_bounds(now);
    jiff::Timestamp::from_second(i64::try_from(end).unwrap_or(i64::MAX))
        .map(|at| {
            at.to_zoned(jiff::tz::TimeZone::UTC)
                .strftime("%B %-d")
                .to_string()
        })
        .unwrap_or_else(|_| "the 1st".to_owned())
}

fn previous_month(month: &str) -> String {
    let (year, number) = month.split_once('-').unwrap_or(("1970", "01"));
    let year: i32 = year.parse().unwrap_or(1970);
    let number: i32 = number.parse().unwrap_or(1);
    if number == 1 {
        format!("{:04}-12", year - 1)
    } else {
        format!("{year:04}-{:02}", number - 1)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 2026-11-10T12:00:00Z.
    pub const NOV_10: u64 = 1_794_312_000;
    const DAY: u64 = 86_400;

    pub fn usage() -> Usage {
        Usage::new(
            Prices::from_settings(&crate::settings::for_tests()),
            "USD".to_owned(),
            10 * price::MICROS,
            NOV_10,
        )
    }

    #[test]
    fn the_month_and_day_are_utcs() {
        assert_eq!(month_of(NOV_10), "2026-11");
        assert_eq!(day_of(NOV_10), 10);
        let (start, end) = month_bounds(NOV_10);
        assert_eq!(month_of(start), "2026-11");
        assert_eq!(day_of(start), 1);
        assert_eq!(month_of(end), "2026-12");
        assert_eq!((end - start) / DAY, 30);
        assert_eq!(previous_month("2026-01"), "2025-12");
        assert_eq!(previous_month("2026-11"), "2026-10");
    }

    #[test]
    fn calls_are_counted_by_day_target_and_category_and_billed_ones_listed() {
        let mut usage = usage();
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 408, Why::Schedule);
        usage.record(
            NOV_10,
            "VIN1",
            Endpoint::WakeUp,
            200,
            Why::WakeForCommand {
                what: "climate on".into(),
            },
        );
        usage.record(
            NOV_10 + DAY,
            "site:1",
            Endpoint::LiveStatus,
            200,
            Why::Schedule,
        );
        usage.record(NOV_10, "account", Endpoint::Token, 200, Why::Setup);
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 503, Why::Schedule);

        let total = usage.month.total();
        assert_eq!((total.data, total.wake, total.free), (3, 1, 1));
        assert_eq!(usage.spent(), 3 * 2_000 + 20_000);
        assert_eq!(
            usage.calls.len(),
            4,
            "signing in and a server error aren't listed"
        );
        assert_eq!(usage.calls[1].status, 408);
        assert_eq!(usage.month.by_day()[&11].data, 1);
        assert_eq!(usage.month.by_target()["VIN1"].data, 2);
        assert!(usage.dirty);
    }

    #[test]
    fn the_list_of_calls_keeps_the_newest() {
        let mut usage = usage();
        for n in 0..(CALLS_KEPT as u64 + 10) {
            usage.record(
                NOV_10 + n,
                "VIN1",
                Endpoint::VehicleData,
                200,
                Why::Schedule,
            );
        }
        assert_eq!(usage.calls.len(), CALLS_KEPT);
        assert_eq!(
            usage.calls.back().expect("one").at,
            NOV_10 + CALLS_KEPT as u64 + 9
        );
    }

    #[test]
    fn a_new_month_starts_a_new_count_and_keeps_the_last_one() {
        let mut usage = usage();
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        usage
            .calibrate(NOV_10, 4 * price::MICROS)
            .expect("first reading");
        let (_, end) = month_bounds(NOV_10);
        usage.record(end + 60, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        assert_eq!(usage.month.month, "2026-12");
        assert_eq!(usage.month.total().data, 1);
        assert_eq!(usage.last_month.month, "2026-11");
        assert!(
            usage.points.is_empty(),
            "November's dashboard figures are let go"
        );
        // Two months on, the gap isn't shown as "last month".
        usage.roll(end + 70 * DAY);
        assert_eq!(usage.last_month, Month::default());
    }

    #[test]
    fn the_dashboard_figure_only_goes_up_within_a_month() {
        let mut usage = usage();
        usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        usage.calibrate(NOV_10, 3 * price::MICROS).expect("fine");
        assert_eq!(usage.points[0].irori, 2_000);
        let lower = usage
            .calibrate(NOV_10 + DAY, 2 * price::MICROS)
            .expect_err("lower than before");
        assert!(lower.contains("$3.00"), "{lower}");
    }

    #[test]
    fn what_is_stored_reads_back() {
        let mut usage = usage();
        usage.record(
            NOV_10,
            "VIN1",
            Endpoint::VehicleData,
            200,
            Why::Command {
                what: "Lock".into(),
                by: "an automation".into(),
            },
        );
        let month: Month =
            serde_json::from_value(serde_json::to_value(&usage.month).expect("json"))
                .expect("back");
        assert_eq!(month, usage.month);
        let calls: VecDeque<Call> =
            serde_json::from_value(serde_json::to_value(&usage.calls).expect("json"))
                .expect("back");
        assert_eq!(calls, usage.calls);
        // A full list of calls fits in one stored value.
        let mut full = usage.clone();
        for n in 0..CALLS_KEPT as u64 {
            full.record(
                NOV_10 + n,
                "5YJ3E1EA7KF000000",
                Endpoint::SignedCommand,
                200,
                Why::Command {
                    what: "Climate on".into(),
                    by: "an automation".into(),
                },
            );
        }
        let bytes = serde_json::to_vec(&full.calls).expect("json").len();
        assert!(bytes < irori_protocol::MAX_STORED_VALUE, "{bytes}");
    }
}
