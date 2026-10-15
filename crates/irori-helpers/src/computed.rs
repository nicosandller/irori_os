//! Helpers worked out from other entities: a group of lights that's one light, a threshold on
//! a temperature, the energy used today, the hours the heating ran. Each reads its sources
//! through the home the core hands built-ins ([`irori_protocol::home::Home`]) and says what it
//! comes to as an ordinary entity, so a flow reads it like anything else.
//!
//! There's no template helper: each kind is a typed table, checked when it's written, and a
//! source that isn't there, or isn't a number where one is needed, is said on the helper
//! rather than guessed around.

use irori_protocol::types::{
    Availability, BinarySensorCapabilities, BinarySensorState, Capabilities, CoverCapabilities,
    CoverState, Entity, EntityId, EntityKind, EntityState, FanCapabilities, FanState,
    LightCapabilities, LightState, LockCapabilities, LockState, LockStatus, Name, ObjectId,
    OpenState, SensorCapabilities, SensorState, SensorValue, SensorValueType, SirenCapabilities,
    SirenState, State, StateClass, SwitchCapabilities, SwitchState, Timestamp, Typed,
    ValveCapabilities, ValveState,
};
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Made;
use crate::maths::{self, Integral, Measure, Meter, Period, Sample, Statistic};

/// `[groups.<id>]`: several entities of one kind as one.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub name: Name,
    /// What they are, and so what the group is: `light`, `switch`, `fan`, `cover`,
    /// `binary_sensor` or `sensor`.
    pub kind: GroupKind,
    /// Its members, by entity id. At least one.
    #[schemars(length(min = 1, max = 64))]
    pub members: Vec<EntityId>,
    /// On only while every member is (open while every member is, for covers). Otherwise on
    /// while any is.
    #[serde(default)]
    pub all: bool,
    /// For a group of sensors: what the readings come to. The average unless said.
    #[serde(default)]
    pub of: Option<SensorGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    Light,
    Switch,
    Fan,
    Cover,
    BinarySensor,
    Sensor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SensorGroup {
    Mean,
    Median,
    Min,
    Max,
    /// The most less the least.
    Range,
    Sum,
    /// Whichever changed last.
    Last,
}

/// `[combined.<id>]`: the readings of several sensors as one: their average, least, most…
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Combined {
    pub name: Name,
    /// The sensors (or numbers) it reads, by entity id. At least one.
    #[schemars(length(min = 1, max = 64))]
    pub sources: Vec<EntityId>,
    /// What their readings come to. The average unless said.
    #[serde(default)]
    pub of: Option<SensorGroup>,
}

/// `[switch_as.<id>]`: a switch shown as what it powers: a lamp's plug as a light, a fan's as
/// a fan. On is on, open, sounding or locked, and what the new one is told, the switch is.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchAs {
    pub name: Name,
    /// The switch: `switch.lamp_plug`.
    pub switch: EntityId,
    /// What it's shown as: `light`, `fan`, `cover`, `valve`, `lock` or `siren`.
    #[serde(rename = "as")]
    pub shown_as: ShownAs,
    /// The other way round: off is on, open or locked.
    #[serde(default)]
    pub invert: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShownAs {
    Light,
    Fan,
    Cover,
    Valve,
    Lock,
    Siren,
}

impl SwitchAs {
    /// The switch's action for what this was asked to do, or why a switch can't. `turn_on`
    /// for a light's `turn_on`, a cover's `open`, a lock's `lock`.
    pub(crate) fn action(&self, asked: &str) -> Result<&'static str, String> {
        let on = match asked {
            "turn_on" | "open" | "lock" => true,
            "turn_off" | "close" | "unlock" => false,
            other => {
                return Err(format!(
                    "a switch can only be turned on and off, not asked to {}",
                    other.replace('_', " ")
                ));
            }
        };
        Ok(if on != self.invert {
            "turn_on"
        } else {
            "turn_off"
        })
    }
}

/// `[thresholds.<id>]`: on while a number is above, below, or between lines.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Threshold {
    pub name: Name,
    pub source: EntityId,
    /// One of the source's values beside its main one, by name. Left out, its main value.
    #[serde(default)]
    pub field: Option<ObjectId>,
    #[serde(default)]
    pub above: Option<f64>,
    #[serde(default)]
    pub below: Option<f64>,
    /// How far past a line the reading has to go to change it, so one hovering there doesn't
    /// flick it on and off. None unless said.
    #[serde(default)]
    pub hysteresis: f64,
}

/// `[trends.<id>]`: on while a number is rising (or falling) by at least `by` over `over`
/// seconds.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trend {
    pub name: Name,
    pub source: EntityId,
    #[serde(default)]
    pub field: Option<ObjectId>,
    pub direction: Direction,
    pub by: f64,
    /// Seconds to look back over.
    pub over: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Rising,
    Falling,
}

