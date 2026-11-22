//! What each control does: the command it sends to a car or an energy site, what the usage page
//! calls it, and what the car or site shows once it's done.
//!
//! Once a command is done, its entity shows the new state straight away, credited to the call:
//! no read follows it, which would cost twice what the command did (Home Assistant does the same
//! and lets the next read put things right).

use irori_types::{HvacMode, Service};
use prost::Message;
use serde_json::{Value, json};

use crate::energy::MODES;
use crate::signed::Domain;
use crate::signed::proto::{
    self, Action, ClosureMoveRequest, UnsignedMessage, VehicleAction, Void, closure,
    unsigned_message, vehicle_action,
};
use crate::vehicle::{PRESETS, SEAT_LEVELS, SEATS};

/// Tesla's names for the climate keeper's modes, in [`PRESETS`]' order.
const KEEPER: [&str; 4] = ["off", "on", "dog", "camp"];

const WINDOWS: [&str; 4] = [
    "vehicle_state.fd_window",
    "vehicle_state.fp_window",
    "vehicle_state.rd_window",
    "vehicle_state.rp_window",
];

/// A command to a car.
#[derive(Debug, Clone, PartialEq)]
pub enum CarCommand {
    Lock(bool),
    Climate(bool),
    /// The cabin temperature, °C, for both sides.
    Temperature(f64),
    /// The climate keeper, as an index into [`PRESETS`].
    Keeper(usize),
    Windows {
        vent: bool,
    },
    Trunk {
        open: bool,
    },
    /// The frunk only opens: it's shut by hand.
    Frunk,
    ChargePort {
        open: bool,
    },
    Charge(bool),
    Defrost(bool),
    Sentry(bool),
    SteeringWheelHeater(bool),
    ChargeLimit(u8),
    ChargeCurrent(u8),
    Honk,
    Flash,
    /// A seat, as an index into [`SEATS`], and a level, into [`SEAT_LEVELS`].
    SeatHeater {
        seat: usize,
        level: usize,
    },
}

/// A whole number in `range`, from a number entity's value.
fn whole(value: f64, range: std::ops::RangeInclusive<u8>, what: &str) -> Result<u8, String> {
    let rounded = value.round();
    if !rounded.is_finite()
        || rounded < f64::from(*range.start())
        || rounded > f64::from(*range.end())
    {
        return Err(format!(
            "{what} must be between {} and {}",
            range.start(),
            range.end()
        ));
    }
    Ok(rounded as u8)
}

impl CarCommand {
    /// The command a service call on one of a car's entities asks for. `None` when that entity
    /// isn't one that commands the car (Refresh, Wake, a sensor).
    pub fn from_call(key: &str, service: &Service) -> Option<Result<CarCommand, String>> {
        use CarCommand::*;
        let switch = |on| match key {
            "charge" => Some(Charge(on)),
            "defrost" => Some(Defrost(on)),
            "sentry" => Some(Sentry(on)),
            "steering_wheel_heater" => Some(SteeringWheelHeater(on)),
            _ => None,
        };
        let cover = |open| match key {
            "windows" => Some(Windows { vent: open }),
            "trunk" => Some(Trunk { open }),
            "charge_port" => Some(ChargePort { open }),
            _ => None,
        };
        let command = match (key, service) {
            ("lock", Service::LockLock(_)) => Lock(true),
            ("lock", Service::LockUnlock(_)) => Lock(false),
            ("climate", Service::ClimateTurnOn) => Climate(true),
            ("climate", Service::ClimateTurnOff) => Climate(false),
            ("climate", Service::ClimateSetHvacMode(mode)) => {
                Climate(mode.hvac_mode != HvacMode::Off)
            }
            ("climate", Service::ClimateSetTemperature(set)) => match set.temperature {
                Some(temperature) => Temperature(temperature),
                None => {
                    return Some(Err(
                        "a Tesla takes one cabin temperature, not a range".into()
                    ));
                }
            },
            ("climate", Service::ClimateSetPresetMode(preset)) => {
                match PRESETS
                    .iter()
                    .position(|known| *known == preset.preset_mode)
                {
                    Some(at) => Keeper(at),
                    None => {
                        return Some(Err(format!(
                            "no climate mode called {}",
                            preset.preset_mode
                        )));
                    }
                }
            }
            (_, Service::SwitchTurnOn) => switch(true)?,
            (_, Service::SwitchTurnOff) => switch(false)?,
            (_, Service::CoverOpen) => cover(true)?,
            (_, Service::CoverClose) => cover(false)?,
            ("charge_limit", Service::NumberSetValue(set)) => {
                match whole(set.value, 50..=100, "The charge limit") {
                    Ok(percent) => ChargeLimit(percent),
                    Err(why) => return Some(Err(why)),
                }
            }
            ("charge_current", Service::NumberSetValue(set)) => {
                match whole(set.value, 0..=80, "The charge current") {
                    Ok(amps) => ChargeCurrent(amps),
                    Err(why) => return Some(Err(why)),
                }
            }
            ("honk", Service::ButtonPress) => Honk,
            ("flash", Service::ButtonPress) => Flash,
            ("open_frunk", Service::ButtonPress) => Frunk,
            (_, Service::SelectSelectOption(choice)) => {
                let seat = SEATS.iter().position(|(seat, _, _)| *seat == key)?;
                match SEAT_LEVELS.iter().position(|level| *level == choice.option) {
                    Some(level) => SeatHeater { seat, level },
                    None => return Some(Err(format!("no level called {}", choice.option))),
                }
            }
            _ => return None,
        };
        Some(Ok(command))
    }

