//! Firmware a demo device can be told to install: an update is waiting from the start, flashing
//! takes half a minute and says how far it has got, and a newer version comes out a few minutes
//! after each install, so there's always one to try. A part of a gadget, not a gadget.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{
    Capabilities, Service, State, UpdateCapabilities, UpdateClass, UpdateState,
};

use super::Entity;

/// How long flashing takes.
const FLASHING: Duration = Duration::from_secs(30);

/// How long after an install the next version comes out.
const NEXT_RELEASE_AFTER: Duration = Duration::from_secs(300);

pub(super) struct Firmware {
    unique_id: &'static str,
    title: &'static str,
    /// Versions are `1.<minor>.0`.
    installed: u32,
    latest: u32,
    flashing_since: Option<Instant>,
    next_release: Option<Instant>,
    /// The progress last reported, so a tick that changes nothing says nothing.
    reported: Option<u32>,
}

impl Firmware {
    pub(super) fn new(unique_id: &'static str, title: &'static str, installed: u32) -> Self {
        Self {
            unique_id,
            title,
            installed,
            latest: installed + 1,
            flashing_since: None,
            next_release: None,
            reported: None,
        }
    }

    pub(super) fn entity(&self) -> Entity {
        Entity::named(
            self.unique_id,
            "Firmware",
            Capabilities::Update(UpdateCapabilities {
                device_class: Some(UpdateClass::Firmware),
                install: true,
                check: true,
            }),
        )
        .config()
    }

    pub(super) fn state(&self) -> (&'static str, State) {
        let flashing = self.flashing_since.is_some();
        let available = self.latest > self.installed;
        (
            self.unique_id,
            State::Update(UpdateState {
                available,
                installed_version: Some(format!("1.{}.0", self.installed)),
                latest_version: Some(format!("1.{}.0", self.latest)),
                in_progress: flashing,
                progress: flashing.then(|| f64::from(self.reported.unwrap_or(0))),
                title: Some(self.title.to_owned()),
                release_summary: available
                    .then(|| "Fixes a rare reboot and uses a little less power.".to_owned()),
                release_url: None,
                skipped_version: None,
                last_error: None,
            }),
        )
    }

    /// Whether `unique_id` is this firmware's entity.
    pub(super) fn is(&self, unique_id: &str) -> bool {
        unique_id == self.unique_id
    }

    /// Carries out an install or a check. `Err` for anything else, and for an install with
    /// nothing to install.
    pub(super) fn call(&mut self, service: &Service, now: Instant) -> Result<&'static str, String> {
        match service {
            Service::UpdateInstall if self.flashing_since.is_some() => {
                return Err("it's already installing".to_owned());
            }
            Service::UpdateInstall if self.latest == self.installed => {
                return Err("it's up to date".to_owned());
            }
            Service::UpdateInstall => {
                self.flashing_since = Some(now);
                self.reported = Some(0);
            }
            // Looking again brings the next version out at once.
            Service::UpdateCheck => {
                if self.next_release.take().is_some() {
                    self.latest = self.installed + 1;
                }
            }
            service => return Err(super::cannot(self.unique_id, service)),
        }
        Ok(self.unique_id)
    }

    /// Whether it changed by `now`: further along, finished, or a new version out.
    pub(super) fn tick(&mut self, now: Instant) -> bool {
        if let Some(since) = self.flashing_since {
            let elapsed = now.saturating_duration_since(since);
            if elapsed >= FLASHING {
                self.installed = self.latest;
                self.flashing_since = None;
                self.reported = None;
                self.next_release = Some(now + NEXT_RELEASE_AFTER);
                return true;
            }
            // In tenths: a report for every second would be thirty lines of history.
            let done = u32::try_from(elapsed.as_secs() * 10 / FLASHING.as_secs()).unwrap_or(9) * 10;
            let further = self.reported != Some(done);
            self.reported = Some(done);
            return further;
        }
        if self.next_release.is_some_and(|at| now >= at) {
            self.next_release = None;
            self.latest = self.installed + 1;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(firmware: &Firmware) -> UpdateState {
        match firmware.state().1 {
            State::Update(update) => update,
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn firmware_installs_in_half_a_minute_and_another_comes_out() {
        let now = Instant::now();
        let mut firmware = Firmware::new("lock-firmware", "Lock firmware", 4);
        assert!(update(&firmware).available);
        firmware.call(&Service::UpdateInstall, now).expect("starts");
        assert!(firmware.call(&Service::UpdateInstall, now).is_err());
        assert!(!firmware.tick(now + Duration::from_secs(1)), "still at 0%");
        assert!(firmware.tick(now + Duration::from_secs(15)));
        let halfway = update(&firmware);
        assert_eq!((halfway.in_progress, halfway.progress), (true, Some(50.0)));
        assert!(halfway.validate().is_ok());

        assert!(firmware.tick(now + FLASHING));
        let done = update(&firmware);
        assert!(!done.available && !done.in_progress);
        assert_eq!(done.installed_version.as_deref(), Some("1.5.0"));
        assert!(firmware.call(&Service::UpdateInstall, now).is_err());

        let later = now + FLASHING + NEXT_RELEASE_AFTER;
        assert!(firmware.tick(later));
        assert_eq!(update(&firmware).latest_version.as_deref(), Some("1.6.0"));
    }
}
