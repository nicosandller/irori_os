//! Installing updates one at a time per protocol.
//!
//! Flashing a Zigbee device takes from ten minutes to an hour and fills the network it shares
//! with every other device, so two at once make both slower and likelier to fail. The core
//! therefore sends a protocol one `update.install` at a time: the rest wait their turn here, and
//! the next goes when the one running ends, however it ends.
//!
//! This is only the bookkeeping, with the clock handed in: who is installing, who waits, and
//! when the one installing has gone quiet for too long. The core does the sending.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use irori_types::{Context, EntityId, ProtocolId, UpdateState};
use serde::Serialize;
use tokio::time::Instant;

/// How long a device has to say it started. One that reports nothing while it flashes is taken
/// to have failed after this, and the next in line goes.
pub(crate) const MUST_START_WITHIN: Duration = Duration::from_secs(120);

/// How long an install can go without a word before it's given up on. A Zigbee update can
/// honestly take an hour; this is well past that.
pub(crate) const QUIET_FOR_TOO_LONG: Duration = Duration::from_secs(90 * 60);

pub(crate) const NEVER_STARTED: &str = "It was asked to install the update and never started.";
pub(crate) const WENT_QUIET: &str = "The install stopped reporting and was given up on.";

/// An install waiting for its turn, as a page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueuedInstall {
    pub entity_id: EntityId,
    /// How many go first, the one installing now included.
    pub ahead: usize,
}

#[derive(Debug)]
struct Running {
    entity_id: EntityId,
    /// The version it had when it was asked, to tell an install that finished from one that
    /// never said it started.
    installed_before: Option<String>,
    started: bool,
    /// When it was sent, or last said something while installing.
    heard_at: Instant,
}

#[derive(Debug, Default)]
struct Line {
    running: Option<Running>,
    waiting: VecDeque<(EntityId, Context)>,
}

/// What asking for an install comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Nothing else is installing on this protocol: send it.
    Go,
    /// It waits, or was already installing or waiting.
    Waits,
}

/// Whether the install being watched needs anything done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Watch {
    /// Still going, or not yet late.
    Fine,
    /// Late: it failed, for this reason.
    Overdue(&'static str),
    /// It isn't the one installing any more; there's nothing left to watch.
    Over,
}

#[derive(Debug, Default)]
pub(crate) struct Installs {
    lines: HashMap<ProtocolId, Line>,
}

impl Installs {
    /// Someone asked `entity_id` to install. `installed` is the version it has now.
    pub fn ask(
        &mut self,
        protocol: &ProtocolId,
        entity_id: &EntityId,
        context: &Context,
        installed: Option<&str>,
        now: Instant,
    ) -> Asked {
        let line = self.lines.entry(protocol.clone()).or_default();
        let running = line.running.as_ref().map(|running| &running.entity_id);
        if running == Some(entity_id) || line.waiting.iter().any(|(id, _)| id == entity_id) {
            return Asked::Waits;
        }
        if running.is_some() {
            line.waiting.push_back((entity_id.clone(), context.clone()));
            return Asked::Waits;
        }
        line.running = Some(Running {
            entity_id: entity_id.clone(),
            installed_before: installed.map(str::to_owned),
            started: false,
            heard_at: now,
        });
        Asked::Go
    }

    /// An update entity's state changed. Returns its protocol when that ends the install
    /// running there: it finished, or it stopped.
    pub fn heard(
        &mut self,
        entity_id: &EntityId,
        state: Option<&UpdateState>,
        now: Instant,
    ) -> Option<ProtocolId> {
        let (protocol, running) = self.lines.iter_mut().find_map(|(protocol, line)| {
            line.running
                .as_mut()
                .filter(|running| running.entity_id == *entity_id)
                .map(|running| (protocol, running))
        })?;
        let state = state?;
        if state.in_progress {
            running.started = true;
            running.heard_at = now;
            return None;
        }
        let moved_on = state.installed_version.is_some()
            && state.installed_version != running.installed_before;
        (running.started || moved_on).then(|| protocol.clone())
    }

    /// `entity_id`'s install on `protocol` is over. Returns the next to send, which is now the
    /// one running; `installed` says what version an entity has. Nothing happens when
    /// `entity_id` isn't the one installing there, so being told twice is harmless.
    pub fn next(
        &mut self,
        protocol: &ProtocolId,
        entity_id: &EntityId,
        installed: impl Fn(&EntityId) -> Option<String>,
        now: Instant,
    ) -> Option<(EntityId, Context)> {
        let line = self.lines.get_mut(protocol)?;
        if line.running.as_ref().map(|running| &running.entity_id) != Some(entity_id) {
            return None;
        }
        line.running = None;
        let (entity_id, context) = line.waiting.pop_front()?;
        line.running = Some(Running {
            installed_before: installed(&entity_id),
            entity_id: entity_id.clone(),
            started: false,
            heard_at: now,
        });
        Some((entity_id, context))
    }

    /// Whether `entity_id`, sent to `protocol`, is taking too long.
    pub fn watch(&self, protocol: &ProtocolId, entity_id: &EntityId, now: Instant) -> Watch {
        let Some(running) = self
            .lines
            .get(protocol)
            .and_then(|line| line.running.as_ref())
            .filter(|running| running.entity_id == *entity_id)
        else {
            return Watch::Over;
        };
        let quiet = now.saturating_duration_since(running.heard_at);
        match running.started {
            false if quiet >= MUST_START_WITHIN => Watch::Overdue(NEVER_STARTED),
            true if quiet >= QUIET_FOR_TOO_LONG => Watch::Overdue(WENT_QUIET),
            _ => Watch::Fine,
        }
    }

