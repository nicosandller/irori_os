//! Helpers: values Irori keeps itself rather than a device reporting them, for flows to use and
//! for people to set from a page. A switch for "guests are over", a number for how bright the
//! evening should be, a button for "movie time".
//!
//! Defined in `extensions/helpers.toml` in the config directory, a table per kind of helper:
//!
//! ```toml
//! [toggles.guests_over]
//! name = "Guests are over"
//!
//! [numbers.evening_brightness]
//! name = "Evening brightness"
//! min = 0
//! max = 100
//! unit = "%"
//!
//! [counters.doorbell_today]
//! name = "Doorbell rings today"
//!
//! [selects.house_mode]
//! name = "House mode"
//! options = ["home", "away", "asleep"]
//!
//! [texts.hall_display]
//! name = "Hall display message"
//!
//! [buttons.movie_time]
//! name = "Movie time"
//!
//! [times.wake_up]
//! name = "Wake up"
//! initial = "06:45"
//! ```
//!
//! Every helper is an ordinary entity of a kind Irori already has, with no device: a toggle is
//! `switch.<id>`, a number and a counter `number.<id>`, a choice `select.<id>`, a text
//! `text.<id>`, a button `button.<id>` with `event.<id>` for flows to start from, and a date, a
//! time and a moment `date.<id>`, `time.<id>` and `datetime.<id>`. So nothing
//! that reads or sets an entity needs to know a helper from a device. Each kind has its own
//! module, which says what it's made of; this one keeps the values and answers the calls.
//!
//! A value is kept in the protocol's own storage (`docs/specs/protocols.md` §5), so it survives
//! Irori restarting, and this protocol restarting, which is how a new or removed helper
//! arrives.

mod button;
mod computed;
mod counter;
mod guide;
mod maths;
mod moment;
mod number;
mod schedule;
mod select;
mod text;
mod timer;
mod toggle;

use std::collections::{BTreeMap, BTreeSet};

use irori_protocol::types::{
    Capabilities, ContextId, EntityDescription, EntityId, EntityState, EventState, Name, ObjectId,
    ScheduleState, Service, State, StateReport, TimerState, Timestamp, Typed, UniqueId,
};
use irori_protocol::{Protocol, ProtocolContext, ProtocolError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;

pub use button::Button;
pub use computed::{
    DerivativeSettings, Group, HistorySettings, IntegralSettings, MeterSettings,
    StatisticsSettings, Threshold, Trend,
};
pub use counter::Counter;
pub use guide::{GUIDE, Guide, guide};
pub use moment::{Date, Datetime, Time};
pub use number::Number;
pub use schedule::Schedule;
pub use select::Select;
pub use text::Text;
pub use timer::Timer;
pub use toggle::Toggle;

/// The helpers protocol.
#[derive(Debug)]
pub struct Helpers;

/// `extensions/helpers.toml`: each kind of helper in its own table, by the id its entity gets.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// On/off values: `switch.<id>`.
    #[serde(default)]
    pub toggles: BTreeMap<ObjectId, Toggle>,
    /// Values within a range: `number.<id>`.
    #[serde(default)]
    pub numbers: BTreeMap<ObjectId, Number>,
    /// Numbers that are counted up and down: `number.<id>`.
    #[serde(default)]
    pub counters: BTreeMap<ObjectId, Counter>,
    /// One of a list: `select.<id>`.
    #[serde(default)]
    pub selects: BTreeMap<ObjectId, Select>,
    /// A few words: `text.<id>`.
    #[serde(default)]
    pub texts: BTreeMap<ObjectId, Text>,
    /// Something to press: `button.<id>`, and `event.<id>` for when it was.
    #[serde(default)]
    pub buttons: BTreeMap<ObjectId, Button>,
    /// A calendar day: `date.<id>`.
    #[serde(default)]
    pub dates: BTreeMap<ObjectId, Date>,
    /// A time of day: `time.<id>`.
    #[serde(default)]
    pub times: BTreeMap<ObjectId, Time>,
    /// A day and a time: `datetime.<id>`.
    #[serde(default)]
    pub datetimes: BTreeMap<ObjectId, Datetime>,
    /// Something that counts down: `timer.<id>`, and `event.<id>` for when it starts and ends.
    #[serde(default)]
    pub timers: BTreeMap<ObjectId, Timer>,
    /// On during the times a week sets out: `schedule.<id>`.
    #[serde(default)]
    pub schedules: BTreeMap<ObjectId, Schedule>,
    /// Several entities of one kind as one: `<kind>.<id>`.
    #[serde(default)]
    pub groups: BTreeMap<ObjectId, Group>,
    /// On while a number is past a line: `binary_sensor.<id>`.
    #[serde(default)]
    pub thresholds: BTreeMap<ObjectId, Threshold>,
    /// On while a number is rising or falling: `binary_sensor.<id>`.
    #[serde(default)]
    pub trends: BTreeMap<ObjectId, Trend>,
    /// What a counting reading has gone up by this hour, day, week, month or year:
    /// `sensor.<id>`.
    #[serde(default)]
    pub meters: BTreeMap<ObjectId, MeterSettings>,
    /// The area under a reading over time, power into energy: `sensor.<id>`.
    #[serde(default)]
    pub integrals: BTreeMap<ObjectId, IntegralSettings>,
    /// How fast a reading changes: `sensor.<id>`.
    #[serde(default)]
    pub derivatives: BTreeMap<ObjectId, DerivativeSettings>,
    /// A reading's average, least or most over a while: `sensor.<id>`.
    #[serde(default)]
    pub statistics: BTreeMap<ObjectId, StatisticsSettings>,
    /// How long, how often or how much of the time something was a value: `sensor.<id>`.
    #[serde(default)]
    pub history: BTreeMap<ObjectId, HistorySettings>,
}

