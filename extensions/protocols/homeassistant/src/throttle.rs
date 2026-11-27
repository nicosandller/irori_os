//! How fast this extension tells the core about changes (rule 6 of milestone 11).
//!
//! Every extension shares the core's one event broadcast, its history queue and the home's write
//! lock, so a chatty Home Assistant must not crowd out a home's own devices. Changes wait here,
//! and leave in small batches:
//!
//! - **At most [`PER_SECOND`] a second in all**, a batch of [`PER_TICK`] every [`TICK`].
//! - **A sensor at most once a second** ([`SENSOR_GAP`]). A power meter reporting ten times a
//!   second still reports once.
//! - **The newest value wins.** A value replaced before it left is never sent: what matters is
//!   where the entity is now.
//! - **An event's every occurrence counts** (a button pressed twice is two presses), so those
//!   queue in order instead, up to [`MAX_OCCURRENCES`] per entity.
//! - **What answers a command goes first**, and isn't held by the sensor gap: someone is waiting.
//! - Otherwise, **the longest waiting goes first**, so nothing waits for ever behind a busy one.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use irori_types::{StateReport, UniqueId};
use tokio::time::Instant;

/// How often a batch leaves.
pub const TICK: Duration = Duration::from_millis(100);
/// How many reports leave per batch.
pub const PER_TICK: usize = 15;
/// The most reports a second, in all.
pub const PER_SECOND: usize = PER_TICK * 10;
/// The least time between two reports of one sensor.
pub const SENSOR_GAP: Duration = Duration::from_secs(1);
/// The most occurrences of one event waiting; past this the oldest goes.
pub const MAX_OCCURRENCES: usize = 16;

#[derive(Debug)]
struct Waiting {
    reports: VecDeque<StateReport>,
    /// A sensor, held by [`SENSOR_GAP`].
    sensor: bool,
    /// Answers a command.
    urgent: bool,
    /// When the oldest of what's waiting arrived.
    since: Instant,
}

/// Changes waiting to leave.
#[derive(Debug, Default)]
pub struct Throttle {
    waiting: HashMap<UniqueId, Waiting>,
    /// When each sensor last reported.
    last_sent: HashMap<UniqueId, Instant>,
    /// Replaced before they left, since the last [`Throttle::take_replaced`].
    replaced: u64,
}

impl Throttle {
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    /// Adds a change: `sensor` when it's a sensor's.
    pub fn push(&mut self, report: StateReport, sensor: bool, now: Instant) {
        let urgent = report.caused_by.is_some();
        let occurrence = report.is_occurrence();
        let waiting = self
            .waiting
            .entry(report.unique_id.clone())
            .or_insert_with(|| Waiting {
                reports: VecDeque::new(),
                sensor,
                urgent: false,
                since: now,
            });
        waiting.urgent |= urgent;
        if occurrence {
            if waiting.reports.len() >= MAX_OCCURRENCES {
                waiting.reports.pop_front();
                self.replaced += 1;
            }
            waiting.reports.push_back(report);
        } else {
            // The newest wins; it keeps what the one it replaced answered.
            let caused_by = waiting.reports.back().and_then(|old| old.caused_by.clone());
            self.replaced += waiting.reports.len() as u64;
            waiting.reports.clear();
            let mut report = report;
            if report.caused_by.is_none() {
                report.caused_by = caused_by;
            }
            waiting.reports.push_back(report);
        }
    }

    /// The next batch: at most `budget` reports.
    pub fn take(&mut self, budget: usize, now: Instant) -> Vec<StateReport> {
        let mut ready: Vec<(bool, Instant, UniqueId)> = self
            .waiting
            .iter()
            .filter(|(id, waiting)| {
                waiting.urgent
                    || !waiting.sensor
                    || self
                        .last_sent
                        .get(*id)
                        .is_none_or(|last| now.duration_since(*last) >= SENSOR_GAP)
            })
            .map(|(id, waiting)| (!waiting.urgent, waiting.since, id.clone()))
            .collect();
        // Urgent first, then the longest waiting.
        ready.sort_unstable();
        let mut batch = Vec::new();
        for (_, _, id) in ready {
            if batch.len() >= budget {
                break;
            }
            let Some(waiting) = self.waiting.get_mut(&id) else {
                continue;
            };
            let Some(report) = waiting.reports.pop_front() else {
                continue;
            };
            if waiting.sensor {
                self.last_sent.insert(id.clone(), now);
            }
            if waiting.reports.is_empty() {
                self.waiting.remove(&id);
            } else {
                waiting.since = now;
                waiting.urgent = waiting.reports.iter().any(|r| r.caused_by.is_some());
            }
            batch.push(report);
        }
        batch
    }

