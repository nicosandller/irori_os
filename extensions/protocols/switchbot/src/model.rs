//! SwitchBot's devices, as Irori reads them: which family a `deviceType` is, and one
//! [`Reading`] per device that every source (the status API now, webhooks and Bluetooth later)
//! merges into.
//!
//! SwitchBot names the same thing differently from model to model and between its status API and
//! its webhooks (`power`, `powerState`, `switchStatus`; `moveDetected`, `detectionState`). That is
//! all settled here, once, so the mapping to entities only ever sees one name per thing.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Which kind of SwitchBot device, as far as reading and commanding it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// A hub. Hub 2 and Hub 3 also measure the room.
    Hub(HubSensors),
    /// Meter, Meter Plus, Outdoor Meter, Meter Pro; `co2` for the Meter Pro CO2.
    Meter {
        co2: bool,
    },
    Contact,
    Motion,
    Presence,
    Leak,
    /// The Smart Lock family. `door`: it has a door sensor. `deadbolt`: it can pull the latch.
    Lock {
        door: bool,
        deadbolt: bool,
    },
    Bot,
    /// A plug (`outlet`) or a single wall relay. `meter` says what it measures.
    Plug {
        meter: Meter,
        outlet: bool,
    },
    /// Relay Switch 2PM: two channels, each switched and metered.
    Relay2,
    /// Curtain, Curtain 3: `slidePosition` counts from open (0) to closed (100).
    Curtain,
    RollerShade,
    BlindTilt,
    Garage,
    /// The ultrasonic Humidifier.
    Humidifier,
    /// The Evaporative Humidifier ("Humidifier2").
    Evaporative,
    Vacuum(VacuumApi),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubSensors {
    None,
    /// Temperature, humidity, light level 1-20.
    Hub2,
    /// Temperature, humidity, light level 1-10, motion.
    Hub3,
}

/// What a plug or relay measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meter {
    /// Only on or off (the first Plug, Relay Switch 1).
    None,
    /// Plug Mini US and JP: voltage, current, and `weight`, which is watts.
    Mini,
    /// Plug Mini EU and Relay Switch 1PM: power, voltage, current, energy today.
    Full,
}

/// SwitchBot's robot vacuums take two different sets of commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VacuumApi {
    /// K10+, K10+ Pro, S1, S1 Plus: `start`, `stop`, `dock`, `PowLevel 0-3`.
    Simple,
    /// K10+ Pro Combo, K11+, K20 Plus Pro: `startClean`, `pause`, `dock`, `changeParam`.
    Clean,
    /// S10, S20: as `Clean`, and they mop, so their parameters carry a water level.
    Mop,
}

impl Family {
    /// The family a device list's `deviceType` names, or `None` for one Irori can't use yet.
    pub fn of(device_type: &str) -> Option<Family> {
        Some(match device_type {
            "Hub" | "Hub Plus" | "Hub Mini" | "Hub Mini2" | "AI Hub" => {
                Family::Hub(HubSensors::None)
            }
            "Hub 2" => Family::Hub(HubSensors::Hub2),
            "Hub 3" => Family::Hub(HubSensors::Hub3),
            "Meter" | "MeterPlus" | "Meter Plus" | "WoIOSensor" | "MeterPro" => {
                Family::Meter { co2: false }
            }
            "MeterPro(CO2)" => Family::Meter { co2: true },
            "Contact Sensor" => Family::Contact,
            "Motion Sensor" => Family::Motion,
            "Presence Sensor" => Family::Presence,
            "Water Detector" => Family::Leak,
            "Smart Lock" | "Smart Lock Pro" | "Smart Lock Pro Wifi" | "Smart Lock Ultra" => {
                Family::Lock {
                    door: true,
                    deadbolt: true,
                }
            }
            "Smart Lock Lite" | "Lock Lite" => Family::Lock {
                door: false,
                deadbolt: true,
            },
            "Lock Vision" | "Lock Vision Pro" => Family::Lock {
                door: true,
                deadbolt: false,
            },
            "Bot" => Family::Bot,
            "Plug" => Family::Plug {
                meter: Meter::None,
                outlet: true,
            },
            "Relay Switch 1" => Family::Plug {
                meter: Meter::None,
                outlet: false,
            },
            "Plug Mini (US)" | "Plug Mini (JP)" => Family::Plug {
                meter: Meter::Mini,
                outlet: true,
            },
            "Plug Mini (EU)" => Family::Plug {
                meter: Meter::Full,
                outlet: true,
            },
            "Relay Switch 1PM" => Family::Plug {
                meter: Meter::Full,
                outlet: false,
            },
            "Relay Switch 2PM" => Family::Relay2,
            "Curtain" | "Curtain3" => Family::Curtain,
            "Roller Shade" => Family::RollerShade,
            "Blind Tilt" => Family::BlindTilt,
            "Garage Door Opener" => Family::Garage,
            "Humidifier" => Family::Humidifier,
            "Humidifier2" => Family::Evaporative,
            "K10+" | "K10+ Pro" | "Robot Vacuum Cleaner S1" | "Robot Vacuum Cleaner S1 Plus" => {
                Family::Vacuum(VacuumApi::Simple)
            }
            "Robot Vacuum Cleaner K10+ Pro Combo"
            | "Robot Vacuum Cleaner K11+"
            | "Robot Vacuum Cleaner K20 Plus Pro" => Family::Vacuum(VacuumApi::Clean),
            "Robot Vacuum Cleaner S10" | "Robot Vacuum Cleaner S20" => {
                Family::Vacuum(VacuumApi::Mop)
            }
            _ => return None,
        })
    }

