//! Backups on a schedule, and the record of how they went (`docs/specs/backup.md` §4).
//!
//! The runner wakes every minute and asks one question: has the latest scheduled time passed
//! without a backup since? That answers a machine that was off at 03:00 (it is backed up when it
//! comes on), a changed schedule (it applies at once), and a failure (tried again, but not more
//! than every fifteen minutes).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::civil::Weekday;
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use super::settings::{self, Settings};
use super::{Plan, dir};

const STATUS: &str = "backup-status.json";
/// After a failure, how long before trying again.
const RETRY: i64 = 15 * 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DestinationState {
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub last_attempt: Option<String>,
    pub last_success: Option<String>,
    pub last_error: Option<String>,
    /// Backup file → the destinations that hold a copy.
    #[serde(default)]
    pub copies: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub destinations: BTreeMap<String, DestinationState>,
}

fn status_path(data: &Path) -> PathBuf {
    data.join(STATUS)
}

pub fn status(data: &Path) -> Status {
    std::fs::read(status_path(data))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_status(data: &Path, status: &Status) {
    if let Ok(bytes) = serde_json::to_vec_pretty(status) {
        let part = status_path(data).with_extension("json.part");
        if std::fs::write(&part, bytes).is_ok() {
            let _ = std::fs::rename(&part, status_path(data));
        }
    }
}

/// The plan a backup made now follows: sealed if the settings say so, with the key from
/// `secrets.toml` (made the first time).
pub fn plan_for<'a>(
    config: &Path,
    settings: &Settings,
    key: &'a mut Option<String>,
) -> Result<Plan<'a>, String> {
    if settings.encrypt {
        *key = Some(settings::key_or_make(config)?.0);
    }
    Ok(Plan {
        key: key.as_deref(),
        include_history: settings.include_history,
    })
}

/// Makes a backup the way the settings say, copies it out, prunes, and records how it went.
pub fn run_once(config: &Path, data: &Path) -> Result<PathBuf, String> {
    let settings = settings::load(config);
    let mut status = status(data);
    let now = Timestamp::now();
    status.last_attempt = Some(now.to_string());
    let mut key = None;
    let result = plan_for(config, &settings, &mut key).and_then(|plan| {
        super::create(config, data, &dir(data), plan).map_err(|e| format!("{e:#}"))
    });
    match &result {
        Ok(file) => {
            status.last_success = Some(now.to_string());
            status.last_error = None;
            super::prune(&dir(data), settings.keep, settings.keep_days);
            let have: Vec<String> = super::list(&dir(data))
                .into_iter()
                .map(|e| e.name)
                .collect();
            status.copies.retain(|name, _| have.contains(name));
            status.copies.entry(file_name(file)).or_default();
        }
        Err(error) => status.last_error = Some(error.clone()),
    }
    save_status(data, &status);
    result
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The latest scheduled moment at or before `now`, looking back a week.
pub fn latest_slot(now: &Zoned, at: (i8, i8), days: &[Weekday]) -> Option<Timestamp> {
    for back in 0..8 {
        let date = now.date().checked_sub(jiff::Span::new().days(back)).ok()?;
        if !days.is_empty() && !days.contains(&date.weekday()) {
            continue;
        }
        let slot = date
            .at(at.0, at.1, 0, 0)
            .to_zoned(now.time_zone().clone())
            .ok()?;
        if slot <= *now {
            return Some(slot.timestamp());
        }
    }
    None
}

/// Whether a backup is due: the latest slot has passed with no success since, and the last
/// failure wasn't just now.
pub fn due(settings: &Settings, status: &Status, now: &Zoned) -> bool {
    if !settings.enabled {
        return false;
    }
    let Some(at) = settings::parse_time(&settings.at) else {
        return false;
    };
    let days: Vec<Weekday> = settings
        .days
        .iter()
        .filter_map(|d| settings::weekday(d))
        .collect();
    let Some(slot) = latest_slot(now, at, &days) else {
        return false;
    };
    let last_success = status
        .last_success
        .as_deref()
        .and_then(|t| t.parse::<Timestamp>().ok());
    if last_success.is_some_and(|done| done >= slot) {
        return false;
    }
    let tried_lately = status
        .last_attempt
        .as_deref()
        .and_then(|t| t.parse::<Timestamp>().ok())
        .is_some_and(|tried| {
            status.last_error.is_some() && now.timestamp().as_second() - tried.as_second() < RETRY
        });
    !tried_lately
}

/// The home's time zone, or the machine's.
pub fn home_zone(config: &Path) -> jiff::tz::TimeZone {
    let mut store = irori_config::Store::new(config);
    let _ = store.reload();
    store
        .home()
        .time_zone
        .and_then(|name| jiff::tz::TimeZone::get(name.as_str()).ok())
        .unwrap_or_else(jiff::tz::TimeZone::system)
}

/// Runs for as long as Irori does.
pub async fn run(config: PathBuf, data: PathBuf) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let (c, d) = (config.clone(), data.clone());
        let ran = tokio::task::spawn_blocking(move || {
            let settings = settings::load(&c);
            if !settings.enabled {
                return None;
            }
            let now = Timestamp::now().to_zoned(home_zone(&c));
            if !due(&settings, &status(&d), &now) {
                return None;
            }
            Some(run_once(&c, &d))
        })
        .await;
        match ran {
            Ok(Some(Ok(file))) => tracing::info!(file = %file.display(), "backup written"),
            Ok(Some(Err(error))) => tracing::warn!(%error, "scheduled backup failed"),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "scheduled backup failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Zoned {
        text.parse::<Timestamp>()
            .expect("a time")
            .to_zoned(jiff::tz::TimeZone::UTC)
    }

    fn on() -> Settings {
        Settings {
            enabled: true,
            at: "03:00".into(),
            ..Settings::default()
        }
    }

    #[test]
    fn a_backup_is_due_once_the_slot_has_passed_and_not_before() {
        let settings = on();
        let none = Status::default();
        // 02:00 on the 5th: today's slot hasn't come, yesterday's has, and nothing was done.
        assert!(due(&settings, &none, &at("2026-03-05T02:00:00Z")));
        let done = Status {
            last_success: Some("2026-03-05T03:00:30Z".into()),
            ..Status::default()
        };
        assert!(!due(&settings, &done, &at("2026-03-05T09:00:00Z")));
        // The next morning it is due again.
        assert!(due(&settings, &done, &at("2026-03-06T03:00:10Z")));
        assert!(!due(
            &Settings::default(),
            &none,
            &at("2026-03-06T03:00:10Z")
        ));
    }

    #[test]
    fn only_the_chosen_days_count() {
        let settings = Settings {
            days: vec!["mon".into()],
            ..on()
        };
        // Monday the 2nd at 03:00 was the last slot; a success after it settles Tuesday to Sunday.
        let done = Status {
            last_success: Some("2026-03-02T03:05:00Z".into()),
            ..Status::default()
        };
        assert!(!due(&settings, &done, &at("2026-03-04T12:00:00Z")));
        assert!(due(&settings, &done, &at("2026-03-09T03:00:01Z")));
    }

    #[test]
    fn a_failure_waits_before_trying_again() {
        let settings = on();
        let failed = Status {
            last_attempt: Some("2026-03-05T03:00:00Z".into()),
            last_error: Some("disk full".into()),
            ..Status::default()
        };
        assert!(!due(&settings, &failed, &at("2026-03-05T03:10:00Z")));
        assert!(due(&settings, &failed, &at("2026-03-05T03:16:00Z")));
    }
}
