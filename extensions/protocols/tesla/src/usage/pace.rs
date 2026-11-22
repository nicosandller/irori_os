//! How the month is going, and how often reading may happen so it ends inside the allowance.
//!
//! [`Outlook`] is the month as of now: what Irori spent, what other apps on the account spent
//! (from the dashboard figures the person entered, carried forward at their rate), and where the
//! account will be at the end of the month at the rates so far. [`plan`] turns that into gaps:
//! how often to check which cars are awake, how often to read a car that's doing something, and
//! how often to read an energy site.
//!
//! What's left for reading is the allowance, less what's spent, less what other apps will spend
//! by the month's end, less a reserve for commands and wakes, which a person asks for and which
//! always go out. That's shared between the three kinds of reading, and each gets the gap that
//! makes its share last; never shorter than the polling mode's own gap.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::price::{Category, Endpoint, Micros, Prices};
use super::{Month, Point, month_bounds};

/// How often the person wants the cars read. Every mode stays inside the allowance; they differ
/// in how quick to be when there's money to spare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Nothing is read on its own: only Refresh and commands.
    Off,
    /// Cars are read only while charging or driving, every ten minutes.
    Saving,
    /// As often as the allowance allows, from every two minutes.
    Balanced,
    /// From every minute while a car is doing something.
    Responsive,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Off, Mode::Saving, Mode::Balanced, Mode::Responsive];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Off => "Off",
            Mode::Saving => "Saving",
            Mode::Balanced => "Balanced",
            Mode::Responsive => "Responsive",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.label() == label)
    }

    /// The shortest gaps this mode allows: checking which cars are awake (when that's free and
    /// when it's billed), reading a busy car, and reading an energy site.
    fn floors(self) -> Option<Floors> {
        let minutes = |m: u64| Duration::from_secs(m * 60);
        match self {
            Mode::Off => None,
            Mode::Saving => Some(Floors {
                state_free: minutes(10),
                state_billed: minutes(30),
                car: minutes(10),
                site: minutes(15),
            }),
            Mode::Balanced => Some(Floors {
                state_free: minutes(2),
                state_billed: minutes(15),
                car: minutes(2),
                site: minutes(5),
            }),
            Mode::Responsive => Some(Floors {
                state_free: minutes(1),
                state_billed: minutes(5),
                car: minutes(1),
                site: minutes(1),
            }),
        }
    }
}

struct Floors {
    state_free: Duration,
    state_billed: Duration,
    car: Duration,
    site: Duration,
}

/// The longest any gap gets: a reading at least every few hours, so the page never looks dead
/// while there's anything left at all.
pub const LONGEST_GAP: Duration = Duration::from_secs(6 * 3600);

/// A site's day so far is read once every this many readings of its power flows, and counted
/// in each reading's cost.
pub const HISTORY_EVERY: u32 = 6;

/// The share of the month a car is assumed to be doing something (charging, driving, being
/// warmed) until Irori has watched the cars long enough to say ([`Activity`]).
const ASSUMED_ACTIVE: f64 = 0.15;

/// The month, as of `now`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Outlook {
    pub now: u64,
    pub month_start: u64,
    pub month_end: u64,
    pub allowance: Micros,
    /// What Irori spent this month.
    pub irori: Micros,
    /// Of that, on commands and wakes.
    pub irori_commands: Micros,
    /// What other apps spent this month, as far as the dashboard figures say: the last figure,
    /// carried forward at their rate. `None` with no figure this month.
    pub others: Option<Micros>,
    /// Their rate, per day.
    pub others_per_day: Micros,
    /// When the last dashboard figure was entered.
    pub checked_at: Option<u64>,
    /// Where the whole account ends the month at the rates so far.
    pub projected: Micros,
    /// When the account runs past the allowance at those rates, if it does this month.
    pub runs_out_at: Option<u64>,
}

