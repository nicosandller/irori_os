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

use super::dest;
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
            copy_out(config, &settings, file, &mut status, now);
        }
        Err(error) => status.last_error = Some(error.clone()),
    }
    save_status(data, &status);
    result
}

/// Copies a fresh backup to each destination and keeps each within its retention. A place that
/// can't be reached is said in the status and tried again with the next backup; it doesn't
/// undo the backup that was made here.
fn copy_out(config: &Path, settings: &Settings, file: &Path, status: &mut Status, now: Timestamp) {
    let name = file_name(file);
    for destination in &settings.destination {
        let place = destination.name().to_owned();
        let state = status.destinations.entry(place.clone()).or_default();
        // Nothing leaves this machine readable (`settings::Settings::check`), whatever the
        // settings said when this backup was made.
        if !name.ends_with(".enc") {
            state.last_error = Some("this backup isn't encrypted, so it wasn't copied".to_owned());
            continue;
        }
        let secret = destination
            .secret_name()
            .and_then(|s| settings::secret(config, &s));
        match dest::put(destination, secret.as_deref(), file, &name) {
            Ok(()) => {
                state.last_ok = Some(now.to_string());
                state.last_error = None;
                status
                    .copies
                    .entry(name.clone())
                    .or_default()
                    .push(place.clone());
                if let Ok(there) = dest::list(destination, secret.as_deref()) {
                    for old in dest::expired_at(&there, settings.keep, settings.keep_days) {
                        let _ = dest::delete(destination, secret.as_deref(), &old);
                        if let Some(held) = status.copies.get_mut(&old) {
                            held.retain(|p| p != &place);
                        }
                    }
                }
            }
            Err(error) => state.last_error = Some(error),
        }
    }
}

/// Where this Irori's config and data are, for what runs without being handed them: an update
/// about to be installed. Set once at start.
pub static HOME: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();

/// Called as an update starts. Makes a backup first, unless that is turned off, and says why
/// when it couldn't: an update that can't be taken back is better not started.
pub async fn before_update() -> Result<(), String> {
    let Some((config, data)) = HOME.get().cloned() else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || {
        let settings = settings::load(&config);
        if !settings.before_update {
            return Ok(());
        }
        let mut key = None;
        let plan = plan_for(&config, &settings, &mut key)?;
        let into = dir(&data);
        super::create_named(&config, &data, &into, super::UPDATE_PREFIX, plan)
            .map_err(|e| format!("couldn't back up before updating: {e:#}"))?;
        super::prune_kind(&into, super::UPDATE_PREFIX, 3, None);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
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

    #[test]
    fn a_backup_is_copied_out_and_each_place_keeps_to_its_retention() {
        let tmp = tempfile::tempdir().expect("dir");
        let (config, data, place) = (
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("nas"),
        );
        std::fs::create_dir_all(&place).expect("dir");
        std::fs::create_dir_all(&config).expect("dir");
        std::fs::write(config.join("areas.toml"), "").expect("file");
        let settings = Settings {
            enabled: true,
            keep: 1,
            destination: vec![settings::Destination::Path {
                name: "nas".into(),
                path: place.clone(),
            }],
            ..Settings::default()
        };
        settings::save(&config, &settings).expect("saved");
        // An old copy at the place, and something that isn't ours.
        std::fs::write(place.join("irori-backup-20200101-000000.tar.gz.enc"), "old").expect("file");
        std::fs::write(place.join("notes.txt"), "mine").expect("file");

        let file = run_once(&config, &data).expect("backed up");
        let name = file_name(&file);
        assert!(name.ends_with(".tar.gz.enc"));
        assert!(place.join(&name).exists(), "copied out");
        assert!(
            !place
                .join("irori-backup-20200101-000000.tar.gz.enc")
                .exists(),
            "kept to its retention"
        );
        assert!(
            place.join("notes.txt").exists(),
            "what isn't ours is left alone"
        );
        let state = status(&data);
        assert_eq!(state.copies[&name], ["nas"]);
        assert!(
            state.destinations["nas"].last_ok.is_some()
                && state.destinations["nas"].last_error.is_none()
        );

        // A place that isn't there fails on its own, and the backup here still stands.
        std::fs::remove_dir_all(&place).expect("gone");
        let again = run_once(&config, &data).expect("backed up here");
        assert!(again.exists());
        assert!(
            status(&data).destinations["nas"]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("mounted"))
        );
    }
}