    /// Whether its readings change by themselves often enough to be worth polling first.
    /// Leaks, doors and locks matter most when they change.
    pub fn urgent(self) -> bool {
        matches!(
            self,
            Family::Leak | Family::Contact | Family::Lock { .. } | Family::Garage
        )
    }

    /// A hub with nothing to read: listed so the devices behind it can say they're reached
    /// through it, never polled.
    pub fn silent(self) -> bool {
        matches!(self, Family::Hub(HubSensors::None))
    }
}

/// Where a lock's bolt is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bolt {
    Locked,
    Unlocked,
    /// On its way, as Bluetooth says while the motor turns.
    Locking,
    Unlocking,
    Jammed,
}

/// Everything known about one device, under one name per thing, in Irori's units: °C, %, W, V,
/// A, Wh, and positions from 0 (closed) to 100 (open). Each field is `None` until a source says.
/// Kept in the extension's store, so a restart starts from what was last known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reading {
    pub temperature: Option<f64>,
    pub humidity: Option<f64>,
    pub co2: Option<f64>,
    /// SwitchBot's light level: 1-20 (Hub 2, Presence Sensor) or 1-10 (Hub 3). Not lux.
    pub light_level: Option<f64>,
    /// Bright, as a contact or motion sensor judges it.
    pub bright: Option<bool>,
    pub battery: Option<f64>,
    pub motion: Option<bool>,
    /// A contact sensor apart (or left open past its timeout).
    pub contact_open: Option<bool>,
    pub occupied: Option<bool>,
    pub leak: Option<bool>,
    pub bolt: Option<Bolt>,
    pub door_open: Option<bool>,
    pub calibrated: Option<bool>,
    pub on: Option<bool>,
    /// The second channel of a Relay Switch 2PM; the first is `on`.
    pub on_2: Option<bool>,
    pub power: Option<f64>,
    pub power_2: Option<f64>,
    pub voltage: Option<f64>,
    pub voltage_2: Option<f64>,
    pub current: Option<f64>,
    pub current_2: Option<f64>,
    /// Energy used today, Wh. SwitchBot starts it again every day.
    pub energy_today: Option<f64>,
    pub energy_today_2: Option<f64>,
    pub position: Option<f64>,
    pub moving: Option<bool>,
    /// A Bot's mode: `pressMode`, `switchMode`, `customizeMode`.
    pub bot_mode: Option<String>,
    /// The ultrasonic humidifier's Auto mode, or the evaporative one's mode number (1-8).
    pub humidifier_auto: Option<bool>,
    pub humidifier_mode: Option<u8>,
    pub lack_water: Option<bool>,
    /// A vacuum's `workingStatus`, as SwitchBot words it.
    pub working: Option<String>,
    /// Whether the device says it's online, for those that say.
    pub online: Option<bool>,
    pub version: Option<String>,
}