impl Outlook {
    pub fn new(
        now: u64,
        month: &Month,
        prices: Prices,
        points: &[Point],
        allowance: Micros,
    ) -> Self {
        let (month_start, month_end) = month_bounds(now);
        let total = month.total();
        let irori = total.cost(prices);
        let irori_commands = super::Counts {
            data: 0,
            free: 0,
            ..total
        }
        .cost(prices);
        let elapsed = now.saturating_sub(month_start).max(3_600);
        let rest = month_end.saturating_sub(now);

        let (others, others_per_second, checked_at) = others(points, month_start, now);
        let others_now = others.unwrap_or(0);
        let irori_per_second = irori as f64 / elapsed as f64;
        let spent_now = irori.saturating_add(others_now);
        let per_second = irori_per_second + others_per_second;
        let projected = spent_now.saturating_add((per_second * rest as f64) as Micros);
        let runs_out_at = (projected > allowance).then(|| {
            if spent_now >= allowance || per_second <= 0.0 {
                now
            } else {
                now + ((allowance - spent_now) as f64 / per_second) as u64
            }
        });
        Self {
            now,
            month_start,
            month_end,
            allowance,
            irori,
            irori_commands,
            others,
            others_per_day: (others_per_second * 86_400.0) as Micros,
            checked_at,
            projected,
            runs_out_at,
        }
    }

    /// What the whole account has spent so far, as far as is known.
    pub fn account(&self) -> Micros {
        self.irori.saturating_add(self.others.unwrap_or(0))
    }

    /// Whether the month is on course to end past the allowance.
    pub fn over(&self) -> bool {
        self.projected > self.allowance
    }

    /// What reading can still spend this month: the allowance, less what's spent, less what other
    /// apps will spend by the month's end, less a reserve for commands and wakes.
    pub fn for_reading(&self) -> Micros {
        let rest = self.month_end.saturating_sub(self.now) as f64;
        let others_rest = (self.others_per_day as f64 / 86_400.0 * rest) as Micros;
        let left = self
            .allowance
            .saturating_sub(self.account())
            .saturating_sub(others_rest);
        let elapsed = self.now.saturating_sub(self.month_start).max(3_600) as f64;
        let commands_rest = (self.irori_commands as f64 / elapsed * rest) as Micros;
        let reserve = commands_rest.max(left * 15 / 100);
        left.saturating_sub(reserve)
    }
}

/// Other apps' spend now, their rate per second, and when the last figure was entered.
fn others(points: &[Point], month_start: u64, now: u64) -> (Option<Micros>, f64, Option<u64>) {
    let points = points.iter().filter(|point| point.at >= month_start);
    let Some(last) = points.clone().next_back() else {
        return (None, 0.0, None);
    };
    let theirs = |point: &Point| point.account.saturating_sub(point.irori);
    let others_then = theirs(last);
    // Their rate between the last two figures when they're far enough apart to say something,
    // else since the month began.
    let earlier = points
        .rev()
        .nth(1)
        .filter(|earlier| last.at - earlier.at >= 6 * 3_600);
    let rate = match earlier {
        Some(earlier) => {
            theirs(last).saturating_sub(theirs(earlier)) as f64 / (last.at - earlier.at) as f64
        }
        None => {
            let since = last.at.saturating_sub(month_start);
            if since >= 3_600 {
                others_then as f64 / since as f64
            } else {
                0.0
            }
        }
    };
    let now_estimate = others_then + (rate * now.saturating_sub(last.at) as f64) as Micros;
    (Some(now_estimate), rate, Some(last.at))
}

/// How often to read, as gaps. `None` means not at all on a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Checking which cars are awake: one call covers them all.
    pub state: Option<Duration>,
    /// Reading a car that's doing something.
    pub car: Option<Duration>,
    /// Reading an energy site.
    pub site: Option<Duration>,
    /// Nothing is left for reading this month: readings come from Refresh and commands only.
    pub starved: bool,
}

/// What's being read: how many cars and sites, and how the cars spend their time, from
/// [`Activity`] once it has seen enough.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub cars: usize,
    pub sites: usize,
    pub shares: Option<Shares>,
}

/// How much of the time the cars are busy (read as often as the plan allows) and awake but idle
/// (read about hourly), as fractions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shares {
    pub busy: f64,
    pub idle: f64,
}

/// What a car was doing when Irori last looked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    Busy,
    Idle,
    Asleep,
}

/// How the cars have spent their time since Irori started, summed over the cars: each stretch
/// between two looks at a car counts as what it was doing at the first.
#[derive(Debug, Clone, Default)]
pub struct Activity {
    seconds: [f64; 3],
    last: std::collections::BTreeMap<String, (u64, Doing)>,
}

/// How much has to be seen before [`Activity`] is trusted over the assumed figures.
const SEEN_ENOUGH: f64 = 6.0 * 3_600.0;

/// An awake car that isn't doing anything, as a share of the time, until there's a figure.
const ASSUMED_IDLE: f64 = 0.10;

