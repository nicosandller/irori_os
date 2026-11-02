//! A car, as Irori reads it: what `vehicle_data` said last, its entities, and their states.
//!
//! A reading keeps every field Irori uses from the parts Tesla answered with, and only those
//! parts: the cached data has no location or driving in it, so a cached read leaves the last
//! position alone. A field Tesla sends as null in a part it did answer is forgotten (a parked
//! car's speed is null). Nothing is ever made up for a car that's asleep: its trunk stays what
//! it was last seen as (Home Assistant's #136011 showed "open").
//!
//! The reading is kept in the store, so after a restart the entities have their values at once
//! (Home Assistant's #146030 left them unknown until the car woke).
//!
//! Tesla speaks miles; Irori shows kilometres, as it shows °C.

use std::collections::BTreeMap;

use irori_types::{
    BinarySensorClass, Capabilities, ClimateCapabilities, ClimateState, CoverCapabilities,
    CoverClass, CoverState, HvacMode, LockCapabilities, LockState, LockStatus, NumberMode,
    OpenState, SensorClass, State, StateClass, TrackerCapabilities, TrackerSource, TrackerState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::entity::{self, Entity};

const KM_PER_MILE: f64 = 1.609_344;

/// The fields Irori reads, as `part.field`. Nothing else is kept: not the route, not what's
/// playing.
const FIELDS: &[&str] = &[
    "charge_state.battery_level",
    "charge_state.battery_range",
    "charge_state.charging_state",
    "charge_state.charger_power",
    "charge_state.charge_energy_added",
    "charge_state.charge_rate",
    "charge_state.minutes_to_full_charge",
    "charge_state.time_to_full_charge",
    "charge_state.charger_voltage",
    "charge_state.charger_actual_current",
    "charge_state.charge_limit_soc",
    "charge_state.charge_limit_soc_min",
    "charge_state.charge_limit_soc_max",
    "charge_state.charge_current_request",
    "charge_state.charge_current_request_max",
    "charge_state.conn_charge_cable",
    "charge_state.charge_port_door_open",
    "climate_state.inside_temp",
    "climate_state.outside_temp",
    "climate_state.is_climate_on",
    "climate_state.is_preconditioning",
    "climate_state.driver_temp_setting",
    "climate_state.min_avail_temp",
    "climate_state.max_avail_temp",
    "climate_state.climate_keeper_mode",
    "climate_state.is_front_defroster_on",
    "climate_state.seat_heater_left",
    "climate_state.seat_heater_right",
    "climate_state.seat_heater_rear_left",
    "climate_state.seat_heater_rear_center",
    "climate_state.seat_heater_rear_right",
    "climate_state.steering_wheel_heater",
    "drive_state.latitude",
    "drive_state.longitude",
    "drive_state.shift_state",
    "drive_state.speed",
    "drive_state.power",
    "vehicle_state.locked",
    "vehicle_state.sentry_mode",
    "vehicle_state.sentry_mode_available",
    "vehicle_state.is_user_present",
    "vehicle_state.odometer",
    "vehicle_state.car_version",
    "vehicle_state.df",
    "vehicle_state.dr",
    "vehicle_state.pf",
    "vehicle_state.pr",
    "vehicle_state.ft",
    "vehicle_state.rt",
    "vehicle_state.fd_window",
    "vehicle_state.fp_window",
    "vehicle_state.rd_window",
    "vehicle_state.rp_window",
    "vehicle_state.tpms_pressure_fl",
    "vehicle_state.tpms_pressure_fr",
    "vehicle_state.tpms_pressure_rl",
    "vehicle_state.tpms_pressure_rr",
    "vehicle_config.car_type",
    "vehicle_config.can_actuate_trunks",
    "vehicle_config.motorized_charge_port",
    "vehicle_config.rear_seat_heaters",
];

/// Everything known about a car. Kept in the store under `car/{vin}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CarReading {
    /// "online", "asleep" or "offline", from the last listing or read.
    pub state: Option<String>,
    /// When the car itself was last read, in seconds since 1970.
    pub read_at: Option<u64>,
    pub fields: BTreeMap<String, Value>,
}

/// The store key a car's reading is kept under.
pub fn stored(vin: &str) -> String {
    format!("car/{vin}")
}

