//! A video doorbell on the approach, the kind with a camera: it rings, it sees who's at the door
//! and records them, and its chime can be changed or silenced.

use tokio::time::Instant;

use irori_protocol::Picture;
use irori_protocol::types::{
    BinarySensorClass, CameraCapabilities, CameraState, CameraStatus, Capabilities,
    EventCapabilities, EventClass, EventState, SelectCapabilities, SelectState, Service, State,
};

use super::camera::Scene;
use super::{Device, Entity, Gadget, cannot, flag, picture};
use crate::world::{World, during};

const DOORBELL_RING: &str = "approach-doorbell-ring";
const DOORBELL_CAMERA: &str = "approach-doorbell-camera";
const DOORBELL_PERSON: &str = "approach-doorbell-person";
const DOORBELL_CHIME: &str = "approach-doorbell-chime";

const CHIMES: [&str; 3] = ["Ding-dong", "Westminster", "Off"];
/// Someone at the door every two and a half minutes, ringing twice six seconds apart: two of
/// the same event in a row, each one its own occurrence.
const RING_EVERY: u64 = 150;
const RINGS: [u64; 2] = [60, 66];

pub(super) struct Doorbell {
    chime: String,
    on: bool,
    detecting: bool,
    world: World,
}

impl Doorbell {
    pub(super) fn new() -> Self {
        Self {
            chime: CHIMES[0].into(),
            on: true,
            detecting: true,
            world: World::at(0),
        }
    }

    /// Someone on the doorstep: whoever's ringing, and the post in the morning.
    fn someone(&self) -> bool {
        let at_the_bell = RINGS
            .iter()
            .any(|ring| (self.world.secs + RING_EVERY - ring + 6) % RING_EVERY < 16);
        at_the_bell || during(self.world.hour, &[(10.3, 10.7)])
    }

    fn seen(&self) -> bool {
        self.on && self.detecting && self.someone()
    }

    fn camera(&self) -> CameraState {
        CameraState {
            state: match (self.on, self.seen()) {
                (false, _) => CameraStatus::Off,
                (true, true) => CameraStatus::Recording,
                (true, false) => CameraStatus::Idle,
            },
            motion_detection: Some(self.detecting),
        }
    }
}

impl Gadget for Doorbell {
    fn device(&self) -> Device {
        Device::new(
            "approach-doorbell",
            "Doorbell",
            "video doorbell",
            "Approach",
        )
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::named(
                DOORBELL_RING,
                "Ring",
                Capabilities::Event(EventCapabilities {
                    event_types: vec!["ring".into()],
                    device_class: Some(EventClass::Doorbell),
                }),
            ),
            Entity::named(
                DOORBELL_CAMERA,
                "Camera",
                Capabilities::Camera(CameraCapabilities {
                    on_off: true,
                    motion_detection: true,
                }),
            ),
            Entity::flag(DOORBELL_PERSON, "Person", BinarySensorClass::Occupancy),
            Entity::named(
                DOORBELL_CHIME,
                "Chime",
                Capabilities::Select(SelectCapabilities {
                    options: CHIMES.iter().map(|&c| c.into()).collect(),
                }),
            )
            .config(),
        ]
    }

    /// It has rung for nobody yet.
    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (DOORBELL_CAMERA, State::Camera(self.camera())),
            (DOORBELL_PERSON, flag(self.seen())),
            (
                DOORBELL_CHIME,
                State::Select(SelectState {
                    option: self.chime.clone(),
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
                return Ok(DOORBELL_CHIME);
            }
            (DOORBELL_CAMERA, Service::CameraTurnOn) => self.on = true,
            (DOORBELL_CAMERA, Service::CameraTurnOff) => self.on = false,
            (DOORBELL_CAMERA, Service::CameraEnableMotionDetection) => self.detecting = true,
            (DOORBELL_CAMERA, Service::CameraDisableMotionDetection) => self.detecting = false,
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(DOORBELL_CAMERA)
    }

    /// Its rings come by the clock instead ([`rings`]).
    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        let before = self.states();
        self.world = *world;
        if self.states() == before {
            Vec::new()
        } else {
            vec![DOORBELL_CAMERA, DOORBELL_PERSON]
        }
    }

    fn picture(&self, unique_id: &str) -> Option<Picture> {
        (unique_id == DOORBELL_CAMERA).then(|| {
            picture::draw(&picture::Shot {
                scene: Scene::Doorstep,
                name: "Doorbell",
                hour: self.world.hour,
                on: self.on,
                recording: self.seen(),
                someone: self.someone(),
            })
        })
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
    fn the_doorbell_rings_twice_every_so_often_and_sees_who_rang() {
        assert!(rings(0, 0).is_empty());
        assert!(rings(0, 59).is_empty());
        assert_eq!(rings(58, 60).len(), 1);
        assert_eq!(rings(58, 66).len(), 2);
        assert_eq!(rings(0, 300).len(), 4);
        assert!(rings(66, 209).is_empty());
        assert_eq!(rings(209, 211).len(), 1);
        let mut bell = Doorbell::new();
        super::super::assert_fits(&bell, &rings(0, 300));
        bell.tick(Instant::now(), &World::at(61));
        assert!(bell.seen(), "someone is at the door when it rings");
        assert_eq!(bell.camera().state, CameraStatus::Recording);
        super::super::assert_fits(&bell, &bell.states());
        bell.tick(Instant::now(), &World::at(100));
        assert!(!bell.seen(), "and not long after");
        let shot = bell.picture(DOORBELL_CAMERA).expect("a picture");
        assert_eq!(shot.check(), Ok(()));
    }
}
