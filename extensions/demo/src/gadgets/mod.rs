//! The demo's devices that do more than switch and measure: a front door lock that locks itself
//! again, a doorbell with a chime and a screen, a living room blind that takes a moment to move,
//! a ceiling fan, a TV on a one-minute script, a water shut-off with a leak alarm, a thermostat,
//! a hot water tank and a dehumidifier. Between them, one of every kind beyond lights, switches
//! and sensors. Each is a [`Gadget`] in its own module.

mod blind;
mod dehumidifier;
mod doorbell;
mod fan;
mod lock;
mod shutoff;
mod tank;
mod thermostat;
pub(super) mod tv;

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, EntityCategory, EntityDescription, Name, ObjectId, Service, State,
};
use irori_protocol::{ProtocolContext, ProtocolError};

pub(crate) use self::doorbell::rings;
use crate::{device, id};

/// One of the demo's devices: what it is, what it says, what it does when it's told something,
/// and what it does on its own.
trait Gadget: Send {
    /// The device as a real one would describe itself.
    fn device(&self) -> Device;

    /// Its entities, as it describes them.
    fn entities(&self) -> Vec<Entity>;

    /// Every state worth reporting at the start.
    fn states(&self) -> Vec<(&'static str, State)>;

    /// Carries out `service` on its entity `unique_id`: the entity whose state that changed.
    /// `Err` for a service it can't do.
    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<&'static str, String>;

    /// The entities that changed on their own by `now`, in `room`. `secs` is how long the demo
    /// has been running on the sensor clock; the TV's script counts that.
    fn tick(&mut self, now: Instant, room: Room, secs: u64) -> Vec<&'static str>;
}

/// A device, as `crate::device` takes it.
struct Device {
    unique_id: &'static str,
    name: &'static str,
    model: &'static str,
    room: &'static str,
}

/// One of a device's entities.
struct Entity {
    unique_id: &'static str,
    /// `None` when it's the device's main feature and takes the device's name.
    name: Option<&'static str>,
    /// Set when the entity should keep a stable id, such as `demo_tv`.
    suggested_object_id: Option<&'static str>,
    capabilities: Capabilities,
    category: Option<EntityCategory>,
}

/// The living room, as the demo's devices feel it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Room {
    /// In °C.
    pub(crate) temperature: f64,
    /// In %.
    pub(crate) humidity: f64,
}

/// What the devices are doing.
pub(crate) struct Gadgets {
    all: Vec<Box<dyn Gadget>>,
}

impl Gadgets {
    pub(crate) fn new() -> Self {
        Self {
            all: vec![
                Box::new(lock::FrontDoorLock::new()),
                Box::new(doorbell::Doorbell::new()),
                Box::new(blind::Blind::new()),
                Box::new(fan::CeilingFan::new()),
                Box::new(tv::DemoTv::new()),
                Box::new(thermostat::Thermostat::new()),
                Box::new(dehumidifier::Dehumidifier::new()),
                Box::new(tank::HotWaterTank::new()),
                Box::new(shutoff::WaterShutoff::new()),
            ],
        }
    }

    /// Every state worth reporting at the start. The doorbell has rung for nobody yet, and the
    /// calibrate button has no state.
    pub(crate) fn states(&self) -> Vec<(&'static str, State)> {
        self.all.iter().flat_map(|gadget| gadget.states()).collect()
    }

    /// Carries out a call to one of these devices: the entity whose state it changed, and that
    /// state. `None` when the call is for another of the demo's devices.
    pub(crate) fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Option<Result<(&'static str, State), String>> {
        let gadget = self.all.iter_mut().find(|gadget| {
            gadget
                .entities()
                .iter()
                .any(|entity| entity.unique_id == unique_id)
        })?;
        Some(
            gadget
                .call(unique_id, service, now)
                .map(|changed| state_of(gadget.as_ref(), changed)),
        )
    }