impl CarReading {
    /// Takes in what `vehicle_data` (or its cached twin) answered. Says whether anything
    /// changed.
    pub fn merge(&mut self, data: &Value) -> bool {
        let before = self.fields.clone();
        let state_before = self.state.clone();
        if let Some(state) = data.get("state").and_then(Value::as_str) {
            self.state = Some(state.to_owned());
        }
        for field in FIELDS {
            let Some((part, name)) = field.split_once('.') else {
                continue;
            };
            let Some(part) = data.get(part).filter(|part| part.is_object()) else {
                continue;
            };
            match part.get(name) {
                Some(value) if !value.is_null() && !value.is_object() && !value.is_array() => {
                    self.fields.insert((*field).to_owned(), value.clone());
                }
                // A parked car's speed and gear are null.
                _ if *field == "drive_state.speed" => {
                    self.fields.insert((*field).to_owned(), Value::from(0));
                }
                _ if *field == "drive_state.shift_state" => {
                    self.fields.insert((*field).to_owned(), Value::from("P"));
                }
                _ => {
                    self.fields.remove(*field);
                }
            }
        }
        self.fields != before || self.state != state_before
    }

    fn number(&self, field: &str) -> Option<f64> {
        self.fields.get(field)?.as_f64()
    }

    fn flag(&self, field: &str) -> Option<bool> {
        self.fields.get(field)?.as_bool()
    }

    fn word(&self, field: &str) -> Option<&str> {
        self.fields.get(field)?.as_str()
    }

    fn km(&self, field: &str) -> Option<f64> {
        self.number(field)
            .map(|miles| round(miles * KM_PER_MILE, 1))
    }

    /// Whether any of these doors (or windows, trunks) is open: Tesla says 0 for closed.
    fn any_open(&self, fields: &[&str]) -> Option<bool> {
        let seen: Vec<f64> = fields.iter().filter_map(|f| self.number(f)).collect();
        (!seen.is_empty()).then(|| seen.iter().any(|v| *v != 0.0))
    }

    /// Charging, as Irori names it.
    fn charging(&self) -> Option<&'static str> {
        Some(match self.word("charge_state.charging_state")? {
            "Starting" => "starting",
            "Charging" => "charging",
            "Stopped" => "stopped",
            "Complete" => "complete",
            "Disconnected" => "disconnected",
            "NoPower" => "no_power",
            _ => return None,
        })
    }

    /// Whether the car is doing something worth following closely: charging or driving, and,
    /// unless `saving`, warming or cooling the cabin. An idle car is left alone so it can sleep.
    pub fn busy(&self, saving: bool) -> bool {
        let charging = matches!(self.charging(), Some("charging" | "starting"));
        let driving = matches!(self.word("drive_state.shift_state"), Some("D" | "R" | "N"))
            || self
                .number("drive_state.speed")
                .is_some_and(|speed| speed > 0.0);
        let conditioning = self.flag("climate_state.is_climate_on") == Some(true)
            || self.flag("climate_state.is_preconditioning") == Some(true);
        charging || driving || (!saving && conditioning)
    }

    pub fn awake(&self) -> bool {
        self.state.as_deref() == Some("online")
    }

    /// The model, as Tesla sells it.
    pub fn model(&self) -> Option<&'static str> {
        Some(match self.word("vehicle_config.car_type")? {
            "model3" => "Model 3",
            "modely" => "Model Y",
            "models" | "models2" | "lychee" => "Model S",
            "modelx" | "tamarind" => "Model X",
            "cybertruck" => "Cybertruck",
            _ => return None,
        })
    }

    /// The software version, without the build hash Tesla puts after it.
    pub fn version(&self) -> Option<String> {
        self.word("vehicle_state.car_version")?
            .split_whitespace()
            .next()
            .map(str::to_owned)
    }
}

fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

const SEAT_LEVELS: [&str; 4] = ["off", "low", "medium", "high"];
const PRESETS: [&str; 4] = ["normal", "keep", "dog", "camp"];

const SEATS: [(&str, &str, &str); 5] = [
    (
        "seat_heater_front_left",
        "Front left seat heater",
        "climate_state.seat_heater_left",
    ),
    (
        "seat_heater_front_right",
        "Front right seat heater",
        "climate_state.seat_heater_right",
    ),
    (
        "seat_heater_rear_left",
        "Rear left seat heater",
        "climate_state.seat_heater_rear_left",
    ),
    (
        "seat_heater_rear_center",
        "Rear centre seat heater",
        "climate_state.seat_heater_rear_center",
    ),
    (
        "seat_heater_rear_right",
        "Rear right seat heater",
        "climate_state.seat_heater_rear_right",
    ),
];