    /// What it is, for "Where it went" and for a person.
    pub fn what(&self) -> String {
        use CarCommand::*;
        let on = |on: bool| if on { "on" } else { "off" };
        match self {
            Lock(true) => "Lock".into(),
            Lock(false) => "Unlock".into(),
            Climate(on_) => format!("Climate {}", on(*on_)),
            Temperature(temperature) => format!("Set the cabin to {temperature} °C"),
            Keeper(at) => format!("Climate mode {}", PRESETS.get(*at).unwrap_or(&"?")),
            Windows { vent: true } => "Vent the windows".into(),
            Windows { vent: false } => "Close the windows".into(),
            Trunk { open: true } => "Open the trunk".into(),
            Trunk { open: false } => "Close the trunk".into(),
            Frunk => "Open the frunk".into(),
            ChargePort { open: true } => "Open the charge port".into(),
            ChargePort { open: false } => "Close the charge port".into(),
            Charge(true) => "Start charging".into(),
            Charge(false) => "Stop charging".into(),
            Defrost(on_) => format!("Defrost {}", on(*on_)),
            Sentry(on_) => format!("Sentry mode {}", on(*on_)),
            SteeringWheelHeater(on_) => format!("Steering wheel heater {}", on(*on_)),
            ChargeLimit(percent) => format!("Set the charge limit to {percent}%"),
            ChargeCurrent(amps) => format!("Set the charge current to {amps} A"),
            Honk => "Honk".into(),
            Flash => "Flash the lights".into(),
            SeatHeater { seat, level } => format!(
                "{} {}",
                SEATS
                    .get(*seat)
                    .map(|(_, name, _)| *name)
                    .unwrap_or("Seat heater"),
                SEAT_LEVELS.get(*level).unwrap_or(&"?")
            ),
        }
    }

    /// What to do before sending it, given what's known of the car. The trunk and the frunk
    /// open with a toggle, so sending it to an open one can shut it: it goes only to one known
    /// to be shut by a reading from the last [`FRESH`] seconds, and otherwise the car is read
    /// first. Home Assistant, the Tesla app or a hand may have opened it since an older reading.
    pub fn check(&self, reading: &crate::vehicle::CarReading, now: u64) -> Check {
        let field = match self {
            CarCommand::Trunk { open: true } => TRUNK_FIELD,
            CarCommand::Frunk => FRUNK_FIELD,
            _ => return Check::Send,
        };
        let fresh = reading
            .read_at
            .is_some_and(|at| now.saturating_sub(at) <= FRESH);
        match reading.fields.get(field).and_then(Value::as_f64) {
            Some(open) if fresh && open != 0.0 => Check::AlreadyDone,
            Some(_) if fresh => Check::Send,
            _ => Check::LookFirst,
        }
    }

