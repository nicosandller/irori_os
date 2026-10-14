//! What a helper would be, before it's made. One worked out from other entities is played
//! over its sources' last day, as though it had been running all along, so a page can draw what
//! it would have said; one a person sets is shown as it would start.

use std::collections::BTreeMap;

use irori_protocol::types::{
    Capabilities, Entity, EntityId, EntityState, ObjectId, State, Timestamp,
};

use crate::computed::{self, Running, Spec};
use crate::{HelperKind, Role, maths, schedule};

/// How far back a preview looks.
const DAY: f64 = 24.0 * 3600.0;

/// How often a helper that changes with time alone (a statistic's window moving on) is asked
/// again between its sources' changes.
const EVERY: f64 = 10.0 * 60.0;

/// A helper as it would be: its entity, and what it would have said.
#[derive(Debug, Clone)]
pub struct Preview {
    /// The entity a person thinks of as the helper: `binary_sensor.too_warm`.
    pub entity_id: String,
    pub capabilities: Capabilities,
    /// Each change over the last day, oldest first, ending with what it would say now. One a
    /// person sets has only how it starts.
    pub said: Vec<Said>,
    /// Whether it's worked out from other entities: what it said is history, not a start.
    pub computed: bool,
}

/// One thing a helper would have said.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub at: Timestamp,
    /// Its state, or nothing while it couldn't be worked out.
    pub state: Option<State>,
    /// Why it couldn't, when it couldn't.
    pub problem: Option<String>,
}

impl HelperKind {
    /// What one helper of this kind, with these settings, would be at `now`. `entities` are the
    /// home's, for a source's unit; `history` is an entity's changes since a moment, oldest
    /// first; `time_zone` is the home's.
    pub fn preview(
        &self,
        id: &ObjectId,
        settings: &serde_json::Value,
        entities: &[Entity],
        history: &dyn Fn(&EntityId, Timestamp) -> Vec<EntityState>,
        now: Timestamp,
        time_zone: Option<&str>,
    ) -> Result<Preview, String> {
        let made = self.made(id, settings)?;
        let first = made
            .first()
            .ok_or_else(|| format!("a {} makes no entity", self.word))?;
        let entity_id = first.entity_id();
        let capabilities = first.description.capabilities.clone();
        let zone = schedule::zone(time_zone);
        Ok(match &first.role {
            Role::Computed(spec) => Preview {
                entity_id,
                capabilities: computed::capabilities(
                    spec,
                    computed::source_unit(spec, entities).as_deref(),
                ),
                said: replay(spec, history, now, &zone),
                computed: true,
            },
            Role::Value { initial } => Preview {
                entity_id,
                said: vec![Said {
                    at: now,
                    state: initial.as_ref().and_then(|value| first.holding(value)),
                    problem: None,
                }],
                capabilities,
                computed: false,
            },
            Role::Schedule(week) => Preview {
                entity_id,
                capabilities,
                said: vec![Said {
                    at: now,
                    state: Some(State::Schedule(schedule::state(week, &zone, now))),
                    problem: None,
                }],
                computed: false,
            },
            Role::Press { .. } | Role::Happens | Role::Timer { .. } => Preview {
                entity_id,
                capabilities,
                said: Vec::new(),
                computed: false,
            },
        })
    }
}