impl Reading {
    /// What the status API (or a webhook's `context`) says about a device of `family`.
    pub fn from_status(family: Family, status: &Map<String, Value>) -> Reading {
        let fahrenheit =
            text(status, "scale").is_some_and(|s| s.eq_ignore_ascii_case("FAHRENHEIT"));
        let mut reading = Reading {
            temperature: number(status, "temperature")
                .map(|t| {
                    if fahrenheit {
                        (t - 32.0) * 5.0 / 9.0
                    } else {
                        t
                    }
                })
                .map(|t| (t * 10.0).round() / 10.0),
            humidity: number(status, "humidity"),
            co2: number(status, "CO2"),
            light_level: number(status, "lightLevel"),
            bright: text(status, "brightness").map(|b| b.eq_ignore_ascii_case("bright")),
            battery: number(status, "battery"),
            motion: flag(status, "moveDetected").or_else(|| detected(status)),
            calibrated: flag(status, "calibrate"),
            moving: flag(status, "moving"),
            version: text(status, "version")
                .map(str::to_owned)
                .or_else(|| number(status, "version").map(|v| v.to_string())),
            online: flag(status, "online")
                .or_else(|| text(status, "onlineStatus").map(|s| s.eq_ignore_ascii_case("online"))),
            ..Reading::default()
        };
        match family {
            Family::Contact => {
                reading.contact_open =
                    text(status, "openState").map(|state| !state.eq_ignore_ascii_case("close"));
            }
            Family::Presence => {
                reading.occupied = flag(status, "Detected")
                    .or_else(|| flag(status, "detected"))
                    .or_else(|| detected(status));
                reading.motion = None;
            }
            Family::Leak => {
                reading.leak = number(status, "status")
                    .map(|status| status != 0.0)
                    .or_else(|| number(status, "detectionState").map(|state| state != 0.0));
                reading.motion = None;
            }
            Family::Lock { .. } => {
                reading.bolt = text(status, "lockState").and_then(bolt);
                reading.door_open = text(status, "doorState").map(|door| door.starts_with("open"));
            }
            Family::Bot => {
                reading.on = switched(status);
                reading.bot_mode = text(status, "deviceMode").map(str::to_owned);
            }
            Family::Plug { meter, .. } => {
                reading.on = switched(status);
                reading.voltage = number(status, "voltage");
                reading.current = number(status, "electricCurrent").map(milliamps);
                reading.power = match meter {
                    // The Plug Mini US and JP call their watts `weight`.
                    Meter::Mini => number(status, "weight"),
                    _ => number(status, "power"),
                };
                reading.energy_today = number(status, "usedElectricity").map(watt_minutes);
            }
            Family::Relay2 => {
                let channel = |key: &str| number(status, key);
                reading.on = channel("switch1Status").map(|s| s != 0.0);
                reading.on_2 = channel("switch2Status").map(|s| s != 0.0);
                reading.power = channel("switch1Power");
                reading.power_2 = channel("switch2Power");
                reading.voltage = channel("switch1Voltage");
                reading.voltage_2 = channel("switch2Voltage");
                reading.current = channel("switch1ElectricCurrent").map(milliamps);
                reading.current_2 = channel("switch2ElectricCurrent").map(milliamps);
                reading.energy_today = channel("switch1UsedElectricity").map(watt_minutes);
                reading.energy_today_2 = channel("switch2UsedElectricity").map(watt_minutes);
            }
            Family::Curtain | Family::RollerShade => {
                // Counted from the open position: 0 is open.
                reading.position =
                    number(status, "slidePosition").map(|p| 100.0 - p.clamp(0.0, 100.0));
            }
            Family::BlindTilt => {
                reading.position = number(status, "slidePosition").map(|p| p.clamp(0.0, 100.0));
            }
            Family::Garage => {
                // `doorStatus` 0 is "on": open.
                reading.door_open = number(status, "doorStatus").map(|s| s == 0.0);
            }
            Family::Humidifier => {
                reading.on = switched(status);
                reading.humidifier_auto = flag(status, "auto");
                reading.lack_water = flag(status, "lackWater");
            }
            Family::Evaporative => {
                reading.on = switched(status);
                reading.humidifier_mode = number(status, "mode")
                    .filter(|mode| (1.0..=8.0).contains(mode))
                    .map(|mode| mode as u8);
            }
            Family::Vacuum(_) => {
                reading.working = text(status, "workingStatus").map(str::to_owned);
            }
            Family::Hub(_) | Family::Meter { .. } | Family::Motion => {}
        }
        reading
    }

