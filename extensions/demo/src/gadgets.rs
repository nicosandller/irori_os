//! The demo's devices that do more than switch and measure: a front door lock that locks itself
//! again, a doorbell with a chime and a screen, a living room blind that takes a moment to move,
//! a ceiling fan, a water shut-off with a leak alarm, a thermostat, a hot water tank and a
//! dehumidifier. Between them, one of every kind beyond lights, switches and sensors.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{
    ButtonCapabilities, Capabilities, ClimateCapabilities, ClimateState, CoverCapabilities,
    CoverClass, CoverState, EntityCategory, EntityDescription, EventCapabilities, EventClass,
    EventState, FanCapabilities, FanDirection, FanState, HvacAction, HvacMode, LockCapabilities,
    LockState, LockStatus, Name, NumberCapabilities, NumberMode, NumberState, OpenState,
    SelectCapabilities, SelectState, SensorClass, Service, SirenCapabilities, SirenState, State,
    TextCapabilities, TextMode, TextState, ValveCapabilities, ValveClass, ValveState,
    percentage_to_speed, speed_to_percentage,
};
use irori_protocol::{ProtocolContext, ProtocolError};

use irori_protocol::types::{
    HumidifierAction, HumidifierCapabilities, HumidifierClass, HumidifierState, HumidityRange,
};
use irori_protocol::types::{WaterHeaterCapabilities, WaterHeaterMode, WaterHeaterState};

use crate::{device, id};

const LOCK: &str = "front-door-lock";
const LOCK_LOCK: &str = "front-door-lock-lock";
const LOCK_AUTO: &str = "front-door-lock-auto-lock";
const DOORBELL: &str = "doorbell";
const DOORBELL_RING: &str = "doorbell-ring";
const DOORBELL_CHIME: &str = "doorbell-chime";
const DOORBELL_MESSAGE: &str = "doorbell-message";
const BLIND: &str = "blind";
const BLIND_COVER: &str = "blind-cover";
const BLIND_CALIBRATE: &str = "blind-calibrate";
const FAN: &str = "ceiling-fan";
const FAN_FAN: &str = "ceiling-fan-fan";
const SHUTOFF: &str = "water-shutoff";
const SHUTOFF_VALVE: &str = "water-shutoff-valve";
const SHUTOFF_ALARM: &str = "water-shutoff-alarm";
const THERMOSTAT: &str = "thermostat";
const THERMOSTAT_CLIMATE: &str = "thermostat-climate";
const TANK: &str = "hot-water-tank";
const TANK_HEATER: &str = "hot-water-tank-heater";
const DRYER: &str = "dehumidifier";
const DRYER_HUMIDIFIER: &str = "dehumidifier-humidifier";

const CHIMES: [&str; 3] = ["Ding-dong", "Westminster", "Off"];
const FAN_SPEEDS: u16 = 3;
/// How far the blind moves between readings, in percent.
const BLIND_STEP: u8 = 20;
/// Someone at the door every two and a half minutes, ringing twice six seconds apart: two of
/// the same event in a row, each one its own occurrence.
const RING_EVERY: u64 = 150;
const RINGS: [u64; 2] = [60, 66];

/// What the devices are doing.
#[derive(Debug, Clone)]
pub(crate) struct Gadgets {
    lock: LockStatus,
    /// Seconds after unlocking before it locks again; 0 never.
    auto_lock: f64,
    unlocked_at: Option<Instant>,
    chime: String,
    message: String,
    blind: Blind,
    fan: FanState,
    valve: OpenState,
    alarm: bool,
    alarm_until: Option<Instant>,
    thermostat: ClimateState,
    /// The mode `turn_on` goes back to.
    thermostat_on: HvacMode,
    tank: WaterHeaterState,
    /// The mode it's in when its switch is on.
    tank_mode: WaterHeaterMode,
    dryer: HumidifierState,
}

fn dryer_capabilities() -> HumidifierCapabilities {
    HumidifierCapabilities {
        device_class: Some(HumidifierClass::Dehumidifier),
        humidity: HumidityRange {
            min: 35.0,
            max: 80.0,
        },
        modes: vec!["normal".into(), "sleep".into()],
    }
}

/// What a dehumidifier is doing in a room at `room` %: drying while it's above the target.
fn drying(dryer: &HumidifierState, room: f64) -> HumidifierAction {
    match dryer.target_humidity {
        _ if !dryer.on => HumidifierAction::Off,
        Some(target) if room > target => HumidifierAction::Drying,
        _ => HumidifierAction::Idle,
    }
}

