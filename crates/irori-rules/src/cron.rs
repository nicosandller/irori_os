//! Five-field cron, read into the sets it stands for (`docs/specs/rules.md` §5.2).
//!
//! `minute hour day-of-month month day-of-week`. Each field is `*`, a number, a range (`1-5`),
//! a step (`*/15`, `10-40/10`), or several of those joined by commas. The day of the week is
//! 0–6 with Sunday as 0 (7 is Sunday too), or `SUN`–`SAT`; the month may be `JAN`–`DEC`.
//!
//! Plain Rust with no clock in it, so the page can check a cron as it's typed and say what it
//! means; when it next comes round is `clock`'s to work out.

/// A cron's fields, as the values each one allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSpec {
    pub minutes: Vec<u8>,
    pub hours: Vec<u8>,
    pub days: Vec<u8>,
    pub months: Vec<u8>,
    /// 0 is Sunday.
    pub weekdays: Vec<u8>,
    /// Whether the day of the month was written as anything but `*`.
    pub days_given: bool,
    /// Whether the day of the week was written as anything but `*`.
    pub weekdays_given: bool,
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

impl CronSpec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else {
            return Err(
                "cron needs 5 fields: minute hour day-of-month month day-of-week".to_owned(),
            );
        };
        let mut weekdays = field(weekday, "day of the week", 0, 7, &DAYS, 0)?;
        // 7 is Sunday, the same as 0.
        for day in &mut weekdays {
            if *day == 7 {
                *day = 0;
            }
        }
        weekdays.sort_unstable();
        weekdays.dedup();
        Ok(Self {
            minutes: field(minute, "minute", 0, 59, &[], 0)?,
            hours: field(hour, "hour", 0, 23, &[], 0)?,
            days: field(day, "day of the month", 1, 31, &[], 0)?,
            months: field(month, "month", 1, 12, &MONTHS, 1)?,
            weekdays,
            days_given: *day != "*",
            weekdays_given: *weekday != "*",
        })
    }

    /// Whether a date is one of this cron's days. As cron has always had it: when both the day
    /// of the month and the day of the week are given, either one is enough.
    pub fn on_day(&self, month: u8, day: u8, weekday_from_sunday: u8) -> bool {
        if !self.months.contains(&month) {
            return false;
        }
        let by_date = self.days.contains(&day);
        let by_weekday = self.weekdays.contains(&weekday_from_sunday);
        if self.days_given && self.weekdays_given {
            by_date || by_weekday
        } else {
            by_date && by_weekday
        }
    }
}

/// One field's values, in order. `names` are words that stand for numbers, the first of them
/// meaning `first_name`.
fn field(
    text: &str,
    what: &str,
    low: u8,
    high: u8,
    names: &[&str],
    first_name: u8,
) -> Result<Vec<u8>, String> {
    let number = |piece: &str| -> Result<u8, String> {
        if let Some(at) = names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(piece))
        {
            return Ok(first_name + u8::try_from(at).unwrap_or(0));
        }
        let value: u8 = piece
            .parse()
            .map_err(|_| format!("cron's {what} can't be {piece:?}"))?;
        if value < low || value > high {
            return Err(format!(
                "cron's {what} goes from {low} to {high}, not {value}"
            ));
        }
        Ok(value)
    };
    let mut values = Vec::new();
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u8 = step.parse().ok().filter(|step| *step > 0).ok_or_else(|| {
                    format!("cron's {what} has a step that isn't a number above 0")
                })?;
                (range, step)
            }
            None => (part, 1),
        };
        let (from, to) = if range == "*" {
            (low, high)
        } else if let Some((from, to)) = range.split_once('-') {
            (number(from)?, number(to)?)
        } else {
            let from = number(range)?;
            // `5/10` means from 5 on, every 10; a bare `5` is only 5.
            (from, if part.contains('/') { high } else { from })
        };
        if from > to {
            return Err(format!(
                "cron's {what} has a range that runs backwards: {range}"
            ));
        }
        values.extend((from..=to).step_by(usize::from(step)));
    }
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        return Err(format!("cron's {what} is empty"));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_of_field_reads() -> Result<(), String> {
        let spec = CronSpec::parse("*/15 7-9,18 1 JAN,jul MON-FRI")?;
        assert_eq!(spec.minutes, [0, 15, 30, 45]);
        assert_eq!(spec.hours, [7, 8, 9, 18]);
        assert_eq!(spec.days, [1]);
        assert_eq!(spec.months, [1, 7]);
        assert_eq!(spec.weekdays, [1, 2, 3, 4, 5]);
        Ok(())
    }

    #[test]
    fn seven_is_sunday_and_a_step_can_start_anywhere() -> Result<(), String> {
        let spec = CronSpec::parse("5/20 0 * * 7")?;
        assert_eq!(spec.minutes, [5, 25, 45]);
        assert_eq!(spec.weekdays, [0]);
        Ok(())
    }

    #[test]
    fn a_bad_field_says_which_and_why() {
        let why = |text| CronSpec::parse(text).expect_err("a bad cron");
        assert!(why("0 7 * *").contains("5 fields"));
        assert!(why("61 7 * * *").contains("minute goes from 0 to 59"));
        assert!(why("0 7 * * FUN").contains("day of the week"));
        assert!(why("0 9-7 * * *").contains("runs backwards"));
        assert!(why("*/0 7 * * *").contains("step"));
    }

    #[test]
    fn a_day_given_both_ways_needs_only_one() -> Result<(), String> {
        // The 13th, or any Friday.
        let either = CronSpec::parse("0 0 13 * FRI")?;
        assert!(either.on_day(3, 13, 2));
        assert!(either.on_day(3, 20, 5));
        assert!(!either.on_day(3, 21, 6));
        // Only weekdays given: the date is free.
        let weekdays = CronSpec::parse("0 0 * * MON")?;
        assert!(weekdays.on_day(3, 21, 1));
        assert!(!weekdays.on_day(3, 21, 2));
        Ok(())
    }
}