    /// Takes `entity_id` out of the line it waits in. Returns whether it was waiting.
    pub fn leave(&mut self, entity_id: &EntityId) -> bool {
        let mut left = false;
        for line in self.lines.values_mut() {
            let before = line.waiting.len();
            line.waiting.retain(|(id, _)| id != entity_id);
            left |= line.waiting.len() != before;
        }
        left
    }

    /// Forgets everything about a protocol that's gone.
    pub fn forget(&mut self, protocol: &ProtocolId) -> bool {
        self.lines.remove(protocol).is_some()
    }

    /// Everything waiting, by entity.
    pub fn waiting(&self) -> Vec<QueuedInstall> {
        let mut waiting: Vec<QueuedInstall> = self
            .lines
            .values()
            .flat_map(|line| {
                let running = usize::from(line.running.is_some());
                line.waiting
                    .iter()
                    .enumerate()
                    .map(move |(place, (entity_id, _))| QueuedInstall {
                        entity_id: entity_id.clone(),
                        ahead: place + running,
                    })
            })
            .collect();
        waiting.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
        waiting
    }
}

#[cfg(test)]
mod tests {
    use irori_types::Origin;

    use super::*;

    fn id(text: &str) -> EntityId {
        text.parse().expect("an entity id")
    }

    fn context() -> Context {
        Context {
            id: "01K5B2Q9A1B2C3D4E5F6G7H8J9".parse().expect("a context id"),
            parent_id: None,
            origin: Origin::System,
        }
    }

    fn installing(progress: f64) -> UpdateState {
        UpdateState {
            available: true,
            installed_version: Some("1".into()),
            latest_version: Some("2".into()),
            in_progress: true,
            progress: Some(progress),
            ..UpdateState::default()
        }
    }

    #[test]
    fn a_second_install_on_the_same_protocol_waits_for_the_first() {
        let zigbee: ProtocolId = "zigbee".parse().expect("a protocol id");
        let esphome: ProtocolId = "esphome".parse().expect("a protocol id");
        let (plug, bulb, lamp) = (id("update.plug"), id("update.bulb"), id("update.lamp"));
        let now = Instant::now();
        let mut installs = Installs::default();
        let ask = |installs: &mut Installs, protocol, entity| {
            installs.ask(protocol, entity, &context(), Some("1"), now)
        };
        assert_eq!(ask(&mut installs, &zigbee, &plug), Asked::Go);
        assert_eq!(ask(&mut installs, &zigbee, &bulb), Asked::Waits);
        assert_eq!(
            ask(&mut installs, &zigbee, &bulb),
            Asked::Waits,
            "asked twice"
        );
        assert_eq!(
            ask(&mut installs, &esphome, &lamp),
            Asked::Go,
            "another protocol is another network"
        );
        assert_eq!(
            installs.waiting(),
            vec![QueuedInstall {
                entity_id: bulb.clone(),
                ahead: 1
            }]
        );

        assert_eq!(installs.heard(&plug, Some(&installing(10.0)), now), None);
        let done = UpdateState {
            available: false,
            installed_version: Some("2".into()),
            latest_version: Some("2".into()),
            ..UpdateState::default()
        };
        assert_eq!(
            installs.heard(&plug, Some(&done), now),
            Some(zigbee.clone())
        );
        assert_eq!(
            installs.next(&zigbee, &bulb, |_| None, now),
            None,
            "the bulb isn't the one installing"
        );
        let next = installs.next(&zigbee, &plug, |_| Some("1".into()), now);
        assert_eq!(next.map(|(entity, _)| entity), Some(bulb));
        assert!(installs.waiting().is_empty());
    }

    #[test]
    fn an_install_that_never_starts_or_goes_quiet_is_overdue() {
        let mqtt: ProtocolId = "mqtt".parse().expect("a protocol id");
        let plug = id("update.plug");
        let sent = Instant::now();
        let mut installs = Installs::default();
        installs.ask(&mqtt, &plug, &context(), Some("1"), sent);
        assert_eq!(
            installs.watch(&mqtt, &plug, sent + Duration::from_secs(60)),
            Watch::Fine
        );
        assert_eq!(
            installs.watch(&mqtt, &plug, sent + MUST_START_WITHIN),
            Watch::Overdue(NEVER_STARTED)
        );

        let started = sent + Duration::from_secs(30);
        installs.heard(&plug, Some(&installing(1.0)), started);
        assert_eq!(
            installs.watch(&mqtt, &plug, started + Duration::from_secs(3600)),
            Watch::Fine,
            "an hour is an honest Zigbee update"
        );
        assert_eq!(
            installs.watch(&mqtt, &plug, started + QUIET_FOR_TOO_LONG),
            Watch::Overdue(WENT_QUIET)
        );
        installs.next(&mqtt, &plug, |_| None, started);
        assert_eq!(installs.watch(&mqtt, &plug, started), Watch::Over);
    }

    #[test]
    fn one_that_reports_no_progress_ends_when_its_version_changes() {
        let mqtt: ProtocolId = "mqtt".parse().expect("a protocol id");
        let plug = id("update.plug");
        let now = Instant::now();
        let mut installs = Installs::default();
        installs.ask(&mqtt, &plug, &context(), Some("1"), now);
        let same = UpdateState {
            available: true,
            installed_version: Some("1".into()),
            latest_version: Some("2".into()),
            ..UpdateState::default()
        };
        assert_eq!(installs.heard(&plug, Some(&same), now), None);
        let newer = UpdateState {
            available: false,
            installed_version: Some("2".into()),
            ..same
        };
        assert_eq!(installs.heard(&plug, Some(&newer), now), Some(mqtt));
    }
}