impl Protocol for Helpers {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        run(settings, ctx).await
    }
}

/// A kind of helper: where it's defined, and what it's called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelperKind {
    /// Its table in `extensions/helpers.toml`, and its part of the API's path: `toggles`.
    pub table: &'static str,
    /// What one is called, in a sentence: `toggle`.
    pub word: &'static str,
    /// What its entities' unique ids begin with: `toggle` for `toggle-<id>`. The first is the
    /// entity a person thinks of as the helper.
    pub prefixes: &'static [&'static str],
}

/// Every kind of helper there is, in the order pages offer them.
pub const KINDS: &[HelperKind] = &[
    HelperKind {
        table: "toggles",
        word: "toggle",
        prefixes: &["toggle"],
    },
    HelperKind {
        table: "numbers",
        word: "number",
        prefixes: &["number"],
    },
    HelperKind {
        table: "counters",
        word: "counter",
        prefixes: &["counter"],
    },
    HelperKind {
        table: "selects",
        word: "choice",
        prefixes: &["select"],
    },
    HelperKind {
        table: "texts",
        word: "text",
        prefixes: &["text"],
    },
    HelperKind {
        table: "buttons",
        word: "button",
        prefixes: &["button", "pressed"],
    },
    HelperKind {
        table: "dates",
        word: "date",
        prefixes: &["date"],
    },
    HelperKind {
        table: "times",
        word: "time",
        prefixes: &["time"],
    },
    HelperKind {
        table: "datetimes",
        word: "date and time",
        prefixes: &["datetime"],
    },
    HelperKind {
        table: "timers",
        word: "timer",
        prefixes: &["timer", "timerevent"],
    },
    HelperKind {
        table: "schedules",
        word: "schedule",
        prefixes: &["schedule"],
    },
    HelperKind {
        table: "groups",
        word: "group",
        prefixes: &["group"],
    },
    HelperKind {
        table: "thresholds",
        word: "threshold",
        prefixes: &["threshold"],
    },
    HelperKind {
        table: "trends",
        word: "trend",
        prefixes: &["trend"],
    },
    HelperKind {
        table: "meters",
        word: "meter",
        prefixes: &["meter"],
    },
    HelperKind {
        table: "integrals",
        word: "integral",
        prefixes: &["integral"],
    },
    HelperKind {
        table: "derivatives",
        word: "derivative",
        prefixes: &["derivative"],
    },
    HelperKind {
        table: "statistics",
        word: "statistic",
        prefixes: &["statistics"],
    },
    HelperKind {
        table: "history",
        word: "history",
        prefixes: &["history"],
    },
];

impl HelperKind {
    /// The kind kept in `table`.
    pub fn of_table(table: &str) -> Option<&'static Self> {
        KINDS.iter().find(|kind| kind.table == table)
    }

    /// The kind of helper an entity with this unique id belongs to, its id in the kind's table,
    /// and whether this is the entity a person thinks of as the helper (a button's `button`,
    /// not the `event` that says it was pressed).
    pub fn of_unique(unique: &str) -> Option<(&'static Self, &str, bool)> {
        let (prefix, id) = unique.split_once('-')?;
        KINDS.iter().find_map(|kind| {
            let at = kind.prefixes.iter().position(|p| *p == prefix)?;
            Some((kind, id, at == 0))
        })
    }

    /// Checks one helper of this kind as it would be written in the file, and says which
    /// entities it would be: `["button.movie", "event.movie"]`. `Err` says what's wrong with
    /// it, in a sentence.
    pub fn check(
        &self,
        id: &ObjectId,
        settings: &serde_json::Value,
    ) -> Result<Vec<String>, String> {
        fn read<T: serde::de::DeserializeOwned>(settings: &serde_json::Value) -> Result<T, String> {
            serde_json::from_value(settings.clone()).map_err(|e| e.to_string())
        }
        let made = match self.table {
            "toggles" => toggle::made(id, &read(settings)?),
            "numbers" => number::made(id, &read(settings)?),
            "counters" => counter::made(id, &read(settings)?),
            "selects" => select::made(id, &read(settings)?),
            "texts" => text::made(id, &read(settings)?),
            "buttons" => button::made(id, &read(settings)?),
            "dates" => moment::date(id, &read(settings)?),
            "times" => moment::time(id, &read(settings)?),
            "datetimes" => moment::datetime(id, &read(settings)?),
            "timers" => timer::made(id, &read(settings)?),
            "schedules" => schedule::made(id, &read(settings)?),
            "groups" => computed_made(id, computed::Spec::Group(read(settings)?)),
            "thresholds" => computed_made(id, computed::Spec::Threshold(read(settings)?)),
            "trends" => computed_made(id, computed::Spec::Trend(read(settings)?)),
            "meters" => computed_made(id, computed::Spec::Meter(read(settings)?)),
            "integrals" => computed_made(id, computed::Spec::Integral(read(settings)?)),
            "derivatives" => computed_made(id, computed::Spec::Derivative(read(settings)?)),
            "statistics" => computed_made(id, computed::Spec::Statistics(read(settings)?)),
            "history" => computed_made(id, computed::Spec::History(read(settings)?)),
            other => return Err(format!("there are no helpers called `{other}`")),
        }
        .map_err(|e| e.to_string())?;
        for one in &made {
            if let Role::Value {
                initial: Some(initial),
            } = &one.role
                && one.holding(initial).is_none()
            {
                return Err(format!(
                    "`initial` isn't something this {} can be",
                    self.word
                ));
            }
        }
        Ok(made.iter().map(Made::entity_id).collect())
    }
}

