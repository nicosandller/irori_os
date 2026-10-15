//! A smart plug that measures what it powers: power, the energy used since it was plugged in,
//! the mains voltage and the current. What it draws is its appliance's: the fridge's compressor
//! running now and then, the dishwasher's programme after dinner.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, SensorCapabilities, SensorClass, SensorValueType, Service, State, StateClass,
    SwitchCapabilities, SwitchClass, SwitchState,
};

use super::{Device, Entity, Gadget, cannot, number};
use crate::world::{DAY_SECS, World, daily, during, round1, round2};

/// What's plugged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Appliance {
    Fridge,
    Dishwasher,
}

impl Appliance {
    /// What it draws `hour` hours into the day, in W.
    fn watts(self, hour: f64) -> f64 {
        match self {
            // The compressor runs for a little over an hour in every three.
            Self::Fridge if hour % 3.0 < 1.2 => {
                round1(95.0 + 6.0 * (hour * std::f64::consts::TAU).sin())
            }
            Self::Fridge => 3.0,
            // Heating the water, washing, heating again to rinse, drying.
            Self::Dishwasher => match hour {
                h if during(h, &[(20.0, 20.4)]) => 2050.0,
                h if during(h, &[(20.4, 21.0)]) => 160.0,
                h if during(h, &[(21.0, 21.25)]) => 1980.0,
                h if during(h, &[(21.25, 21.6)]) => 40.0,
                _ => 0.8,
            },
        }
    }
}

/// The mains, wandering a little around 230 V.
fn voltage(hour: f64) -> f64 {
    round1(230.0 + 2.5 * (hour * std::f64::consts::TAU / 5.0).sin() - 1.5 * daily(hour, 19.0))
}

pub(super) struct Plug {
    device: Device,
    /// Its switch, power, energy, voltage and current.
    ids: [&'static str; 5],
    appliance: Appliance,
    on: bool,
    /// Energy used since the demo started, in kWh.
    used: f64,
    world: World,
}

impl Plug {
    pub(super) fn new(device: Device, ids: [&'static str; 5], appliance: Appliance) -> Self {
        Self {
            device,
            ids,
            appliance,
            on: true,
            used: 0.0,
            world: World::at(0),
        }
    }

    fn watts(&self) -> f64 {
        if self.on {
            self.appliance.watts(self.world.hour)
        } else {
            0.0
        }
    }
}

impl Gadget for Plug {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        let [switch, power, energy, volts, current] = self.ids;
        vec![
            Entity::main(
                switch,
                Capabilities::Switch(SwitchCapabilities {
                    device_class: Some(SwitchClass::Outlet),
                }),
            ),
            Entity::measured(power, "Power", SensorClass::Power, "W"),
            Entity::named(
                energy,
                "Energy",
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: Some(SensorClass::Energy),
                    unit: Some("kWh".into()),
                    state_class: Some(StateClass::TotalIncreasing),
                    options: Vec::new(),
                }),
            ),
            Entity::measured(volts, "Voltage", SensorClass::Voltage, "V").diagnostic(),
            Entity::measured(current, "Current", SensorClass::Current, "A").diagnostic(),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        let [switch, power, energy, volts, current] = self.ids;
        let mains = voltage(self.world.hour);
        let watts = self.watts();
        vec![
            (switch, State::Switch(SwitchState { on: self.on })),
            (power, number(watts)),
            (energy, number(round2(self.used))),
            (volts, number(mains)),
            (current, number(round2(watts / mains))),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        let switch = self.ids[0];
        match service {
            _ if unique_id != switch => return Err(cannot(unique_id, service)),
            Service::SwitchTurnOn => self.on = true,
            Service::SwitchTurnOff => self.on = false,
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(switch)
    }

    /// Counts what it used since the last reading, in the day's hours, then reads again.
    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        let hours = world.secs.saturating_sub(self.world.secs) as f64 / DAY_SECS as f64 * 24.0;
        self.used += self.watts() * hours / 1000.0;
        self.world = *world;
        self.ids[1..].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plug(appliance: Appliance) -> Plug {
        Plug::new(
            Device::new("p", "Plug", "plug", "Kitchen"),
            ["p-switch", "p-power", "p-energy", "p-voltage", "p-current"],
            appliance,
        )
    }

    #[test]
    fn the_fridge_cycles_the_dishwasher_runs_after_dinner_and_energy_only_goes_up() {
        let hours: Vec<f64> = (0..240).map(|tenth| f64::from(tenth) / 10.0).collect();
        let fridge: Vec<f64> = hours.iter().map(|h| Appliance::Fridge.watts(*h)).collect();
        assert!(fridge.iter().any(|w| *w > 80.0) && fridge.iter().any(|w| *w < 5.0));
        let washer: Vec<f64> = hours
            .iter()
            .map(|h| Appliance::Dishwasher.watts(*h))
            .collect();
        assert!(washer.iter().filter(|w| **w > 1000.0).count() > 2);
        assert!(Appliance::Dishwasher.watts(12.0) < 1.0);

        let mut washer = plug(Appliance::Dishwasher);
        let now = Instant::now();
        let mut last = 0.0;
        for secs in (0..120).step_by(2) {
            washer.tick(now, &World::at(secs));
            super::super::assert_fits(&washer, &washer.states());
            assert!(washer.used >= last);
            last = washer.used;
        }
        // Two days, a programme each: one and a half kWh a time.
        assert!((2.0..4.0).contains(&last), "{last}");
        washer
            .call("p-switch", &Service::SwitchTurnOff, now)
            .expect("off");
        assert_eq!(washer.watts(), 0.0);
        for secs in [0, 15, 30, 45] {
            assert!((225.0..=235.0).contains(&voltage(World::at(secs).hour)));
        }
    }
}