    /// What Irori should forget about the car when this command fails, so the next one looks
    /// before it acts: a failed command may still have moved it.
    pub fn failure_forgets(&self) -> Option<&'static str> {
        match self {
            CarCommand::Trunk { .. } => Some(TRUNK_FIELD),
            CarCommand::Frunk => Some(FRUNK_FIELD),
            _ => None,
        }
    }

    /// Whether sending it twice does no harm. One that isn't is sent again only when the car
    /// surely never got it (asleep, or turned away unread).
    pub fn repeatable(&self) -> bool {
        // Opening the trunk and the frunk are toggles: a second one undoes the first.
        !matches!(
            self,
            CarCommand::Honk
                | CarCommand::Flash
                | CarCommand::Trunk { open: true }
                | CarCommand::Frunk
        )
    }

    /// The car's reasons for not acting that mean it's already as asked.
    pub fn done_anyway(&self) -> &'static [&'static str] {
        match self {
            CarCommand::Charge(true) => &["is_charging"],
            CarCommand::Charge(false) => &["not_charging"],
            _ => &[],
        }
    }

    /// The computer it's for, and the command as that computer reads it.
    pub fn message(&self) -> (Domain, Vec<u8>) {
        use CarCommand::*;
        use vehicle_action::Msg;
        let security = |sub| {
            (
                Domain::VehicleSecurity,
                UnsignedMessage { sub: Some(sub) }.encode_to_vec(),
            )
        };
        let closure = |request| security(unsigned_message::Sub::ClosureMoveRequest(request));
        let action = |msg| {
            (
                Domain::Infotainment,
                Action {
                    msg: Some(proto::action::Msg::VehicleAction(VehicleAction {
                        msg: Some(msg),
                    })),
                }
                .encode_to_vec(),
            )
        };
        let open_close = |open| if open { closure::OPEN } else { closure::CLOSE };
        match self {
            Lock(lock) => security(unsigned_message::Sub::RkeAction(if *lock {
                unsigned_message::LOCK
            } else {
                unsigned_message::UNLOCK
            })),
            // Tesla's own library opens the trunk with MOVE, a toggle ([`CarCommand::check`]
            // keeps it from shutting an open one), and closes it with CLOSE.
            Trunk { open } => closure(ClosureMoveRequest {
                rear_trunk: if *open { closure::MOVE } else { closure::CLOSE },
                ..ClosureMoveRequest::default()
            }),
            Frunk => closure(ClosureMoveRequest {
                front_trunk: closure::MOVE,
                ..ClosureMoveRequest::default()
            }),
            ChargePort { open } => closure(ClosureMoveRequest {
                charge_port: open_close(*open),
                ..ClosureMoveRequest::default()
            }),
            Climate(on) => action(Msg::HvacAuto(proto::HvacAutoAction {
                power_on: *on,
                manual_override: false,
            })),
            Temperature(temperature) => action(Msg::HvacTemperatureAdjustment(
                proto::HvacTemperatureAdjustmentAction {
                    driver_temp_celsius: *temperature as f32,
                    passenger_temp_celsius: *temperature as f32,
                },
            )),
            Keeper(at) => action(Msg::HvacClimateKeeper(proto::HvacClimateKeeperAction {
                climate_keeper_action: i32::try_from(*at).unwrap_or(0),
                manual_override: true,
            })),
            Windows { vent } => action(Msg::Window(proto::VehicleControlWindowAction {
                kind: Some(if *vent {
                    proto::window::Kind::Vent(Void {})
                } else {
                    proto::window::Kind::Close(Void {})
                }),
            })),
            Charge(on) => action(Msg::ChargingStartStop(proto::ChargingStartStopAction {
                kind: Some(if *on {
                    proto::charging_start_stop::Kind::Start(Void {})
                } else {
                    proto::charging_start_stop::Kind::Stop(Void {})
                }),
            })),
            Defrost(on) => action(Msg::HvacSetPreconditioningMax(
                proto::HvacSetPreconditioningMaxAction {
                    on: *on,
                    manual_override: false,
                },
            )),
            Sentry(on) => action(Msg::SetSentryMode(
                proto::VehicleControlSetSentryModeAction { on: *on },
            )),
            SteeringWheelHeater(on) => action(Msg::HvacSteeringWheelHeater(
                proto::HvacSteeringWheelHeaterAction { power_on: *on },
            )),
            ChargeLimit(percent) => action(Msg::ChargingSetLimit(proto::ChargingSetLimitAction {
                percent: i32::from(*percent),
            })),
            ChargeCurrent(amps) => action(Msg::SetChargingAmps(proto::SetChargingAmpsAction {
                charging_amps: i32::from(*amps),
            })),
            Honk => action(Msg::HonkHorn(Void {})),
            Flash => action(Msg::FlashLights(Void {})),
            SeatHeater { seat, level } => {
                use proto::seat_heater::{Level, Seat};
                let level = match level {
                    0 => Level::Off(Void {}),
                    1 => Level::Low(Void {}),
                    2 => Level::Medium(Void {}),
                    _ => Level::High(Void {}),
                };
                let seat = match seat {
                    0 => Seat::FrontLeft(Void {}),
                    1 => Seat::FrontRight(Void {}),
                    2 => Seat::RearLeft(Void {}),
                    3 => Seat::RearCenter(Void {}),
                    _ => Seat::RearRight(Void {}),
                };
                action(Msg::HvacSeatHeater(proto::HvacSeatHeaterActions {
                    actions: vec![proto::HvacSeatHeaterAction {
                        level: Some(level),
                        seat: Some(seat),
                    }],
                }))
            }
        }
    }

    /// What the car's reading says once it's done, as `part.field` and value.
    pub fn effects(&self) -> Vec<(&'static str, Value)> {
        use CarCommand::*;
        let open = |open: bool| Value::from(u8::from(open));
        match self {
            Lock(lock) => vec![("vehicle_state.locked", json!(lock))],
            Climate(on) => vec![("climate_state.is_climate_on", json!(on))],
            Temperature(temperature) => {
                vec![("climate_state.driver_temp_setting", json!(temperature))]
            }
            Keeper(at) => vec![(
                "climate_state.climate_keeper_mode",
                json!(KEEPER.get(*at).unwrap_or(&"off")),
            )],
            Windows { vent } => WINDOWS.iter().map(|field| (*field, open(*vent))).collect(),
            Trunk { open: is } => vec![("vehicle_state.rt", open(*is))],
            Frunk => vec![("vehicle_state.ft", open(true))],
            ChargePort { open } => vec![("charge_state.charge_port_door_open", json!(open))],
            Charge(on) => vec![(
                "charge_state.charging_state",
                json!(if *on { "Charging" } else { "Stopped" }),
            )],
            Defrost(on) => vec![("climate_state.is_front_defroster_on", json!(on))],
            Sentry(on) => vec![("vehicle_state.sentry_mode", json!(on))],
            SteeringWheelHeater(on) => vec![("climate_state.steering_wheel_heater", json!(on))],
            ChargeLimit(percent) => vec![("charge_state.charge_limit_soc", json!(percent))],
            ChargeCurrent(amps) => vec![("charge_state.charge_current_request", json!(amps))],
            Honk | Flash => Vec::new(),
            SeatHeater { seat, level } => SEATS
                .get(*seat)
                .map(|(_, _, field)| vec![(*field, json!(level))])
                .unwrap_or_default(),
        }
    }
}