/// One entity a helper is, and how it behaves.
pub(crate) struct Made {
    /// `<what>-<id>`: `toggle-guests_over`. Which helper it is, for the pages too.
    pub unique: UniqueId,
    pub description: EntityDescription,
    pub role: Role,
}

pub(crate) enum Role {
    /// Holds a value someone sets, kept through restarts. `initial` until they have, and
    /// unknown until then for one that has none (a date nobody has picked).
    Value { initial: Option<Typed> },
    /// Is pressed, and each press is reported on the entity `fires`.
    Press { fires: UniqueId },
    /// Reports things happening; nothing can be asked of it.
    Happens,
    /// Counts down from `duration` seconds, and says what happens to it on the entity `fires`.
    Timer {
        fires: UniqueId,
        duration: f64,
        restore: bool,
    },
    /// On during its week's times; nothing can be asked of it.
    Schedule(schedule::Week),
    /// Worked out from other entities. A group passes what it's asked on to its members.
    Computed(Box<computed::Spec>),
}

impl Made {
    fn new(
        what: &str,
        id: &ObjectId,
        name: &Name,
        capabilities: Capabilities,
        role: Role,
    ) -> Result<Self, ProtocolError> {
        let unique = UniqueId::try_from(format!("{what}-{id}"))?;
        Ok(Self {
            description: EntityDescription {
                unique_id: unique.clone(),
                name: Some(name.clone()),
                device_unique_id: None,
                // `switch.guests_over`: a helper's id is chosen by a person, so it reads well
                // in flows.
                suggested_object_id: Some(id.clone()),
                capabilities,
                entity_category: None,
            },
            unique,
            role,
        })
    }

    pub(crate) fn value(
        what: &str,
        id: &ObjectId,
        name: &Name,
        capabilities: Capabilities,
        initial: impl Into<Option<Typed>>,
    ) -> Result<Self, ProtocolError> {
        Self::new(
            what,
            id,
            name,
            capabilities,
            Role::Value {
                initial: initial.into(),
            },
        )
    }

    pub(crate) fn press(
        what: &str,
        id: &ObjectId,
        name: &Name,
        capabilities: Capabilities,
        fires: UniqueId,
    ) -> Result<Self, ProtocolError> {
        Self::new(what, id, name, capabilities, Role::Press { fires })
    }

    pub(crate) fn timer(
        id: &ObjectId,
        name: &Name,
        capabilities: Capabilities,
        role: Role,
    ) -> Result<Self, ProtocolError> {
        Self::new("timer", id, name, capabilities, role)
    }

    pub(crate) fn new_schedule(
        id: &ObjectId,
        name: &Name,
        capabilities: Capabilities,
        week: schedule::Week,
    ) -> Result<Self, ProtocolError> {
        Self::new("schedule", id, name, capabilities, Role::Schedule(week))
    }

    pub(crate) fn computed(
        what: &str,
        id: &ObjectId,
        name: &Name,
        capabilities: Capabilities,
        spec: computed::Spec,
    ) -> Result<Self, ProtocolError> {
        Self::new(what, id, name, capabilities, Role::Computed(Box::new(spec)))
    }

    pub(crate) fn happens(
        what: &str,
        id: &ObjectId,
        name: &Name,
        capabilities: Capabilities,
    ) -> Result<Self, ProtocolError> {
        Self::new(what, id, name, capabilities, Role::Happens)
    }

    /// The entity id it asks for: `switch.guests_over`.
    fn entity_id(&self) -> String {
        format!(
            "{}.{}",
            self.description.capabilities.kind(),
            self.description
                .suggested_object_id
                .as_ref()
                .map_or("", ObjectId::as_str)
        )
    }

    /// The state it's in when it holds `value`, if it can hold it: a number still in its range,
    /// a choice still among its options.
    fn holding(&self, value: &Typed) -> Option<State> {
        let capabilities = &self.description.capabilities;
        let state = State::with_primary(capabilities.kind(), None, value)?;
        capabilities.fits(&state).is_ok().then_some(state)
    }
}

/// Where the list of entities this protocol last described is kept, so one whose helper has
/// been deleted from the file can be removed rather than left behind.
const DESCRIBED: &str = "described";

/// Where an entity's value is kept: `toggle.guests_over` for `toggle-guests_over`.
fn value_key(unique: &str) -> String {
    unique.replacen('-', ".", 1)
}

/// Checks the whole of `extensions/helpers.toml` as it would be written, for what one helper
/// alone can't show: two that would be one entity, or helpers that read each other in a ring.
pub fn check_settings(file: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let settings: Settings = serde_json::from_value(serde_json::Value::Object(file.clone()))
        .map_err(|e| e.to_string())?;
    all(&settings).map(|_| ()).map_err(|e| e.to_string())
}

