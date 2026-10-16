//! A security camera: it watches for motion, records while there is some, and can be turned off
//! for privacy. A page asks it for a picture of what it sees, which it draws ([`super::picture`]).

use tokio::time::Instant;

use irori_protocol::Picture;
use irori_protocol::types::{
    BinarySensorClass, CameraCapabilities, CameraState, CameraStatus, Capabilities, Service, State,
};

use super::{Device, Entity, Gadget, cannot, flag, picture};
use crate::world::{World, during};

/// What a camera looks at, for its picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scene {
    Garage,
    LivingRoom,
    /// The lawn, from the back of the house.
    Lawn,
    /// The shed at the end of the garden.
    Shed,
    /// The path to the front door, from the doorbell.
    Doorstep,
}

pub(super) struct Camera {
    device: Device,
    camera: &'static str,
    motion: &'static str,
    scene: Scene,
    /// Whether something moves in front of it in this world.
    moves: fn(&World) -> bool,
    on: bool,
    detecting: bool,
    world: World,
}

impl Camera {
    pub(super) fn new(
        device: Device,
        camera: &'static str,
        motion: &'static str,
        scene: Scene,
        moves: fn(&World) -> bool,
    ) -> Self {
        Self {
            device,
            camera,
            motion,
            scene,
            moves,
            on: true,
            detecting: true,
            world: World::at(0),
        }
    }

    /// Whether it sees motion now: only while it's on and watching for it.
    fn moving(&self) -> bool {
        self.on && self.detecting && (self.moves)(&self.world)
    }

    fn state(&self) -> CameraState {
        CameraState {
            state: match (self.on, self.moving()) {
                (false, _) => CameraStatus::Off,
                (true, true) => CameraStatus::Recording,
                (true, false) => CameraStatus::Idle,
            },
            motion_detection: Some(self.detecting),
        }
    }
}

/// The car leaving in the morning and back in the evening.
pub(super) fn in_the_garage(world: &World) -> bool {
    during(world.hour, &[(7.9, 8.4), (17.4, 17.9)])
}

/// People about in the morning and the evening.
pub(super) fn in_the_living_room(world: &World) -> bool {
    during(world.hour, &[(7.0, 7.6), (18.0, 22.8)])
}

/// A cat crossing in the night, and someone gardening in the afternoon.
pub(super) fn on_the_lawn(world: &World) -> bool {
    during(world.hour, &[(3.0, 3.4), (15.0, 15.8)])
}

/// A fox at night, and the shed opened for the mower.
pub(super) fn by_the_shed(world: &World) -> bool {
    during(world.hour, &[(15.6, 16.0), (22.0, 22.3)])
}

impl Gadget for Camera {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::main(
                self.camera,
                Capabilities::Camera(CameraCapabilities {
                    on_off: true,
                    motion_detection: true,
                }),
            ),
            Entity::flag(self.motion, "Motion", BinarySensorClass::Motion),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (self.camera, State::Camera(self.state())),
            (self.motion, flag(self.moving())),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match service {
            _ if unique_id != self.camera => return Err(cannot(unique_id, service)),
            Service::CameraTurnOn => self.on = true,
            Service::CameraTurnOff => self.on = false,
            Service::CameraEnableMotionDetection => self.detecting = true,
            Service::CameraDisableMotionDetection => self.detecting = false,
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(self.camera)
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        let before = self.states();
        self.world = *world;
        if self.states() == before {
            Vec::new()
        } else {
            vec![self.camera, self.motion]
        }
    }

    fn picture(&self, unique_id: &str) -> Option<Picture> {
        (unique_id == self.camera).then(|| {
            picture::draw(&picture::Shot {
                scene: self.scene,
                name: self.device.name,
                hour: self.world.hour,
                on: self.on,
                recording: self.moving(),
                someone: (self.moves)(&self.world),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lawn() -> Camera {
        Camera::new(
            Device::new("g", "Garden camera 1", "camera", "Garden"),
            "g-camera",
            "g-motion",
            Scene::Lawn,
            on_the_lawn,
        )
    }

    #[test]
    fn a_camera_records_while_something_moves_unless_told_not_to_watch() {
        let now = Instant::now();
        let mut camera = lawn();
        // 15:00 is a quarter to the end of a minute-long day.
        let gardening = World::at(38);
        assert!(on_the_lawn(&gardening));
        assert_eq!(camera.tick(now, &gardening), vec!["g-camera", "g-motion"]);
        assert_eq!(camera.state().state, CameraStatus::Recording);
        super::super::assert_fits(&camera, &camera.states());
        assert!(camera.tick(now, &gardening).is_empty(), "nothing new");

        camera
            .call("g-camera", &Service::CameraDisableMotionDetection, now)
            .expect("stops watching");
        assert_eq!(camera.state().state, CameraStatus::Idle);
        assert_eq!(camera.states()[1].1, flag(false));
        camera
            .call("g-camera", &Service::CameraTurnOff, now)
            .expect("turns off");
        assert_eq!(camera.state().state, CameraStatus::Off);
        super::super::assert_fits(&camera, &camera.states());
        assert!(
            camera
                .call("g-motion", &Service::CameraTurnOn, now)
                .is_err()
        );
        let dark = camera.picture("g-camera").expect("a picture, even off");
        assert_eq!(dark.check(), Ok(()));
        assert!(camera.picture("g-motion").is_none());
    }
}
