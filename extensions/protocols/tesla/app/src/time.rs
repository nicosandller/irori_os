//! Times as a person reads them: the viewer's own clock for when things happened, plain words
//! for how often and how long ago.

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `2026-11` → "November 2026".
pub fn month_name(month: &str) -> String {
    let (year, number) = month.split_once('-').unwrap_or(("", ""));
    let name = number
        .parse::<usize>()
        .ok()
        .and_then(|n| MONTHS.get(n.wrapping_sub(1)))
        .copied()
        .unwrap_or("");
    format!("{name} {year}").trim().to_owned()
}

/// `2026-11` → "November".
pub fn month_only(month: &str) -> String {
    month_name(month)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// `2026-11`, day 10 → "Nov 10".
pub fn day_label(month: &str, day: u8) -> String {
    let short: String = month_only(month).chars().take(3).collect();
    format!("{short} {day}")
}

fn date(at: u64) -> js_sys::Date {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at as f64 * 1000.0))
}

/// "14:05" today, "Nov 9, 14:05" another day, on the viewer's clock.
pub fn when(at: u64, now: u64) -> String {
    let then = date(at);
    let today = date(now);
    let clock = format!("{:02}:{:02}", then.get_hours(), then.get_minutes());
    let same_day = then.get_date() == today.get_date()
        && then.get_month() == today.get_month()
        && then.get_full_year() == today.get_full_year();
    if same_day {
        clock
    } else {
        let month = MONTHS
            .get(then.get_month() as usize)
            .map(|name| &name[..3])
            .unwrap_or("");
        format!("{month} {}, {clock}", then.get_date())
    }
}

/// "December 3" on the viewer's calendar.
pub fn date_label(at: u64) -> String {
    let then = date(at);
    let month = MONTHS.get(then.get_month() as usize).copied().unwrap_or("");
    format!("{month} {}", then.get_date())
}

/// "just now", "5 minutes ago", "3 days ago".
pub fn ago(at: u64, now: u64) -> String {
    let seconds = now.saturating_sub(at);
    let (n, unit) = match seconds {
        0..=89 => return "just now".to_owned(),
        90..=5_399 => (seconds.div_ceil(60).max(2), "minute"),
        5_400..=129_599 => ((seconds + 1_800) / 3_600, "hour"),
        _ => ((seconds + 43_200) / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// A gap in plain words: "every minute", "every 4 minutes", "every 1½ hours".
pub fn every(seconds: u64) -> String {
    match seconds {
        0..=89 => "every minute".to_owned(),
        90..=3_569 => format!("every {} minutes", (seconds + 30) / 60),
        _ => {
            // To the half hour.
            let halves = (seconds + 900) / 1_800;
            let (whole, half) = (halves / 2, halves % 2 == 1);
            match (whole, half) {
                (1, false) => "every hour".to_owned(),
                (whole, true) => format!("every {whole}½ hours"),
                (whole, false) => format!("every {whole} hours"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_read_in_plain_words() {
        assert_eq!(every(60), "every minute");
        assert_eq!(every(420), "every 7 minutes");
        assert_eq!(every(3_600), "every hour");
        assert_eq!(every(5_160), "every 1½ hours");
        assert_eq!(every(21_600), "every 6 hours");
        assert_eq!(month_name("2026-11"), "November 2026");
        assert_eq!(day_label("2026-11", 4), "Nov 4");
    }
}