/// A computed helper, named for its prefix.
fn computed_made(id: &ObjectId, spec: computed::Spec) -> Result<Vec<Made>, ProtocolError> {
    let (what, name) = match &spec {
        computed::Spec::Group(s) => ("group", &s.name),
        computed::Spec::Threshold(s) => ("threshold", &s.name),
        computed::Spec::Trend(s) => ("trend", &s.name),
        computed::Spec::Meter(s) => ("meter", &s.name),
        computed::Spec::Integral(s) => ("integral", &s.name),
        computed::Spec::Derivative(s) => ("derivative", &s.name),
        computed::Spec::Statistics(s) => ("statistics", &s.name),
        computed::Spec::History(s) => ("history", &s.name),
    };
    let name = name.clone();
    computed::made(what, id, &name, spec)
}

/// Every entity the settings ask for. Two helpers that would be the same entity (a number and a
/// counter with one id are both `number.<id>`) are refused, naming both.
fn all(settings: &Settings) -> Result<Vec<Made>, ProtocolError> {
    let mut made = Vec::new();
    for (id, toggle) in &settings.toggles {
        made.extend(toggle::made(id, toggle)?);
    }
    for (id, number) in &settings.numbers {
        made.extend(number::made(id, number)?);
    }
    for (id, counter) in &settings.counters {
        made.extend(counter::made(id, counter)?);
    }
    for (id, select) in &settings.selects {
        made.extend(select::made(id, select)?);
    }
    for (id, text) in &settings.texts {
        made.extend(text::made(id, text)?);
    }
    for (id, button) in &settings.buttons {
        made.extend(button::made(id, button)?);
    }
    for (id, date) in &settings.dates {
        made.extend(moment::date(id, date)?);
    }
    for (id, time) in &settings.times {
        made.extend(moment::time(id, time)?);
    }
    for (id, moment) in &settings.datetimes {
        made.extend(moment::datetime(id, moment)?);
    }
    for (id, counting) in &settings.timers {
        made.extend(timer::made(id, counting)?);
    }
    for (id, week) in &settings.schedules {
        made.extend(schedule::made(id, week)?);
    }
    let computed = settings
        .groups
        .iter()
        .map(|(id, s)| (id, computed::Spec::Group(s.clone())))
        .chain(
            settings
                .thresholds
                .iter()
                .map(|(id, s)| (id, computed::Spec::Threshold(s.clone()))),
        )
        .chain(
            settings
                .trends
                .iter()
                .map(|(id, s)| (id, computed::Spec::Trend(s.clone()))),
        )
        .chain(
            settings
                .meters
                .iter()
                .map(|(id, s)| (id, computed::Spec::Meter(s.clone()))),
        )
        .chain(
            settings
                .integrals
                .iter()
                .map(|(id, s)| (id, computed::Spec::Integral(s.clone()))),
        )
        .chain(
            settings
                .derivatives
                .iter()
                .map(|(id, s)| (id, computed::Spec::Derivative(s.clone()))),
        )
        .chain(
            settings
                .statistics
                .iter()
                .map(|(id, s)| (id, computed::Spec::Statistics(s.clone()))),
        )
        .chain(
            settings
                .history
                .iter()
                .map(|(id, s)| (id, computed::Spec::History(s.clone()))),
        );
    for (id, spec) in computed {
        made.extend(computed_made(id, spec)?);
    }
    let mut taken: BTreeMap<String, &UniqueId> = BTreeMap::new();
    for one in &made {
        let entity = one.entity_id();
        if let Some(other) = taken.insert(entity.clone(), &one.unique) {
            return Err(ProtocolError::new(format!(
                "two helpers would both be `{entity}`: `{other}` and `{}`. Give one another id",
                one.unique
            )));
        }
    }
    no_loops(&made)?;
    Ok(made)
}

/// Helpers that read each other in a ring would each wait on the other for ever: refused,
/// naming the ring.
fn no_loops(made: &[Made]) -> Result<(), ProtocolError> {
    let reads: BTreeMap<String, Vec<String>> = made
        .iter()
        .filter_map(|one| match &one.role {
            Role::Computed(spec) => Some((
                one.entity_id(),
                spec.sources().iter().map(|id| id.to_string()).collect(),
            )),
            _ => None,
        })
        .collect();
    fn visit(
        at: &str,
        reads: &BTreeMap<String, Vec<String>>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if let Some(from) = path.iter().position(|seen| seen == at) {
            let mut ring = path[from..].to_vec();
            ring.push(at.to_owned());
            return Some(ring);
        }
        path.push(at.to_owned());
        for next in reads.get(at).into_iter().flatten() {
            if let Some(ring) = visit(next, reads, path) {
                return Some(ring);
            }
        }
        path.pop();
        None
    }
    for start in reads.keys() {
        if let Some(ring) = visit(start, &reads, &mut Vec::new()) {
            return Err(ProtocolError::new(format!(
                "these helpers read each other in a ring: {}",
                ring.join(" → ")
            )));
        }
    }
    Ok(())
}

