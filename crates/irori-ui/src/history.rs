//! Which entities have their last 24 hours rolled down, and the days that were fetched for
//! them.
//!
//! Kept by the list rather than by each row, so a list that is drawn again — regrouped, or
//! because a device joined — neither rolls the drawers back up nor asks for the same day twice.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use irori_types::{EntityId, EntityState};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// An entity's day: nothing yet, what the server had, or why it couldn't say.
pub type Day = Option<Result<Vec<EntityState>, String>>;

/// Most changes kept after a day was fetched: a sensor that reports every second is still a
/// drawing worth of them.
const LATER_KEPT: usize = 2000;

/// An entity's day as it's kept: what the server had when it was asked, and what has changed
/// since. The second part is what lets a row be drawn again without its chart forgetting
/// everything it watched arrive.
#[derive(Debug, Clone)]
pub struct Kept {
    pub day: ArcRwSignal<Day>,
    later: Arc<Mutex<VecDeque<EntityState>>>,
}

impl Kept {
    fn new() -> Self {
        Self {
            day: ArcRwSignal::new(None),
            later: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Notes a change that arrived after the day was fetched. One already noted is left alone,
    /// and so is everything until the day has been asked for and has come: a row nobody opened
    /// keeps nothing, and what happens while the answer is on its way is in the answer.
    pub fn note(&self, state: EntityState) {
        if self.day.with_untracked(Option::is_none) {
            return;
        }
        let Ok(mut later) = self.later.lock() else {
            return;
        };
        if later
            .back()
            .is_none_or(|last| last.last_changed < state.last_changed)
        {
            later.push_back(state);
            if later.len() > LATER_KEPT {
                later.pop_front();
            }
        }
    }

    /// `fetched`, followed by whatever changed after its last entry.
    pub fn with_later(&self, mut fetched: Vec<EntityState>) -> Vec<EntityState> {
        let Ok(later) = self.later.lock() else {
            return fetched;
        };
        let after = fetched.last().map(|last| last.last_changed);
        fetched.extend(
            later
                .iter()
                .filter(|state| after.is_none_or(|after| state.last_changed > after))
                .cloned(),
        );
        fetched
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Histories {
    open: RwSignal<BTreeSet<EntityId>>,
    // Not tied to the row that first asked: a row is drawn again, the day is kept.
    days: StoredValue<BTreeMap<EntityId, Kept>>,
    /// The entities whose day has been asked for, answered or not.
    asked: StoredValue<BTreeSet<EntityId>>,
}

impl Histories {
    pub fn new() -> Self {
        Self {
            open: RwSignal::new(BTreeSet::new()),
            days: StoredValue::new(BTreeMap::new()),
            asked: StoredValue::new(BTreeSet::new()),
        }
    }

    /// Whether `id`'s drawer is down, as something a row can watch.
    pub fn open(self, id: &EntityId) -> Signal<bool> {
        let id = id.clone();
        Memo::new(move |_| self.open.with(|open| open.contains(&id))).into()
    }

    /// `id`'s day. Fetched once, the first time its drawer opens, and kept: reopening shows the
    /// same day it loaded, which is honest about what was on file then.
    pub fn day(self, id: &EntityId) -> Kept {
        if let Some(day) = self.days.with_value(|days| days.get(id).cloned()) {
            return day;
        }
        let day = Kept::new();
        self.days.update_value(|days| {
            days.insert(id.clone(), day.clone());
        });
        day
    }

    /// Asks for `id`'s day, the first time only.
    pub fn ask(self, id: &EntityId) {
        if self.asked.with_value(|asked| asked.contains(id)) {
            return;
        }
        self.asked.update_value(|asked| {
            asked.insert(id.clone());
        });
        let (id, day) = (id.clone(), self.day(id).day);
        spawn_local(async move {
            day.set(Some(crate::api::entity_history(&id).await));
        });
    }

    /// Rolls `id`'s drawer down, or back up.
    pub fn toggle(self, id: &EntityId) {
        if !self.open.with_untracked(|open| open.contains(id)) {
            self.ask(id);
        }
        self.open.update(|open| {
            if !open.remove(id) {
                open.insert(id.clone());
            }
        });
    }

    /// What a row needs to offer its history: whether it's open, and the way to open it.
    pub fn unroll(self, id: &EntityId) -> crate::devices::Unroll {
        let toggled = id.clone();
        crate::devices::Unroll {
            open: self.open(id),
            toggle: Callback::new(move |()| self.toggle(&toggled)),
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{Availability, BinarySensorState, Context, Origin, State};

    use super::*;

    fn at(second: u8, on: bool) -> EntityState {
        let when = format!("2026-10-07T10:00:{second:02}Z")
            .parse()
            .expect("a valid timestamp");
        EntityState {
            entity_id: "binary_sensor.door".parse().expect("a valid entity id"),
            state: Some(State::BinarySensor(BinarySensorState { on })),
            availability: Availability::Available,
            attributes: Default::default(),
            last_changed: when,
            last_updated: when,
            last_reported: when,
            context: Context {
                id: "01K5B2Q9A1B2C3D4E5F6G7H8J9".parse().expect("valid"),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    fn seconds(states: &[EntityState]) -> Vec<String> {
        states
            .iter()
            .map(|state| state.last_changed.to_string()[17..19].to_owned())
            .collect()
    }

    /// Nothing is kept for a day nobody asked for; once it has come, each later change is kept
    /// once, and joins the day after what the server already had.
    #[test]
    fn later_changes_join_the_day_that_was_fetched() {
        let kept = Kept::new();
        kept.note(at(1, true));
        assert_eq!(kept.with_later(Vec::new()), Vec::new());

        kept.day.set(Some(Ok(vec![at(2, false)])));
        kept.note(at(2, false));
        kept.note(at(5, true));
        kept.note(at(5, true));
        kept.note(at(9, false));
        let day = kept.with_later(vec![at(2, false), at(5, true)]);
        assert_eq!(seconds(&day), ["02", "05", "09"]);
        // A day that came back empty still gets what happened after it.
        assert_eq!(seconds(&kept.with_later(Vec::new())), ["02", "05", "09"]);
    }
}
