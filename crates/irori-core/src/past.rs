//! History from before Irori recorded an entity, read from the system its protocol bridges
//! (`docs/specs/protocols.md` §5.5).
//!
//! Nothing here is written to Irori's history. A stretch is asked for when a page or an engine
//! wants it, for one entity, and only ever before Irori's own record of that entity begins, so
//! the two never cover the same moment. Answers are kept in memory for a few minutes, so a
//! drawer opened twice doesn't ask twice. Only a protocol whose manifest says `history` is ever
//! asked: a home without one does no extra work at all.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use irori_protocol::{Past, PastHour};
use irori_types::{EntityId, EntityState, Timestamp};
use serde::Serialize;

/// How long a page or an engine waits for the bridged system, its turn included. After this
/// Irori's own history is shown alone.
pub const PAST_TIMEOUT: Duration = Duration::from_secs(8);
/// How far back changes are asked for: a page asks for a day and a backtest a little more.
pub const PAST_CHANGES_DAYS: i64 = 7;
/// How far back hourly summaries are asked for: the longest chart is a year.
pub const PAST_HOURS_DAYS: i64 = 366;
/// How many entities of one engine question are filled in from the bridged system. Past this,
/// the rest get Irori's own history alone, so one question can't become a flood of them.
pub const PAST_ENTITIES_PER_ASK: usize = 24;
/// How many questions go to bridged systems at once, across the home.
const AT_ONCE: usize = 2;
/// How long an answer is kept in memory.
const KEPT_FOR: Duration = Duration::from_secs(10 * 60);
/// How many answers are kept. The oldest goes first.
const KEPT_MOST: usize = 128;

/// Where a stretch of an entity's history came from: the extension that read it, by name, and
/// the moment Irori's own record takes over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Borrowed {
    pub from: String,
    pub until: Timestamp,
}

/// Changes from before Irori recorded an entity, oldest first, all before `borrowed.until`.
#[derive(Debug, Clone, PartialEq)]
pub struct BorrowedChanges {
    pub states: Vec<EntityState>,
    pub borrowed: Borrowed,
}

/// Hourly summaries from before Irori summarized an entity, oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct BorrowedHours {
    pub hours: Vec<PastHour>,
    pub borrowed: Borrowed,
}

/// Irori's own changes after the borrowed ones. Irori's first change of a bridged entity is the
/// bridge telling it what the entity is when it connects, not a change: when it says the same
/// as the last borrowed one, it's left out, so a chart doesn't show a step and a backtest
/// doesn't see the entity "turn on" again at the seam.
pub fn join(borrowed: Vec<EntityState>, own: Vec<EntityState>) -> Vec<EntityState> {
    let mut joined = borrowed;
    let mut own = own.into_iter().peekable();
    if let (Some(last), Some(first)) = (joined.last(), own.peek())
        && last.state == first.state
        && last.availability == first.availability
    {
        own.next();
    }
    joined.extend(own);
    joined
}

#[derive(Debug)]
struct Kept {
    at: Instant,
    since: Timestamp,
    until: Timestamp,
    /// Asked up to "now": Irori had no record of the entity yet.
    open: bool,
    past: Past,
}

/// What's kept between questions.
#[derive(Debug)]
pub(crate) struct PastCache {
    answers: Mutex<HashMap<(EntityId, bool), Kept>>,
    /// Where Irori's own record of an entity begins, once it has one: (changes, hours).
    began: Mutex<HashMap<(EntityId, bool), Timestamp>>,
    pub(crate) at_once: tokio::sync::Semaphore,
}

impl Default for PastCache {
    fn default() -> Self {
        Self {
            answers: Mutex::default(),
            began: Mutex::default(),
            at_once: tokio::sync::Semaphore::new(AT_ONCE),
        }
    }
}