/// A heat pump hot water tank with its own switch.
fn tank_capabilities() -> WaterHeaterCapabilities {
    WaterHeaterCapabilities {
        operation_modes: vec![
            WaterHeaterMode::Eco,
            WaterHeaterMode::HeatPump,
            WaterHeaterMode::Performance,
        ],
        min_temp: 40.0,
        max_temp: 65.0,
        temp_step: 1.0,
        target_temperature: true,
        on_off: true,
    }
}

/// The living room's thermostat: heats, or follows its schedule, in °C.
fn thermostat_capabilities() -> ClimateCapabilities {
    ClimateCapabilities {
        hvac_modes: vec![HvacMode::Off, HvacMode::Heat, HvacMode::Auto],
        min_temp: 7.0,
        max_temp: 30.0,
        temp_step: 0.5,
        target_temperature: true,
        target_temperature_range: false,
        target_humidity: None,
        fan_modes: Vec::new(),
        swing_modes: Vec::new(),
        preset_modes: vec!["comfort".into(), "eco".into(), "away".into()],
    }
}

/// What a thermostat is doing in a room at `room` °C: heating while it's below the target.
fn heating(thermostat: &ClimateState, room: f64) -> HvacAction {
    match (thermostat.hvac_mode, thermostat.target_temperature) {
        (HvacMode::Off, _) => HvacAction::Off,
        (_, Some(target)) if room < target => HvacAction::Heating,
        _ => HvacAction::Idle,
    }
}

/// Where the blind is, and where it's going.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Blind {
    position: u8,
    target: u8,
    tilt: u8,
    /// Where to go once it gets there: calibrating runs it down, then back up.
    then: Option<u8>,
}

impl Blind {
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

impl Gadgets {
    pub(crate) fn new() -> Self {
        Self {
            lock: LockStatus::Locked,
            auto_lock: 30.0,
            unlocked_at: None,
            chime: CHIMES[0].into(),
            message: "Parcels by the bench, please".into(),
            blind: Blind {
                position: 100,
                target: 100,
                tilt: 50,
                then: None,
            },
            fan: FanState {
                on: false,
                percentage: Some(speed_to_percentage(1, FAN_SPEEDS)),
                oscillating: None,
                direction: Some(FanDirection::Forward),
                preset_mode: None,
            },
            valve: OpenState::Open,
            alarm: false,
            alarm_until: None,
            thermostat: ClimateState {
                hvac_action: Some(HvacAction::Idle),
                current_temperature: Some(21.0),
                target_temperature: Some(21.0),
                preset_mode: Some("comfort".into()),
                ..ClimateState::in_mode(HvacMode::Heat)
            },
            thermostat_on: HvacMode::Heat,
            tank: WaterHeaterState {
                operation_mode: WaterHeaterMode::HeatPump,
                current_temperature: Some(50.0),
                target_temperature: Some(55.0),
            },
            tank_mode: WaterHeaterMode::HeatPump,
            dryer: HumidifierState {
                on: true,
                target_humidity: Some(50.0),
                current_humidity: Some(50.0),
                mode: Some("normal".into()),
                action: Some(HumidifierAction::Idle),
            },
        }
    }

