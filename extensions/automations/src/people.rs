//! What each person can reach (`docs/specs/access.md` §4).
//!
//! The engine never works this out. It asks the core (`get_reach`) what a flow's owner can see
//! and may send, keeps the answer, and checks the flow against it whenever it is saved and
//! whenever the home changes. A flow's entities are literals (`docs/specs/rules.md` §10.1), so
//! what the check does not see, the flow cannot name.

use std::collections::BTreeSet;

use irori_protocol::engine::PersonReach;
use irori_rules::MapRegistry;
use irori_types::{EntityId, EntityState};

/// Where one person stands: whether they are still here, what they may send, and the home as
/// they have it.
#[derive(Debug, Clone)]
pub struct Standing {
    /// What the core said, and which reading of the home it was applied to: while neither
    /// changes the answer is the same, and a page that polls does not rebuild it.
    pub told: PersonReach,
    pub home_was: u64,
    pub here: bool,
    pub name: String,
    pub owner: bool,
    /// The entities they have; `None` is the whole home.
    pub seen: Option<BTreeSet<EntityId>>,
    /// The registry the checks read for their flows: the home, narrowed to their rooms, so an
    /// entity outside them is unknown here as one that was never there.
    pub registry: MapRegistry,
}

impl Standing {
    /// What the core said of a person, applied to the home as it is.
    pub fn of(reach: PersonReach, home: &MapRegistry, home_was: u64) -> Self {
        let told = reach.clone();
        let seen = reach
            .entities
            .map(|ids| ids.into_iter().collect::<BTreeSet<_>>());
        let registry = match &seen {
            None => home.clone(),
            Some(seen) => MapRegistry {
                entities: home
                    .entities
                    .iter()
                    .filter(|(id, _)| seen.contains(*id))
                    .map(|(id, entity)| (id.clone(), entity.clone()))
                    .collect(),
                timezone: home.timezone,
                location: home.location,
            },
        };
        Self {
            told,
            home_was,
            here: reach.here,
            name: reach.name,
            owner: reach.owner,
            seen,
            registry,
        }
    }
}

/// What a flow is checked against, and what a question about it may be told.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    /// Whether the flow's owner may change how the home is set up.
    pub owner: bool,
    pub registry: &'a MapRegistry,
    /// The entities they have; `None` is the whole home.
    pub seen: Option<&'a BTreeSet<EntityId>>,
}

impl View<'_> {
    pub fn sees(&self, id: &EntityId) -> bool {
        self.seen.is_none_or(|seen| seen.contains(id))
    }

    /// The states of what they have, and nothing of the rest.
    pub fn states(&self, mut states: Vec<EntityState>) -> Vec<EntityState> {
        if self.seen.is_some() {
            states.retain(|state| self.sees(&state.entity_id));
        }
        states
    }
}