impl PastCache {
    pub(crate) fn began(&self, entity_id: &EntityId, hourly: bool) -> Option<Timestamp> {
        self.began
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(entity_id.clone(), hourly))
            .copied()
    }

    /// Once there is a first row it stays the first, until retention deletes it, and then the
    /// one kept is only earlier than the truth: less is borrowed, never the same twice.
    pub(crate) fn note_began(&self, entity_id: &EntityId, hourly: bool, at: Timestamp) {
        self.began
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((entity_id.clone(), hourly), at);
    }

    /// The answer kept for this stretch, cut to it.
    pub(crate) fn get(
        &self,
        entity_id: &EntityId,
        hourly: bool,
        since: Timestamp,
        until: Timestamp,
    ) -> Option<Past> {
        let answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        let kept = answers.get(&(entity_id.clone(), hourly))?;
        let covers = kept.since <= since && (kept.open || kept.until >= until);
        if kept.at.elapsed() > KEPT_FOR || !covers {
            return None;
        }
        Some(Past {
            changes: kept
                .past
                .changes
                .iter()
                .filter(|change| change.at >= since && change.at < until)
                .cloned()
                .collect(),
            hours: kept
                .past
                .hours
                .iter()
                .filter(|hour| hour.start >= since && hour.start < until)
                .cloned()
                .collect(),
        })
    }

    pub(crate) fn put(
        &self,
        entity_id: &EntityId,
        hourly: bool,
        since: Timestamp,
        until: Timestamp,
        open: bool,
        past: Past,
    ) {
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        answers.retain(|_, kept| kept.at.elapsed() <= KEPT_FOR);
        while answers.len() >= KEPT_MOST {
            let Some(oldest) = answers
                .iter()
                .min_by_key(|(_, kept)| kept.at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            answers.remove(&oldest);
        }
        answers.insert(
            (entity_id.clone(), hourly),
            Kept {
                at: Instant::now(),
                since,
                until,
                open,
                past,
            },
        );
    }

    /// The entity has left the home.
    pub(crate) fn forget(&self, entity_id: &EntityId) {
        for hourly in [false, true] {
            let key = (entity_id.clone(), hourly);
            self.answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
            self.began
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{Availability, Context, Origin, State, SwitchState};

    use super::*;

    fn switch(on: bool, at: &str) -> EntityState {
        let at: Timestamp = at.parse().expect("a timestamp");
        EntityState {
            entity_id: EntityId::try_from("switch.kettle").expect("an id"),
            availability: Availability::Available,
            state: Some(State::Switch(SwitchState { on })),
            attributes: Default::default(),
            last_changed: at,
            last_updated: at,
            last_reported: at,
            context: Context {
                id: crate::context_id::new_context_id(at),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    #[test]
    fn the_seam_is_not_a_change() {
        let borrowed = vec![
            switch(false, "2026-11-20T08:00:00Z"),
            switch(true, "2026-11-20T10:00:00Z"),
        ];
        // Irori's first row is the bridge saying "on" as it connected: the same as before.
        let own = vec![
            switch(true, "2026-11-20T12:00:00Z"),
            switch(false, "2026-11-20T13:00:00Z"),
        ];
        let joined = join(borrowed, own);
        let ons: Vec<bool> = joined
            .iter()
            .map(|state| matches!(state.state, Some(State::Switch(SwitchState { on: true }))))
            .collect();
        assert_eq!(ons, [false, true, false]);
        assert_eq!(joined[1].last_updated.to_string(), "2026-11-20T10:00:00Z");
    }

    #[test]
    fn a_real_change_at_the_seam_is_kept() {
        let joined = join(
            vec![switch(false, "2026-11-20T08:00:00Z")],
            vec![switch(true, "2026-11-20T12:00:00Z")],
        );
        assert_eq!(joined.len(), 2);
    }

    #[test]
    fn a_kept_answer_is_cut_to_the_stretch_asked_for() {
        let cache = PastCache::default();
        let id = EntityId::try_from("switch.kettle").expect("an id");
        let at = |text: &str| text.parse::<Timestamp>().expect("a timestamp");
        let change = |text: &str| irori_protocol::PastChange {
            at: at(text),
            availability: Availability::Available,
            state: None,
        };
        cache.put(
            &id,
            false,
            at("2026-11-19T00:00:00Z"),
            at("2026-11-20T12:00:00Z"),
            false,
            Past {
                changes: vec![change("2026-11-19T06:00:00Z"), change("2026-11-20T06:00:00Z")],
                hours: Vec::new(),
            },
        );
        let cut = cache
            .get(&id, false, at("2026-11-20T00:00:00Z"), at("2026-11-20T12:00:00Z"))
            .expect("covered");
        assert_eq!(cut.changes.len(), 1);
        // Further back than was asked: not covered, asked again.
        assert!(
            cache
                .get(&id, false, at("2026-11-18T00:00:00Z"), at("2026-11-20T12:00:00Z"))
                .is_none()
        );
        cache.forget(&id);
        assert!(
            cache
                .get(&id, false, at("2026-11-20T00:00:00Z"), at("2026-11-20T12:00:00Z"))
                .is_none()
        );
    }
}