async fn run(settings: Settings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
    let helpers: BTreeMap<UniqueId, Made> = all(&settings)?
        .into_iter()
        .map(|made| (made.unique.clone(), made))
        .collect();

    // What was here last time and isn't now goes.
    let before: BTreeSet<String> = ctx
        .load(DESCRIBED)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    for gone in before
        .iter()
        .filter(|unique| !helpers.keys().any(|kept| kept.as_str() == unique.as_str()))
    {
        // Removing one that's already gone (a person deleted it by hand) is nothing to fail over.
        let _ = ctx.remove_entity(UniqueId::try_from(gone.as_str())?).await;
        ctx.forget(&value_key(gone)).await?;
    }

    // Each timer as it is now, by its entity.
    let mut timers: BTreeMap<UniqueId, TimerState> = BTreeMap::new();
    // Each schedule as it is now, and the home's clock they keep to. A new time zone reaches
    // them when the helpers next start.
    let zone = schedule::zone(
        ctx.home()
            .and_then(|home| home.place().time_zone)
            .as_ref()
            .map(|zone| zone.as_str()),
    );
    let mut schedules: BTreeMap<UniqueId, ScheduleState> = BTreeMap::new();
    // The helpers worked out from other entities: what each keeps, the home's states as they
    // last were, and which ones have been described with their source's unit.
    let home = ctx.home();
    // Watching first, so nothing that changes from here on is missed, the helpers' own
    // entities included: a group of toggles reads them like any other.
    let mut changes = home.as_ref().map(|home| home.watch());
    let entities = home
        .as_ref()
        .map(|home| home.entities())
        .unwrap_or_default();
    let mut latest: BTreeMap<EntityId, EntityState> = home
        .as_ref()
        .map(|home| home.states())
        .unwrap_or_default()
        .into_iter()
        .map(|state| (state.entity_id.clone(), state))
        .collect();
    let mut running: BTreeMap<UniqueId, computed::Running> = BTreeMap::new();
    let mut said: BTreeMap<UniqueId, computed::Outcome> = BTreeMap::new();
    let mut with_unit: BTreeSet<UniqueId> = BTreeSet::new();
    for (unique, helper) in &helpers {
        if let Role::Computed(spec) = &helper.role {
            let unit = computed::source_unit(spec, &entities);
            if unit.is_some() {
                with_unit.insert(unique.clone());
            }
            let mut description = helper.description.clone();
            description.capabilities = computed::capabilities(spec, unit.as_deref());
            ctx.describe_entity(description).await?;
            let mut run = computed::Running {
                kept: ctx
                    .load(&value_key(unique.as_str()))
                    .await?
                    .and_then(|value| serde_json::from_value(value).ok()),
                ..computed::Running::default()
            };
            // The kinds that look back start from the history they look back over.
            if let (Some(home), Some(since)) = (&home, spec_since(spec, now(), &zone)) {
                let at = now();
                for source in spec.sources() {
                    for state in home.history(source, since) {
                        run.feed(spec, &state, at, &zone);
                    }
                }
            }
            running.insert(unique.clone(), run);
            continue;
        }
        ctx.describe_entity(helper.description.clone()).await?;
        if let Role::Timer {
            fires,
            duration,
            restore,
        } = &helper.role
        {
            let kept = ctx
                .load(&value_key(unique.as_str()))
                .await?
                .and_then(|value| serde_json::from_value(value).ok());
            let (state, said) = timer::restored(kept, *duration, *restore, now());
            keep_timer(&ctx, unique, &state).await?;
            ctx.report_state(report(unique.clone(), State::Timer(state.clone()), None));
            if let Some(said) = said {
                ctx.report_state(happened(fires, said, None));
            }
            timers.insert(unique.clone(), state);
            continue;
        }
        if let Role::Schedule(week) = &helper.role {
            let state = schedule::state(week, &zone, now());
            ctx.report_state(report(unique.clone(), State::Schedule(state), None));
            schedules.insert(unique.clone(), state);
            continue;
        }
        let Role::Value { initial } = &helper.role else {
            continue;
        };
        // What it was left at, unless it can't be that any more (its range was narrowed, the
        // option taken away): then it's as if nobody had set it.
        let kept = ctx
            .load(&value_key(unique.as_str()))
            .await?
            .and_then(|value| typed(&value))
            .filter(|value| helper.holding(value).is_some());
        match kept.or_else(|| initial.clone()) {
            // Nobody has set it and it starts as nothing: it's described, and unknown.
            None => {}
            Some(value) => match helper.holding(&value) {
                Some(state) => ctx.report_state(report(unique.clone(), state, None)),
                None => tracing::warn!(
                    helper = %unique,
                    "its `initial` isn't a value it can have, so it has none until it's set"
                ),
            },
        }
    }
    let described: Vec<&str> = helpers.keys().map(UniqueId::as_str).collect();
    ctx.store(DESCRIBED, serde_json::json!(described)).await?;
    if home.is_none() && !running.is_empty() {
        tracing::warn!("helpers worked out from other entities can't read the home");
    }
    for (unique, helper) in &helpers {
        if let (Role::Computed(spec), Some(run)) = (&helper.role, running.get_mut(unique)) {
            let outcome = run.outcome(spec, &|id: &EntityId| latest.get(id).cloned(), now(), &zone);
            tell(&ctx, unique, &outcome, &mut said);
        }
    }
    // The kinds that look back over time change as time passes, readings or not.
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // The next timer to run out, if one is running.
        let next = timers
            .values()
            .filter_map(|state| state.finishes_at)
            .chain(schedules.values().filter_map(|state| state.next_change))
            .min();
        let incoming = tokio::select! {
            incoming = ctx.next_call() => match incoming {
                Some(incoming) => incoming,
                None => break,
            },
            Some(changed) = next_change(&mut changes) => {
                let entity = changed.entity_id.clone();
                latest.insert(entity.clone(), changed.clone());
                let at = now();
                for (unique, helper) in &helpers {
                    let Role::Computed(spec) = &helper.role else { continue };
                    if !spec.sources().contains(&&entity) {
                        continue;
                    }
                    // Its source has come since it was described: now its unit is known.
                    if !with_unit.contains(unique)
                        && let Some(home) = &home
                        && let Some(unit) = computed::source_unit(spec, &home.entities())
                    {
                        let mut description = helper.description.clone();
                        description.capabilities = computed::capabilities(spec, Some(&unit));
                        let _ = ctx.describe_entity(description).await;
                        with_unit.insert(unique.clone());
                    }
                    let Some(run) = running.get_mut(unique) else { continue };
                    let kept = run.kept;
                    run.feed(spec, &changed, at, &zone);
                    let outcome = run.outcome(spec, &|id: &EntityId| latest.get(id).cloned(), at, &zone);
                    if run.kept != kept && let Some(now_kept) = run.kept {
                        let value = serde_json::to_value(now_kept).unwrap_or(serde_json::Value::Null);
                        ctx.store(&value_key(unique.as_str()), value).await?;
                    }
                    tell(&ctx, unique, &outcome, &mut said);
                }
                continue;
            }
            _ = tick.tick() => {
                let at = now();
                for (unique, helper) in &helpers {
                    if let (Role::Computed(spec), Some(run)) = (&helper.role, running.get_mut(unique)) {
                        let outcome = run.outcome(spec, &|id: &EntityId| latest.get(id).cloned(), at, &zone);
                        tell(&ctx, unique, &outcome, &mut said);
                    }
                }
                continue;
            }
            () = sleep_until(next) => {
                let at = now();
                for (unique, state) in &mut schedules {
                    if !state.next_change.is_some_and(|change| change <= at) {
                        continue;
                    }
                    if let Some(Role::Schedule(week)) = helpers.get(unique).map(|h| &h.role) {
                        *state = schedule::state(week, &zone, at);
                        ctx.report_state(report(unique.clone(), State::Schedule(*state), None));
                    }
                }
                for (unique, state) in &mut timers {
                    if !timer::due(state, at) {
                        continue;
                    }
                    *state = TimerState::idle(state.duration);
                    keep_timer(&ctx, unique, state).await?;
                    ctx.report_state(report(unique.clone(), State::Timer(state.clone()), None));
                    if let Some(Role::Timer { fires, .. }) = helpers.get(unique).map(|h| &h.role) {
                        ctx.report_state(happened(fires, timer::FINISHED, None));
                    }
                }
                continue;
            }
        };
        let unique = incoming.call.unique_id.clone();
        let caused_by = Some(incoming.call.context.id.clone());
        let Some(helper) = helpers.get(&unique) else {
            let why = format!("`{unique}` isn't a helper any more");
            incoming.reply(Err(ServiceError::failed(why)));
            continue;
        };
        let service = &incoming.call.service;
        match &helper.role {
            Role::Value { .. } => {
                // The core checked the call against the entity; a helper checks again, since
                // what it keeps is what it'll come back as after a restart.
                let asked = helper
                    .description
                    .capabilities
                    .supports(service)
                    .map_err(|why| format!("`{unique}` {why}"))
                    .and_then(|()| {
                        let value = service
                            .asks_for()
                            .ok_or_else(|| format!("`{unique}` can't do `{}`", service.name()))?;
                        let state = helper
                            .holding(&value)
                            .ok_or_else(|| format!("`{unique}` can't be that"))?;
                        Ok((value, state))
                    });
                let (value, state) = match asked {
                    Ok(asked) => asked,
                    Err(why) => {
                        incoming.reply(Err(ServiceError::failed(why)));
                        continue;
                    }
                };
                // Kept first: a value that was reported but not kept would come back wrong
                // after a restart, which is exactly what a helper mustn't do.
                if let Err(e) = ctx
                    .store(&value_key(unique.as_str()), value.to_json())
                    .await
                {
                    incoming.reply(Err(ServiceError::failed(format!(
                        "couldn't keep the value: {e}"
                    ))));
                    continue;
                }
                incoming.reply(Ok(()));
                ctx.report_state(report(unique, state, caused_by));
            }
            Role::Press { fires } => {
                if !matches!(service, Service::ButtonPress) {
                    incoming.reply(Err(ServiceError::failed("a button is only pressed")));
                    continue;
                }
                incoming.reply(Ok(()));
                ctx.report_state(happened(fires, button::PRESSED, caused_by));
            }
            Role::Timer {
                fires, duration, ..
            } => {
                let before = timers
                    .get(&unique)
                    .cloned()
                    .unwrap_or_else(|| TimerState::idle(*duration));
                let (state, said) = match timer::apply(&before, service, now()) {
                    Ok(done) => done,
                    Err(why) => {
                        incoming.reply(Err(ServiceError::failed(format!("`{unique}` {why}"))));
                        continue;
                    }
                };
                // Kept first, as a value is: a timer that came back from a restart counting
                // something it had been told to stop would be worse than one that didn't start.
                if let Err(e) = keep_timer(&ctx, &unique, &state).await {
                    incoming.reply(Err(ServiceError::failed(format!(
                        "couldn't keep the timer: {e}"
                    ))));
                    continue;
                }
                incoming.reply(Ok(()));
                timers.insert(unique.clone(), state.clone());
                ctx.report_state(report(unique, State::Timer(state), caused_by.clone()));
                if let Some(said) = said {
                    ctx.report_state(happened(fires, said, caused_by));
                }
            }
            Role::Happens => {
                incoming.reply(Err(ServiceError::failed(
                    "nothing can be asked of it: it says when something happened",
                )));
            }
            Role::Schedule(_) => {
                incoming.reply(Err(ServiceError::failed(
                    "nothing can be asked of it: it follows its week",
                )));
            }
            Role::Computed(spec) => {
                let (computed::Spec::Group(group), Some(home)) = (spec.as_ref(), &home) else {
                    incoming.reply(Err(ServiceError::failed(
                        "nothing can be asked of it: it's worked out from other entities",
                    )));
                    continue;
                };
                // What a group is asked, each of its members is asked, on behalf of the call.
                // From a task of its own: a member may be one of these helpers, whose call this
                // loop has to be free to answer.
                let action = service.name().action().to_owned();
                let data = service.data().unwrap_or_default();
                let (home, members) = (std::sync::Arc::clone(home), group.members.clone());
                tokio::spawn(async move {
                    let mut refused = Vec::new();
                    for member in members {
                        if let Err(why) = home
                            .call(
                                member.clone(),
                                action.clone(),
                                data.clone(),
                                caused_by.clone(),
                            )
                            .await
                        {
                            refused.push(format!("{member}: {why}"));
                        }
                    }
                    incoming.reply(if refused.is_empty() {
                        Ok(())
                    } else {
                        Err(ServiceError::failed(refused.join("; ")))
                    });
                });
            }
        }
    }
    Ok(())
}

