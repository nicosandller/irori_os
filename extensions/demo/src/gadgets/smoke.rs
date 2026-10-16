//! The kitchen's smoke detector, on a battery, with a button that tests it: smoke for a few
//! seconds, as the real one sounds when its button is held.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{BinarySensorClass, ButtonCapabilities, Capabilities, Service, State};

use super::{Device, Entity, Gadget, cannot, flag, number};
use crate::world::World;

const SMOKE: &str = "kitchen-smoke";
const SMOKE_SMOKE: &str = "kitchen-smoke-smoke";
const SMOKE_TEST: &str = "kitchen-smoke-test";
const SMOKE_BATTERY: &str = "kitchen-smoke-battery";

/// How long a test says there's smoke.
const TEST: Duration = Duration::from_secs(8);

pub(super) struct SmokeDetector {
    testing_since: Option<Instant>,
    charge: f64,
}

impl SmokeDetector {
    pub(super) fn new() -> Self {
        Self {
            testing_since: None,
            charge: 100.0,
        }
    }
}

impl Gadget for SmokeDetector {
    fn device(&self) -> Device {
        Device::new(SMOKE, "Smoke detector", "smoke detector", "Kitchen")
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::flag(SMOKE_SMOKE, "Smoke", BinarySensorClass::Smoke),
            Entity::named(
                SMOKE_TEST,
                "Test",
                Capabilities::Button(ButtonCapabilities::default()),
            )
            .diagnostic(),
            Entity::battery(SMOKE_BATTERY),
        ]
    }

    /// The test button has no state.
    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (SMOKE_SMOKE, flag(self.testing_since.is_some())),
            (SMOKE_BATTERY, number(self.charge)),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (SMOKE_TEST, Service::ButtonPress) => {
                self.testing_since = Some(now);
                Ok(SMOKE_SMOKE)
            }
            _ => Err(cannot(unique_id, service)),
        }
    }

    fn tick(&mut self, now: Instant, world: &World) -> Vec<&'static str> {
        self.charge = world.battery;
        let mut changed = vec![SMOKE_BATTERY];
        if self.testing_since.is_some_and(|since| now >= since + TEST) {
            self.testing_since = None;
            changed.push(SMOKE_SMOKE);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_says_smoke_for_a_moment() {
        let now = Instant::now();
        let mut detector = SmokeDetector::new();
        assert_eq!(
            detector.call(SMOKE_TEST, &Service::ButtonPress, now),
            Ok(SMOKE_SMOKE)
        );
        assert_eq!(detector.states()[0].1, flag(true));
        assert!(
            !detector
                .tick(now + Duration::from_secs(7), &World::at(0))
                .contains(&SMOKE_SMOKE)
        );
        assert!(
            detector
                .tick(now + TEST, &World::at(0))
                .contains(&SMOKE_SMOKE)
        );
        assert_eq!(detector.states()[0].1, flag(false));
        super::super::assert_fits(&detector, &detector.states());
    }
}