    /// Takes everything `newer` knows, keeping what it doesn't say. A webhook that carries only
    /// a lock's bolt leaves its door as it was.
    pub fn merge(&mut self, newer: Reading) {
        macro_rules! take {
            ($($field:ident),* $(,)?) => {
                $( if newer.$field.is_some() { self.$field = newer.$field; } )*
            };
        }
        take!(
            temperature,
            humidity,
            co2,
            light_level,
            bright,
            battery,
            motion,
            contact_open,
            occupied,
            leak,
            bolt,
            door_open,
            calibrated,
            on,
            on_2,
            power,
            power_2,
            voltage,
            voltage_2,
            current,
            current_2,
            energy_today,
            energy_today_2,
            position,
            moving,
            bot_mode,
            humidifier_auto,
            humidifier_mode,
            lack_water,
            working,
            online,
            version,
        );
    }
}

fn number(status: &Map<String, Value>, key: &str) -> Option<f64> {
    match status.get(key)? {
        Value::Number(number) => number.as_f64(),
        // Curtain 3 sends its position as text.
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
    .filter(|value: &f64| value.is_finite())
}

fn text<'a>(status: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    status
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn flag(status: &Map<String, Value>, key: &str) -> Option<bool> {
    status.get(key)?.as_bool()
}

/// A webhook's `detectionState`: `DETECTED` or `NOT_DETECTED`.
fn detected(status: &Map<String, Value>) -> Option<bool> {
    text(status, "detectionState").map(|state| state.eq_ignore_ascii_case("DETECTED"))
}

/// On or off, under any of the names SwitchBot uses for it.
fn switched(status: &Map<String, Value>) -> Option<bool> {
    text(status, "power")
        .or_else(|| text(status, "powerState"))
        .map(|power| power.eq_ignore_ascii_case("on"))
        .or_else(|| number(status, "switchStatus").map(|status| status != 0.0))
}

fn bolt(state: &str) -> Option<Bolt> {
    let state = state.to_ascii_lowercase();
    if state == "jammed" {
        Some(Bolt::Jammed)
    } else if state.starts_with("unlock") {
        Some(Bolt::Unlocked)
    } else if state.starts_with("lock") || state == "latchboltlocked" {
        // A latch bolt that's thrown is locked too (Lock Ultra, Lock Pro Wifi).
        Some(Bolt::Locked)
    } else {
        None
    }
}

fn milliamps(value: f64) -> f64 {
    value / 1000.0
}

/// SwitchBot counts a day's energy in watt-minutes.
fn watt_minutes(value: f64) -> f64 {
    (value / 60.0 * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(value: serde_json::Value) -> Map<String, Value> {
        value.as_object().expect("an object").clone()
    }

    #[test]
    fn a_meter_pro_co2_reads_temperature_humidity_co2_and_battery() {
        let family = Family::of("MeterPro(CO2)").expect("known");
        let reading = Reading::from_status(
            family,
            &status(serde_json::json!({
                "temperature": 22.8, "humidity": 60, "CO2": 1094, "battery": 100, "version": "V1.2"
            })),
        );
        assert_eq!(reading.temperature, Some(22.8));
        assert_eq!(reading.humidity, Some(60.0));
        assert_eq!(reading.co2, Some(1094.0));
        assert_eq!(reading.battery, Some(100.0));
        assert_eq!(reading.version.as_deref(), Some("V1.2"));
    }

    #[test]
    fn a_webhook_in_fahrenheit_becomes_celsius() {
        let reading = Reading::from_status(
            Family::Meter { co2: false },
            &status(serde_json::json!({ "temperature": 72.5, "scale": "FAHRENHEIT" })),
        );
        assert_eq!(reading.temperature, Some(22.5));
    }

    #[test]
    fn a_lock_webhook_keeps_the_door_it_did_not_mention() {
        let family = Family::of("Smart Lock Pro").expect("known");
        let mut known = Reading::from_status(
            family,
            &status(serde_json::json!({
                "lockState": "locked", "doorState": "closed", "calibrate": true, "battery": 90
            })),
        );
        assert_eq!(known.bolt, Some(Bolt::Locked));
        assert_eq!(known.door_open, Some(false));
        known.merge(Reading::from_status(
            family,
            &status(serde_json::json!({ "lockState": "UNLOCKED", "battery": 89 })),
        ));
        assert_eq!(known.bolt, Some(Bolt::Unlocked));
        assert_eq!(known.door_open, Some(false), "HA forgets this (#148396)");
        assert_eq!(known.battery, Some(89.0));
    }

    #[test]
    fn a_thrown_latch_bolt_is_locked() {
        assert_eq!(bolt("latchBoltLocked"), Some(Bolt::Locked));
        assert_eq!(bolt("jammed"), Some(Bolt::Jammed));
        assert_eq!(bolt("unlock"), Some(Bolt::Unlocked));
        assert_eq!(bolt("lock"), Some(Bolt::Locked));
        assert_eq!(bolt("calibrating"), None);
    }

    #[test]
    fn plugs_read_on_under_any_name_and_in_irori_units() {
        let us = Reading::from_status(
            Family::of("Plug Mini (US)").expect("known"),
            &status(serde_json::json!({
                "power": "on", "voltage": 120.1, "weight": 35.5, "electricCurrent": 290
            })),
        );
        assert_eq!(us.on, Some(true));
        assert_eq!(us.power, Some(35.5), "the Plug Mini's `weight` is watts");
        assert_eq!(us.current, Some(0.29), "mA become A");
        let eu = Reading::from_status(
            Family::of("Plug Mini (EU)").expect("known"),
            &status(serde_json::json!({
                "switchStatus": 1, "power": 12.0, "usedElectricity": 90
            })),
        );
        assert_eq!(eu.on, Some(true));
        assert_eq!(eu.power, Some(12.0));
        assert_eq!(eu.energy_today, Some(1.5), "90 watt-minutes are 1.5 Wh");
        let webhook = Reading::from_status(
            Family::of("Plug Mini (JP)").expect("known"),
            &status(serde_json::json!({ "powerState": "OFF" })),
        );
        assert_eq!(webhook.on, Some(false));
    }

    #[test]
    fn a_curtain_counts_from_open_and_irori_from_closed() {
        let curtain = Reading::from_status(
            Family::Curtain,
            &status(serde_json::json!({ "slidePosition": "80", "moving": false })),
        );
        assert_eq!(curtain.position, Some(20.0));
        let tilt = Reading::from_status(
            Family::BlindTilt,
            &status(serde_json::json!({ "slidePosition": 80 })),
        );
        assert_eq!(tilt.position, Some(80.0));
    }

    #[test]
    fn contact_motion_presence_and_leak_read_their_own_words() {
        let contact = Reading::from_status(
            Family::Contact,
            &status(serde_json::json!({
                "openState": "timeOutNotClose", "moveDetected": false, "brightness": "dim"
            })),
        );
        assert_eq!(contact.contact_open, Some(true), "left open is open");
        assert_eq!(contact.motion, Some(false));
        assert_eq!(contact.bright, Some(false));
        let presence = Reading::from_status(
            Family::Presence,
            &status(serde_json::json!({ "Detected": true, "lightLevel": 12 })),
        );
        assert_eq!(presence.occupied, Some(true));
        assert_eq!(presence.motion, None);
        let leak = Reading::from_status(Family::Leak, &status(serde_json::json!({ "status": 1 })));
        assert_eq!(leak.leak, Some(true));
        let webhook = Reading::from_status(
            Family::Motion,
            &status(serde_json::json!({ "detectionState": "DETECTED" })),
        );
        assert_eq!(webhook.motion, Some(true));
    }

    #[test]
    fn device_types_name_their_families() {
        assert_eq!(Family::of("Hub Mini"), Some(Family::Hub(HubSensors::None)));
        assert_eq!(
            Family::of("Lock Lite"),
            Some(Family::Lock {
                door: false,
                deadbolt: true
            })
        );
        assert_eq!(
            Family::of("Robot Vacuum Cleaner S10"),
            Some(Family::Vacuum(VacuumApi::Mop))
        );
        assert_eq!(Family::of("Kata Friends"), None);
    }
}

/// A device type as a person reads it: SwitchBot's cloud uses ids like `MeterPro(CO2)` and
/// `WoIOSensor`. The rest already read well and are kept as they are.
pub fn readable(device_type: &str) -> &str {
    match device_type {
        "MeterPlus" => "Meter Plus",
        "WoIOSensor" => "Outdoor Meter",
        "MeterPro" => "Meter Pro",
        "MeterPro(CO2)" => "Meter Pro CO2",
        "Curtain3" => "Curtain 3",
        "Humidifier2" => "Evaporative Humidifier",
        "Smart Lock Pro Wifi" => "Smart Lock Pro Wi-Fi",
        other => other,
    }
}