/// How far back a computed helper reads history as it starts.
fn spec_since(
    spec: &computed::Spec,
    now: Timestamp,
    zone: &jiff::tz::TimeZone,
) -> Option<Timestamp> {
    match spec {
        computed::Spec::Trend(s) => Some(maths::plus(now, -s.over)),
        computed::Spec::Derivative(s) => Some(maths::plus(now, -s.over)),
        computed::Spec::Statistics(s) => Some(maths::plus(now, -s.over)),
        computed::Spec::History(s) => Some(match s.over {
            Some(over) => maths::plus(now, -over),
            None => maths::period_start(maths::Period::Day, now, zone),
        }),
        _ => None,
    }
}

/// Says what a computed helper comes to, when that's changed: its state, or, while it can't
/// be worked out, unknown with the reason as its `problem` attribute.
fn tell(
    ctx: &ProtocolContext,
    unique: &UniqueId,
    outcome: &computed::Outcome,
    said: &mut BTreeMap<UniqueId, computed::Outcome>,
) {
    if said.get(unique) == Some(outcome) {
        return;
    }
    said.insert(unique.clone(), outcome.clone());
    ctx.report_state(match outcome {
        Ok(state) => report(unique.clone(), state.clone(), None),
        Err(why) => StateReport {
            unique_id: unique.clone(),
            state: None,
            attributes: irori_protocol::types::AttributeKey::try_from("problem")
                .ok()
                .map(|key| (key, serde_json::json!(why)))
                .into_iter()
                .collect(),
            caused_by: None,
            replayed: false,
        },
    });
}