const TIRES: [(&str, &str, &str); 4] = [
    (
        "tire_front_left",
        "Front left tire",
        "vehicle_state.tpms_pressure_fl",
    ),
    (
        "tire_front_right",
        "Front right tire",
        "vehicle_state.tpms_pressure_fr",
    ),
    (
        "tire_rear_left",
        "Rear left tire",
        "vehicle_state.tpms_pressure_rl",
    ),
    (
        "tire_rear_right",
        "Rear right tire",
        "vehicle_state.tpms_pressure_rr",
    ),
];

/// The car's entities, as far as what's known of it says. One per thing the car has: a car
/// without a powered trunk has no trunk to open. Ranges (charge limit, cabin temperatures) come
/// from the car, so they're described again when it says otherwise.
pub fn entities(reading: &CarReading) -> Vec<Entity> {
    use SensorClass as C;
    use StateClass::{Measurement, TotalIncreasing};
    let mut list = vec![
        entity::listed("state", "State", &["online", "asleep", "offline"]).diagnostic(),
        entity::measured("battery", "Battery", Some(C::Battery), "%", Measurement),
        entity::measured("range", "Range", Some(C::Distance), "km", Measurement),
        entity::listed(
            "charging",
            "Charging",
            &[
                "starting",
                "charging",
                "stopped",
                "complete",
                "disconnected",
                "no_power",
            ],
        ),
        entity::measured(
            "charger_power",
            "Charger power",
            Some(C::Power),
            "kW",
            Measurement,
        ),
        entity::measured(
            "charge_added",
            "Charge energy added",
            Some(C::Energy),
            "kWh",
            TotalIncreasing,
        ),
        entity::measured(
            "charge_rate",
            "Charge rate",
            Some(C::Speed),
            "km/h",
            Measurement,
        ),
        entity::measured(
            "time_to_full",
            "Time to full charge",
            Some(C::Duration),
            "min",
            Measurement,
        ),
        entity::measured(
            "charger_voltage",
            "Charger voltage",
            Some(C::Voltage),
            "V",
            Measurement,
        )
        .diagnostic(),
        entity::measured(
            "charger_current",
            "Charger current",
            Some(C::Current),
            "A",
            Measurement,
        )
        .diagnostic(),
        entity::measured(
            "inside_temperature",
            "Inside temperature",
            Some(C::Temperature),
            "°C",
            Measurement,
        ),
        entity::measured(
            "outside_temperature",
            "Outside temperature",
            Some(C::Temperature),
            "°C",
            Measurement,
        ),
        entity::measured(
            "odometer",
            "Odometer",
            Some(C::Distance),
            "km",
            TotalIncreasing,
        ),
        entity::measured("speed", "Speed", Some(C::Speed), "km/h", Measurement),
        entity::measured("power", "Power", Some(C::Power), "kW", Measurement),
        entity::listed("shift", "Gear", &["p", "d", "r", "n"]),
        entity::binary("cable", "Charge cable", BinarySensorClass::Plug),
        entity::binary("doors", "Doors", BinarySensorClass::Door),
        entity::binary("frunk", "Frunk", BinarySensorClass::Opening),
        entity::binary(
            "user_present",
            "Driver present",
            BinarySensorClass::Presence,
        ),
        Entity::new(
            "lock",
            "Lock",
            Capabilities::Lock(LockCapabilities {
                open: false,
                requires_code: false,
                code_format: None,
            }),
        ),
        Entity::new("climate", "Climate", climate(reading)),
        cover("windows", "Windows", CoverClass::Window),
        entity::switch("charge", "Charge"),
        entity::switch("defrost", "Defrost"),
        entity::number(
            "charge_limit",
            "Charge limit",
            (
                reading
                    .number("charge_state.charge_limit_soc_min")
                    .unwrap_or(50.0),
                reading
                    .number("charge_state.charge_limit_soc_max")
                    .unwrap_or(100.0),
                1.0,
            ),
            "%",
            NumberMode::Slider,
        ),
        entity::number(
            "charge_current",
            "Charge current",
            (
                0.0,
                reading
                    .number("charge_state.charge_current_request_max")
                    .unwrap_or(32.0),
                1.0,
            ),
            "A",
            NumberMode::Box,
        )
        .config(),
        entity::button("honk", "Honk"),
        entity::button("flash", "Flash the lights"),
        entity::button("open_frunk", "Open the frunk"),
        Entity::new(
            "location",
            "Location",
            Capabilities::DeviceTracker(TrackerCapabilities {
                source_type: Some(TrackerSource::Gps),
            }),
        ),
    ];
    if reading.flag("vehicle_config.can_actuate_trunks") != Some(false) {
        list.push(cover("trunk", "Trunk", CoverClass::Door));
    }
    if reading.flag("vehicle_config.motorized_charge_port") != Some(false) {
        list.push(cover("charge_port", "Charge port", CoverClass::Door));
    }
    if reading.flag("vehicle_state.sentry_mode_available") != Some(false) {
        list.push(entity::switch("sentry", "Sentry mode"));
    }
    if reading
        .flag("climate_state.steering_wheel_heater")
        .is_some()
    {
        list.push(entity::switch(
            "steering_wheel_heater",
            "Steering wheel heater",
        ));
    }
    let rear = reading
        .number("vehicle_config.rear_seat_heaters")
        .is_some_and(|rear| rear > 0.0);
    for (key, name, _) in SEATS.iter().take(if rear { 5 } else { 2 }) {
        list.push(entity::select(key, name, &SEAT_LEVELS));
    }
    for (key, name, _) in TIRES {
        list.push(entity::measured(key, name, Some(C::Pressure), "bar", Measurement).diagnostic());
    }
    list
}

