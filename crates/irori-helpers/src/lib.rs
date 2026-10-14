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
mod counter;
mod moment;
mod number;
mod schedule;
mod select;
mod text;
mod timer;
mod toggle;

use std::collections::{BTreeMap, BTreeSet};

use irori_protocol::types::{
    Capabilities, ContextId, EntityDescription, EventState, Name, ObjectId, ScheduleState, Service,
    State, StateReport, TimerState, Timestamp, Typed, UniqueId,
};
use irori_protocol::{Protocol, ProtocolContext, ProtocolError, ServiceError};
use schemars::JsonSchema;
use serde::Deserialize;

pub use button::Button;
pub use counter::Counter;
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
    Ok(made)
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
    for (unique, helper) in &helpers {
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
        }
    }
    Ok(())
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