/// The next state change, or never when the home can't be watched.
async fn next_change(
    changes: &mut Option<tokio::sync::mpsc::UnboundedReceiver<EntityState>>,
) -> Option<EntityState> {
    match changes {
        Some(changes) => changes.recv().await,
        None => std::future::pending().await,
    }
}

/// The time now, by this machine's clock: the helpers are the device that keeps time here.
fn now() -> Timestamp {
    Timestamp::from_jiff(jiff::Timestamp::now())
}

/// Sleeps until `at` by this machine's clock, or for ever when there's nothing to wait for.
async fn sleep_until(at: Option<Timestamp>) {
    match at {
        Some(at) => {
            let left = at
                .as_jiff()
                .duration_since(jiff::Timestamp::now())
                .as_secs_f64()
                .max(0.0);
            tokio::time::sleep(std::time::Duration::from_secs_f64(left)).await;
        }
        None => std::future::pending().await,
    }
}

async fn keep_timer(
    ctx: &ProtocolContext,
    unique: &UniqueId,
    state: &TimerState,
) -> Result<(), irori_protocol::Rejected> {
    let value = serde_json::to_value(state).unwrap_or(serde_json::Value::Null);
    ctx.store(&value_key(unique.as_str()), value).await
}

/// Something having happened, on the event entity `fires`: a press, a timer finishing.
fn happened(fires: &UniqueId, what: &str, caused_by: Option<ContextId>) -> StateReport {
    report(
        fires.clone(),
        State::Event(EventState {
            event_type: what.to_owned(),
        }),
        caused_by,
    )
}

/// A kept value as what it is: on or off, a number, a text.
fn typed(value: &serde_json::Value) -> Option<Typed> {
    match value {
        serde_json::Value::Bool(on) => Some(Typed::Bool(*on)),
        serde_json::Value::Number(n) => n.as_f64().map(Typed::Number),
        serde_json::Value::String(text) => Some(Typed::Text(text.clone())),
        _ => None,
    }
}