fn climate(reading: &CarReading) -> Capabilities {
    let min = reading
        .number("climate_state.min_avail_temp")
        .unwrap_or(15.0);
    let max = reading
        .number("climate_state.max_avail_temp")
        .unwrap_or(28.0);
    Capabilities::Climate(ClimateCapabilities {
        hvac_modes: vec![HvacMode::Off, HvacMode::HeatCool],
        min_temp: min.min(max),
        max_temp: max.max(min),
        temp_step: 0.5,
        target_temperature: true,
        target_temperature_range: false,
        target_humidity: None,
        fan_modes: Vec::new(),
        swing_modes: Vec::new(),
        preset_modes: PRESETS.iter().map(|preset| (*preset).to_owned()).collect(),
    })
}

fn cover(key: &'static str, name: &'static str, class: CoverClass) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Cover(CoverCapabilities {
            device_class: Some(class),
            position: false,
            tilt: false,
            stop: false,
        }),
    )
}

fn opened(open: Option<bool>) -> Option<State> {
    open.map(|open| {
        State::Cover(CoverState {
            state: if open {
                OpenState::Open
            } else {
                OpenState::Closed
            },
            position: None,
            tilt: None,
        })
    })
}

/// The state of the entity with `key`, or `None` while nothing is known (or it's a button).
pub fn state(key: &str, reading: &CarReading) -> Option<State> {
    let number = |field: &str| entity::reading(reading.number(field));
    match key {
        "state" => entity::text(reading.state.as_deref()),
        "battery" => number("charge_state.battery_level"),
        "range" => entity::reading(reading.km("charge_state.battery_range")),
        "charging" => entity::text(reading.charging()),
        "charger_power" => number("charge_state.charger_power"),
        "charge_added" => number("charge_state.charge_energy_added"),
        "charge_rate" => entity::reading(reading.km("charge_state.charge_rate")),
        "time_to_full" => entity::reading(
            reading
                .number("charge_state.minutes_to_full_charge")
                .or_else(|| {
                    reading
                        .number("charge_state.time_to_full_charge")
                        .map(|hours| (hours * 60.0).round())
                }),
        ),
        "charger_voltage" => number("charge_state.charger_voltage"),
        "charger_current" => number("charge_state.charger_actual_current"),
        "inside_temperature" => number("climate_state.inside_temp"),
        "outside_temperature" => number("climate_state.outside_temp"),
        "odometer" => entity::reading(reading.km("vehicle_state.odometer")),
        "speed" => entity::reading(reading.km("drive_state.speed")),
        "power" => number("drive_state.power"),
        "shift" => entity::text(
            reading
                .word("drive_state.shift_state")
                .map(str::to_ascii_lowercase)
                .filter(|gear| ["p", "d", "r", "n"].contains(&gear.as_str()))
                .as_deref(),
        ),
        "cable" => entity::on(
            reading
                .word("charge_state.conn_charge_cable")
                .map(|cable| !cable.is_empty() && cable != "<invalid>"),
        ),
        "doors" => entity::on(reading.any_open(&[
            "vehicle_state.df",
            "vehicle_state.dr",
            "vehicle_state.pf",
            "vehicle_state.pr",
        ])),
        "frunk" => entity::on(reading.any_open(&["vehicle_state.ft"])),
        "user_present" => entity::on(reading.flag("vehicle_state.is_user_present")),
        "lock" => reading.flag("vehicle_state.locked").map(|locked| {
            State::Lock(LockState {
                state: if locked {
                    LockStatus::Locked
                } else {
                    LockStatus::Unlocked
                },
            })
        }),
        "climate" => climate_state(reading),
        "windows" => opened(reading.any_open(&[
            "vehicle_state.fd_window",
            "vehicle_state.fp_window",
            "vehicle_state.rd_window",
            "vehicle_state.rp_window",
        ])),
        "trunk" => opened(reading.any_open(&["vehicle_state.rt"])),
        "charge_port" => opened(reading.flag("charge_state.charge_port_door_open")),
        "charge" => entity::switched(
            reading
                .charging()
                .map(|charging| matches!(charging, "charging" | "starting")),
        ),
        "defrost" => entity::switched(reading.flag("climate_state.is_front_defroster_on")),
        "sentry" => entity::switched(reading.flag("vehicle_state.sentry_mode")),
        "steering_wheel_heater" => {
            entity::switched(reading.flag("climate_state.steering_wheel_heater"))
        }
        "charge_limit" => entity::set_to(reading.number("charge_state.charge_limit_soc")),
        "charge_current" => entity::set_to(reading.number("charge_state.charge_current_request")),
        "location" => {
            let latitude = reading.number("drive_state.latitude")?;
            let longitude = reading.number("drive_state.longitude")?;
            let tracker = TrackerState {
                // The core works out whether it's home, from the home's own location.
                home: false,
                latitude: Some(latitude),
                longitude: Some(longitude),
                accuracy: None,
            };
            tracker.validate().ok()?;
            Some(State::DeviceTracker(tracker))
        }
        _ => {
            if let Some((_, _, field)) = SEATS.iter().find(|(seat, _, _)| *seat == key) {
                let level = reading.number(field)? as usize;
                return entity::chosen(SEAT_LEVELS.get(level).copied());
            }
            if let Some((_, _, field)) = TIRES.iter().find(|(tire, _, _)| *tire == key) {
                return entity::reading(reading.number(field).map(|bar| round(bar, 2)));
            }
            None
        }
    }
}