    /// What changed on its own by `now`, with the living room at `temperature` °C and `humidity`
    /// %: the blind moving, the lock locking itself again, the alarm running out, the thermostat
    /// and dehumidifier reading the room.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        temperature: f64,
        humidity: f64,
        secs: u64,
    ) -> Vec<(&'static str, State)> {
        let room = Room {
            temperature,
            humidity,
        };
        let mut changed = Vec::new();
        for gadget in &mut self.all {
            for entity in gadget.tick(now, room, secs) {
                changed.push(state_of(gadget.as_ref(), entity));
            }
        }
        changed
    }
}

/// `entity`'s state, as `gadget` reports it.
fn state_of(gadget: &dyn Gadget, entity: &'static str) -> (&'static str, State) {
    gadget
        .states()
        .into_iter()
        .find(|(unique_id, _)| *unique_id == entity)
        .expect("every entity that takes a call or changes has a state")
}

/// The error for a service one of these entities doesn't take.
fn cannot(unique_id: &str, service: &Service) -> String {
    format!("the demo's `{unique_id}` can't {}", service.name())
}

pub(crate) async fn describe(ctx: &ProtocolContext) -> Result<(), ProtocolError> {
    for gadget in Gadgets::new().all {
        let Device {
            unique_id,
            name,
            model,
            room,
        } = gadget.device();
        ctx.describe_device(device(unique_id, name, model, room)?)
            .await?;
        for entity in gadget.entities() {
            ctx.describe_entity(EntityDescription {
                unique_id: id(entity.unique_id)?,
                name: entity.name.map(Name::try_from).transpose()?,
                device_unique_id: Some(id(unique_id)?),
                suggested_object_id: entity
                    .suggested_object_id
                    .map(ObjectId::try_from)
                    .transpose()?,
                capabilities: entity.capabilities,
                entity_category: entity.category,
            })
            .await?;
        }
    }
    Ok(())
}

/// Irori refuses a report that doesn't fit what the entity said it is; here that would only be a
/// line in the log, so every state is checked against its entity.
#[cfg(test)]
fn assert_fits(gadget: &dyn Gadget, states: &[(&'static str, State)]) {
    let entities = gadget.entities();
    for (unique_id, state) in states {
        let entity = entities
            .iter()
            .find(|entity| entity.unique_id == *unique_id)
            .expect("described");
        assert_eq!(entity.capabilities.fits(state), Ok(()), "{unique_id}");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tokio::time::Duration;

    use super::*;

    #[test]
    fn every_state_fits_its_entity() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        let fits = |gadgets: &Gadgets, states: &[(&'static str, State)]| {
            for gadget in &gadgets.all {
                let ids: BTreeSet<_> = gadget.entities().iter().map(|e| e.unique_id).collect();
                let own: Vec<_> = states
                    .iter()
                    .filter(|(id, _)| ids.contains(id))
                    .cloned()
                    .collect();
                assert_fits(gadget.as_ref(), &own);
            }
        };
        fits(&gadgets, &gadgets.states());
        for second in 0..20 {
            let ticked = gadgets.tick(now + Duration::from_secs(second), 20.0, 50.0, second);
            fits(&gadgets, &ticked);
        }
        fits(&gadgets, &rings(0, 1000));
    }

    #[test]
    fn every_entity_belongs_to_one_device() {
        let gadgets = Gadgets::new();
        let mut seen = BTreeSet::new();
        for gadget in &gadgets.all {
            assert!(seen.insert(gadget.device().unique_id));
            for entity in gadget.entities() {
                assert!(seen.insert(entity.unique_id), "{}", entity.unique_id);
            }
        }
    }

    #[test]
    fn calls_for_the_other_demo_devices_pass_through() {
        let mut gadgets = Gadgets::new();
        assert!(
            gadgets
                .call("lamp-light", &Service::SwitchTurnOn, Instant::now())
                .is_none()
        );
        assert!(matches!(
            gadgets.call("blind-cover", &Service::ValveOpen, Instant::now()),
            Some(Err(_))
        ));
    }
}