/// What to do with a command before it goes, from [`CarCommand::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Send it.
    Send,
    /// The car is already as asked: send nothing.
    AlreadyDone,
    /// Read the car first (once it's awake for the command), then decide.
    LookFirst,
}

/// How recent, in seconds, a reading must be for the trunk toggle to go without reading the
/// car again.
pub const FRESH: u64 = 120;

/// Where a reading says whether the trunk is open.
const TRUNK_FIELD: &str = "vehicle_state.rt";

/// Where a reading says whether the frunk is open.
const FRUNK_FIELD: &str = "vehicle_state.ft";

/// A command to an energy site: plain REST, no signing.
#[derive(Debug, Clone, PartialEq)]
pub enum SiteCommand {
    BackupReserve(u8),
    /// One of [`MODES`].
    Mode(&'static str),
    StormWatch(bool),
}

impl SiteCommand {
    pub fn from_call(key: &str, service: &Service) -> Option<Result<SiteCommand, String>> {
        let command = match (key, service) {
            ("backup_reserve", Service::NumberSetValue(set)) => {
                match whole(set.value, 0..=100, "The backup reserve") {
                    Ok(percent) => SiteCommand::BackupReserve(percent),
                    Err(why) => return Some(Err(why)),
                }
            }
            ("operation_mode", Service::SelectSelectOption(choice)) => {
                match MODES.iter().find(|mode| **mode == choice.option) {
                    Some(mode) => SiteCommand::Mode(mode),
                    None => {
                        return Some(Err(format!("no operation mode called {}", choice.option)));
                    }
                }
            }
            ("storm_mode", Service::SwitchTurnOn) => SiteCommand::StormWatch(true),
            ("storm_mode", Service::SwitchTurnOff) => SiteCommand::StormWatch(false),
            _ => return None,
        };
        Some(Ok(command))
    }