/// `[meters.<id>]`: how much a counting reading (an energy total, a water meter) has gone up
/// since the hour, day, week, month or year began.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeterSettings {
    pub name: Name,
    pub source: EntityId,
    pub every: Period,
}

/// `[integrals.<id>]`: the area under a reading over time: power into energy.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntegralSettings {
    pub name: Name,
    pub source: EntityId,
    #[serde(default)]
    pub field: Option<ObjectId>,
    /// What it's per: watts over hours are watt-hours. Hours unless said.
    #[serde(default = "hours")]
    pub per: Per,
}

/// `[derivatives.<id>]`: how fast a reading changes, per hour (or minute, or second), over
/// the last `over` seconds.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DerivativeSettings {
    pub name: Name,
    pub source: EntityId,
    #[serde(default)]
    pub field: Option<ObjectId>,
    #[serde(default = "hours")]
    pub per: Per,
    /// Seconds to look back over.
    pub over: f64,
}

/// `[statistics.<id>]`: the average, least or most of a reading over the last `over` seconds.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatisticsSettings {
    pub name: Name,
    pub source: EntityId,
    #[serde(default)]
    pub field: Option<ObjectId>,
    pub of: Statistic,
    /// Seconds to look back over.
    pub over: f64,
}

/// `[history.<id>]`: how long, how often, or how much of the time something was a value, today
/// or over the last `over` seconds. "Hours the heating ran today".
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistorySettings {
    pub name: Name,
    pub source: EntityId,
    /// The value it counts: `true` for on, or a text such as `"heating"`.
    pub is: serde_json::Value,
    pub measure: Measure,
    /// Seconds to look back over. Left out, since midnight at home.
    #[serde(default)]
    pub over: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Per {
    Second,
    Minute,
    Hour,
}

fn hours() -> Per {
    Per::Hour
}

impl Per {
    fn seconds(self) -> f64 {
        match self {
            Self::Second => 1.0,
            Self::Minute => 60.0,
            Self::Hour => 3600.0,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Second => "s",
            Self::Minute => "min",
            Self::Hour => "h",
        }
    }
}

/// The longest any of them looks back: a week. History further than that is summaries.
const LONGEST: f64 = 7.0 * 24.0 * 3600.0;

/// A computed helper's settings, of whichever kind.
#[derive(Debug, Clone)]
pub(crate) enum Spec {
    Group(Group),
    Combined(Combined),
    SwitchAs(SwitchAs),
    Threshold(Threshold),
    Trend(Trend),
    Meter(MeterSettings),
    Integral(IntegralSettings),
    Derivative(DerivativeSettings),
    Statistics(StatisticsSettings),
    History(HistorySettings),
}

impl Spec {
    /// The entities it reads.
    pub(crate) fn sources(&self) -> Vec<&EntityId> {
        match self {
            Self::Group(group) => group.members.iter().collect(),
            Self::Combined(s) => s.sources.iter().collect(),
            Self::SwitchAs(s) => vec![&s.switch],
            Self::Threshold(s) => vec![&s.source],
            Self::Trend(s) => vec![&s.source],
            Self::Meter(s) => vec![&s.source],
            Self::Integral(s) => vec![&s.source],
            Self::Derivative(s) => vec![&s.source],
            Self::Statistics(s) => vec![&s.source],
            Self::History(s) => vec![&s.source],
        }
    }

    fn field(&self) -> Option<&ObjectId> {
        match self {
            Self::Threshold(s) => s.field.as_ref(),
            Self::Trend(s) => s.field.as_ref(),
            Self::Integral(s) => s.field.as_ref(),
            Self::Derivative(s) => s.field.as_ref(),
            Self::Statistics(s) => s.field.as_ref(),
            Self::Group(_)
            | Self::Combined(_)
            | Self::SwitchAs(_)
            | Self::Meter(_)
            | Self::History(_) => None,
        }
    }

    /// How far back it reads history to start from, for the kinds that look back.
    pub(crate) fn looks_back(&self, now: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
        match self {
            Self::Trend(s) => Some(maths::plus(now, -s.over)),
            Self::Derivative(s) => Some(maths::plus(now, -s.over)),
            Self::Statistics(s) => Some(maths::plus(now, -s.over)),
            Self::History(s) => Some(history_start(s, now, zone)),
            _ => None,
        }
    }

    /// What the settings say wrong, before any source is looked at.
    fn check(&self) -> Result<(), String> {
        let span = |over: f64| {
            if over.is_finite() && over > 0.0 && over <= LONGEST {
                Ok(())
            } else {
                Err(format!(
                    "`over` is seconds to look back, above nought and at most a week, not {over}"
                ))
            }
        };
        match self {
            Self::Group(group) => {
                if group.members.is_empty() {
                    return Err("a group needs at least one member".to_owned());
                }
                let wanted = group_kind(group.kind);
                if let Some(other) = group.members.iter().find(|member| member.kind() != wanted) {
                    return Err(format!(
                        "it's a group of {wanted}s, and {other} is a {}",
                        other.kind()
                    ));
                }
                if group.of.is_some() && group.kind != GroupKind::Sensor {
                    return Err("`of` is for a group of sensors".to_owned());
                }
                Ok(())
            }
            Self::Combined(s) => {
                if s.sources.is_empty() {
                    return Err("it needs at least one sensor to combine".to_owned());
                }
                if let Some(other) = s.sources.iter().find(|source| {
                    !matches!(source.kind(), EntityKind::Sensor | EntityKind::Number)
                }) {
                    return Err(format!(
                        "it combines sensors and numbers, and {other} is a {}",
                        other.kind()
                    ));
                }
                Ok(())
            }
            Self::SwitchAs(s) => {
                if s.switch.kind() == EntityKind::Switch {
                    Ok(())
                } else {
                    Err(format!(
                        "`switch` is a switch, and {} is a {}",
                        s.switch,
                        s.switch.kind()
                    ))
                }
            }
            Self::Threshold(s) => match (s.above, s.below) {
                (None, None) => Err("a threshold needs `above`, `below`, or both".to_owned()),
                (Some(above), Some(below)) if above >= below => Err(format!(
                    "`above` {above} has to be less than `below` {below}: it's on between them"
                )),
                _ if !s.hysteresis.is_finite() || s.hysteresis < 0.0 => {
                    Err("`hysteresis` is nought or more".to_owned())
                }
                _ => Ok(()),
            },
            Self::Trend(s) => {
                span(s.over)?;
                if s.by.is_finite() && s.by > 0.0 {
                    Ok(())
                } else {
                    Err("`by` is how much it has to change, above nought".to_owned())
                }
            }
            Self::Derivative(s) => span(s.over),
            Self::Statistics(s) => span(s.over),
            Self::History(s) => {
                if let Some(over) = s.over {
                    span(over)?;
                }
                match &s.is {
                    serde_json::Value::Bool(_) | serde_json::Value::String(_) => Ok(()),
                    other => Err(format!("`is` is `true`, `false` or a text, not {other}")),
                }
            }
            Self::Meter(_) | Self::Integral(_) => Ok(()),
        }
    }
}

fn group_kind(kind: GroupKind) -> EntityKind {
    match kind {
        GroupKind::Light => EntityKind::Light,
        GroupKind::Switch => EntityKind::Switch,
        GroupKind::Fan => EntityKind::Fan,
        GroupKind::Cover => EntityKind::Cover,
        GroupKind::BinarySensor => EntityKind::BinarySensor,
        GroupKind::Sensor => EntityKind::Sensor,
    }
}

fn number_sensor(unit: Option<String>, state_class: Option<StateClass>) -> Capabilities {
    Capabilities::Sensor(SensorCapabilities {
        value_type: SensorValueType::Number,
        device_class: None,
        unit,
        state_class,
        options: Vec::new(),
    })
}

/// What the helper is, given the unit its source reads in when that's known.
pub(crate) fn capabilities(spec: &Spec, unit: Option<&str>) -> Capabilities {
    let unit = unit.map(str::to_owned);
    let flag = Capabilities::BinarySensor(BinarySensorCapabilities { device_class: None });
    match spec {
        Spec::Group(group) => match group.kind {
            GroupKind::Light => Capabilities::Light(LightCapabilities {
                brightness: false,
                color_temp_kelvin: None,
                rgb: false,
            }),
            GroupKind::Switch => Capabilities::Switch(SwitchCapabilities { device_class: None }),
            GroupKind::Fan => Capabilities::Fan(FanCapabilities {
                speed_count: 0,
                oscillate: false,
                direction: false,
                preset_modes: Vec::new(),
            }),
            GroupKind::Cover => Capabilities::Cover(CoverCapabilities {
                device_class: None,
                position: false,
                tilt: false,
                stop: false,
            }),
            GroupKind::BinarySensor => flag,
            GroupKind::Sensor => number_sensor(unit, Some(StateClass::Measurement)),
        },
        Spec::Combined(_) => number_sensor(unit, Some(StateClass::Measurement)),
        Spec::SwitchAs(s) => match s.shown_as {
            ShownAs::Light => Capabilities::Light(LightCapabilities {
                brightness: false,
                color_temp_kelvin: None,
                rgb: false,
            }),
            ShownAs::Fan => Capabilities::Fan(FanCapabilities {
                speed_count: 0,
                oscillate: false,
                direction: false,
                preset_modes: Vec::new(),
            }),
            ShownAs::Cover => Capabilities::Cover(CoverCapabilities {
                device_class: None,
                position: false,
                tilt: false,
                stop: false,
            }),
            ShownAs::Valve => Capabilities::Valve(ValveCapabilities::default()),
            ShownAs::Lock => Capabilities::Lock(LockCapabilities {
                open: false,
                requires_code: false,
                code_format: None,
            }),
            ShownAs::Siren => Capabilities::Siren(SirenCapabilities::default()),
        },
        Spec::Threshold(_) | Spec::Trend(_) => flag,
        Spec::Meter(_) => number_sensor(unit, Some(StateClass::TotalIncreasing)),
        Spec::Integral(s) => number_sensor(
            unit.map(|unit| format!("{unit}{}", s.per.suffix())),
            Some(StateClass::Total),
        ),
        Spec::Derivative(s) => number_sensor(
            unit.map(|unit| format!("{unit}/{}", s.per.suffix())),
            Some(StateClass::Measurement),
        ),
        Spec::Statistics(_) => number_sensor(unit, Some(StateClass::Measurement)),
        Spec::History(s) => number_sensor(
            match s.measure {
                Measure::Time => Some("h".to_owned()),
                Measure::Ratio => Some("%".to_owned()),
                Measure::Count => None,
            },
            Some(StateClass::Measurement),
        ),
    }
}

/// The entity for a computed helper of kind `what` (its prefix) and its settings.
pub(crate) fn made(
    what: &str,
    id: &ObjectId,
    name: &Name,
    spec: Spec,
) -> Result<Vec<Made>, irori_protocol::ProtocolError> {
    spec.check()
        .map_err(|why| irori_protocol::ProtocolError::new(format!("the {what} `{id}`: {why}")))?;
    let capabilities = capabilities(&spec, None);
    Ok(vec![Made::computed(what, id, name, capabilities, spec)?])
}

/// When a history helper with no `over` starts looking: midnight at home.
fn history_start(s: &HistorySettings, now: Timestamp, zone: &TimeZone) -> Timestamp {
    match s.over {
        Some(over) => maths::plus(now, -over),
        None => maths::period_start(Period::Day, now, zone),
    }
}

/// A source's value as the helper reads it: its main one, or the field it names. `None` while
/// it has none to give.
pub(crate) fn value_of(state: &EntityState, field: Option<&ObjectId>) -> Option<Typed> {
    if state.availability == Availability::Unavailable {
        return None;
    }
    let state = state.state.as_ref()?;
    match field {
        Some(field) => state.field(field.as_str()),
        None => Some(state.primary()),
    }
}

fn number(value: Option<Typed>) -> Option<f64> {
    match value? {
        Typed::Number(n) => Some(n),
        _ => None,
    }
}

/// What a computed helper keeps between readings.
#[derive(Debug, Clone, Default)]
pub(crate) struct Running {
    /// Readings taken, oldest first, for the kinds that look back.
    pub samples: Vec<(Timestamp, Typed)>,
    /// Whether a threshold or trend was on, for hysteresis.
    pub on: Option<bool>,
    /// A meter's or an integral's running count, kept across restarts.
    pub kept: Option<Kept>,
}

/// What a meter or an integral keeps in the helpers' storage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Kept {
    Meter(Meter),
    Integral(Integral),
}