/// What a computed helper would have said over the last day: its sources' changes fed in as
/// they happened, and the helper asked again at each and every few minutes between.
fn replay(
    spec: &Spec,
    history: &dyn Fn(&EntityId, Timestamp) -> Vec<EntityState>,
    now: Timestamp,
    zone: &jiff::tz::TimeZone,
) -> Vec<Said> {
    let day_ago = maths::plus(now, -DAY);
    // Those that look back need their window filled before the day starts.
    let start = spec
        .looks_back(day_ago, zone)
        .filter(|start| *start < day_ago)
        .unwrap_or(day_ago);
    let sources: Vec<EntityId> = spec.sources().into_iter().cloned().collect();
    let mut changes: Vec<EntityState> = sources
        .iter()
        .flat_map(|source| history(source, start))
        .collect();
    changes.sort_by_key(|state| state.last_updated);

    let mut latest: BTreeMap<EntityId, EntityState> = BTreeMap::new();
    let mut running = Running::default();
    let mut said: Vec<Said> = Vec::new();
    let mut ask = |running: &mut Running, latest: &BTreeMap<EntityId, EntityState>, at| {
        let outcome = running.outcome(spec, &|id| latest.get(id).cloned(), at, zone);
        if at < day_ago {
            return;
        }
        let (state, problem) = match outcome {
            Ok(state) => (Some(state), None),
            Err(why) => (None, Some(why)),
        };
        // Only what changed: the chart draws each value until the next.
        if said
            .last()
            .is_none_or(|last| last.state != state || last.problem != problem)
        {
            said.push(Said { at, state, problem });
        }
    };

    let mut tick = day_ago;
    let mut changes = changes.into_iter().peekable();
    loop {
        let next_change = changes.peek().map(|state| state.last_updated);
        match next_change {
            Some(at) if at <= tick && at <= now => {
                let Some(state) = changes.next() else { break };
                running.feed(spec, &state, at, zone);
                latest.insert(state.entity_id.clone(), state);
                ask(&mut running, &latest, at);
            }
            _ if tick <= now => {
                ask(&mut running, &latest, tick);
                tick = maths::plus(tick, EVERY);
            }
            _ => break,
        }
    }
    // What it says now, so the chart reaches the present.
    ask(&mut running, &latest, now);
    if let Some(last) = said.last()
        && last.at != now
    {
        said.push(Said {
            at: now,
            ..last.clone()
        });
    }
    said
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{
        Availability, Context, ContextId, NumberCapabilities, Origin, ProtocolId, SensorState,
        SensorValue, State,
    };

    use super::*;
    use crate::KINDS;

    fn at(seconds_ago: f64, now: Timestamp) -> Timestamp {
        maths::plus(now, -seconds_ago)
    }

    fn reading(entity: &EntityId, value: f64, when: Timestamp) -> EntityState {
        EntityState {
            entity_id: entity.clone(),
            availability: Availability::Available,
            state: Some(State::Sensor(SensorState {
                value: SensorValue::Number(value),
            })),
            attributes: Default::default(),
            last_changed: when,
            last_updated: when,
            last_reported: when,
            context: Context {
                id: ContextId::try_from("01J0000000000000000000000A").expect("a context id"),
                parent_id: None,
                origin: Origin::Device {
                    protocol: ProtocolId::try_from("demo").expect("a protocol"),
                },
            },
        }
    }

    fn kind(table: &str) -> &'static HelperKind {
        KINDS
            .iter()
            .find(|kind| kind.table == table)
            .expect("a kind")
    }

    /// A threshold played over a day of readings is on while they were above its line.
    #[test]
    fn a_threshold_is_played_over_its_sources_day() {
        let now =
            Timestamp::from_jiff(jiff::Timestamp::from_second(1_800_000_000).expect("a time"));
        let source = EntityId::try_from("sensor.lounge").expect("an id");
        let readings = [
            reading(&source, 20.0, at(30.0 * 3600.0, now)),
            reading(&source, 27.0, at(6.0 * 3600.0, now)),
            reading(&source, 22.0, at(2.0 * 3600.0, now)),
        ];
        let history = |entity: &EntityId, since: Timestamp| -> Vec<EntityState> {
            assert_eq!(entity, &source);
            // The reading that held when the day began is asked for too.
            readings
                .iter()
                .filter(|r| r.last_updated >= since)
                .cloned()
                .chain(readings.iter().rfind(|r| r.last_updated < since).cloned())
                .collect()
        };
        let entities: Vec<Entity> = Vec::new();
        let preview = kind("thresholds")
            .preview(
                &ObjectId::try_from("too_warm").expect("an id"),
                &serde_json::json!({"name": "Too warm", "source": "sensor.lounge", "above": 25}),
                &entities,
                &history,
                now,
                Some("UTC"),
            )
            .expect("a preview");
        assert_eq!(preview.entity_id, "binary_sensor.too_warm");
        assert!(preview.computed);
        let on: Vec<Option<bool>> = preview
            .said
            .iter()
            .map(|said| match &said.state {
                Some(State::BinarySensor(state)) => Some(state.on),
                _ => None,
            })
            .collect();
        assert_eq!(
            on,
            [Some(false), Some(true), Some(false), Some(false)],
            "{:?}",
            preview.said
        );
        assert_eq!(preview.said.last().map(|said| said.at), Some(now));
    }

    /// One a person sets starts as its `initial`, and isn't history.
    #[test]
    fn a_number_previews_as_it_starts() {
        let now =
            Timestamp::from_jiff(jiff::Timestamp::from_second(1_800_000_000).expect("a time"));
        let preview = kind("numbers")
            .preview(
                &ObjectId::try_from("evening").expect("an id"),
                &serde_json::json!({"name": "Evening", "min": 0, "max": 100, "initial": 40}),
                &[],
                &|_, _| Vec::new(),
                now,
                None,
            )
            .expect("a preview");
        assert!(!preview.computed);
        assert!(matches!(
            preview.capabilities,
            Capabilities::Number(NumberCapabilities { .. })
        ));
        assert!(matches!(
            preview.said.first().and_then(|said| said.state.clone()),
            Some(State::Number(number)) if number.value == 40.0
        ));
    }
}
