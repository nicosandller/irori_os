//! A doorbell that rings now and then, with a chime to pick and a message on its screen.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, EntityCategory, EventCapabilities, EventClass, EventState, SelectCapabilities,
    SelectState, Service, State, TextCapabilities, TextMode, TextState,
};

use super::{Device, Entity, Gadget, Room, cannot};

const DOORBELL: &str = "doorbell";
const DOORBELL_RING: &str = "doorbell-ring";
const DOORBELL_CHIME: &str = "doorbell-chime";
const DOORBELL_MESSAGE: &str = "doorbell-message";

const CHIMES: [&str; 3] = ["Ding-dong", "Westminster", "Off"];
/// Someone at the door every two and a half minutes, ringing twice six seconds apart: two of
/// the same event in a row, each one its own occurrence.
const RING_EVERY: u64 = 150;
const RINGS: [u64; 2] = [60, 66];

pub(super) struct Doorbell {
    chime: String,
    message: String,
}

impl Doorbell {
    pub(super) fn new() -> Self {
        Self {
            chime: CHIMES[0].into(),
            message: "Parcels by the bench, please".into(),
        }
    }
}

impl Gadget for Doorbell {
    fn device(&self) -> Device {
        Device {
            unique_id: DOORBELL,
            name: "Demo doorbell",
            model: "Virtual doorbell",
            room: "Hallway",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity {
                unique_id: DOORBELL_RING,
                name: Some("Ring"),
                capabilities: Capabilities::Event(EventCapabilities {
                    event_types: vec!["ring".into()],
                    device_class: Some(EventClass::Doorbell),
                }),
                suggested_object_id: None,
                category: None,
            },
            Entity {
                unique_id: DOORBELL_MESSAGE,
                name: Some("Screen message"),
                capabilities: Capabilities::Text(TextCapabilities {
                    min_length: 0,
                    max_length: 40,
                    pattern: None,
                    mode: TextMode::Text,
                }),
                suggested_object_id: None,
                category: None,
            },
            Entity {
                unique_id: DOORBELL_CHIME,
                name: Some("Chime"),
                capabilities: Capabilities::Select(SelectCapabilities {
                    options: CHIMES.iter().map(|&c| c.into()).collect(),
                }),
                suggested_object_id: None,
                category: Some(EntityCategory::Config),
            },
        ]
    }

    /// It has rung for nobody yet.
    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (
                DOORBELL_CHIME,
                State::Select(SelectState {
                    option: self.chime.clone(),
                }),
            ),
            (
                DOORBELL_MESSAGE,
                State::Text(TextState {
                    value: self.message.clone(),
                }),
            ),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (DOORBELL_CHIME, Service::SelectSelectOption(data)) => {
                self.chime.clone_from(&data.option);
                Ok(DOORBELL_CHIME)
            }
            (DOORBELL_MESSAGE, Service::TextSetValue(data)) => {
                self.message.clone_from(&data.value);
                Ok(DOORBELL_MESSAGE)
            }
            _ => Err(cannot(unique_id, service)),
        }
    }

    /// Its rings come by the clock instead ([`rings`]).
    fn tick(&mut self, _: Instant, _: Room, _: u64) -> Vec<&'static str> {
        Vec::new()
    }
}

/// The doorbell's rings after `from` seconds and by `to`, one report each.
pub(crate) fn rings(from: u64, to: u64) -> Vec<(&'static str, State)> {
    let count = RINGS
        .iter()
        .map(|at| {
            // Rings at `at`, `at + RING_EVERY`, …: how many fall in (from, to].
            let by = |secs: u64| (secs + RING_EVERY - at) / RING_EVERY;
            by(to) - by(from)
        })
        .sum::<u64>();
    (0..count)
        .map(|_| {
            (
                DOORBELL_RING,
                State::Event(EventState {
                    event_type: "ring".into(),
                }),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doorbell_rings_twice_every_so_often_and_not_at_the_start() {
        assert!(rings(0, 0).is_empty());
        assert!(rings(0, 59).is_empty());
        assert_eq!(rings(58, 60).len(), 1);
        assert_eq!(rings(58, 66).len(), 2);
        assert_eq!(rings(0, 300).len(), 4);
        assert!(rings(66, 209).is_empty());
        assert_eq!(rings(209, 211).len(), 1);
        super::super::assert_fits(&Doorbell::new(), &rings(0, 300));
    }
}
