use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The stretch of time a vendor's limit covers, in UTC: SwitchBot counts calls a day, Tesla bills
/// by the month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    Day,
    Month,
}

impl Period {
    /// Which one `now` (seconds since 1970, UTC) falls in, as a number that only goes up.
    fn index(self, now: u64) -> u64 {
        let days = now / 86_400;
        match self {
            Period::Day => days,
            Period::Month => {
                let (year, month) = year_month(days);
                year * 12 + (month - 1)
            }
        }
    }

    /// When the period with this index ends, in seconds since 1970.
    fn end(self, index: u64) -> u64 {
        match self {
            Period::Day => (index + 1) * 86_400,
            Period::Month => {
                let next = index + 1;
                days_from_civil(next / 12, next % 12 + 1) * 86_400
            }
        }
    }
}

/// How much a period allows, and how much of it is gone. Units are whatever the vendor counts:
/// calls for SwitchBot, thousandths of a dollar for Tesla. Kept as JSON in the extension's store
/// (`docs/specs/protocols.md` §5), so a restart doesn't hand out the day's calls again.
///
/// It counts only this extension's own spending. Another app on the same account (Home Assistant,
/// often) spends from the same allowance, which is why `limit` is a share a person sets and why
/// [`Budget::exhaust`] exists: the vendor saying "no more" wins over this count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    period: Period,
    limit: u64,
    spent: u64,
    /// Which period `spent` belongs to.
    index: u64,
}

impl Budget {
    pub fn new(period: Period, limit: u64, now: u64) -> Self {
        Self {
            period,
            limit,
            spent: 0,
            index: period.index(now),
        }
    }

    /// A budget restored from the store, with today's limit (a person may have changed it). A
    /// stored one for another kind of period starts afresh.
    pub fn restored(stored: Option<Budget>, period: Period, limit: u64, now: u64) -> Self {
        let mut budget = match stored {
            Some(stored) if stored.period == period => stored,
            _ => Self::new(period, limit, now),
        };
        budget.limit = limit;
        budget.roll(now);
        budget
    }

    /// Starts a new period's count when `now` is past the last one.
    pub fn roll(&mut self, now: u64) {
        let index = self.period.index(now);
        if index != self.index {
            self.index = index;
            self.spent = 0;
        }
    }

    /// Takes `cost` if it fits. Polling asks this way, so it stops before the limit.
    pub fn try_spend(&mut self, cost: u64, now: u64) -> bool {
        self.roll(now);
        if self.spent + cost > self.limit {
            return false;
        }
        self.spent += cost;
        true
    }

    /// Counts `cost` whether or not it fits. A command a person asked for still goes out, and
    /// what it cost still counts.
    pub fn spend(&mut self, cost: u64, now: u64) {
        self.roll(now);
        self.spent += cost;
    }

    /// The vendor said the allowance is used up, whatever this count says.
    pub fn exhaust(&mut self, now: u64) {
        self.roll(now);
        self.spent = self.spent.max(self.limit);
    }

    pub fn spent(&self) -> u64 {
        self.spent
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// What's left of this period's allowance.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.spent)
    }

    /// When this period's count starts again, in seconds since 1970.
    pub fn resets_at(&self) -> u64 {
        self.period.end(self.index)
    }

    /// How long to wait between rounds of polling so what's left lasts until the reset: each
    /// round costs `per_round`, and `reserve` is kept back for commands. Never less than `floor`,
    /// and when nothing is left, until the reset.
    pub fn pace(&self, now: u64, per_round: u64, reserve: u64, floor: Duration) -> Duration {
        let left = self.resets_at().saturating_sub(now).max(1);
        let usable = self.remaining().saturating_sub(reserve);
        let rounds = usable / per_round.max(1);
        if rounds == 0 {
            return Duration::from_secs(left).max(floor);
        }
        Duration::from_secs(left / rounds).max(floor)
    }
}

/// The year and month (1-12) of a day counted from 1970-01-01 (Howard Hinnant's algorithm).
fn year_month(days: u64) -> (u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month)
}

/// The day, counted from 1970-01-01, a month starts on.
fn days_from_civil(year: u64, month: u64) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-24T12:00:00Z.
    const NOON: u64 = 1_792_843_200;

    #[test]
    fn a_day_resets_at_midnight_utc() {
        let mut budget = Budget::new(Period::Day, 10, NOON);
        assert!(budget.try_spend(10, NOON));
        assert!(!budget.try_spend(1, NOON), "the day's allowance is gone");
        assert_eq!(budget.resets_at(), 1_792_886_400, "2026-10-25T00:00:00Z");
        assert!(budget.try_spend(1, budget.resets_at()), "a new day");
        assert_eq!(budget.spent(), 1);
    }

    #[test]
    fn a_month_resets_on_the_first() {
        let budget = Budget::new(Period::Month, 10_000, NOON);
        assert_eq!(budget.resets_at(), 1_793_491_200, "2026-11-01T00:00:00Z");
        let december = Budget::new(Period::Month, 1, 1_796_083_200 + 3_600); // 2026-12-01T01:00Z
        assert_eq!(december.resets_at(), 1_798_761_600, "2027-01-01T00:00:00Z");
    }

    #[test]
    fn a_command_counts_even_past_the_limit_and_the_vendor_can_end_the_day() {
        let mut budget = Budget::new(Period::Day, 5, NOON);
        budget.spend(7, NOON);
        assert_eq!(budget.remaining(), 0);
        let mut other = Budget::new(Period::Day, 5, NOON);
        other.exhaust(NOON);
        assert!(!other.try_spend(1, NOON));
    }

    #[test]
    fn polling_is_spread_over_what_is_left_of_the_day() {
        let budget = Budget::new(Period::Day, 10_000, NOON);
        // 12 hours left, 9 000 usable calls, 10 calls a round: 900 rounds, one every 48 s.
        let pace = budget.pace(NOON, 10, 1_000, Duration::from_secs(30));
        assert_eq!(pace, Duration::from_secs(48));
        // Never faster than the floor.
        let pace = budget.pace(NOON, 1, 0, Duration::from_secs(60));
        assert_eq!(pace, Duration::from_secs(60));
        // Nothing left: wait for midnight.
        let mut spent = budget.clone();
        spent.exhaust(NOON);
        assert_eq!(
            spent.pace(NOON, 1, 0, Duration::from_secs(1)),
            Duration::from_secs(43_200)
        );
    }

    #[test]
    fn a_restored_budget_keeps_its_count_but_takes_the_new_limit() {
        let mut stored = Budget::new(Period::Day, 100, NOON);
        stored.spend(40, NOON);
        let json = serde_json::to_value(&stored).expect("json");
        let back: Budget = serde_json::from_value(json).expect("json");
        let restored = Budget::restored(Some(back), Period::Day, 50, NOON + 60);
        assert_eq!((restored.spent(), restored.limit()), (40, 50));
        let yesterday = Budget::restored(Some(stored), Period::Day, 50, NOON + 86_400);
        assert_eq!(yesterday.spent(), 0);
    }
}