/// What a helper comes to now: its state, or why it has none.
pub(crate) type Outcome = Result<State, String>;

fn numbers(samples: &[(Timestamp, Typed)]) -> Vec<Sample> {
    samples
        .iter()
        .filter_map(|(at, value)| match value {
            Typed::Number(n) => Some((*at, *n)),
            _ => None,
        })
        .collect()
}

fn reading(value: f64) -> State {
    State::Sensor(SensorState {
        value: SensorValue::Number(maths::round(value, 2)),
    })
}

fn flag(on: bool) -> State {
    State::BinarySensor(BinarySensorState { on })
}

/// What it's waiting for, before its first reading.
fn waiting(source: &EntityId) -> String {
    format!("waiting for {source} to say what it is")
}

impl Running {
    /// Takes in a reading of its source, when it's one it reads.
    pub(crate) fn feed(
        &mut self,
        spec: &Spec,
        state: &EntityState,
        now: Timestamp,
        zone: &TimeZone,
    ) {
        let field = spec.field();
        let Some(value) = value_of(state, field) else {
            return;
        };
        match spec {
            Spec::Meter(s) => {
                let Some(n) = number(Some(value)) else { return };
                let meter = match &mut self.kept {
                    Some(Kept::Meter(meter)) => meter,
                    _ => {
                        self.kept = Some(Kept::Meter(Meter::new(maths::period_start(
                            s.every, now, zone,
                        ))));
                        let Some(Kept::Meter(meter)) = &mut self.kept else {
                            return;
                        };
                        meter
                    }
                };
                meter.roll(s.every, now, zone);
                meter.add(n);
            }
            Spec::Integral(s) => {
                let Some(n) = number(Some(value)) else { return };
                let mut integral = match self.kept {
                    Some(Kept::Integral(integral)) => integral,
                    _ => Integral::default(),
                };
                integral.add(state.last_updated, n, s.per.seconds());
                self.kept = Some(Kept::Integral(integral));
            }
            Spec::Group(_) | Spec::Combined(_) | Spec::SwitchAs(_) | Spec::Threshold(_) => {}
            _ => {
                // The same value again isn't a new reading for the kinds that look back.
                if self.samples.last().is_none_or(|(_, last)| *last != value) {
                    self.samples.push((state.last_updated, value));
                }
            }
        }
    }