    /// Forgets an entity that's no longer brought over.
    pub fn forget(&mut self, id: &UniqueId) {
        self.waiting.remove(id);
        self.last_sent.remove(id);
    }

    /// How many values were replaced before they left since the last call.
    pub fn take_replaced(&mut self) -> u64 {
        std::mem::take(&mut self.replaced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{ContextId, EventState, SensorState, SensorValue, State};

    fn sensor(id: &str, value: f64) -> StateReport {
        StateReport {
            unique_id: UniqueId::try_from(id).expect("id"),
            state: Some(State::Sensor(SensorState {
                value: SensorValue::Number(value),
            })),
            attributes: Default::default(),
            caused_by: None,
            replayed: false,
        }
    }

    fn press(id: &str, kind: &str) -> StateReport {
        StateReport {
            state: Some(State::Event(EventState {
                event_type: kind.into(),
            })),
            ..sensor(id, 0.0)
        }
    }

    fn value(report: &StateReport) -> f64 {
        match &report.state {
            Some(State::Sensor(SensorState {
                value: SensorValue::Number(n),
            })) => *n,
            _ => f64::NAN,
        }
    }

    #[test]
    fn a_sensor_reports_at_most_once_a_second_and_the_newest_value_wins() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        throttle.push(sensor("power", 1.0), true, start);
        assert_eq!(throttle.take(PER_TICK, start).len(), 1);
        for n in 2..=10 {
            throttle.push(sensor("power", f64::from(n)), true, start);
        }
        assert!(throttle.take(PER_TICK, start + TICK).is_empty());
        assert!(throttle.take(PER_TICK, start + SENSOR_GAP / 2).is_empty());
        let later = throttle.take(PER_TICK, start + SENSOR_GAP);
        assert_eq!(later.iter().map(value).collect::<Vec<_>>(), [10.0]);
        assert_eq!(throttle.take_replaced(), 8);
        assert!(throttle.is_empty());
    }

    #[test]
    fn no_more_than_a_batch_leaves_at_once_and_the_longest_waiting_goes_first() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        for n in 0..(PER_TICK * 3) {
            throttle.push(
                sensor(&format!("s{n:03}"), 1.0),
                true,
                start + Duration::from_millis(n as u64),
            );
        }
        let first = throttle.take(PER_TICK, start + TICK);
        assert_eq!(first.len(), PER_TICK);
        assert_eq!(first[0].unique_id.as_str(), "s000");
        assert_eq!(throttle.len(), PER_TICK * 2);
    }

    #[test]
    fn what_answers_a_command_goes_first_whatever_the_gap() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        throttle.push(sensor("busy", 1.0), true, start);
        throttle.take(PER_TICK, start);
        for n in 0..PER_TICK {
            throttle.push(sensor(&format!("other{n}"), 1.0), false, start);
        }
        let mut answer = sensor("busy", 2.0);
        answer.caused_by = Some(ContextId::try_from("01J0000000000000000000000A").expect("ulid"));
        throttle.push(answer, true, start + TICK);
        let batch = throttle.take(PER_TICK, start + TICK);
        assert_eq!(batch[0].unique_id.as_str(), "busy");
        assert_eq!(batch.len(), PER_TICK);
    }

    #[test]
    fn every_press_of_an_event_counts() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        throttle.push(press("button", "single"), false, start);
        throttle.push(press("button", "double"), false, start);
        let first = throttle.take(PER_TICK, start);
        let second = throttle.take(PER_TICK, start + TICK);
        assert_eq!(first.len() + second.len(), 2);
        assert_eq!(throttle.take_replaced(), 0);
    }
}