fn report(unique_id: UniqueId, state: State, caused_by: Option<ContextId>) -> StateReport {
    StateReport {
        unique_id,
        state: Some(state),
        attributes: BTreeMap::new(),
        caused_by,
        replayed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(toml_text: &str) -> Settings {
        toml::from_str(toml_text).expect("valid settings")
    }

    #[test]
    fn each_kind_of_helper_is_an_entity_of_a_kind_irori_already_has() {
        let made = all(&settings(
            r#"
            [toggles.guests_over]
            name = "Guests are over"
            [numbers.evening]
            name = "Evening brightness"
            min = 0
            max = 100
            unit = "%"
            [counters.rings]
            name = "Doorbell rings"
            [selects.mode]
            name = "House mode"
            options = ["home", "away"]
            [texts.note]
            name = "Note"
            [buttons.movie]
            name = "Movie time"
            [times.wake]
            name = "Wake up"
            initial = "06:45"
            [dates.bins]
            name = "Bins go out"
            "#,
        ))
        .expect("made");
        let ids: Vec<String> = made
            .iter()
            .map(|one| {
                format!(
                    "{}={}.{}",
                    one.unique,
                    one.description.capabilities.kind(),
                    one.description.suggested_object_id.as_ref().expect("an id")
                )
            })
            .collect();
        assert_eq!(
            ids,
            [
                "toggle-guests_over=switch.guests_over",
                "number-evening=number.evening",
                "counter-rings=number.rings",
                "select-mode=select.mode",
                "text-note=text.note",
                "button-movie=button.movie",
                "pressed-movie=event.movie",
                "date-bins=date.bins",
                "time-wake=time.wake",
            ]
        );
        // Until someone sets them: off, the least, nought, the first option, nothing; and for
        // a day nobody has picked, unknown.
        let initial: Vec<Option<Typed>> = made
            .iter()
            .map(|one| match &one.role {
                Role::Value { initial } => initial.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(
            initial,
            [
                Some(Typed::Bool(false)),
                Some(Typed::Number(0.0)),
                Some(Typed::Number(0.0)),
                Some(Typed::Text("home".into())),
                Some(Typed::Text(String::new())),
                None,
                None,
                None,
                Some(Typed::Text("06:45:00".into())),
            ]
        );
    }

    #[test]
    fn a_helper_holds_only_what_it_can_be() {
        let made = all(&settings(
            r#"
            [numbers.evening]
            name = "Evening brightness"
            min = 0
            max = 100
            [selects.mode]
            name = "House mode"
            options = ["home", "away"]
            "#,
        ))
        .expect("made");
        assert!(made[0].holding(&Typed::Number(40.0)).is_some());
        // Kept from before its range was narrowed, or its option taken away.
        assert!(made[0].holding(&Typed::Number(140.0)).is_none());
        assert!(made[0].holding(&Typed::Text("bright".into())).is_none());
        assert!(made[1].holding(&Typed::Text("away".into())).is_some());
        assert!(made[1].holding(&Typed::Text("asleep".into())).is_none());
    }

    #[test]
    fn two_helpers_that_would_be_one_entity_are_refused_by_name() {
        let error = all(&settings(
            r#"
            [numbers.level]
            name = "Level"
            min = 0
            max = 10
            [counters.level]
            name = "Level count"
            "#,
        ))
        .err()
        .expect("refused")
        .to_string();
        assert!(error.contains("`number.level`"), "{error}");
        assert!(
            error.contains("number-level") && error.contains("counter-level"),
            "{error}"
        );
    }

    #[test]
    fn a_range_that_makes_no_sense_names_its_helper() {
        let error = all(&settings(
            r#"
            [numbers.level]
            name = "Level"
            min = 10
            max = 1
            "#,
        ))
        .err()
        .expect("refused")
        .to_string();
        assert!(error.starts_with("the number `level`"), "{error}");
    }

    #[test]
    fn one_helper_is_checked_as_it_would_be_written() {
        let id = ObjectId::try_from("movie").expect("valid");
        let button = HelperKind::of_table("buttons").expect("a kind");
        assert_eq!(
            button.check(&id, &serde_json::json!({"name": "Movie time"})),
            Ok(vec!["button.movie".to_owned(), "event.movie".to_owned()])
        );
        let select = HelperKind::of_table("selects").expect("a kind");
        let wrong = serde_json::json!({"name": "Mode", "options": ["home"], "initial": "away"});
        assert_eq!(
            select.check(&id, &wrong),
            Err("`initial` isn't something this choice can be".to_owned())
        );
        let number = HelperKind::of_table("numbers").expect("a kind");
        let error = number
            .check(&id, &serde_json::json!({"name": "Level", "min": 0}))
            .expect_err("no max");
        assert!(error.contains("missing field `max`"), "{error}");
        assert!(HelperKind::of_table("gadgets").is_none());

        // Which helper an entity is, from its unique id.
        let (kind, of, main) = HelperKind::of_unique("pressed-movie").expect("a helper");
        assert_eq!((kind.table, of, main), ("buttons", "movie", false));
        let (kind, of, main) = HelperKind::of_unique("counter-rings-2").expect("a helper");
        assert_eq!((kind.table, of, main), ("counters", "rings-2", true));
        assert!(HelperKind::of_unique("hub-cpu").is_none());
    }

    #[test]
    fn a_value_is_kept_under_its_helpers_own_key() {
        assert_eq!(value_key("toggle-guests_over"), "toggle.guests_over");
        assert_eq!(value_key("number-up-down"), "number.up-down");
    }
}
