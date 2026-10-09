//! The last day of state changes, for the Devices page's per-entity table.
//!
//! The changes themselves live in `irori-recorder`, in the home's `irori.db`, so they are still
//! there after a restart. This module is the small front the server already had: it listens to
//! the core, asks the recorder for the last day when the page does, and for everything since a
//! timestamp when an engine does. An entity that leaves the home takes its history with it.

use std::path::Path;
#[cfg(test)]
use std::sync::Arc;

use irori_core::Event;
use irori_types::{EntityId, EntityState, Timestamp};
use jiff::ToSpan as _;
use tokio::sync::broadcast;

/// How far back the page looks. The database keeps longer; the page asks for a day.
const PAGE_WINDOW_HOURS: i64 = 24;

/// The diary the page and the engines read.
#[derive(Clone, Debug)]
pub struct History {
    recorder: irori_recorder::Recorder,
    /// Holds a throwaway directory open for tests that do not name a file. Dropped with the
    /// last clone, which is after the recorder thread has stopped.
    #[cfg(test)]
    _kept: Option<Arc<tempfile::TempDir>>,
}

impl History {
    /// Opens the diary in `path` (the home's `irori.db`) and keeps `retain_days` of it.
    pub fn open(path: &Path, retain_days: u32) -> anyhow::Result<Self> {
        Ok(Self {
            recorder: irori_recorder::Recorder::open(path, retain_days)?,
            #[cfg(test)]
            _kept: None,
        })
    }

    /// A diary in a directory that lives as long as this value does. For tests.
    #[cfg(test)]
    pub(crate) fn temporary() -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for history");
        let path = dir.path().join("irori.db");
        Self {
            recorder: irori_recorder::Recorder::open(&path, irori_recorder::DEFAULT_RETAIN_DAYS)
                .expect("a temporary recorder"),
            _kept: Some(Arc::new(dir)),
        }
    }

    /// Remembers a change. The recorder drops it if its queue is full, rather than making the
    /// event loop wait.
    pub fn record(&self, entity_id: EntityId, state: EntityState) {
        debug_assert_eq!(entity_id, state.entity_id);
        self.recorder.append(state);
    }

    /// Drops everything recorded for an entity: it has left the home, and a device removed from
    /// the home takes its history with it (`docs/specs/config.md` §3.2). Were it added again, its
    /// table starts from then.
    pub fn forget(&self, entity_id: &EntityId) {
        self.recorder.forget(entity_id);
    }

    /// Changes from the last day, oldest first. Empty when the entity has not changed in that
    /// day, including when the database has older rows the page does not ask for.
    pub fn for_entity(&self, entity_id: &EntityId) -> Vec<EntityState> {
        self.recorder.since(entity_id, day_ago())
    }

    /// Whether the database can be read and the last write succeeded.
    pub fn check(&self) -> bool {
        self.recorder.check()
    }
}

impl irori_core::HistorySource for History {
    fn changes(&self, entity_id: &EntityId, since: Timestamp) -> Vec<EntityState> {
        self.recorder.since(entity_id, since)
    }
}

fn day_ago() -> Timestamp {
    Timestamp::from_jiff(
        jiff::Timestamp::now()
            .checked_sub(PAGE_WINDOW_HOURS.hours())
            .expect("a day ago is a representable timestamp"),
    )
}

/// Feeds [`History`] from the core's events, for the life of the server. The channel closing is
/// the runtime shutting down. A listener that falls behind skips ahead to now, like the others
/// (`crates/irori/src/extensions.rs`): a missed sample is better than grinding through the
/// backlog. The recorder drops the same way when its own queue is full.
pub async fn record(history: History, mut events: broadcast::Receiver<Event>) {
    loop {
        match events.recv().await {
            Ok(Event::StateChanged {
                entity_id,
                new_state,
                ..
            }) => history.record(entity_id, *new_state),
            Ok(Event::EntityRemoved { entity_id }) => history.forget(&entity_id),
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_core::HistorySource as _;
    use irori_types::{Availability, Context, Origin, State, SwitchState};

    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a valid timestamp")
    }

    fn now() -> Timestamp {
        Timestamp::from_jiff(jiff::Timestamp::now())
    }

    fn later(base: Timestamp, millis: i64) -> Timestamp {
        Timestamp::from_jiff(
            base.as_jiff()
                .checked_add(millis.milliseconds())
                .expect("the test timestamp stays in range"),
        )
    }

    fn state(name: &str, on: bool, at: Timestamp) -> EntityState {
        EntityState {
            entity_id: name.parse().expect("a valid entity id"),
            availability: Availability::Available,
            state: Some(State::Switch(SwitchState { on })),
            attributes: Default::default(),
            last_changed: at,
            last_updated: at,
            last_reported: at,
            context: Context {
                id: "01K5B2Q9A1B2C3D4E5F6G7H8J9"
                    .parse()
                    .expect("a valid context id"),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    fn plug() -> EntityId {
        "switch.plug".parse().expect("a valid entity id")
    }

    #[test]
    fn changes_come_back_oldest_first() {
        let history = History::temporary();
        let first = now();
        history.record(plug(), state("switch.plug", true, first));
        history.record(plug(), state("switch.plug", false, later(first, 1)));
        let seen = history.for_entity(&plug());
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].state, Some(State::Switch(SwitchState { on: true })));
        assert_eq!(
            seen[1].state,
            Some(State::Switch(SwitchState { on: false }))
        );
    }

    #[test]
    fn an_entitys_history_is_its_own() {
        let history = History::temporary();
        let when = now();
        history.record(plug(), state("switch.plug", true, when));
        history.record(
            "switch.other".parse().expect("a valid entity id"),
            state("switch.other", false, later(when, 1)),
        );
        assert_eq!(history.for_entity(&plug()).len(), 1);
    }

    #[test]
    fn unknown_entities_have_no_history() {
        let history = History::temporary();
        history.record(plug(), state("switch.plug", true, now()));
        assert!(
            history
                .for_entity(&"switch.somewhere_else".parse().expect("a valid entity id"))
                .is_empty()
        );
    }

    #[test]
    fn the_page_asks_for_a_day_and_an_engine_can_ask_for_longer() {
        let history = History::temporary();
        let three_days_ago = Timestamp::from_jiff(
            jiff::Timestamp::now()
                .checked_sub((3 * PAGE_WINDOW_HOURS).hours())
                .expect("three days ago is in range"),
        );
        history.record(plug(), state("switch.plug", true, three_days_ago));
        history.record(plug(), state("switch.plug", false, now()));
        assert_eq!(
            history.for_entity(&plug()).len(),
            1,
            "the page is the last day"
        );
        assert_eq!(
            history.changes(&plug(), at("2020-01-01T00:00:00Z")).len(),
            2,
            "an engine's since reaches the older row the page does not show"
        );
    }

    #[test]
    fn a_change_is_still_there_after_the_file_is_reopened() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("irori.db");
        let when = now();
        {
            let history =
                History::open(&path, irori_recorder::DEFAULT_RETAIN_DAYS).expect("the diary opens");
            history.record(plug(), state("switch.plug", true, when));
            assert_eq!(history.for_entity(&plug()).len(), 1);
        }
        let history =
            History::open(&path, irori_recorder::DEFAULT_RETAIN_DAYS).expect("the diary reopens");
        let seen = history.for_entity(&plug());
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].last_updated, when);
        assert!(history.check());
    }
}