    /// What it comes to at `now`. `states` finds a source's current state.
    pub(crate) fn outcome(
        &mut self,
        spec: &Spec,
        states: &dyn Fn(&EntityId) -> Option<EntityState>,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Outcome {
        // Readings older than anything needs are let go of, keeping the one that held at the
        // start of what's looked at.
        if let Some(start) = spec.looks_back(now, zone)
            && let Some(keep_from) = self.samples.iter().rposition(|(at, _)| *at <= start)
        {
            self.samples.drain(..keep_from);
        }
        match spec {
            Spec::Group(group) => group_outcome(group, states),
            Spec::Combined(s) => combined(&s.sources, s.of, states).map(reading),
            Spec::SwitchAs(s) => {
                let state = states(&s.switch).ok_or_else(|| waiting(&s.switch))?;
                let Some(Typed::Bool(on)) = value_of(&state, None) else {
                    return Err(format!("{} isn't saying whether it's on", s.switch));
                };
                Ok(shown_as(s.shown_as, on != s.invert))
            }
            Spec::Threshold(s) => {
                let state = states(&s.source).ok_or_else(|| waiting(&s.source))?;
                let value = number(value_of(&state, s.field.as_ref()))
                    .ok_or_else(|| format!("{} isn't saying a number", s.source))?;
                let on = maths::threshold(
                    value,
                    s.above,
                    s.below,
                    s.hysteresis,
                    self.on.unwrap_or(false),
                );
                self.on = Some(on);
                Ok(flag(on))
            }
            Spec::Trend(s) => {
                let samples = maths::window(&numbers(&self.samples), now, s.over);
                if samples.is_empty() {
                    return Err(waiting(&s.source));
                }
                Ok(flag(maths::trend(
                    &samples,
                    s.direction == Direction::Rising,
                    s.by,
                )))
            }
            Spec::Derivative(s) => {
                let samples = maths::window(&numbers(&self.samples), now, s.over);
                maths::derivative(&samples, s.per.seconds())
                    .map(reading)
                    .ok_or_else(|| format!("waiting for {} to change", s.source))
            }
            Spec::Statistics(s) => {
                let samples = maths::window(&numbers(&self.samples), now, s.over);
                maths::statistic(&samples, now, s.of)
                    .map(reading)
                    .ok_or_else(|| waiting(&s.source))
            }
            Spec::History(s) => {
                let start = history_start(s, now, zone);
                let wanted = match &s.is {
                    serde_json::Value::Bool(on) => Typed::Bool(*on),
                    serde_json::Value::String(text) => Typed::Text(text.clone()),
                    _ => return Err("`is` is `true`, `false` or a text".to_owned()),
                };
                if self.samples.is_empty() {
                    return Err(waiting(&s.source));
                }
                Ok(reading(maths::history(
                    &self.samples,
                    start,
                    now,
                    |value| *value == wanted,
                    s.measure,
                )))
            }
            Spec::Meter(s) => match &mut self.kept {
                Some(Kept::Meter(meter)) => {
                    meter.roll(s.every, now, zone);
                    Ok(reading(meter.used))
                }
                _ => Err(waiting(&s.source)),
            },
            Spec::Integral(s) => match self.kept {
                Some(Kept::Integral(integral)) => Ok(reading(integral.total)),
                _ => Err(waiting(&s.source)),
            },
        }
    }
}

fn group_outcome(group: &Group, states: &dyn Fn(&EntityId) -> Option<EntityState>) -> Outcome {
    let values: Vec<Typed> = group
        .members
        .iter()
        .filter_map(|member| states(member).and_then(|state| value_of(&state, None)))
        .collect();
    if values.is_empty() {
        return Err("none of its members has said anything yet".to_owned());
    }
    let ons: Vec<bool> = values
        .iter()
        .filter_map(|value| match value {
            Typed::Bool(on) => Some(*on),
            _ => None,
        })
        .collect();
    let on = if group.all {
        ons.iter().all(|on| *on)
    } else {
        ons.iter().any(|on| *on)
    };
    Ok(match group.kind {
        GroupKind::Light => State::Light(LightState {
            on,
            brightness: None,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        }),
        GroupKind::Switch => State::Switch(SwitchState { on }),
        GroupKind::Fan => State::Fan(FanState {
            on,
            percentage: None,
            oscillating: None,
            direction: None,
            preset_mode: None,
        }),
        GroupKind::BinarySensor => flag(on),
        GroupKind::Cover => {
            let said: Vec<OpenState> = values
                .iter()
                .filter_map(|value| match value {
                    Typed::Text(text) => OpenState::parse(text),
                    _ => None,
                })
                .collect();
            let any = |state: OpenState| said.contains(&state);
            let every = |state: OpenState| said.iter().all(|s| *s == state);
            let state = if any(OpenState::Opening) {
                OpenState::Opening
            } else if any(OpenState::Closing) {
                OpenState::Closing
            } else if group.all {
                if every(OpenState::Open) {
                    OpenState::Open
                } else {
                    OpenState::Closed
                }
            } else if every(OpenState::Closed) {
                OpenState::Closed
            } else {
                OpenState::Open
            };
            State::Cover(CoverState {
                state,
                position: None,
                tilt: None,
            })
        }
        GroupKind::Sensor => reading(combined(&group.members, group.of, states)?),
    })
}

/// What several readings come to, by `of` (the average unless said).
fn combined(
    sources: &[EntityId],
    of: Option<SensorGroup>,
    states: &dyn Fn(&EntityId) -> Option<EntityState>,
) -> Result<f64, String> {
    let mut said: Vec<(Timestamp, f64)> = sources
        .iter()
        .filter_map(|source| {
            let state = states(source)?;
            number(value_of(&state, None)).map(|n| (state.last_updated, n))
        })
        .collect();
    if said.is_empty() {
        return Err("none of them is saying a number yet".to_owned());
    }
    let mut numbers: Vec<f64> = said.iter().map(|(_, n)| *n).collect();
    let count = f64::from(u32::try_from(numbers.len()).unwrap_or(u32::MAX));
    let least = numbers.iter().copied().fold(f64::INFINITY, f64::min);
    let most = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok(match of.unwrap_or(SensorGroup::Mean) {
        SensorGroup::Mean => numbers.iter().sum::<f64>() / count,
        SensorGroup::Sum => numbers.iter().sum(),
        SensorGroup::Min => least,
        SensorGroup::Max => most,
        SensorGroup::Range => most - least,
        SensorGroup::Median => {
            numbers.sort_by(f64::total_cmp);
            let middle = numbers.len() / 2;
            if numbers.len().is_multiple_of(2) {
                f64::midpoint(numbers[middle - 1], numbers[middle])
            } else {
                numbers[middle]
            }
        }
        SensorGroup::Last => {
            said.sort_by_key(|(at, _)| *at);
            said.last().map_or(0.0, |(_, n)| *n)
        }
    })
}

/// A switch that's `on`, as what it's shown as.
fn shown_as(shown_as: ShownAs, on: bool) -> State {
    let open = if on {
        OpenState::Open
    } else {
        OpenState::Closed
    };
    match shown_as {
        ShownAs::Light => State::Light(LightState {
            on,
            brightness: None,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        }),
        ShownAs::Fan => State::Fan(FanState {
            on,
            percentage: None,
            oscillating: None,
            direction: None,
            preset_mode: None,
        }),
        ShownAs::Cover => State::Cover(CoverState {
            state: open,
            position: None,
            tilt: None,
        }),
        ShownAs::Valve => State::Valve(ValveState {
            state: open,
            position: None,
        }),
        ShownAs::Lock => State::Lock(LockState {
            state: if on {
                LockStatus::Locked
            } else {
                LockStatus::Unlocked
            },
        }),
        ShownAs::Siren => State::Siren(SirenState { on }),
    }
}

/// The unit its source reads in, from the registry, for the helpers whose value is in it.
pub(crate) fn source_unit(spec: &Spec, entities: &[Entity]) -> Option<String> {
    let source = match spec {
        Spec::Group(group) if group.kind == GroupKind::Sensor => group.members.first()?,
        Spec::Combined(s) => s.sources.first()?,
        Spec::Group(_)
        | Spec::SwitchAs(_)
        | Spec::History(_)
        | Spec::Threshold(_)
        | Spec::Trend(_) => return None,
        _ => spec.sources().into_iter().next()?,
    };
    // A field's unit isn't known: only the main value has one.
    if spec.field().is_some() {
        return None;
    }
    match &entities
        .iter()
        .find(|entity| &entity.id == source)?
        .capabilities
    {
        Capabilities::Sensor(sensor) => sensor.unit.clone(),
        Capabilities::Number(number) => number.unit.clone(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{Context, ContextId, Origin};

    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_790_000_000 + second).expect("an instant")
    }

    fn state(entity: &str, value: State, second: i64) -> EntityState {
        EntityState {
            entity_id: entity.parse().expect("an entity id"),
            availability: Availability::Available,
            state: Some(value),
            attributes: Default::default(),
            last_changed: at(second),
            last_updated: at(second),
            last_reported: at(second),
            context: Context {
                id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    fn lamp(entity: &str, on: bool) -> EntityState {
        state(
            entity,
            State::Light(LightState {
                on,
                brightness: None,
                color_mode: None,
                color_temp_kelvin: None,
                rgb: None,
            }),
            0,
        )
    }

    fn temperature(value: f64, second: i64) -> EntityState {
        state(
            "sensor.room",
            State::Sensor(SensorState {
                value: SensorValue::Number(value),
            }),
            second,
        )
    }

    fn group(all: bool) -> Group {
        Group {
            name: Name::try_from("Lounge lights").expect("valid"),
            kind: GroupKind::Light,
            members: vec![
                "light.a".parse().expect("id"),
                "light.b".parse().expect("id"),
            ],
            all,
            of: None,
        }
    }

    #[test]
    fn a_group_is_on_while_any_member_is_or_while_all_are() {
        let states = |entity: &EntityId| match entity.as_str() {
            "light.a" => Some(lamp("light.a", true)),
            "light.b" => Some(lamp("light.b", false)),
            _ => None,
        };
        let on =
            |outcome: Outcome| matches!(outcome, Ok(State::Light(LightState { on: true, .. })));
        assert!(on(group_outcome(&group(false), &states)));
        assert!(!on(group_outcome(&group(true), &states)));
        assert!(group_outcome(&group(false), &|_| None).is_err());
        // Members of another kind are refused when it's written.
        let mixed = Group {
            members: vec![
                "light.a".parse().expect("id"),
                "switch.b".parse().expect("id"),
            ],
            ..group(false)
        };
        let error = Spec::Group(mixed).check().expect_err("refused");
        assert!(error.contains("switch.b is a switch"), "{error}");
    }

    fn reading_of(entity: &str, value: f64, second: i64) -> EntityState {
        state(
            entity,
            State::Sensor(SensorState {
                value: SensorValue::Number(value),
            }),
            second,
        )
    }

    /// Several readings come to their average, middle, spread or latest.
    #[test]
    fn combined_readings_come_to_what_they_are_asked() {
        let readings = [
            reading_of("sensor.a", 20.0, 30),
            reading_of("sensor.b", 26.0, 10),
            reading_of("sensor.c", 21.0, 20),
            reading_of("sensor.d", 19.0, 0),
        ];
        let states = |id: &EntityId| readings.iter().find(|r| &r.entity_id == id).cloned();
        let sources: Vec<EntityId> = ["sensor.a", "sensor.b", "sensor.c", "sensor.d"]
            .iter()
            .map(|id| id.parse().expect("id"))
            .collect();
        let of = |of| combined(&sources, Some(of), &states).expect("a number");
        assert_eq!(combined(&sources, None, &states), Ok(21.5));
        assert_eq!(of(SensorGroup::Median), 20.5);
        assert_eq!(of(SensorGroup::Range), 7.0);
        assert_eq!(of(SensorGroup::Last), 20.0);
        assert_eq!(of(SensorGroup::Sum), 86.0);
        assert_eq!(
            combined(&sources[..3], Some(SensorGroup::Median), &states),
            Ok(21.0)
        );
        assert!(combined(&sources, None, &|_| None).is_err());

        let light = Spec::Combined(Combined {
            name: Name::try_from("Mixed").expect("valid"),
            sources: vec![
                "sensor.a".parse().expect("id"),
                "light.b".parse().expect("id"),
            ],
            of: None,
        });
        assert!(
            light
                .check()
                .expect_err("refused")
                .contains("light.b is a light")
        );
    }

    /// A switch shown as a light is on while it is; a lock, locked; and what each is asked
    /// becomes the switch's on or off, the other way round when inverted.
    #[test]
    fn a_switch_shown_as_another_kind_follows_and_is_told_through() {
        let shown = |shown_as, invert| SwitchAs {
            name: Name::try_from("Reading lamp").expect("valid"),
            switch: "switch.plug".parse().expect("id"),
            shown_as,
            invert,
        };
        let plug = state("switch.plug", State::Switch(SwitchState { on: true }), 0);
        let states = |id: &EntityId| (id.as_str() == "switch.plug").then(|| plug.clone());
        let mut running = Running::default();
        let zone = TimeZone::UTC;
        let outcome =
            |spec: Spec, running: &mut Running| running.outcome(&spec, &states, at(0), &zone);
        assert!(matches!(
            outcome(Spec::SwitchAs(shown(ShownAs::Light, false)), &mut running),
            Ok(State::Light(LightState { on: true, .. }))
        ));
        assert!(matches!(
            outcome(Spec::SwitchAs(shown(ShownAs::Lock, true)), &mut running),
            Ok(State::Lock(LockState {
                state: LockStatus::Unlocked
            }))
        ));
        assert!(matches!(
            outcome(Spec::SwitchAs(shown(ShownAs::Valve, false)), &mut running),
            Ok(State::Valve(ValveState {
                state: OpenState::Open,
                ..
            }))
        ));

        assert_eq!(
            shown(ShownAs::Light, false).action("turn_on"),
            Ok("turn_on")
        );
        assert_eq!(shown(ShownAs::Cover, false).action("close"), Ok("turn_off"));
        assert_eq!(shown(ShownAs::Lock, true).action("lock"), Ok("turn_off"));
        assert!(shown(ShownAs::Cover, false).action("set_position").is_err());

        let not_a_switch = SwitchAs {
            switch: "light.lamp".parse().expect("id"),
            ..shown(ShownAs::Light, false)
        };
        assert!(Spec::SwitchAs(not_a_switch).check().is_err());
    }

    #[test]
    fn a_threshold_follows_its_source_with_hysteresis() {
        let spec = Spec::Threshold(Threshold {
            name: Name::try_from("Hot").expect("valid"),
            source: "sensor.room".parse().expect("id"),
            field: None,
            above: Some(26.0),
            below: None,
            hysteresis: 0.5,
        });
        let zone = TimeZone::UTC;
        let mut running = Running::default();
        let read = |running: &mut Running, value: f64| {
            let now = temperature(value, 0);
            running.outcome(&spec, &|_| Some(now.clone()), at(0), &zone)
        };
        assert_eq!(read(&mut running, 26.7), Ok(flag(true)));
        assert_eq!(
            read(&mut running, 25.8),
            Ok(flag(true)),
            "not yet back past the line"
        );
        assert_eq!(read(&mut running, 25.4), Ok(flag(false)));
        let words = Running::default()
            .outcome(&spec, &|_| None, at(0), &zone)
            .expect_err("nothing yet");
        assert!(words.contains("waiting for sensor.room"), "{words}");
    }

    #[test]
    fn a_statistic_and_a_trend_look_back_over_their_window() {
        let zone = TimeZone::UTC;
        let source: EntityId = "sensor.room".parse().expect("id");
        let mean = Spec::Statistics(StatisticsSettings {
            name: Name::try_from("Average").expect("valid"),
            source: source.clone(),
            field: None,
            of: Statistic::Mean,
            over: 60.0,
        });
        let rising = Spec::Trend(Trend {
            name: Name::try_from("Warming").expect("valid"),
            source,
            field: None,
            direction: Direction::Rising,
            by: 1.0,
            over: 60.0,
        });
        let (mut a, mut b) = (Running::default(), Running::default());
        for (value, second) in [(20.0, 0), (22.0, 30)] {
            a.feed(&mean, &temperature(value, second), at(second), &zone);
            b.feed(&rising, &temperature(value, second), at(second), &zone);
        }
        assert_eq!(
            a.outcome(&mean, &|_| None, at(60), &zone),
            Ok(reading(21.0))
        );
        assert_eq!(b.outcome(&rising, &|_| None, at(60), &zone), Ok(flag(true)));
        // Long after, only the last reading is left in the window: no change, not rising.
        assert_eq!(
            b.outcome(&rising, &|_| None, at(500), &zone),
            Ok(flag(false))
        );
    }

    #[test]
    fn what_makes_no_sense_is_said_when_it_is_written() {
        let threshold = |above, below| {
            Spec::Threshold(Threshold {
                name: Name::try_from("T").expect("valid"),
                source: "sensor.room".parse().expect("id"),
                field: None,
                above,
                below,
                hysteresis: 0.0,
            })
            .check()
        };
        assert!(threshold(None, None).is_err());
        assert!(threshold(Some(30.0), Some(20.0)).is_err());
        assert!(threshold(Some(20.0), Some(30.0)).is_ok());
        let history = Spec::History(HistorySettings {
            name: Name::try_from("H").expect("valid"),
            source: "climate.hall".parse().expect("id"),
            is: serde_json::json!(3),
            measure: Measure::Time,
            over: None,
        });
        assert!(history.check().is_err());
    }
}