    /// Every state worth reporting at the start. The doorbell has rung for nobody yet, and the
    /// calibrate button has no state.
    pub(crate) fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (LOCK_LOCK, State::Lock(LockState { state: self.lock })),
            (
                LOCK_AUTO,
                State::Number(NumberState {
                    value: self.auto_lock,
                }),
            ),
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
            (BLIND_COVER, State::Cover(self.blind.state())),
            (FAN_FAN, State::Fan(self.fan.clone())),
            (
                SHUTOFF_VALVE,
                State::Valve(ValveState {
                    state: self.valve,
                    position: None,
                }),
            ),
            (SHUTOFF_ALARM, State::Siren(SirenState { on: self.alarm })),
            (THERMOSTAT_CLIMATE, State::Climate(self.thermostat.clone())),
            (TANK_HEATER, State::WaterHeater(self.tank.clone())),
            (DRYER_HUMIDIFIER, State::Humidifier(self.dryer.clone())),
        ]
    }

    fn state_of(&self, entity: &'static str) -> (&'static str, State) {
        self.states()
            .into_iter()
            .find(|(unique_id, _)| *unique_id == entity)
            .expect("every entity that takes a call has a state")
    }

    /// Carries out a call to one of these devices: the entity whose state it changed, and that
    /// state. `None` when the call is for another of the demo's devices.
    pub(crate) fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Option<Result<(&'static str, State), String>> {
        let changed = match (unique_id, service) {
            (LOCK_LOCK, Service::LockLock(_)) => {
                self.lock = LockStatus::Locked;
                self.unlocked_at = None;
                LOCK_LOCK
            }
            (LOCK_LOCK, Service::LockUnlock(_)) => {
                self.lock = LockStatus::Unlocked;
                self.unlocked_at = Some(now);
                LOCK_LOCK
            }
            (LOCK_AUTO, Service::NumberSetValue(data)) => {
                self.auto_lock = data.value;
                LOCK_AUTO
            }
            (DOORBELL_CHIME, Service::SelectSelectOption(data)) => {
                self.chime.clone_from(&data.option);
                DOORBELL_CHIME
            }
            (DOORBELL_MESSAGE, Service::TextSetValue(data)) => {
                self.message.clone_from(&data.value);
                DOORBELL_MESSAGE
            }
            (BLIND_COVER, Service::CoverOpen) => {
                self.blind.go(100);
                BLIND_COVER
            }
            (BLIND_COVER, Service::CoverClose) => {
                self.blind.go(0);
                BLIND_COVER
            }
            (BLIND_COVER, Service::CoverStop) => {
                self.blind.go(self.blind.position);
                BLIND_COVER
            }
            (BLIND_COVER, Service::CoverSetPosition(data)) => {
                self.blind.go(data.position);
                BLIND_COVER
            }
            (BLIND_COVER, Service::CoverSetTilt(data)) => {
                self.blind.tilt = data.tilt;
                BLIND_COVER
            }
            // Down to the bottom to find it, then back to where it was.
            (BLIND_CALIBRATE, Service::ButtonPress) => {
                let back = self.blind.target;
                self.blind.go(0);
                self.blind.then = Some(back);
                BLIND_COVER
            }
            (FAN_FAN, Service::FanTurnOn(data)) => {
                self.fan.on = true;
                if let Some(percentage) = data.percentage {
                    self.fan_speed(percentage);
                }
                if data.preset_mode.is_some() {
                    self.fan.preset_mode.clone_from(&data.preset_mode);
                }
                FAN_FAN
            }
            (FAN_FAN, Service::FanTurnOff) => {
                self.fan.on = false;
                FAN_FAN
            }
            (FAN_FAN, Service::FanSetPercentage(data)) => {
                if data.percentage == 0 {
                    self.fan.on = false;
                } else {
                    self.fan.on = true;
                    self.fan_speed(data.percentage);
                }
                FAN_FAN
            }
            (FAN_FAN, Service::FanSetDirection(data)) => {
                self.fan.direction = Some(data.direction);
                FAN_FAN
            }
            (FAN_FAN, Service::FanSetPresetMode(data)) => {
                self.fan.on = true;
                self.fan.preset_mode = Some(data.preset_mode.clone());
                FAN_FAN
            }
            (SHUTOFF_VALVE, Service::ValveOpen) => {
                self.valve = OpenState::Open;
                SHUTOFF_VALVE
            }
            (SHUTOFF_VALVE, Service::ValveClose) => {
                self.valve = OpenState::Closed;
                SHUTOFF_VALVE
            }
            (SHUTOFF_ALARM, Service::SirenTurnOn(data)) => {
                self.alarm = true;
                self.alarm_until = data
                    .duration
                    .map(|secs| now + Duration::from_secs(secs.into()));
                SHUTOFF_ALARM
            }
            (SHUTOFF_ALARM, Service::SirenTurnOff) => {
                self.alarm = false;
                self.alarm_until = None;
                SHUTOFF_ALARM
            }
            (THERMOSTAT_CLIMATE, Service::ClimateSetHvacMode(data)) => {
                self.thermostat_mode(data.hvac_mode);
                THERMOSTAT_CLIMATE
            }
            (THERMOSTAT_CLIMATE, Service::ClimateTurnOn) => {
                self.thermostat_mode(self.thermostat_on);
                THERMOSTAT_CLIMATE
            }
            (THERMOSTAT_CLIMATE, Service::ClimateTurnOff) => {
                self.thermostat_mode(HvacMode::Off);
                THERMOSTAT_CLIMATE
            }
            (THERMOSTAT_CLIMATE, Service::ClimateSetTemperature(data)) => {
                if let Some(mode) = data.hvac_mode {
                    self.thermostat_mode(mode);
                }
                if data.temperature.is_some() {
                    self.thermostat.target_temperature = data.temperature;
                    // A target set by hand is no preset's.
                    self.thermostat.preset_mode = None;
                }
                self.thermostat.hvac_action = Some(heating(
                    &self.thermostat,
                    self.thermostat.current_temperature.unwrap_or(21.0),
                ));
                THERMOSTAT_CLIMATE
            }
            // Each preset is a target: comfortable, saving, or nobody home.
            (THERMOSTAT_CLIMATE, Service::ClimateSetPresetMode(data)) => {
                self.thermostat.target_temperature = Some(match data.preset_mode.as_str() {
                    "comfort" => 21.0,
                    "eco" => 18.5,
                    _ => 15.0,
                });
                self.thermostat.preset_mode = Some(data.preset_mode.clone());
                THERMOSTAT_CLIMATE
            }
            (DRYER_HUMIDIFIER, Service::HumidifierTurnOn | Service::HumidifierTurnOff) => {
                self.dryer.on = matches!(service, Service::HumidifierTurnOn);
                self.dryer_action();
                DRYER_HUMIDIFIER
            }
            (DRYER_HUMIDIFIER, Service::HumidifierSetHumidity(data)) => {
                self.dryer.target_humidity = Some(data.humidity);
                self.dryer_action();
                DRYER_HUMIDIFIER
            }
            (DRYER_HUMIDIFIER, Service::HumidifierSetMode(data)) => {
                self.dryer.mode = Some(data.mode.clone());
                DRYER_HUMIDIFIER
            }
            (TANK_HEATER, Service::WaterHeaterTurnOff) => {
                self.tank.operation_mode = WaterHeaterMode::Off;
                TANK_HEATER
            }
            (TANK_HEATER, Service::WaterHeaterTurnOn) => {
                self.tank.operation_mode = self.tank_mode;
                TANK_HEATER
            }
            (TANK_HEATER, Service::WaterHeaterSetOperationMode(data)) => {
                self.tank_mode = data.operation_mode;
                self.tank.operation_mode = data.operation_mode;
                TANK_HEATER
            }
            (TANK_HEATER, Service::WaterHeaterSetTemperature(data)) => {
                if let Some(mode) = data.operation_mode {
                    self.tank_mode = mode;
                    self.tank.operation_mode = mode;
                }
                self.tank.target_temperature = Some(data.temperature);
                TANK_HEATER
            }
            (
                LOCK_LOCK | LOCK_AUTO | DOORBELL_RING | DOORBELL_CHIME | DOORBELL_MESSAGE
                | BLIND_COVER | BLIND_CALIBRATE | FAN_FAN | SHUTOFF_VALVE | SHUTOFF_ALARM
                | THERMOSTAT_CLIMATE | TANK_HEATER | DRYER_HUMIDIFIER,
                _,
            ) => {
                return Some(Err(format!(
                    "the demo's `{unique_id}` can't {}",
                    service.name()
                )));
            }
            _ => return None,
        };
        Some(Ok(self.state_of(changed)))
    }

    fn dryer_action(&mut self) {
        let room = self.dryer.current_humidity.unwrap_or(50.0);
        self.dryer.action = Some(drying(&self.dryer, room));
    }

    fn thermostat_mode(&mut self, mode: HvacMode) {
        self.thermostat.hvac_mode = mode;
        if mode != HvacMode::Off {
            self.thermostat_on = mode;
        }
        self.thermostat.hvac_action = Some(heating(
            &self.thermostat,
            self.thermostat.current_temperature.unwrap_or(21.0),
        ));
    }

    /// A speed set by percentage lands on one of its three, as a real fan's would, and leaves
    /// any preset mode.
    fn fan_speed(&mut self, percentage: u8) {
        let speed = percentage_to_speed(percentage, FAN_SPEEDS).max(1);
        self.fan.percentage = Some(speed_to_percentage(speed, FAN_SPEEDS));
        self.fan.preset_mode = None;
    }

    /// What changed on its own by `now`, with the living room at `room` °C and `humidity` %:
    /// the blind moving, the lock locking itself again, the alarm running out, the thermostat
    /// and dehumidifier reading the room.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        room: f64,
        humidity: f64,
    ) -> Vec<(&'static str, State)> {
        let mut changed = Vec::new();
        let dried = self.dryer.clone();
        self.dryer.current_humidity = Some(humidity);
        self.dryer_action();
        if self.dryer != dried {
            changed.push(DRYER_HUMIDIFIER);
        }
        let before = self.thermostat.clone();
        self.thermostat.current_temperature = Some(room);
        self.thermostat.hvac_action = Some(heating(&self.thermostat, room));
        if self.thermostat != before {
            changed.push(THERMOSTAT_CLIMATE);
        }
        // The tank heats a degree a reading towards its target while it's on, and loses a little
        // to the room while it isn't.
        let water = self.tank.current_temperature.unwrap_or(50.0);
        let heated = match (self.tank.operation_mode, self.tank.target_temperature) {
            (WaterHeaterMode::Off, _) => (water - 0.2).max(20.0),
            (_, Some(target)) if water < target => (water + 1.0).min(target),
            _ => water,
        };
        if (heated - water).abs() > f64::EPSILON {
            self.tank.current_temperature = Some((heated * 10.0).round() / 10.0);
            changed.push(TANK_HEATER);
        }
        if self.blind.step() {
            changed.push(BLIND_COVER);
        }
        if let Some(since) = self.unlocked_at
            && self.auto_lock > 0.0
            && now >= since + Duration::from_secs_f64(self.auto_lock)
        {
            self.lock = LockStatus::Locked;
            self.unlocked_at = None;
            changed.push(LOCK_LOCK);
        }
        if self.alarm_until.is_some_and(|until| now >= until) {
            self.alarm = false;
            self.alarm_until = None;
            changed.push(SHUTOFF_ALARM);
        }
        changed.into_iter().map(|e| self.state_of(e)).collect()
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

pub(crate) async fn describe(ctx: &ProtocolContext) -> Result<(), ProtocolError> {
    let entity = |unique_id: &str,
                  name: Option<&str>,
                  device: &str,
                  capabilities: Capabilities,
                  entity_category: Option<EntityCategory>|
     -> Result<EntityDescription, ProtocolError> {
        Ok(EntityDescription {
            unique_id: id(unique_id)?,
            name: name.map(Name::try_from).transpose()?,
            device_unique_id: Some(id(device)?),
            suggested_object_id: None,
            capabilities,
            entity_category,
        })
    };

    ctx.describe_device(device(
        LOCK,
        "Demo front door lock",
        "Virtual smart lock",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(entity(
        LOCK_LOCK,
        None,
        LOCK,
        Capabilities::Lock(LockCapabilities::default()),
        None,
    )?)
    .await?;
    ctx.describe_entity(entity(
        LOCK_AUTO,
        Some("Lock again after"),
        LOCK,
        Capabilities::Number(NumberCapabilities {
            min: 0.0,
            max: 300.0,
            step: 10.0,
            unit: Some("s".into()),
            device_class: Some(SensorClass::Duration),
            mode: NumberMode::Box,
        }),
        Some(EntityCategory::Config),
    )?)
    .await?;

    ctx.describe_device(device(
        DOORBELL,
        "Demo doorbell",
        "Virtual doorbell",
        "Hallway",
    )?)
    .await?;
    ctx.describe_entity(entity(
        DOORBELL_RING,
        Some("Ring"),
        DOORBELL,
        Capabilities::Event(EventCapabilities {
            event_types: vec!["ring".into()],
            device_class: Some(EventClass::Doorbell),
        }),
        None,
    )?)
    .await?;
    ctx.describe_entity(entity(
        DOORBELL_MESSAGE,
        Some("Screen message"),
        DOORBELL,
        Capabilities::Text(TextCapabilities {
            min_length: 0,
            max_length: 40,
            pattern: None,
            mode: TextMode::Text,
        }),
        None,
    )?)
    .await?;
    ctx.describe_entity(entity(
        DOORBELL_CHIME,
        Some("Chime"),
        DOORBELL,
        Capabilities::Select(SelectCapabilities {
            options: CHIMES.iter().map(|&c| c.into()).collect(),
        }),
        Some(EntityCategory::Config),
    )?)
    .await?;

    ctx.describe_device(device(
        BLIND,
        "Demo living room blind",
        "Virtual venetian blind",
        "Living room",
    )?)
    .await?;
    ctx.describe_entity(entity(
        BLIND_COVER,
        None,
        BLIND,
        Capabilities::Cover(CoverCapabilities {
            device_class: Some(CoverClass::Blind),
            position: true,
            tilt: true,
            stop: true,
        }),
        None,
    )?)
    .await?;
    ctx.describe_entity(entity(
        BLIND_CALIBRATE,
        Some("Calibrate"),
        BLIND,
        Capabilities::Button(ButtonCapabilities::default()),
        Some(EntityCategory::Config),
    )?)
    .await?;

    ctx.describe_device(device(
        FAN,
        "Demo ceiling fan",
        "Virtual ceiling fan",
        "Living room",
    )?)
    .await?;
    ctx.describe_entity(entity(
        FAN_FAN,
        None,
        FAN,
        Capabilities::Fan(FanCapabilities {
            speed_count: FAN_SPEEDS,
            oscillate: false,
            direction: true,
            preset_modes: vec!["breeze".into()],
        }),
        None,
    )?)
    .await?;

    ctx.describe_device(device(
        THERMOSTAT,
        "Demo thermostat",
        "Virtual thermostat",
        "Living room",
    )?)
    .await?;
    ctx.describe_entity(entity(
        THERMOSTAT_CLIMATE,
        None,
        THERMOSTAT,
        Capabilities::Climate(thermostat_capabilities()),
        None,
    )?)
    .await?;

    ctx.describe_device(device(
        DRYER,
        "Demo dehumidifier",
        "Virtual dehumidifier",
        "Living room",
    )?)
    .await?;
    ctx.describe_entity(entity(
        DRYER_HUMIDIFIER,
        None,
        DRYER,
        Capabilities::Humidifier(dryer_capabilities()),
        None,
    )?)
    .await?;

    ctx.describe_device(device(
        TANK,
        "Demo hot water tank",
        "Virtual heat pump water heater",
        "Kitchen",
    )?)
    .await?;
    ctx.describe_entity(entity(
        TANK_HEATER,
        None,
        TANK,
        Capabilities::WaterHeater(tank_capabilities()),
        None,
    )?)
    .await?;

    ctx.describe_device(device(
        SHUTOFF,
        "Demo water shut-off",
        "Virtual water valve with leak alarm",
        "Kitchen",
    )?)
    .await?;
    ctx.describe_entity(entity(
        SHUTOFF_VALVE,
        Some("Main water"),
        SHUTOFF,
        Capabilities::Valve(ValveCapabilities {
            device_class: Some(ValveClass::Water),
            position: false,
            stop: false,
        }),
        None,
    )?)
    .await?;
    ctx.describe_entity(entity(
        SHUTOFF_ALARM,
        Some("Leak alarm"),
        SHUTOFF,
        Capabilities::Siren(SirenCapabilities {
            tones: vec!["beep".into(), "alarm".into()],
            volume: true,
            duration: true,
        }),
        None,
    )?)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{
        FanPercentage, LockCode, NumberSetValue, SetPosition, SirenTurnOn,
    };

    use super::*;

    /// What each entity is, as `describe` says.
    fn capabilities() -> Vec<(&'static str, Capabilities)> {
        vec![
            (LOCK_LOCK, Capabilities::Lock(LockCapabilities::default())),
            (
                LOCK_AUTO,
                Capabilities::Number(NumberCapabilities {
                    min: 0.0,
                    max: 300.0,
                    step: 10.0,
                    unit: None,
                    device_class: None,
                    mode: NumberMode::Box,
                }),
            ),
            (
                DOORBELL_CHIME,
                Capabilities::Select(SelectCapabilities {
                    options: CHIMES.iter().map(|&c| c.into()).collect(),
                }),
            ),
            (
                DOORBELL_MESSAGE,
                Capabilities::Text(TextCapabilities {
                    min_length: 0,
                    max_length: 40,
                    pattern: None,
                    mode: TextMode::Text,
                }),
            ),
            (
                BLIND_COVER,
                Capabilities::Cover(CoverCapabilities {
                    device_class: None,
                    position: true,
                    tilt: true,
                    stop: true,
                }),
            ),
            (
                FAN_FAN,
                Capabilities::Fan(FanCapabilities {
                    speed_count: FAN_SPEEDS,
                    oscillate: false,
                    direction: true,
                    preset_modes: vec!["breeze".into()],
                }),
            ),
            (
                SHUTOFF_VALVE,
                Capabilities::Valve(ValveCapabilities::default()),
            ),
            (
                SHUTOFF_ALARM,
                Capabilities::Siren(SirenCapabilities {
                    tones: vec!["beep".into(), "alarm".into()],
                    volume: true,
                    duration: true,
                }),
            ),
            (
                THERMOSTAT_CLIMATE,
                Capabilities::Climate(thermostat_capabilities()),
            ),
            (TANK_HEATER, Capabilities::WaterHeater(tank_capabilities())),
            (
                DRYER_HUMIDIFIER,
                Capabilities::Humidifier(dryer_capabilities()),
            ),
            (
                DOORBELL_RING,
                Capabilities::Event(EventCapabilities {
                    event_types: vec!["ring".into()],
                    device_class: None,
                }),
            ),
        ]
    }

    /// What a tick said about one entity.
    fn about(ticked: Vec<(&'static str, State)>, entity: &str) -> Vec<(&'static str, State)> {
        ticked.into_iter().filter(|(id, _)| *id == entity).collect()
    }

    /// Irori refuses a report that doesn't fit what the entity said it is; here that would only
    /// be a line in the log, so every state is checked against its entity.
    fn assert_fits(states: &[(&'static str, State)]) {
        let caps = capabilities();
        for (unique_id, state) in states {
            let (_, cap) = caps
                .iter()
                .find(|(id, _)| id == unique_id)
                .expect("described");
            assert_eq!(cap.fits(state), Ok(()), "{unique_id}");
        }
    }

    #[test]
    fn every_state_fits_its_entity() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        assert_fits(&gadgets.states());
        for (unique_id, service) in [
            (
                BLIND_COVER,
                Service::CoverSetPosition(SetPosition { position: 35 }),
            ),
            (BLIND_CALIBRATE, Service::ButtonPress),
            (
                FAN_FAN,
                Service::FanSetPercentage(FanPercentage { percentage: 50 }),
            ),
            (
                SHUTOFF_ALARM,
                Service::SirenTurnOn(SirenTurnOn {
                    duration: Some(10),
                    ..SirenTurnOn::default()
                }),
            ),
        ] {
            let Some(Ok(state)) = gadgets.call(unique_id, &service, now) else {
                panic!("{unique_id} takes {}", service.name());
            };
            assert_fits(&[state]);
        }
        for second in 0..20 {
            assert_fits(&gadgets.tick(now + Duration::from_secs(second), 20.0, 50.0));
        }
        assert_fits(&rings(0, 1000));
    }

    #[test]
    fn the_lock_locks_itself_again_unless_told_not_to() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        let unlock = Service::LockUnlock(LockCode::default());
        gadgets.call(LOCK_LOCK, &unlock, now);
        assert!(
            about(
                gadgets.tick(now + Duration::from_secs(29), 21.0, 50.0),
                LOCK_LOCK
            )
            .is_empty()
        );
        let relocked = about(
            gadgets.tick(now + Duration::from_secs(30), 21.0, 50.0),
            LOCK_LOCK,
        );
        assert_eq!(
            relocked,
            vec![(
                LOCK_LOCK,
                State::Lock(LockState {
                    state: LockStatus::Locked
                })
            )]
        );

        let never = Service::NumberSetValue(NumberSetValue { value: 0.0 });
        gadgets.call(LOCK_AUTO, &never, now);
        gadgets.call(LOCK_LOCK, &unlock, now);
        assert!(
            about(
                gadgets.tick(now + Duration::from_secs(3600), 21.0, 50.0),
                LOCK_LOCK
            )
            .is_empty()
        );
        assert_eq!(gadgets.lock, LockStatus::Unlocked);
    }

    #[test]
    fn the_blind_takes_a_moment_and_calibrating_comes_back() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        let Some(Ok((_, State::Cover(moving)))) = gadgets.call(
            BLIND_COVER,
            &Service::CoverSetPosition(SetPosition { position: 40 }),
            now,
        ) else {
            panic!("the blind moves");
        };
        assert_eq!(
            (moving.state, moving.position),
            (OpenState::Closing, Some(100))
        );
        for _ in 0..3 {
            gadgets.tick(now, 21.0, 50.0);
        }
        assert_eq!(gadgets.blind.state().state, OpenState::Open);
        assert_eq!(gadgets.blind.position, 40);
        assert!(
            about(gadgets.tick(now, 21.0, 50.0), BLIND_COVER).is_empty(),
            "still once it's there"
        );

        gadgets.call(BLIND_CALIBRATE, &Service::ButtonPress, now);
        let mut lowest = 100;
        for _ in 0..10 {
            gadgets.tick(now, 21.0, 50.0);
            lowest = lowest.min(gadgets.blind.position);
        }
        assert_eq!((lowest, gadgets.blind.position), (0, 40));
    }

    #[test]
    fn the_fan_lands_on_one_of_its_speeds() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        gadgets.call(
            FAN_FAN,
            &Service::FanSetPercentage(FanPercentage { percentage: 50 }),
            now,
        );
        assert!(gadgets.fan.on);
        assert_eq!(gadgets.fan.percentage, Some(66));
        gadgets.call(
            FAN_FAN,
            &Service::FanSetPercentage(FanPercentage { percentage: 0 }),
            now,
        );
        assert!(!gadgets.fan.on);
        assert_eq!(gadgets.fan.percentage, Some(66), "comes back on at it");
    }

    #[test]
    fn the_alarm_stops_when_its_time_is_up() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        let sound = Service::SirenTurnOn(SirenTurnOn {
            tone: Some("beep".into()),
            volume_level: None,
            duration: Some(5),
        });
        gadgets.call(SHUTOFF_ALARM, &sound, now);
        assert!(gadgets.alarm);
        let alarm = |ticked| about(ticked, SHUTOFF_ALARM);
        assert!(alarm(gadgets.tick(now + Duration::from_secs(4), 21.0, 50.0)).is_empty());
        assert_eq!(
            alarm(gadgets.tick(now + Duration::from_secs(5), 21.0, 50.0)).len(),
            1
        );
        assert!(!gadgets.alarm);
    }

    #[test]
    fn the_doorbell_rings_twice_every_so_often_and_not_at_the_start() {
        assert!(rings(0, 0).is_empty());
        assert!(rings(0, 59).is_empty());
        assert_eq!(rings(58, 60).len(), 1);
        assert_eq!(rings(58, 66).len(), 2);
        assert_eq!(rings(0, 300).len(), 4);
        assert!(rings(66, 209).is_empty());
        assert_eq!(rings(209, 211).len(), 1);
    }

    #[test]
    fn the_thermostat_heats_a_cold_room_and_comes_back_on_as_it_was() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        let cold = gadgets.tick(now, 19.0, 50.0);
        assert_fits(&cold);
        assert_eq!(gadgets.thermostat.hvac_action, Some(HvacAction::Heating));
        gadgets.tick(now, 22.0, 50.0);
        assert_eq!(gadgets.thermostat.hvac_action, Some(HvacAction::Idle));
        assert!(
            about(gadgets.tick(now, 22.0, 50.0), THERMOSTAT_CLIMATE).is_empty(),
            "nothing new to say"
        );

        let auto = Service::ClimateSetHvacMode(irori_protocol::types::ClimateHvacMode {
            hvac_mode: HvacMode::Auto,
        });
        gadgets.call(THERMOSTAT_CLIMATE, &auto, now);
        gadgets.call(THERMOSTAT_CLIMATE, &Service::ClimateTurnOff, now);
        assert_eq!(gadgets.thermostat.hvac_action, Some(HvacAction::Off));
        let Some(Ok(on)) = gadgets.call(THERMOSTAT_CLIMATE, &Service::ClimateTurnOn, now) else {
            panic!("it turns on");
        };
        assert_fits(&[on]);
        assert_eq!(gadgets.thermostat.hvac_mode, HvacMode::Auto);

        let away = Service::ClimateSetPresetMode(irori_protocol::types::ClimatePresetMode {
            preset_mode: "away".into(),
        });
        gadgets.call(THERMOSTAT_CLIMATE, &away, now);
        assert_eq!(gadgets.thermostat.target_temperature, Some(15.0));
    }

    #[test]
    fn the_tank_heats_while_on_and_cools_while_off() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        gadgets.tick(now, 21.0, 50.0);
        assert_eq!(gadgets.tank.current_temperature, Some(51.0));
        let Some(Ok(off)) = gadgets.call(TANK_HEATER, &Service::WaterHeaterTurnOff, now) else {
            panic!("it switches off");
        };
        assert_fits(&[off]);
        assert_fits(&gadgets.tick(now, 21.0, 50.0));
        assert_eq!(gadgets.tank.current_temperature, Some(50.8));
        gadgets.call(TANK_HEATER, &Service::WaterHeaterTurnOn, now);
        assert_eq!(gadgets.tank.operation_mode, WaterHeaterMode::HeatPump);
    }

    #[test]
    fn the_dehumidifier_dries_a_damp_room() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        assert_fits(&gadgets.tick(now, 21.0, 64.0));
        assert_eq!(gadgets.dryer.action, Some(HumidifierAction::Drying));
        gadgets.tick(now, 21.0, 45.0);
        assert_eq!(gadgets.dryer.action, Some(HumidifierAction::Idle));
        let Some(Ok(off)) = gadgets.call(DRYER_HUMIDIFIER, &Service::HumidifierTurnOff, now) else {
            panic!("it turns off");
        };
        assert_fits(&[off]);
        assert_eq!(gadgets.dryer.action, Some(HumidifierAction::Off));
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
            gadgets.call(BLIND_COVER, &Service::ValveOpen, Instant::now()),
            Some(Err(_))
        ));
    }
}
