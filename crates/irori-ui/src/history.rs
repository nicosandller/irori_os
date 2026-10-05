//! Which entities have their last 24 hours rolled down, and the days that were fetched for
//! them.
//!
//! Kept by the list rather than by each row, so a list that is drawn again — regrouped, or
//! because a device joined — neither rolls the drawers back up nor asks for the same day twice.

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{EntityId, EntityState};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// An entity's day: nothing yet, what the server had, or why it couldn't say.
pub type Day = Option<Result<Vec<EntityState>, String>>;

#[derive(Debug, Clone, Copy)]
pub struct Histories {
    open: RwSignal<BTreeSet<EntityId>>,
    // Not tied to the row that first asked: a row is drawn again, the day is kept.
    days: StoredValue<BTreeMap<EntityId, ArcRwSignal<Day>>>,
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
    pub fn day(self, id: &EntityId) -> ArcRwSignal<Day> {
        if let Some(day) = self.days.with_value(|days| days.get(id).cloned()) {
            return day;
        }
        let day = ArcRwSignal::new(None);
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
        let (id, day) = (id.clone(), self.day(id));
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