fn climate_state(reading: &CarReading) -> Option<State> {
    let on = reading.flag("climate_state.is_climate_on")?;
    Some(State::Climate(ClimateState {
        hvac_mode: if on {
            HvacMode::HeatCool
        } else {
            HvacMode::Off
        },
        hvac_action: None,
        current_temperature: reading.number("climate_state.inside_temp"),
        target_temperature: reading.number("climate_state.driver_temp_setting"),
        target_temp_low: None,
        target_temp_high: None,
        current_humidity: None,
        target_humidity: None,
        fan_mode: None,
        swing_mode: None,
        preset_mode: reading
            .word("climate_state.climate_keeper_mode")
            .map(|mode| if mode == "off" { "normal" } else { mode })
            .filter(|mode| PRESETS.contains(mode))
            .map(str::to_owned),
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    pub fn vehicle_data() -> Value {
        let data: Value =
            serde_json::from_str(include_str!("../testdata/vehicle_data.json")).expect("json");
        data["response"].clone()
    }

    fn sensor(reading: &CarReading, key: &str) -> Option<Value> {
        match state(key, reading)? {
            State::Sensor(sensor) => serde_json::to_value(sensor.value).ok(),
            other => serde_json::to_value(other).ok(),
        }
    }

    #[test]
    fn a_read_becomes_kilometres_and_irori_s_names() {
        let mut reading = CarReading::default();
        assert!(reading.merge(&vehicle_data()));
        assert_eq!(reading.state.as_deref(), Some("online"));
        assert_eq!(sensor(&reading, "battery"), Some(json!(77.0)));
        assert_eq!(sensor(&reading, "range"), Some(json!(429.5)));
        assert_eq!(sensor(&reading, "charging"), Some(json!("stopped")));
        assert_eq!(sensor(&reading, "odometer"), Some(json!(10430.2)));
        assert_eq!(sensor(&reading, "shift"), Some(json!("p")));
        assert_eq!(sensor(&reading, "speed"), Some(json!(0.0)));
        assert_eq!(
            sensor(&reading, "seat_heater_front_left"),
            Some(json!({"kind": "select", "option": "low"}))
        );
        assert_eq!(
            sensor(&reading, "cable"),
            Some(json!({"kind": "binary_sensor", "on": true}))
        );
        assert_eq!(
            sensor(&reading, "frunk"),
            Some(json!({"kind": "binary_sensor", "on": false}))
        );
        assert_eq!(reading.model(), Some("Model 3"));
        assert_eq!(reading.version().as_deref(), Some("2026.32.6"));
        let Some(State::DeviceTracker(tracker)) = state("location", &reading) else {
            panic!("a position");
        };
        assert_eq!(tracker.latitude, Some(35.6581));
        assert!(!reading.busy(false), "parked, plugged in, not charging");
    }

    #[test]
    fn a_cached_read_keeps_the_last_position_and_a_parked_car_has_no_speed() {
        let mut reading = CarReading::default();
        let mut driving = vehicle_data();
        driving["drive_state"]["shift_state"] = json!("D");
        driving["drive_state"]["speed"] = json!(31);
        reading.merge(&driving);
        assert!(reading.busy(true));
        assert_eq!(sensor(&reading, "speed"), Some(json!(49.9)));

        let mut cached = vehicle_data();
        cached
            .as_object_mut()
            .expect("an object")
            .remove("drive_state");
        cached["state"] = json!("asleep");
        cached["vehicle_state"]["rt"] = json!(0);
        reading.merge(&cached);
        assert_eq!(reading.state.as_deref(), Some("asleep"));
        assert_eq!(
            sensor(&reading, "speed"),
            Some(json!(49.9)),
            "untouched without drive_state"
        );
        assert!(state("location", &reading).is_some());

        let parked = vehicle_data();
        reading.merge(&parked);
        assert_eq!(sensor(&reading, "speed"), Some(json!(0.0)));
        assert_eq!(sensor(&reading, "shift"), Some(json!("p")));
        assert!(!reading.merge(&parked), "the same answer changes nothing");
    }

    #[test]
    fn what_a_car_has_follows_what_it_says() {
        let unknown = CarReading::default();
        let keys = |reading: &CarReading| -> Vec<&str> {
            entities(reading).into_iter().map(|e| e.key).collect()
        };
        assert!(
            keys(&unknown).contains(&"trunk"),
            "assumed until the car says"
        );
        assert!(!keys(&unknown).contains(&"seat_heater_rear_left"));
        let mut reading = CarReading::default();
        let mut data = vehicle_data();
        data["vehicle_config"]["can_actuate_trunks"] = json!(false);
        reading.merge(&data);
        let have = keys(&reading);
        assert!(!have.contains(&"trunk"));
        assert!(have.contains(&"seat_heater_rear_center"));
        assert!(have.contains(&"steering_wheel_heater"));
        let Some(Capabilities::Climate(climate)) = entities(&reading)
            .into_iter()
            .find(|e| e.key == "climate")
            .map(|e| e.capabilities)
        else {
            panic!("a climate entity");
        };
        assert_eq!((climate.min_temp, climate.max_temp), (15.0, 28.0));
    }

    /// Every entity is one the core takes: valid as the wire checks it, of a kind the manifest
    /// lists, with a key of its own, and every state is valid for its entity.
    #[test]
    fn every_entity_and_state_is_one_the_core_takes() {
        let manifest = irori_protocol::parse_manifest(include_str!("../irori-extension.toml"))
            .expect("a valid manifest");
        let kinds = &manifest.contributes.protocol[0].entity_kinds;
        let mut reading = CarReading::default();
        reading.merge(&vehicle_data());
        let list = entities(&reading);
        let mut keys = std::collections::BTreeSet::new();
        for entity in &list {
            assert!(keys.insert(entity.key), "{} twice", entity.key);
            let description = irori_types::EntityDescription {
                unique_id: entity::id("5YJ3E1EA7KF000001", entity.key).expect("an id"),
                name: irori_types::Name::try_from(entity.name).ok(),
                device_unique_id: None,
                suggested_object_id: None,
                capabilities: entity.capabilities.clone(),
                entity_category: entity.category,
            };
            let wire = serde_json::to_value(&description).expect("serialises");
            let back: irori_types::EntityDescription =
                serde_json::from_value(wire).expect("the core takes it");
            assert!(
                kinds.contains(&back.capabilities.kind()),
                "{:?} isn't in the manifest",
                back.capabilities.kind()
            );
            if let Some(state) = state(entity.key, &reading) {
                let wire = serde_json::to_value(&state).expect("serialises");
                let _: State = serde_json::from_value(wire).expect("a valid state");
            } else {
                assert!(
                    entity.key.starts_with("honk") || ["flash", "open_frunk"].contains(&entity.key),
                    "{} has no state",
                    entity.key
                );
            }
        }
    }
}
