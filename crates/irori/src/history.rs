//! The last day of state changes, kept in memory for the Devices page's per-entity table.
//!
//! "Last 24 hours" can only honestly mean what this process has seen: the real recorder, the
//! one that survives restarts and keeps the long view, is `irori-recorder` (M1.3, ROADMAP §2.1).
//! Until then, every [`Event::StateChanged`] the core publishes is remembered here, pruned to a
//! day, and served to the page's expandable table.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use irori_core::Event;
use irori_types::{EntityId, EntityState};
use tokio::sync::broadcast;

/// How long a change stays in the table.
const WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// The most changes kept per entity: a sensor that starts reporting a different value every
/// second would otherwise hold ~86k entries a day, which is the page's little table, not the
/// recorder's long view.
const MAX_PER_ENTITY: usize = 2_000;

/// One change: the state it became, and when this process stored it. The state carries its own
/// timestamps; `at` is only for the day-long shelf life.
#[derive(Debug, Clone)]
struct Entry {
    at: Instant,
    state: EntityState,
}

/// The last day of changes, one queue per entity, oldest at the front.
#[derive(Debug, Clone, Default)]
pub struct History(Arc<Mutex<BTreeMap<EntityId, VecDeque<Entry>>>>);

impl History {
    /// Remembers a change. Once the queue is a day long, the oldest go before the newest arrive.
    pub fn record(&self, entity_id: EntityId, state: EntityState) {
        let now = Instant::now();
        let mut by_entity = self.0.lock().expect("history not poisoned");
        let entries = by_entity.entry(entity_id).or_default();
        // A server that started minutes ago has nothing a day old to forget; `checked_sub` is
        // `None` until it has been up a whole day, when `None` would mean "forget everything".
        prune(entries, now.checked_sub(WINDOW));
        entries.push_back(Entry { at: now, state });
        while entries.len() > MAX_PER_ENTITY {
            entries.pop_front();
        }
    }

    /// Everything recorded for an entity within the last day, oldest first. Empty when the
    /// entity is known but has changed nothing since the server started.
    pub fn for_entity(&self, entity_id: &EntityId) -> Vec<EntityState> {
        let by_entity = self.0.lock().expect("history not poisoned");
        by_entity
            .get(entity_id)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.at.elapsed() <= WINDOW)
                    .map(|entry| entry.state.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Drops everything recorded at or before `cutoff`. Nothing happens without a cutoff: a server
/// that started minutes ago has nothing a day old to forget.
fn prune(entries: &mut VecDeque<Entry>, cutoff: Option<Instant>) {
    let Some(cutoff) = cutoff else { return };
    while entries.front().is_some_and(|entry| entry.at <= cutoff) {
        entries.pop_front();
    }
}

/// Feeds [`History`] from the core's events, for the life of the server. The channel closing is
/// the runtime shutting down. A listener that falls behind skips ahead to now, like the others
/// (`crates/irori/src/extensions.rs`); the recorder would rather miss the oldest changes than
/// grind through them.
pub async fn record(history: History, mut events: broadcast::Receiver<Event>) {
    loop {
        match events.recv().await {
            Ok(Event::StateChanged {
                entity_id,
                new_state,
                ..
            }) => history.record(entity_id, *new_state),
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{Availability, Context, Origin, State, SwitchState, Timestamp};

    fn state(on: bool, at: &str) -> EntityState {
        let at: Timestamp = at.parse().expect("a valid timestamp");
        EntityState {
            entity_id: "switch.plug".parse().expect("a valid entity id"),
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
        let history = History::default();
        history.record(plug(), state(true, "2026-09-16T10:00:00Z"));
        history.record(plug(), state(false, "2026-09-16T10:00:01Z"));
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
        let history = History::default();
        history.record(plug(), state(true, "2026-09-16T10:00:00Z"));
        history.record(
            "switch.other".parse().expect("a valid entity id"),
            state(false, "2026-09-16T10:00:01Z"),
        );
        assert_eq!(history.for_entity(&plug()).len(), 1);
    }

    #[test]
    fn unknown_entities_have_no_history() {
        let history = History::default();
        history.record(plug(), state(true, "2026-09-16T10:00:00Z"));
        assert!(
            history
                .for_entity(&"switch.somewhere_else".parse().expect("a valid entity id"))
                .is_empty()
        );
    }

    #[test]
    fn the_cap_drops_the_oldest_first() {
        let history = History::default();
        for _ in 0..(MAX_PER_ENTITY + 10) {
            history.record(plug(), state(true, "2026-09-16T10:00:00Z"));
        }
        let seen = history.for_entity(&plug());
        assert_eq!(
            seen.len(),
            MAX_PER_ENTITY,
            "kept at most the cap, oldest dropped"
        );
    }

    #[test]
    fn pruning_forgets_anything_a_day_old() {
        let now = Instant::now();
        let mut entries = VecDeque::new();
        entries.push_back(Entry {
            at: now - WINDOW - Duration::from_secs(1),
            state: state(true, "2026-09-16T10:00:00Z"),
        });
        entries.push_back(Entry {
            at: now,
            state: state(false, "2026-09-16T10:00:01Z"),
        });
        prune(&mut entries, now.checked_sub(WINDOW));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries.front().map(|entry| entry.state.clone()),
            Some(state(false, "2026-09-16T10:00:01Z"))
        );
    }

    #[test]
    fn pruning_waits_until_the_server_has_a_day_of_uptime() {
        let mut entries = VecDeque::new();
        entries.push_back(Entry {
            at: Instant::now(),
            state: state(true, "2026-09-16T10:00:00Z"),
        });
        prune(&mut entries, None);
        assert_eq!(entries.len(), 1, "no cutoff yet, nothing is a day old");
    }
}