    pub fn what(&self) -> String {
        match self {
            SiteCommand::BackupReserve(percent) => format!("Set the backup reserve to {percent}%"),
            SiteCommand::Mode(mode) => format!("Operation mode {}", mode.replace('_', " ")),
            SiteCommand::StormWatch(on) => {
                format!("Storm watch {}", if *on { "on" } else { "off" })
            }
        }
    }

    /// The endpoint, after `/api/1/energy_sites/{id}/`, and what it's sent.
    pub fn request(&self) -> (&'static str, Value) {
        match self {
            SiteCommand::BackupReserve(percent) => {
                ("backup", json!({ "backup_reserve_percent": percent }))
            }
            SiteCommand::Mode(mode) => ("operation", json!({ "default_real_mode": mode })),
            SiteCommand::StormWatch(on) => ("storm_mode", json!({ "enabled": on })),
        }
    }

    /// What the site's settings say once it's done.
    pub fn effects(&self) -> Vec<(&'static str, Value)> {
        match self {
            SiteCommand::BackupReserve(percent) => {
                vec![("backup_reserve_percent", json!(percent))]
            }
            SiteCommand::Mode(mode) => vec![("default_real_mode", json!(mode))],
            SiteCommand::StormWatch(on) => vec![("user_settings.storm_mode_enabled", json!(on))],
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{ClimatePresetMode, LockCode, NumberSetValue, SelectOption};

    use super::*;
    use crate::signed::tests::to_hex;

    fn bytes(command: CarCommand) -> (Domain, String) {
        let (domain, bytes) = command.message();
        (domain, to_hex(&bytes))
    }

    /// Byte for byte as Tesla's own library sends them. A oneof's zero is still written: unlock
    /// is `RKEAction = 0`, which a plain proto3 field would leave out.
    #[test]
    fn commands_are_the_bytes_the_car_expects() {
        use CarCommand::*;
        use Domain::*;
        assert_eq!(bytes(Lock(false)), (VehicleSecurity, "1000".into()));
        assert_eq!(bytes(Lock(true)), (VehicleSecurity, "1001".into()));
        assert_eq!(bytes(Climate(true)), (Infotainment, "120452020801".into()));
        // Off is power_on false, which proto3 leaves out: an empty HvacAutoAction.
        assert_eq!(bytes(Climate(false)), (Infotainment, "12025200".into()));
        assert_eq!(
            bytes(Trunk { open: true }),
            (VehicleSecurity, "22022801".into())
        );
        assert_eq!(bytes(Frunk), (VehicleSecurity, "22023001".into()));
        assert_eq!(
            bytes(ChargePort { open: false }),
            (VehicleSecurity, "22023804".into())
        );
        assert_eq!(bytes(Honk), (Infotainment, "1203da0100".into()));
        assert_eq!(bytes(Charge(true)), (Infotainment, "120432021200".into()));
        assert_eq!(
            bytes(ChargeLimit(80)),
            (Infotainment, "12042a020850".into())
        );
        // Front right seat, high: level HIGH (5) and seat FRONT_RIGHT (8), both empty.
        assert_eq!(
            bytes(SeatHeater { seat: 1, level: 3 }),
            (Infotainment, "1209a202060a042a004200".into())
        );
    }

    #[test]
    fn the_trunk_toggle_goes_only_to_a_trunk_lately_seen_shut() {
        let open = CarCommand::Trunk { open: true };
        let now = 1_000_000;
        let mut reading = crate::vehicle::CarReading::default();
        assert_eq!(
            open.check(&reading, now),
            Check::LookFirst,
            "unknown: look first"
        );
        reading.fields.insert("vehicle_state.rt".into(), json!(0));
        assert_eq!(
            open.check(&reading, now),
            Check::LookFirst,
            "never read live: look first"
        );
        reading.read_at = Some(now - FRESH);
        assert_eq!(open.check(&reading, now), Check::Send);
        reading.fields.insert("vehicle_state.rt".into(), json!(1));
        assert_eq!(
            open.check(&reading, now),
            Check::AlreadyDone,
            "already open: nothing sent"
        );
        // Shut a while ago: someone may have opened it since.
        reading.fields.insert("vehicle_state.rt".into(), json!(0));
        reading.read_at = Some(now - FRESH - 1);
        assert_eq!(open.check(&reading, now), Check::LookFirst);
        // Closing is CLOSE, not a toggle: always safe.
        assert_eq!(
            CarCommand::Trunk { open: false }.check(&reading, now),
            Check::Send
        );
        assert_eq!(open.failure_forgets(), Some("vehicle_state.rt"));
        assert_eq!(CarCommand::Lock(true).failure_forgets(), None);

        // The frunk is the same toggle, read from its own field.
        let frunk = CarCommand::Frunk;
        let mut reading = crate::vehicle::CarReading::default();
        assert_eq!(frunk.check(&reading, now), Check::LookFirst);
        reading.read_at = Some(now);
        reading.fields.insert("vehicle_state.rt".into(), json!(1));
        reading.fields.insert("vehicle_state.ft".into(), json!(0));
        assert_eq!(frunk.check(&reading, now), Check::Send);
        reading.fields.insert("vehicle_state.ft".into(), json!(1));
        assert_eq!(frunk.check(&reading, now), Check::AlreadyDone);
        assert_eq!(frunk.failure_forgets(), Some("vehicle_state.ft"));
        assert!(
            !open.repeatable() && !CarCommand::Frunk.repeatable(),
            "toggles go once"
        );
        assert!(
            CarCommand::Trunk { open: false }.repeatable(),
            "closing is CLOSE"
        );
    }

    #[test]
    fn calls_become_commands_and_bad_values_are_refused() {
        let call = |key, service| CarCommand::from_call(key, &service);
        assert_eq!(
            call("lock", Service::LockUnlock(LockCode::default())),
            Some(Ok(CarCommand::Lock(false)))
        );
        assert_eq!(
            call(
                "climate",
                Service::ClimateSetPresetMode(ClimatePresetMode {
                    preset_mode: "dog".into()
                })
            ),
            Some(Ok(CarCommand::Keeper(2)))
        );
        assert_eq!(
            call(
                "seat_heater_rear_center",
                Service::SelectSelectOption(SelectOption {
                    option: "medium".into()
                })
            ),
            Some(Ok(CarCommand::SeatHeater { seat: 3, level: 2 }))
        );
        assert!(matches!(
            call(
                "charge_limit",
                Service::NumberSetValue(NumberSetValue { value: 20.0 })
            ),
            Some(Err(_))
        ));
        // Refresh, Wake and sensors aren't commands.
        assert_eq!(call("refresh", Service::ButtonPress), None);
        assert_eq!(call("battery", Service::SwitchTurnOn), None);

        assert_eq!(
            SiteCommand::from_call(
                "operation_mode",
                &Service::SelectSelectOption(SelectOption {
                    option: "backup".into()
                })
            ),
            Some(Ok(SiteCommand::Mode("backup")))
        );
        assert_eq!(
            SiteCommand::BackupReserve(20).request(),
            ("backup", json!({ "backup_reserve_percent": 20 }))
        );
    }
}
