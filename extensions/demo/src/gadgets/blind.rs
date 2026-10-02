//! A living room venetian blind that takes a moment to move, with a button that calibrates it.

use tokio::time::Instant;

use irori_protocol::types::{
    ButtonCapabilities, Capabilities, CoverCapabilities, CoverClass, CoverState, EntityCategory,
    OpenState, Service, State,
};

use super::{Device, Entity, Gadget, Room, cannot};

const BLIND: &str = "blind";
const BLIND_COVER: &str = "blind-cover";
const BLIND_CALIBRATE: &str = "blind-calibrate";
/// How far the blind moves between readings, in percent.
const BLIND_STEP: u8 = 20;

/// Where the blind is, and where it's going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Blind {
    position: u8,
    target: u8,
    tilt: u8,
    /// Where to go once it gets there: calibrating runs it down, then back up.
    then: Option<u8>,
}

impl Blind {
    pub(super) fn new() -> Self {
        Self {
            position: 100,
            target: 100,
            tilt: 50,
            then: None,
        }
    }

    fn state(&self) -> CoverState {
        let state = match self.target.cmp(&self.position) {
            std::cmp::Ordering::Greater => OpenState::Opening,
            std::cmp::Ordering::Less => OpenState::Closing,
            std::cmp::Ordering::Equal if self.position == 0 => OpenState::Closed,
            std::cmp::Ordering::Equal => OpenState::Open,
        };
        CoverState {
            state,
            position: Some(self.position),
            tilt: Some(self.tilt),
        }
    }

    fn go(&mut self, target: u8) {
        self.target = target;
        self.then = None;
    }

    /// One reading's worth of moving. Whether it moved.
    fn step(&mut self) -> bool {
        if self.position == self.target {
            match self.then.take() {
                Some(next) => self.target = next,
                None => return false,
            }
        }
        self.position = if self.target > self.position {
            self.position.saturating_add(BLIND_STEP).min(self.target)
        } else {
            self.position.saturating_sub(BLIND_STEP).max(self.target)
        };
        true
    }
}

impl Gadget for Blind {
    fn device(&self) -> Device {
        Device {
            unique_id: BLIND,
            name: "Demo living room blind",
            model: "Virtual venetian blind",
            room: "Living room",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity {
                unique_id: BLIND_COVER,
                name: None,
                capabilities: Capabilities::Cover(CoverCapabilities {
                    device_class: Some(CoverClass::Blind),
                    position: true,
                    tilt: true,
                    stop: true,
                }),
                suggested_object_id: None,
                category: None,
            },
            Entity {
                unique_id: BLIND_CALIBRATE,
                name: Some("Calibrate"),
                capabilities: Capabilities::Button(ButtonCapabilities::default()),
                suggested_object_id: None,
                category: Some(EntityCategory::Config),
            },
        ]
    }

    /// The calibrate button has no state.
    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(BLIND_COVER, State::Cover(self.state()))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (BLIND_COVER, Service::CoverOpen) => self.go(100),
            (BLIND_COVER, Service::CoverClose) => self.go(0),
            (BLIND_COVER, Service::CoverStop) => self.go(self.position),
            (BLIND_COVER, Service::CoverSetPosition(data)) => self.go(data.position),
            (BLIND_COVER, Service::CoverSetTilt(data)) => self.tilt = data.tilt,
            // Down to the bottom to find it, then back to where it was.
            (BLIND_CALIBRATE, Service::ButtonPress) => {
                let back = self.target;
                self.go(0);
                self.then = Some(back);
            }
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(BLIND_COVER)
    }

    fn tick(&mut self, _: Instant, _: Room, _: u64) -> Vec<&'static str> {
        if self.step() {
            vec![BLIND_COVER]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::SetPosition;

    use super::*;

    const ROOM: Room = Room {
        temperature: 21.0,
        humidity: 50.0,
    };

    #[test]
    fn the_blind_takes_a_moment_and_calibrating_comes_back() {
        let now = Instant::now();
        let mut blind = Blind::new();
        blind
            .call(
                BLIND_COVER,
                &Service::CoverSetPosition(SetPosition { position: 40 }),
                now,
            )
            .expect("moves");
        let moving = blind.state();
        assert_eq!(
            (moving.state, moving.position),
            (OpenState::Closing, Some(100))
        );
        for _ in 0..3 {
            blind.tick(now, ROOM, 0);
        }
        assert_eq!(blind.state().state, OpenState::Open);
        assert_eq!(blind.position, 40);
        assert!(blind.tick(now, ROOM, 0).is_empty(), "still once it's there");

        blind
            .call(BLIND_CALIBRATE, &Service::ButtonPress, now)
            .expect("calibrates");
        let mut lowest = 100;
        for _ in 0..10 {
            blind.tick(now, ROOM, 0);
            lowest = lowest.min(blind.position);
        }
        assert_eq!((lowest, blind.position), (0, 40));
        super::super::assert_fits(&blind, &blind.states());
    }
}