impl Activity {
    pub fn saw(&mut self, car: &str, doing: Doing, now: u64) {
        if let Some((at, was)) = self.last.insert(car.to_owned(), (now, doing)) {
            self.seconds[was as usize] += now.saturating_sub(at) as f64;
        }
    }

    pub fn shares(&self) -> Option<Shares> {
        let total: f64 = self.seconds.iter().sum();
        (total >= SEEN_ENOUGH).then(|| Shares {
            busy: self.seconds[Doing::Busy as usize] / total,
            idle: self.seconds[Doing::Idle as usize] / total,
        })
    }
}

pub fn plan(outlook: &Outlook, mode: Mode, prices: Prices, reading: Reading) -> Plan {
    let Some(floors) = mode.floors() else {
        return Plan {
            state: None,
            car: None,
            site: None,
            starved: false,
        };
    };
    let rest = outlook.month_end.saturating_sub(outlook.now).max(60) as f64;
    let budget = outlook.for_reading() as f64;
    let state_price = prices.call(Endpoint::Vehicles);
    // One round reads every car.
    let car_price = prices.of(Category::Data) * reading.cars as Micros;
    let site_price = (prices.call(Endpoint::LiveStatus)
        + prices.call(Endpoint::EnergyHistory) / Micros::from(HISTORY_EVERY))
        * reading.sites as Micros;

    // Shares of what's left, among the kinds of reading that cost something.
    let state_weight = if reading.cars > 0 && state_price > 0 {
        0.2
    } else {
        0.0
    };
    let site_weight = if site_price > 0 { 0.3 } else { 0.0 };
    let car_weight = if reading.cars > 0 { 0.5 } else { 0.0 };
    let weights = state_weight + site_weight + car_weight;
    let share = |weight: f64| {
        if weights > 0.0 {
            budget * weight / weights
        } else {
            0.0
        }
    };

    // The gap that makes `money` last the rest of the month at `price` a round, for a reader
    // that's at work `busy` of the time.
    let gap = |money: f64, price: Micros, busy: f64, floor: Duration| -> Option<Duration> {
        if price == 0 {
            return Some(floor);
        }
        let rounds = money / price as f64;
        if rounds < 1.0 {
            return None;
        }
        let seconds = rest * busy / rounds;
        Some(Duration::from_secs_f64(seconds).clamp(floor, LONGEST_GAP.max(floor)))
    };

    let starved = budget <= 0.0;
    let shares = reading.shares.unwrap_or(Shares {
        busy: ASSUMED_ACTIVE,
        idle: ASSUMED_IDLE,
    });
    let active = shares.busy.clamp(0.02, 1.0);
    // An awake, idle car is read about hourly whatever the plan: that comes off the cars' share
    // first, at the most it could be (once an hour).
    let idle_reads =
        prices.of(Category::Data) as f64 * reading.cars as f64 * shares.idle.clamp(0.0, 1.0) * rest
            / 3_600.0;
    let state_floor = if state_price > 0 {
        floors.state_billed
    } else {
        floors.state_free
    };
    let car_floor = if mode == Mode::Saving {
        floors.car.max(Duration::from_secs(600))
    } else {
        floors.car
    };
    Plan {
        state: (reading.cars > 0)
            .then(|| gap(share(state_weight), state_price, 1.0, state_floor))
            .flatten()
            .or((reading.cars > 0 && starved).then_some(LONGEST_GAP)),
        car: (reading.cars > 0)
            .then(|| {
                gap(
                    (share(car_weight) - idle_reads).max(0.0),
                    car_price,
                    active,
                    car_floor,
                )
            })
            .flatten()
            // The idle reads may have taken the cars' whole share while money is left: read
            // rarely rather than not at all. Only a month with nothing left stops the cars.
            .or((reading.cars > 0 && !starved).then_some(LONGEST_GAP)),
        site: (reading.sites > 0)
            .then(|| gap(share(site_weight), site_price, 1.0, floors.site))
            .flatten()
            .or((reading.sites > 0 && starved).then_some(LONGEST_GAP)),
        starved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::tests::{NOV_10, usage};
    use crate::usage::{Why, price::MICROS};

    const DAY: u64 = 86_400;

    fn reading(cars: usize, sites: usize) -> Reading {
        Reading {
            cars,
            sites,
            shares: None,
        }
    }

    #[test]
    fn with_no_dashboard_figure_the_outlook_is_irori_alone() {
        let mut usage = usage();
        for _ in 0..450 {
            usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        }
        let outlook = usage.outlook(NOV_10);
        assert_eq!(outlook.irori, 900_000, "$0.90 in 9.5 days");
        assert_eq!(outlook.others, None);
        // 9.5 days in, 20.5 to go: $0.90 becomes about $2.84.
        assert!(
            (2_800_000..2_900_000).contains(&outlook.projected),
            "{}",
            outlook.projected
        );
        assert!(!outlook.over());
        assert_eq!(outlook.runs_out_at, None);
    }

    #[test]
    fn home_assistant_spending_alongside_shows_up_through_the_dashboard() {
        let mut usage = usage();
        for _ in 0..100 {
            usage.record(
                NOV_10 - 5 * DAY,
                "VIN1",
                Endpoint::VehicleData,
                200,
                Why::Schedule,
            );
        }
        // The dashboard said $4.20 on the 5th, while Irori had counted $0.20: $4.00 is others'.
        usage
            .calibrate(NOV_10 - 5 * DAY, 4_200_000)
            .expect("a figure");
        // On the 10th it said $7.20 with Irori still at $0.20: others spent $3.00 in five days.
        usage.calibrate(NOV_10, 7_200_000).expect("a figure");
        let outlook = usage.outlook(NOV_10 + DAY);
        assert_eq!(outlook.others_per_day, 600_000, "$0.60 a day");
        assert_eq!(outlook.others, Some(7_600_000), "carried forward a day");
        assert!(outlook.over(), "{} is past $10", outlook.projected);
        let runs_out = outlook.runs_out_at.expect("it runs out");
        assert!(
            runs_out > NOV_10 + DAY && runs_out < NOV_10 + 6 * DAY,
            "{runs_out}"
        );
        assert_eq!(outlook.for_reading(), 0, "nothing is left for reading");
        let plan = plan(&outlook, Mode::Balanced, usage.prices, reading(1, 0));
        assert!(plan.starved);
        assert_eq!(plan.car, None, "the car isn't read on a schedule");
        assert_eq!(
            plan.state,
            Some(LONGEST_GAP),
            "but it's still looked in on, rarely"
        );
    }

    #[test]
    fn balanced_reads_a_busy_car_as_often_as_the_month_allows() {
        let usage = usage();
        let outlook = usage.outlook(NOV_10);
        let plan = plan(&outlook, Mode::Balanced, usage.prices, reading(1, 1));
        // $10 with nothing spent: 85% of it for reading, a car's half of that over 20.5 days
        // of which it's busy 15%: every couple of minutes is fine.
        let car = plan.car.expect("read");
        assert!(
            car >= Duration::from_secs(120),
            "never under the mode's floor: {car:?}"
        );
        assert!(car < Duration::from_secs(300), "{car:?}");
        let state = plan.state.expect("checked");
        assert!(
            state >= Duration::from_secs(15 * 60),
            "billed, so at most every 15: {state:?}"
        );
        let site = plan.site.expect("read");
        assert!(site >= Duration::from_secs(5 * 60), "{site:?}");
        assert!(!plan.starved);
    }

    #[test]
    fn the_month_ends_inside_the_allowance_when_the_plan_is_followed() {
        // Others spend $0.20 a day; Irori follows the plan for the rest of the month for one,
        // two and three cars, each busy 15% of the time and awake but idle another 10% (read
        // hourly then), and checks state and the site on their gaps.
        for cars in 1..=3u64 {
            let mut usage = usage();
            let (start, end) = month_bounds(NOV_10);
            usage.calibrate(start + DAY, 200_000).expect("a figure");
            usage.calibrate(NOV_10, 1_900_000).expect("a figure");
            let mut now = NOV_10;
            let mut account_others = 1_900_000u64;
            while now < end {
                let outlook = usage.outlook(now);
                let plan = plan(
                    &outlook,
                    Mode::Responsive,
                    usage.prices,
                    reading(cars as usize, 1),
                );
                let hour = 3_600;
                let per_hour = |gap: Option<Duration>| {
                    gap.map_or(0, |gap| (hour as f64 / gap.as_secs_f64()).round() as u64)
                };
                for state in 0..per_hour(plan.state) {
                    let _ = state;
                    usage.record(now, "account", Endpoint::Vehicles, 200, Why::Schedule);
                }
                for car in 0..cars {
                    let vin = format!("VIN{car}");
                    // Busy in 3 hours out of 20, idle and awake in 2.
                    match (now / hour + car * 7) % 20 {
                        0..3 => {
                            for _ in 0..per_hour(plan.car) {
                                usage.record(now, &vin, Endpoint::VehicleData, 200, Why::Schedule);
                            }
                        }
                        3..5 if plan.car.is_some() => {
                            usage.record(now, &vin, Endpoint::VehicleData, 200, Why::Schedule);
                        }
                        _ => {}
                    }
                }
                for _ in 0..per_hour(plan.site) {
                    usage.record(now, "site:1", Endpoint::LiveStatus, 200, Why::Schedule);
                }
                now += hour;
                account_others += 200_000 / 24;
            }
            let total = usage.spent() + account_others;
            assert!(
                total <= 10 * MICROS,
                "{cars} cars: the month ended at {total}"
            );
            assert!(
                total >= 6 * MICROS,
                "{cars} cars: and used most of it: {total}"
            );
        }
    }

    #[test]
    fn two_cars_are_read_half_as_often_as_one() {
        // A tight month, so neither hits the mode's shortest gap: $7 of $10 spent already.
        let mut usage = usage();
        for _ in 0..3_500 {
            usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        }
        let outlook = usage.outlook(NOV_10);
        let gap = |cars| {
            plan(&outlook, Mode::Balanced, usage.prices, reading(cars, 0))
                .car
                .expect("read")
                .as_secs_f64()
        };
        let (one, two) = (gap(1), gap(2));
        assert!(two > one * 1.8, "one car every {one} s, two every {two} s");
    }

    /// Cars that sit awake a lot, late in a tight month: the idle reads budgeted can take the
    /// cars' whole share. With money still left they're read rarely, not dropped.
    #[test]
    fn cars_are_still_read_when_idle_reads_take_their_whole_share() {
        let mut usage = usage();
        for _ in 0..3_500 {
            usage.record(NOV_10, "VIN1", Endpoint::VehicleData, 200, Why::Schedule);
        }
        let outlook = usage.outlook(NOV_10);
        let plan = plan(
            &outlook,
            Mode::Balanced,
            usage.prices,
            Reading {
                cars: 3,
                sites: 0,
                shares: Some(Shares {
                    busy: 0.1,
                    idle: 0.9,
                }),
            },
        );
        assert!(!plan.starved, "there's money left");
        assert_eq!(plan.car, Some(LONGEST_GAP));
    }

    #[test]
    fn the_cars_own_time_replaces_the_assumed_shares_once_there_is_enough() {
        let mut activity = Activity::default();
        activity.saw("A", Doing::Busy, 0);
        activity.saw("A", Doing::Asleep, 3_600);
        assert_eq!(activity.shares(), None, "an hour isn't enough");
        activity.saw("A", Doing::Idle, 3_600 * 8);
        activity.saw("A", Doing::Asleep, 3_600 * 10);
        let shares = activity.shares().expect("ten hours seen");
        assert!((shares.busy - 0.1).abs() < 1e-9, "{shares:?}");
        assert!((shares.idle - 0.2).abs() < 1e-9, "{shares:?}");

        // A car busier than assumed is read less often, so the month still lasts.
        let usage = usage();
        let outlook = usage.outlook(NOV_10);
        let gap = |shares| {
            plan(
                &outlook,
                Mode::Balanced,
                usage.prices,
                Reading {
                    cars: 1,
                    sites: 0,
                    shares,
                },
            )
            .car
            .expect("read")
        };
        let busy = Some(Shares {
            busy: 0.6,
            idle: 0.1,
        });
        assert!(gap(busy) > gap(None));
    }

    #[test]
    fn off_reads_nothing_and_saving_is_slow() {
        let usage = usage();
        let outlook = usage.outlook(NOV_10);
        let off = plan(&outlook, Mode::Off, usage.prices, reading(1, 1));
        assert_eq!((off.state, off.car, off.site), (None, None, None));
        let saving = plan(&outlook, Mode::Saving, usage.prices, reading(1, 1));
        assert!(saving.car.expect("read") >= Duration::from_secs(600));
        assert!(saving.state.expect("checked") >= Duration::from_secs(1_800));
    }

    #[test]
    fn modes_read_back_from_their_labels() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_label(mode.label()), Some(mode));
        }
    }
}
