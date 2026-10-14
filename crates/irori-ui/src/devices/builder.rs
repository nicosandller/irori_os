//! Making a helper (ROADMAP C33): pick a kind, fill in what it needs — in a form, or written
//! out as JSON the way a flow can be — and see what it will be before it is made. The
//! assistant sits beside the form and can fill it in; nothing is made until Create.
//!
//! One value holds what's being made, `{name, ...settings}`, with its kind beside it. The form,
//! the JSON and the assistant all read and write that one value, so none of them can drift
//! from the others, and settings the form doesn't draw (a proposal's `hysteresis`, say) are
//! kept rather than dropped.

use irori_types::{Entity, ValueShape};
use irori_ui_kit::combo::{Choice, Combo};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Map, Value, json};

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{Capabilities, EntityState, State};

use crate::api::{HelperCheck, HelperPreview};
use crate::devices::Controls;

/// Which shelf of the picker a kind is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shelf {
    /// Values a person or a flow sets.
    Kept,
    /// Values worked out from the rest of the home.
    Worked,
}

/// A kind of helper, as the page knows it. The same list as `irori_helpers::KINDS`, which a
/// page can't link against: where each kind is kept, what one is called, what its entities'
/// unique ids begin with, and how the form asks for its settings.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HelperKind {
    pub(crate) table: &'static str,
    pub(crate) word: &'static str,
    pub(crate) prefix: &'static str,
    /// What it's for, in the picker and over the form.
    about: &'static str,
    /// A name one might have.
    example: &'static str,
    shelf: Shelf,
    /// What a new one starts as beside its name, as JSON.
    start: &'static str,
    fields: &'static [Field],
}

impl PartialEq for HelperKind {
    fn eq(&self, other: &Self) -> bool {
        self.table == other.table
    }
}

impl Eq for HelperKind {}

/// How the form asks for one setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Widget {
    /// A number, with what to write when there's nothing.
    Number(&'static str),
    Text(&'static str),
    /// `true`, `false`, a number or a text, read as what it looks like.
    Loose(&'static str),
    OnOff,
    /// One of these, `(value, words)`.
    Pick(&'static [(&'static str, &'static str)]),
    /// The entity it's worked out from; with `numeric`, only those with a number to read.
    Source {
        numeric: bool,
    },
    /// One of the source's other numbers instead of its reading.
    SourceField,
    /// The entities of a group, of the kind it's set to.
    Members,
    /// The things a choice can be.
    Options,
    /// Which of those it starts as.
    OptionPick,
    /// A length of time, kept in seconds.
    Seconds,
    /// A schedule's hours, a row a day.
    Week,
    Date,
    Time,
    DateTime,
}

/// When a setting is asked for at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    Always,
    /// A group of sensors, whose readings are combined.
    SensorGroup,
    /// A group of anything else, which is on while any (or all) of it is.
    OnOffGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    key: &'static str,
    label: &'static str,
    widget: Widget,
    when: When,
}

const fn field(key: &'static str, label: &'static str, widget: Widget) -> Field {
    Field {
        key,
        label,
        widget,
        when: When::Always,
    }
}

impl Field {
    const fn only(mut self, when: When) -> Self {
        self.when = when;
        self
    }

    fn shown(self, value: &Map<String, Value>) -> bool {
        let sensors = value.get("kind").and_then(Value::as_str) == Some("sensor");
        match self.when {
            When::Always => true,
            When::SensorGroup => sensors,
            When::OnOffGroup => !sensors,
        }
    }

    /// Takes the row to itself: lists, the week, and long explanations.
    fn wide(self) -> bool {
        matches!(
            self.widget,
            Widget::Source { .. }
                | Widget::Members
                | Widget::Options
                | Widget::Week
                | Widget::OnOff
                | Widget::Pick(_)
        )
    }
}

const PER: &[(&str, &str)] = &[("second", "Second"), ("minute", "Minute"), ("hour", "Hour")];
const SOURCE: Field = field(
    "source",
    "Worked out from",
    Widget::Source { numeric: true },
);
const SOURCE_FIELD: Field = field("field", "Reading", Widget::SourceField);

pub(crate) const HELPER_KINDS: [HelperKind; 19] = [
    HelperKind {
        table: "toggles",
        word: "Toggle",
        prefix: "toggle",
        about: "On or off. A switch for \"guests are over\" or \"holiday mode\".",
        example: "Guests are over",
        shelf: Shelf::Kept,
        start: "{}",
        fields: &[field("initial", "On to start with", Widget::OnOff)],
    },
    HelperKind {
        table: "numbers",
        word: "Number",
        prefix: "number",
        about: "A value within a range, set with a slider or typed. A flow reads it instead \
                of having the number written into it.",
        example: "Evening brightness",
        shelf: Shelf::Kept,
        start: r#"{"min": 0, "max": 100, "step": 1}"#,
        fields: &[
            field("min", "Least", Widget::Number("0")),
            field("max", "Most", Widget::Number("100")),
            field("step", "Step", Widget::Number("1")),
            field("unit", "Unit", Widget::Text("%")),
            field("initial", "To start with", Widget::Number("the least")),
            field(
                "mode",
                "Set with",
                Widget::Pick(&[("slider", "A slider"), ("box", "A box to type in")]),
            ),
        ],
    },
    HelperKind {
        table: "counters",
        word: "Counter",
        prefix: "counter",
        about: "A number that flows count up and down, and set back to where it starts.",
        example: "Doorbell rings today",
        shelf: Shelf::Kept,
        start: "{}",
        fields: &[
            field("initial", "Starts at", Widget::Number("0")),
            field("step", "Counts by", Widget::Number("1")),
            field("min", "Least", Widget::Number("0")),
            field("max", "Most", Widget::Number("1000000")),
        ],
    },
    HelperKind {
        table: "selects",
        word: "Choice",
        prefix: "select",
        about: "One of a list you write: home, away, asleep.",
        example: "House mode",
        shelf: Shelf::Kept,
        start: r#"{"options": []}"#,
        fields: &[
            field("options", "What it can be", Widget::Options),
            field("initial", "To start with", Widget::OptionPick),
        ],
    },
    HelperKind {
        table: "texts",
        word: "Text",
        prefix: "text",
        about: "A few words: a message for a display, a note a flow reads.",
        example: "Hall display message",
        shelf: Shelf::Kept,
        start: "{}",
        fields: &[
            field("initial", "To start with", Widget::Text("")),
            field("min_length", "Shortest", Widget::Number("0")),
            field("max_length", "Longest", Widget::Number("255")),
        ],
    },
    HelperKind {
        table: "buttons",
        word: "Button",
        prefix: "button",
        about: "Something to press, here or from a flow, that other flows start from.",
        example: "Movie time",
        shelf: Shelf::Kept,
        start: "{}",
        fields: &[],
    },
    HelperKind {
        table: "dates",
        word: "Date",
        prefix: "date",
        about: "A calendar day you pick: when the bins go out, when the guests arrive.",
        example: "Bins go out",
        shelf: Shelf::Kept,
        start: "{}",
        fields: &[field("initial", "To start with", Widget::Date)],
    },
    HelperKind {
        table: "times",
        word: "Time",
        prefix: "time",
        about: "A time of day you pick: when the alarm clock goes off, when the heating may start.",
        example: "Wake up",
        shelf: Shelf::Kept,
        start: "{}",
        fields: &[field("initial", "To start with", Widget::Time)],
    },
    HelperKind {
        table: "datetimes",
        word: "Date and time",
        prefix: "datetime",
        about: "A day and a time together: when the holiday ends.",
        example: "Holiday ends",
        shelf: Shelf::Kept,
        start: "{}",
        fields: &[field("initial", "To start with", Widget::DateTime)],
    },
    HelperKind {
        table: "timers",
        word: "Timer",
        prefix: "timer",
        about: "Counts down and says when it's done: flows start it, and start from it finishing.",
        example: "Lights stay on",
        shelf: Shelf::Kept,
        start: r#"{"duration": 300}"#,
        fields: &[
            field("duration", "Runs for", Widget::Seconds),
            field(
                "restore",
                "Finishes after a restart if it ran out while Irori was off",
                Widget::OnOff,
            ),
        ],
    },
    HelperKind {
        table: "schedules",
        word: "Schedule",
        prefix: "schedule",
        about: "On during the hours you set out for each day of the week, on the home's clock.",
        example: "Heating hours",
        shelf: Shelf::Kept,
        start: r#"{"mon": [["06:30", "08:30"], ["17:00", "22:00"]],
                   "tue": [["06:30", "08:30"], ["17:00", "22:00"]],
                   "wed": [["06:30", "08:30"], ["17:00", "22:00"]],
                   "thu": [["06:30", "08:30"], ["17:00", "22:00"]],
                   "fri": [["06:30", "08:30"], ["17:00", "23:00"]],
                   "sat": [["08:00", "23:00"]], "sun": [["08:00", "22:00"]]}"#,
        fields: &[field("week", "Hours", Widget::Week)],
    },
    HelperKind {
        table: "groups",
        word: "Group",
        prefix: "group",
        about: "Several lights, switches, fans, covers or sensors as one: on while any is, and \
                told as one.",
        example: "Lounge lights",
        shelf: Shelf::Worked,
        start: r#"{"kind": "light", "members": []}"#,
        fields: &[
            field(
                "kind",
                "Of",
                Widget::Pick(&[
                    ("light", "Lights"),
                    ("switch", "Switches"),
                    ("fan", "Fans"),
                    ("cover", "Covers"),
                    ("binary_sensor", "Binary sensors"),
                    ("sensor", "Sensors"),
                ]),
            ),
            field("members", "Members", Widget::Members),
            field("all", "Only when all are on", Widget::OnOff).only(When::OnOffGroup),
            field(
                "of",
                "Their",
                Widget::Pick(&[
                    ("mean", "Average"),
                    ("min", "Least"),
                    ("max", "Most"),
                    ("sum", "Sum"),
                ]),
            )
            .only(When::SensorGroup),
        ],
    },
    HelperKind {
        table: "thresholds",
        word: "Threshold",
        prefix: "threshold",
        about: "On while a reading is above, below, or between lines.",
        example: "Too warm",
        shelf: Shelf::Worked,
        start: "{}",
        fields: &[
            SOURCE,
            SOURCE_FIELD,
            field("above", "On above", Widget::Number("")),
            field("below", "On below", Widget::Number("")),
            field(
                "hysteresis",
                "Changes only past the line by",
                Widget::Number("0"),
            ),
        ],
    },
    HelperKind {
        table: "trends",
        word: "Trend",
        prefix: "trend",
        about: "On while a reading is rising (or falling) by at least so much over a while.",
        example: "Humidity rising",
        shelf: Shelf::Worked,
        start: r#"{"direction": "rising", "over": 600}"#,
        fields: &[
            SOURCE,
            SOURCE_FIELD,
            field(
                "direction",
                "On while it's",
                Widget::Pick(&[("rising", "Rising"), ("falling", "Falling")]),
            ),
            field("by", "By at least", Widget::Number("")),
            field("over", "Over", Widget::Seconds),
        ],
    },
    HelperKind {
        table: "meters",
        word: "Meter",
        prefix: "meter",
        about: "How much a counting reading, like an energy total, went up this hour, day, \
                week, month or year.",
        example: "Energy today",
        shelf: Shelf::Worked,
        start: r#"{"every": "day"}"#,
        fields: &[
            SOURCE,
            field(
                "every",
                "Starts again every",
                Widget::Pick(&[
                    ("hour", "Hour"),
                    ("day", "Day"),
                    ("week", "Week"),
                    ("month", "Month"),
                    ("year", "Year"),
                ]),
            ),
        ],
    },
    HelperKind {
        table: "integrals",
        word: "Integral",
        prefix: "integral",
        about: "The area under a reading over time: a power reading into energy used.",
        example: "Heater energy",
        shelf: Shelf::Worked,
        start: r#"{"per": "hour"}"#,
        fields: &[SOURCE, SOURCE_FIELD, field("per", "Per", Widget::Pick(PER))],
    },
    HelperKind {
        table: "derivatives",
        word: "Derivative",
        prefix: "derivative",
        about: "How fast a reading changes, per hour, over the last while.",
        example: "Tank filling rate",
        shelf: Shelf::Worked,
        start: r#"{"per": "hour", "over": 900}"#,
        fields: &[
            SOURCE,
            SOURCE_FIELD,
            field("per", "Per", Widget::Pick(PER)),
            field("over", "Over the last", Widget::Seconds),
        ],
    },
    HelperKind {
        table: "statistics",
        word: "Statistic",
        prefix: "statistics",
        about: "A reading's average, least or most over the last while.",
        example: "Average outside temperature",
        shelf: Shelf::Worked,
        start: r#"{"of": "mean", "over": 3600}"#,
        fields: &[
            SOURCE,
            SOURCE_FIELD,
            field(
                "of",
                "Its",
                Widget::Pick(&[("mean", "Average"), ("min", "Least"), ("max", "Most")]),
            ),
            field("over", "Over the last", Widget::Seconds),
        ],
    },
    HelperKind {
        table: "history",
        word: "History",
        prefix: "history",
        about: "How long, how often or how much of the time something was a value, today or \
                over a while.",
        example: "Heating hours today",
        shelf: Shelf::Worked,
        start: r#"{"measure": "time"}"#,
        fields: &[
            field("source", "Watching", Widget::Source { numeric: false }),
            field(
                "is",
                "While it is",
                Widget::Loose("true, false, or a word like heat"),
            ),
            field(
                "measure",
                "Counting",
                Widget::Pick(&[
                    ("time", "How long"),
                    ("count", "How often"),
                    ("ratio", "How much of the time"),
                ]),
            ),
            field(
                "over",
                "Over the last (empty: since midnight)",
                Widget::Seconds,
            ),
        ],
    },
];

impl HelperKind {
    fn of_table(table: &str) -> Option<Self> {
        HELPER_KINDS.into_iter().find(|kind| kind.table == table)
    }

    fn start(self) -> Map<String, Value> {
        match serde_json::from_str(self.start) {
            Ok(Value::Object(start)) => start,
            _ => Map::new(),
        }
    }
}

/// What's being made: its kind, once picked, and `{name, ...settings}`.
#[derive(Debug, Clone, Copy)]
struct Draft {
    kind: RwSignal<Option<HelperKind>>,
    value: RwSignal<Map<String, Value>>,
}

impl Draft {
    fn get(self, key: &str) -> Option<Value> {
        self.value.with(|value| value.get(key).cloned())
    }

    fn text(self, key: &str) -> String {
        self.value.with(|value| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
    }

    fn put(self, key: &str, to: Option<Value>) {
        self.value.update(|value| match to {
            Some(to) => {
                value.insert(key.to_owned(), to);
            }
            None => {
                value.remove(key);
            }
        });
    }

    /// Starts the kind over from its beginning, keeping the name typed so far.
    fn pick(self, kind: HelperKind) {
        let name = self.get("name");
        let mut value = kind.start();
        if let Some(name) = name {
            value.insert("name".to_owned(), name);
        }
        self.value.set(value);
        self.kind.set(Some(kind));
    }

    /// Takes what the assistant filled in, `{kind, name, settings}`. Whatever was there goes:
    /// the person asked for this one.
    fn take(self, proposal: &Value) -> bool {
        let Some(kind) = proposal["kind"].as_str().and_then(HelperKind::of_table) else {
            return false;
        };
        let mut value = match &proposal["settings"] {
            Value::Object(settings) => settings.clone(),
            _ => Map::new(),
        };
        if let Some(name) = proposal["name"].as_str() {
            value.insert("name".to_owned(), json!(name));
        }
        self.value.set(value);
        self.kind.set(Some(kind));
        true
    }

    /// The whole of it as JSON to read and change. Which kind it is goes under `helper`: a
    /// group has a `kind` of its own (lights, switches…).
    fn code(self) -> String {
        let mut rest = self.value.get();
        let kind = self.kind.get();
        let mut parts = Vec::new();
        if let Some(kind) = kind {
            parts.push(format!("  \"{KIND_KEY}\": {}", json!(kind.table)));
        }
        // The name, then the settings in the form's order (a schedule's days in the week's),
        // then anything else it has.
        let mut order: Vec<&str> = vec!["name"];
        for field in kind.map_or(&[][..], |kind| kind.fields) {
            if field.widget == Widget::Week {
                order.extend(DAYS.map(|(day, _)| day));
            } else {
                order.push(field.key);
            }
        }
        let mut keys: Vec<String> = order.iter().map(|key| (*key).to_owned()).collect();
        keys.extend(
            rest.keys()
                .filter(|key| !order.contains(&key.as_str()))
                .cloned(),
        );
        for key in keys {
            let Some(value) = rest.remove(&key) else {
                continue;
            };
            let one = irori_ui_kit::json::written(&json!({ key: value }));
            if let Some(body) = one
                .strip_prefix("{\n")
                .and_then(|body| body.strip_suffix("\n}"))
            {
                parts.push(body.to_owned());
            }
        }
        if parts.is_empty() {
            "{}".to_owned()
        } else {
            format!("{{\n{}\n}}", parts.join(",\n"))
        }
    }

    /// Takes what was written as JSON, or says why it can't yet.
    fn take_code(self, text: &str) -> Result<(), String> {
        let Value::Object(mut value) =
            serde_json::from_str::<Value>(text).map_err(|why| format!("Not JSON yet: {why}"))?
        else {
            return Err("It has to be one object, in { and }.".to_owned());
        };
        let kind = match value.remove(KIND_KEY) {
            Some(Value::String(table)) => HelperKind::of_table(&table).ok_or_else(|| {
                let kinds: Vec<&str> = HELPER_KINDS.iter().map(|kind| kind.table).collect();
                format!(
                    "There's no kind of helper \"{table}\". There are: {}.",
                    kinds.join(", ")
                )
            })?,
            Some(_) => {
                return Err(format!("\"{KIND_KEY}\" is a kind of helper, in quotes."));
            }
            None => self.kind.get_untracked().ok_or_else(|| {
                format!("Say which kind of helper it is: \"{KIND_KEY}\": \"toggles\".")
            })?,
        };
        self.value.set(value);
        if self.kind.get_untracked() != Some(kind) {
            self.kind.set(Some(kind));
        }
        Ok(())
    }
}

/// Where the JSON says which kind of helper it is.
const KIND_KEY: &str = "helper";

/// What a number box's words make: nothing when empty, a number when they read as one, and the
/// words themselves otherwise, so the check can say what's wrong with them.
fn number(words: &str) -> Option<Value> {
    let words = words.trim();
    if words.is_empty() {
        return None;
    }
    Some(match words.parse::<f64>() {
        #[allow(clippy::cast_possible_truncation)]
        Ok(n) if n.fract() == 0.0 && n.abs() < 1e15 => json!(n as i64),
        Ok(n) => json!(n),
        Err(_) => json!(words),
    })
}

fn text(words: &str) -> Option<Value> {
    (!words.is_empty()).then(|| json!(words))
}

/// `true`, `false`, a number, or the words as they are.
fn loose(words: &str) -> Option<Value> {
    match words.trim() {
        "" => None,
        "true" => Some(json!(true)),
        "false" => Some(json!(false)),
        trimmed => trimmed
            .parse::<f64>()
            .ok()
            .and_then(|_| number(trimmed))
            .or_else(|| Some(json!(trimmed))),
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A box's words, kept apart from the value they make, so typing `1.` isn't turned into `1`
/// under the caret. Put back from the value when it changes to something they don't say —
/// the assistant filling it in, or the JSON being changed.
fn typed(draft: Draft, key: &'static str, read: fn(&str) -> Option<Value>) -> RwSignal<String> {
    let words = RwSignal::new(
        draft
            .value
            .with_untracked(|value| value.get(key).map(shown).unwrap_or_default()),
    );
    Effect::new(move |_| {
        let now = draft.get(key);
        let said = words.with_untracked(|words| read(words));
        if said != now {
            words.set(now.as_ref().map(shown).unwrap_or_default());
        }
    });
    words
}

/// The entities `keep` keeps, to pick from: each with its icon, under its device's name, as
/// the entity pickers in flows show them.
fn entity_choices(
    home: &crate::api::Home,
    keep: impl Fn(&Entity) -> bool,
    leave_out: &[String],
) -> Vec<Choice> {
    let mut choices: Vec<Choice> = home
        .entities
        .iter()
        .filter(|entity| keep(entity))
        .filter(|entity| !leave_out.iter().any(|out| *out == entity.id.as_str()))
        .map(|entity| {
            let device = entity.device_id.as_ref().and_then(|id| {
                home.devices
                    .iter()
                    .find(|device| device.id == *id)
                    .map(|device| device.name.to_string())
            });
            let choice = Choice::new(entity.id.to_string(), entity.name.to_string())
                .icon(irori_ui_kit::entity_icon::drawing(&entity.capabilities))
                .detail(entity.id.to_string());
            match device {
                Some(device) if device != entity.name.as_str() => choice.group(device),
                _ => choice,
            }
        })
        .collect();
    choices.sort_by(|a, b| (&a.group, &a.label).cmp(&(&b.group, &b.label)));
    choices
}

fn has_a_number(entity: &Entity) -> bool {
    entity.capabilities.primary_shape() == Some(ValueShape::Number)
        || entity
            .capabilities
            .fields()
            .iter()
            .any(|field| field.shape == ValueShape::Number)
}

pub(crate) fn kind_icon(table: &'static str) -> AnyView {
    view! {
        <svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
            inner_html=irori_ui_kit::entity_icon::helper(table)></svg>
    }
    .into_any()
}

/// One setting, asked for the way its widget says.
fn setting(draft: Draft, field: Field) -> AnyView {
    let key = field.key;
    let live = expect_context::<crate::Live>();
    let input = |words: RwSignal<String>, read: fn(&str) -> Option<Value>, numeric: bool| {
        view! {
            <input
                type="text"
                inputmode=if numeric { "decimal" } else { "text" }
                placeholder=match field.widget {
                    Widget::Number(hint) | Widget::Text(hint) | Widget::Loose(hint) => hint,
                    _ => "",
                }
                prop:value=words
                on:input:target=move |ev| {
                    let typed = ev.target().value();
                    draft.put(key, read(&typed));
                    words.set(typed);
                }
            />
        }
        .into_any()
    };
    let asked = match field.widget {
        Widget::Number(_) => input(typed(draft, key, number), number, true),
        Widget::Text(_) => input(typed(draft, key, text), text, false),
        Widget::Loose(_) => input(typed(draft, key, loose), loose, false),
        Widget::OnOff => {
            let on = move || draft.get(key).and_then(|on| on.as_bool()).unwrap_or(false);
            view! {
                <button
                    type="button"
                    class="toggle"
                    aria-label=field.label
                    aria-pressed=move || on().to_string()
                    on:click=move |_| draft.put(key, Some(json!(!on())))
                >
                    <span class="knob"></span>
                </button>
            }
            .into_any()
        }
        Widget::Pick(options) => crate::choices::choices(
            field.label,
            options
                .iter()
                .map(|(value, words)| ((*value).to_owned(), (*words).to_owned()))
                .collect(),
            move || {
                draft
                    .get(key)
                    .and_then(|value| value.as_str().map(str::to_owned))
            },
            || false,
            move |picked| {
                // A group changing kind can't keep members of the old one.
                if key == "kind" && draft.text("kind") != picked {
                    draft.put("members", Some(json!([])));
                }
                draft.put(key, Some(json!(picked)));
            },
        ),
        Widget::Source { numeric } => {
            let choices = Signal::derive(move || {
                live.home.with(|home| {
                    entity_choices(home, |entity| !numeric || has_a_number(entity), &[])
                })
            });
            view! {
                <Combo
                    choices=choices
                    value=Signal::derive(move || draft.text(key))
                    pick=Callback::new(move |picked: String| {
                        draft.put(key, Some(json!(picked)));
                        draft.put("field", None);
                    })
                    placeholder=if numeric { "Pick a reading" } else { "Pick anything" }
                />
            }
            .into_any()
        }
        Widget::SourceField => source_field(draft),
        Widget::Members => members(draft),
        Widget::Options => options(draft),
        Widget::OptionPick => {
            let options = move || {
                draft
                    .get("options")
                    .and_then(|options| options.as_array().cloned())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|option| option.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            };
            let choices = Signal::derive(move || {
                let mut choices = vec![Choice::new("", "The first")];
                choices.extend(
                    options()
                        .into_iter()
                        .map(|option| Choice::new(option.clone(), option)),
                );
                choices
            });
            view! {
                <Combo
                    choices=choices
                    value=Signal::derive(move || draft.text(key))
                    pick=Callback::new(move |picked: String| draft.put(key, text(&picked)))
                    placeholder="The first"
                />
            }
            .into_any()
        }
        Widget::Seconds => seconds(draft, key),
        Widget::Week => week(draft),
        Widget::Date | Widget::Time => {
            let kind = if field.widget == Widget::Date {
                "date"
            } else {
                "time"
            };
            view! {
                <input
                    type=kind
                    prop:value=move || draft.text(key)
                    on:change:target=move |ev| draft.put(key, text(&ev.target().value()))
                />
            }
            .into_any()
        }
        Widget::DateTime => view! {
            <input
                type="datetime-local"
                prop:value=move || local_moment(&draft.text(key))
                on:change:target=move |ev| draft.put(key, utc_moment(&ev.target().value()))
            />
        }
        .into_any(),
    };
    let label_is_the_control = matches!(field.widget, Widget::OnOff);
    view! {
        <div class="builder-field" class:wide=field.wide() class:inline=label_is_the_control>
            <span class="builder-label">{field.label}</span>
            {asked}
        </div>
    }
    .into_any()
}

/// `2026-10-14T06:45:00Z` as a `datetime-local` box shows it, on this browser's clock.
fn local_moment(utc: &str) -> String {
    if utc.is_empty() {
        return String::new();
    }
    let date = web_sys::js_sys::Date::new(&utc.into());
    if date.get_time().is_nan() {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes()
    )
}

/// What a `datetime-local` box holds, on this browser's clock, as the moment it is.
fn utc_moment(local: &str) -> Option<Value> {
    if local.is_empty() {
        return None;
    }
    let date = web_sys::js_sys::Date::new(&local.into());
    if date.get_time().is_nan() {
        return None;
    }
    let iso: String = date.to_iso_string().into();
    // Without the milliseconds, as the settings are written.
    Some(json!(iso.replace(".000Z", "Z")))
}

/// One of the source's other numbers, when it has any: a thermostat's current temperature
/// rather than its setting.
fn source_field(draft: Draft) -> AnyView {
    let live = expect_context::<crate::Live>();
    let choices = Signal::derive(move || {
        let source = draft.text("source");
        let mut choices = vec![Choice::new("", "Its reading")];
        choices.extend(live.home.with(|home| {
            number_fields(home, &source)
                .into_iter()
                .map(|name| Choice::new(name, name.replace('_', " ")))
                .collect::<Vec<_>>()
        }));
        choices
    });
    view! {
        <Combo
            choices=choices
            value=Signal::derive(move || draft.text("field"))
            pick=Callback::new(move |picked: String| draft.put("field", text(&picked)))
            placeholder="Its reading"
        />
    }
    .into_any()
}

/// The other numbers an entity has beside its reading: a thermostat's current temperature.
fn number_fields(home: &crate::api::Home, source: &str) -> Vec<&'static str> {
    home.entities
        .iter()
        .find(|entity| entity.id.as_str() == source)
        .map(|entity| {
            entity
                .capabilities
                .fields()
                .into_iter()
                .filter(|field| field.shape == ValueShape::Number)
                .map(|field| field.name)
                .collect()
        })
        .unwrap_or_default()
}

/// A list of texts kept under `key`, as chips.
fn listed(draft: Draft, key: &str) -> Vec<String> {
    draft
        .get(key)
        .and_then(|list| list.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn chip(words: AnyView, icon: Option<&'static str>, remove: impl Fn() + 'static) -> AnyView {
    view! {
        <span class="chip">
            {icon.map(|drawing| view! {
                <svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                    stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"
                    aria-hidden="true" inner_html=drawing></svg>
            })}
            {words}
            <button type="button" class="chip-remove" aria-label="Remove" on:click=move |_| remove()>
                "×"
            </button>
        </span>
    }
    .into_any()
}

/// A group's members: the entities of the kind it's set to, as chips, and a box to add more.
fn members(draft: Draft) -> AnyView {
    let live = expect_context::<crate::Live>();
    let listed_now = move || listed(draft, "members");
    let choices = Signal::derive(move || {
        let kind = draft.text("kind");
        let prefix = format!("{kind}.");
        let taken = listed_now();
        live.home.with(|home| {
            entity_choices(
                home,
                |entity| entity.id.as_str().starts_with(&prefix),
                &taken,
            )
        })
    });
    let add = Callback::new(move |picked: String| {
        let mut now = listed_now();
        if !now.contains(&picked) {
            now.push(picked);
        }
        draft.put("members", Some(json!(now)));
    });
    view! {
        <div class="chips">
            <For each=listed_now key=|member| member.clone() let:member>
                {
                    let (name, icon) = live.home.with_untracked(|home| {
                        home.entities
                            .iter()
                            .find(|entity| entity.id.as_str() == member)
                            .map_or((member.clone(), None), |entity| {
                                (
                                    entity.name.to_string(),
                                    Some(irori_ui_kit::entity_icon::drawing(&entity.capabilities)),
                                )
                            })
                    });
                    let gone = member.clone();
                    chip(
                        view! { <span title=member.clone()>{name}</span> }.into_any(),
                        icon,
                        move || {
                            let now: Vec<String> =
                                listed_now().into_iter().filter(|m| *m != gone).collect();
                            draft.put("members", Some(json!(now)));
                        },
                    )
                }
            </For>
        </div>
        <Combo
            choices=choices
            value=Signal::derive(String::new)
            pick=add
            placeholder="Add a member"
        />
    }
    .into_any()
}

/// What a choice can be, from what was typed: one at a time, or with commas between them.
fn split_options(typed: &str) -> Vec<String> {
    let mut options: Vec<String> = Vec::new();
    for option in typed.split(['\n', ',']).map(str::trim) {
        if !option.is_empty() && !options.iter().any(|seen| seen == option) {
            options.push(option.to_owned());
        }
    }
    options
}

/// A choice's options, as chips, with a box to type more into.
fn options(draft: Draft) -> AnyView {
    let words = RwSignal::new(String::new());
    let now = move || listed(draft, "options");
    let add = move || {
        let mut options = now();
        for option in split_options(&words.get_untracked()) {
            if !options.contains(&option) {
                options.push(option);
            }
        }
        draft.put("options", Some(json!(options)));
        words.set(String::new());
    };
    view! {
        <div class="chips">
            <For each=now key=|option| option.clone() let:option>
                {
                    let gone = option.clone();
                    chip(
                        option.into_any(),
                        None,
                        move || {
                            let left: Vec<String> =
                                now().into_iter().filter(|o| *o != gone).collect();
                            draft.put("options", Some(json!(left)));
                            if draft.text("initial") == gone {
                                draft.put("initial", None);
                            }
                        },
                    )
                }
            </For>
            <input
                class="chip-input"
                type="text"
                placeholder=move || if now().is_empty() { "home, away, asleep" } else { "Another" }
                prop:value=words
                on:input:target=move |ev| {
                    let typed = ev.target().value();
                    let ends = typed.ends_with(',');
                    words.set(typed);
                    if ends {
                        add();
                    }
                }
                on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
                    "Enter" => {
                        ev.prevent_default();
                        add();
                    }
                    "Backspace" if words.get_untracked().is_empty() => {
                        let mut left = now();
                        left.pop();
                        draft.put("options", Some(json!(left)));
                    }
                    _ => {}
                }
                on:blur=move |_| add()
            />
        </div>
    }
    .into_any()
}

/// The unit a length of time reads best in: hours, minutes or seconds, whichever divides it.
fn best_unit(seconds: f64) -> f64 {
    if seconds >= 3600.0 && seconds % 3600.0 == 0.0 {
        3600.0
    } else if seconds >= 60.0 && seconds % 60.0 == 0.0 {
        60.0
    } else {
        1.0
    }
}

/// A length of time, typed in the unit it reads best in and kept in seconds.
fn seconds(draft: Draft, key: &'static str) -> AnyView {
    let held = move || draft.get(key).and_then(|value| value.as_f64());
    let unit = RwSignal::new(held().map_or(60.0, best_unit));
    let words = RwSignal::new(
        held()
            .map(|seconds| {
                shown(&number(&(seconds / unit.get_untracked()).to_string()).unwrap_or(Value::Null))
            })
            .unwrap_or_default(),
    );
    let made = move |words: &str, unit: f64| {
        words
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(|amount| number(&(amount * unit).to_string()))
            .or_else(|| text(words.trim()))
    };
    // Put back from the value when something else changes it.
    Effect::new(move |_| {
        let now = draft.get(key);
        let said = words.with_untracked(|words| made(words, unit.get_untracked()));
        if said != now {
            match now.as_ref().and_then(Value::as_f64) {
                Some(seconds) => {
                    let best = best_unit(seconds);
                    unit.set(best);
                    words.set(
                        number(&(seconds / best).to_string())
                            .as_ref()
                            .map(shown)
                            .unwrap_or_default(),
                    );
                }
                None => words.set(now.as_ref().map(shown).unwrap_or_default()),
            }
        }
    });
    view! {
        <span class="builder-duration">
            <input
                type="text"
                inputmode="decimal"
                prop:value=words
                on:input:target=move |ev| {
                    let typed = ev.target().value();
                    draft.put(key, made(&typed, unit.get_untracked()));
                    words.set(typed);
                }
            />
            {crate::choices::choices(
                "Unit",
                vec![
                    ("1".to_owned(), "sec".to_owned()),
                    ("60".to_owned(), "min".to_owned()),
                    ("3600".to_owned(), "hours".to_owned()),
                ],
                move || Some(format!("{}", unit.get())),
                || false,
                move |picked| {
                    let picked = picked.parse::<f64>().unwrap_or(1.0);
                    unit.set(picked);
                    draft.put(key, made(&words.get_untracked(), picked));
                },
            )}
        </span>
    }
    .into_any()
}

const DAYS: [(&str, &str); 7] = [
    ("mon", "Mon"),
    ("tue", "Tue"),
    ("wed", "Wed"),
    ("thu", "Thu"),
    ("fri", "Fri"),
    ("sat", "Sat"),
    ("sun", "Sun"),
];

/// `07:30` in minutes since midnight.
fn minutes(time: &str) -> Option<u32> {
    let (hours, minutes) = time.split_once(':')?;
    let (hours, minutes): (u32, u32) = (hours.parse().ok()?, minutes.parse().ok()?);
    (hours <= 24 && minutes < 60).then_some(hours * 60 + minutes)
}

fn clock_time(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A day's hours, `[from, until]` each.
fn hours(draft: Draft, day: &str) -> Vec<(String, String)> {
    draft
        .get(day)
        .and_then(|hours| hours.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|block| {
            let block = block.as_array()?;
            Some((
                block.first()?.as_str()?.to_owned(),
                block.get(1)?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn set_hours(draft: Draft, day: &str, hours: &[(String, String)]) {
    if hours.is_empty() {
        draft.put(day, None);
    } else {
        let blocks: Vec<Value> = hours
            .iter()
            .map(|(from, until)| json!([from, until]))
            .collect();
        draft.put(day, Some(Value::Array(blocks)));
    }
}

/// A schedule's week: a row a day, its hours drawn on a line from midnight to midnight and
/// set with a box each. The bars are kept as hours change, so they slide to their new place.
fn week(draft: Draft) -> AnyView {
    view! {
        <div class="week">
            <div class="week-scale" aria-hidden="true">
                <span></span>
                <span class="week-ticks">
                    <span>"0"</span><span>"6"</span><span>"12"</span><span>"18"</span><span>"24"</span>
                </span>
            </div>
            {DAYS.into_iter().map(|(day, word)| day_row(draft, day, word)).collect_view()}
            <button
                type="button"
                class="quiet-button week-copy"
                on:click=move |_| {
                    let monday = hours(draft, "mon");
                    for day in ["tue", "wed", "thu", "fri"] {
                        set_hours(draft, day, &monday);
                    }
                }
            >
                "Make Tuesday to Friday like Monday"
            </button>
        </div>
    }
    .into_any()
}

fn day_row(draft: Draft, day: &'static str, word: &'static str) -> AnyView {
    let count = Memo::new(move |_| hours(draft, day).len());
    let block = move |at: usize| hours(draft, day).get(at).cloned().unwrap_or_default();
    let percent = |time: &str| f64::from(minutes(time).unwrap_or(0)) / 14.4;
    let change = move |at: usize, end: usize, to: String| {
        let mut now = hours(draft, day);
        if let Some(block) = now.get_mut(at) {
            // A box can't say 24:00: midnight as the end of the hours means the end of the day.
            let to = if end == 1 && to == "00:00" {
                "24:00".to_owned()
            } else {
                to
            };
            if end == 0 {
                block.0 = to;
            } else {
                block.1 = to;
            }
        }
        set_hours(draft, day, &now);
    };
    let add = move |_| {
        let mut now = hours(draft, day);
        let from = now
            .last()
            .and_then(|(_, until)| minutes(until))
            .map_or(18 * 60, |until| (until + 60).min(23 * 60));
        let until = (from + 120).min(24 * 60);
        now.push((clock_time(from), clock_time(until)));
        set_hours(draft, day, &now);
    };
    view! {
        <div class="week-day">
            <span class="week-name">{word}</span>
            <span class="week-track">
                <For each=move || 0..count.get() key=|at| *at let:at>
                    <span
                        class="week-block"
                        style=move || {
                            let (from, until) = block(at);
                            let (from, until) = (percent(&from), percent(&until));
                            format!("left: {from:.2}%; width: {:.2}%", (until - from).max(0.6))
                        }
                    ></span>
                </For>
            </span>
            <span class="week-times">
                <For each=move || 0..count.get() key=|at| *at let:at>
                    <span class="week-time">
                        <input
                            type="time"
                            aria-label=format!("{word}: from")
                            prop:value=move || block(at).0
                            on:change:target=move |ev| change(at, 0, ev.target().value())
                        />
                        <span class="muted">"–"</span>
                        <input
                            type="time"
                            aria-label=format!("{word}: until")
                            prop:value=move || {
                                let until = block(at).1;
                                if until == "24:00" { "00:00".to_owned() } else { until }
                            }
                            on:change:target=move |ev| change(at, 1, ev.target().value())
                        />
                        <button
                            type="button"
                            class="chip-remove"
                            aria-label=format!("Remove these hours on {word}")
                            on:click=move |_| {
                                let mut now = hours(draft, day);
                                if at < now.len() {
                                    now.remove(at);
                                }
                                set_hours(draft, day, &now);
                            }
                        >
                            "×"
                        </button>
                    </span>
                </For>
                <button type="button" class="week-add" on:click=add
                    aria-label=format!("Add hours on {word}")>
                    "+"
                </button>
            </span>
        </div>
    }
    .into_any()
}

/// Form or JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Form,
    Code,
}

/// What the check said of the helper as it stands.
#[derive(Debug, Clone, PartialEq)]
enum Checked {
    /// Nothing to check yet: no kind, or no name.
    Quiet,
    Checking,
    Will(HelperCheck),
    Wont(String),
}

/// How long typing has to pause before what's typed is checked.
const SETTLE_MS: u64 = 350;

/// Making a helper: the kind, what it needs, what it will be, and the assistant to ask. Outside
/// the list, so what's being typed survives the list redrawing (ROADMAP D33).
#[component]
pub(crate) fn HelperBuilder(#[prop(into)] on_added: Callback<()>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let assistant = expect_context::<crate::Assistant>();
    let draft = Draft {
        kind: RwSignal::new(None),
        value: RwSignal::new(Map::new()),
    };
    let mode = RwSignal::new(Mode::Form);
    // What's typed as JSON and not yet set out again: while there is some, the box shows it as
    // typed rather than redrawing it under the caret.
    let code_words = RwSignal::new(None::<String>);
    let code_trouble = RwSignal::new(None::<String>);
    let checked = RwSignal::new(Checked::Quiet);
    // What it would be like, to see and try before it's made.
    let preview = RwSignal::new(None::<Result<HelperPreview, String>>);
    let round = StoredValue::new(0u32);
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let done = RwSignal::new(false);
    // How many times the assistant has filled it in: each one glows the form once.
    let filled = RwSignal::new(0u32);

    // The assistant fills in this builder while it's open, and nothing once it's gone.
    let desk = expect_context::<crate::assistant::Chats>().builder_desk();
    desk.lay(Some(Callback::new(move |proposal: Value| {
        if draft.take(&proposal) {
            code_words.set(None);
            code_trouble.set(None);
            trouble.set(None);
            filled.update(|times| *times += 1);
        }
    })));
    on_cleanup(move || desk.lay(None));

    // Checked a moment after typing stops; an answer to an older question is let go.
    Effect::new(move |_| {
        let kind = draft.kind.get();
        let value = draft.value.get();
        let named = value
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.trim().is_empty());
        let this = round.get_value() + 1;
        round.set_value(this);
        let Some(kind) = kind else {
            checked.set(Checked::Quiet);
            preview.set(None);
            return;
        };
        checked.set(if named {
            Checked::Checking
        } else {
            Checked::Quiet
        });
        spawn_local(async move {
            gloo_timers::future::sleep(std::time::Duration::from_millis(SETTLE_MS)).await;
            if round.try_get_value() != Some(this) {
                return;
            }
            if named {
                let said = crate::api::check_helper(kind.table, &value).await;
                if round.try_get_value() != Some(this) {
                    return;
                }
                checked.set(match said {
                    Ok(will) => Checked::Will(will),
                    Err(why) => Checked::Wont(why),
                });
            }
            // A preview needs no name: picking a source is enough to see what it would do.
            let shown = crate::api::preview_helper(kind.table, &value).await;
            if round.try_get_value() != Some(this) {
                return;
            }
            preview.set(Some(shown));
        });
    });

    let create = move || {
        let Some(kind) = draft.kind.get_untracked() else {
            return;
        };
        if busy.get_untracked() || done.get_untracked() {
            return;
        }
        let value = draft.value.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let made = crate::api::add_helper(kind.table, &value).await;
            busy.set(false);
            match made {
                Ok(()) => {
                    trouble.set(None);
                    done.set(true);
                    crate::refresh(live);
                    // The check draws itself before the window goes.
                    gloo_timers::future::sleep(std::time::Duration::from_millis(650)).await;
                    on_added.run(());
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };

    let ready = Memo::new(move |_| assistant.0.get().is_some_and(|status| status.ready));

    view! {
        <div class="builder">
            <div class="builder-main">
                {move || match draft.kind.get() {
                    None => picker(draft).into_any(),
                    Some(kind) => view! {
                        <div class="builder-kind">
                            <span class="kind-icon picked">{kind_icon(kind.table)}</span>
                            <div class="builder-kind-words">
                                <strong>{kind.word}</strong>
                                <p class="muted">{kind.about}</p>
                            </div>
                            <button
                                type="button"
                                class="quiet-button"
                                on:click=move |_| {
                                    draft.kind.set(None);
                                    code_words.set(None);
                                    code_trouble.set(None);
                                }
                            >
                                "Change"
                            </button>
                        </div>
                    }
                    .into_any(),
                }}
                <Show when=move || draft.kind.get().is_some()>
                    <div class="builder-modes">
                        {crate::segmented::segmented(
                            "Fill in as",
                            vec![(Mode::Form, "Form"), (Mode::Code, "JSON")],
                            mode.into(),
                            move |picked| {
                                code_words.set(None);
                                code_trouble.set(None);
                                mode.set(picked);
                            },
                        )}
                    </div>
                    <div
                        class="builder-body"
                        data-filled=move || {
                            let times = filled.get();
                            (times > 0).then_some(if times % 2 == 1 { "a" } else { "b" })
                        }
                    >
                        {move || match mode.get() {
                            Mode::Form => form(draft).into_any(),
                            Mode::Code => code(draft, code_words, code_trouble).into_any(),
                        }}
                    </div>
                    {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
                    {move || preview.get().and_then(Result::ok).map(|shown| tried(draft, shown))}
                    <div class="builder-foot">
                        {check_line(checked)}
                        <button
                            type="button"
                            class="add-one builder-create"
                            class:busy=move || busy.get()
                            class:done=move || done.get()
                            disabled=move || !matches!(checked.get(), Checked::Will(_)) || busy.get()
                            on:click=move |_| create()
                        >
                            <span class="add-one-label">"Create"</span>
                            <svg class="add-one-check" viewBox="0 0 24 24" aria-hidden="true"
                                fill="none" stroke="currentColor" stroke-width="2.2"
                                stroke-linecap="round" stroke-linejoin="round">
                                <path d="M5 12.5l4.5 4.5L19 7.5" pathLength="1" />
                            </svg>
                        </button>
                    </div>
                </Show>
            </div>
            <aside class="builder-assistant">
                {move || if ready.get() {
                    view! {
                        <crate::assistant::Chat
                            scope="helpers".to_owned()
                            title="Ask the assistant".to_owned()
                        />
                    }
                    .into_any()
                } else {
                    view! {
                        <div class="builder-ask">
                            <crate::assistant::Spark />
                            <p>
                                "With a model set up, the assistant can fill this in for you from \
                                 what you say: it knows what's in your home and every kind of helper."
                            </p>
                            <a href="/settings#assistant">"Set one up in Settings"</a>
                        </div>
                    }
                    .into_any()
                }}
            </aside>
        </div>
    }
}

/// The kinds, on two shelves, to pick one from.
fn picker(draft: Draft) -> impl IntoView {
    let shelf = |shelf: Shelf, heading: &'static str, about: &'static str| {
        view! {
            <section class="kind-shelf">
                <h3>{heading}</h3>
                <p class="muted">{about}</p>
                <div class="kind-cards">
                    {HELPER_KINDS
                        .into_iter()
                        .filter(|kind| kind.shelf == shelf)
                        .enumerate()
                        .map(|(at, kind)| view! {
                            <button
                                type="button"
                                class="kind-card"
                                style=format!("--at: {at}")
                                on:click=move |_| draft.pick(kind)
                            >
                                <span class="kind-icon">{kind_icon(kind.table)}</span>
                                <span class="kind-word">{kind.word}</span>
                                <span class="kind-about">{kind.about}</span>
                            </button>
                        })
                        .collect_view()}
                </div>
            </section>
        }
    };
    view! {
        <div class="kind-picker">
            {shelf(
                Shelf::Kept,
                "Kept by you",
                "Values you or a flow set. They stay as they were left through restarts.",
            )}
            {shelf(
                Shelf::Worked,
                "Worked out from your home",
                "Values Irori works out from other entities as they change.",
            )}
        </div>
    }
}

/// The kind's settings as a form: its name, then what it asks for.
fn form(draft: Draft) -> impl IntoView {
    let name = typed(draft, "name", text);
    view! {
        <div class="builder-form">
            <div class="builder-field wide">
                <span class="builder-label">"Name"</span>
                <input
                    type="text"
                    placeholder=move || draft.kind.get().map(|kind| kind.example).unwrap_or_default()
                    prop:value=name
                    on:input:target=move |ev| {
                        let typed = ev.target().value();
                        draft.put("name", text(&typed));
                        name.set(typed);
                    }
                />
            </div>
            // Drawn once per kind, so a box being typed in is never drawn again under the caret.
            {move || draft.kind.get().map(|kind| {
                kind.fields
                    .iter()
                    .copied()
                    .map(|field| {
                        let live = expect_context::<crate::Live>();
                        let shown = Memo::new(move |_| {
                            draft.value.with(|value| field.shown(value))
                                && (field.widget != Widget::SourceField
                                    || live.home.with(|home| {
                                        !number_fields(home, &draft.text("source")).is_empty()
                                    }))
                        });
                        view! { <Show when=move || shown.get()>{setting(draft, field)}</Show> }
                    })
                    .collect_view()
            })}
        </div>
    }
}

/// The whole helper as JSON, changed in place: what's typed is taken as soon as it reads.
fn code(
    draft: Draft,
    words: RwSignal<Option<String>>,
    trouble: RwSignal<Option<String>>,
) -> impl IntoView {
    let shown = Signal::derive(move || words.get().unwrap_or_else(|| draft.code()));
    let painted = NodeRef::<leptos::html::Pre>::new();
    let copied = RwSignal::new(None::<bool>);
    let copy = move |_| {
        let text = shown.get_untracked();
        spawn_local(async move {
            copied.set(Some(irori_ui_kit::clipboard::copy(&text).await));
            gloo_timers::future::sleep(std::time::Duration::from_millis(1600)).await;
            let _ = copied.try_set(None);
        });
    };
    view! {
        <p class="muted builder-code-about">
            "The same helper as it's kept in helpers.toml, written as JSON. Change anything; \
             the form follows."
        </p>
        <div class="json-box">
            <pre aria-hidden="true" node_ref=painted>
                {move || {
                    shown
                        .with(|text| irori_ui_kit::json::pieces(text))
                        .into_iter()
                        .map(|(kind, piece)| view! { <span class=kind.class()>{piece}</span> })
                        .collect_view()
                }}
                "\n"
            </pre>
            <textarea
                spellcheck="false"
                autocomplete="off"
                aria-label="The helper as JSON"
                prop:value=move || shown.get()
                on:input:target=move |ev| {
                    let typed = ev.target().value();
                    trouble.set(draft.take_code(&typed).err());
                    words.set(Some(typed));
                }
                on:scroll=move |ev| {
                    use web_sys::wasm_bindgen::JsCast as _;
                    let Some(pre) = painted.get_untracked() else { return };
                    if let Some(typed) =
                        ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    {
                        pre.set_scroll_top(typed.scroll_top());
                        pre.set_scroll_left(typed.scroll_left());
                    }
                }
            ></textarea>
        </div>
        {move || trouble.get().map(|why| view! { <p class="json-why" role="alert">{why}</p> })}
        <div class="json-actions">
            <button
                type="button"
                class="quiet-button json-copy"
                class:done=move || copied.get() == Some(true)
                on:click=copy
            >
                <svg class="copy-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none"
                    stroke="currentColor" stroke-width="1.8" stroke-linecap="round"
                    stroke-linejoin="round">
                    <g class="copy-clip">
                        <rect x="8" y="8" width="12" height="12" rx="2" />
                        <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
                    </g>
                    <path class="copy-check" d="M5 12.5l4.5 4.5L19 7.5" pathLength="1" />
                </svg>
                {move || match copied.get() {
                    Some(true) => "Copied",
                    Some(false) => "Couldn't copy",
                    None => "Copy",
                }}
            </button>
            <button
                type="button"
                class="quiet-button"
                title="Set it out again, one thing to a line"
                disabled=move || trouble.get().is_some()
                on:click=move |_| words.set(None)
            >
                "Format"
            </button>
        </div>
    }
}

/// What the helper would be like: for one worked out from other entities, what it would have
/// said over the last day, drawn as its history would be; for one a person sets, its control,
/// to try here without anything being sent. A group of things that switch can be tried on its
/// members.
fn tried(draft: Draft, shown: HelperPreview) -> AnyView {
    let HelperPreview {
        entity,
        states,
        computed,
    } = shown;
    if !computed {
        return try_here(entity, states.into_iter().next());
    }
    let waiting = states.iter().all(|state| state.state.is_none());
    let why = states
        .iter()
        .rev()
        .find_map(|state| {
            state
                .attributes
                .iter()
                .find(|(key, _)| key.as_str() == "problem")
                .and_then(|(_, why)| why.as_str().map(str::to_owned))
        })
        .unwrap_or_default();
    let now = states.last().cloned();
    let reading = crate::devices::control(&entity, now.as_ref(), false, quiet_controls());
    view! {
        <section class="builder-preview">
            <header class="builder-preview-head">
                <span class="builder-label">"Over the last day, had it been running"</span>
                {(!waiting).then(|| view! { <span class="builder-preview-now">"Now " {reading}</span> })}
            </header>
            {if waiting {
                view! { <p class="muted small">{why}</p> }.into_any()
            } else {
                crate::device::drawn_day(&entity, states, Signal::derive(|| None))
            }}
            {group_try(draft)}
        </section>
    }
    .into_any()
}

/// Controls that do nothing: a reading drawn where nothing can be asked of it.
fn quiet_controls() -> Controls {
    Controls {
        busy: RwSignal::new(BTreeSet::new()),
        failures: RwSignal::new(BTreeMap::new()),
        set_on: Callback::new(|_| {}),
        set_light: Callback::new(|_| {}),
        act: Callback::new(|_| {}),
    }
}

/// A helper a person sets, to try here: its own control, answering on this page only.
fn try_here(entity: irori_types::Entity, start: Option<EntityState>) -> AnyView {
    let state = RwSignal::new(start);
    let pressed = RwSignal::new(0u32);
    let capabilities = entity.capabilities.clone();
    let set = move |to: State| {
        state.update(|now| {
            if let Some(now) = now {
                now.state = Some(to);
            }
        });
    };
    let controls = Controls {
        busy: RwSignal::new(BTreeSet::new()),
        failures: RwSignal::new(BTreeMap::new()),
        set_on: Callback::new(move |(_, on): (irori_types::EntityId, bool)| {
            set(State::Switch(irori_types::SwitchState { on }));
        }),
        set_light: Callback::new(|_| {}),
        act: Callback::new(
            move |(_, action, data): (irori_types::EntityId, &'static str, Option<Value>)| {
                let data = data.unwrap_or(Value::Null);
                let now = state
                    .with_untracked(|state| state.as_ref().and_then(|state| state.state.clone()));
                match (action, &capabilities) {
                    ("press", _) => pressed.update(|times| *times += 1),
                    ("set_value", Capabilities::Number(_)) => {
                        if let Some(value) = data["value"].as_f64() {
                            set(State::Number(irori_types::NumberState { value }));
                        }
                    }
                    ("increment" | "decrement", Capabilities::Number(number)) => {
                        let by = data["by"].as_f64().unwrap_or(number.step);
                        let value = match now {
                            Some(State::Number(state)) => state.value,
                            _ => number.min,
                        };
                        let value = if action == "increment" {
                            value + by
                        } else {
                            value - by
                        };
                        set(State::Number(irori_types::NumberState {
                            value: value.clamp(number.min, number.max),
                        }));
                    }
                    ("set_value", Capabilities::Text(_)) => {
                        if let Some(value) = data["value"].as_str() {
                            set(State::Text(irori_types::TextState {
                                value: value.to_owned(),
                            }));
                        }
                    }
                    ("select_option", Capabilities::Select(_)) => {
                        if let Some(option) = data["option"].as_str() {
                            set(State::Select(irori_types::SelectState {
                                option: option.to_owned(),
                            }));
                        }
                    }
                    _ => {}
                }
            },
        ),
    };
    let name = entity.name.to_string();
    view! {
        <section class="builder-preview">
            <header class="builder-preview-head">
                <span class="builder-label">"Try it — only on this page, nothing is made or sent"</span>
            </header>
            <div class="builder-try">
                {crate::icons::entity(&entity.capabilities)}
                <span class="builder-try-name">{name}</span>
                <span class="builder-try-control">
                    {move || {
                        let now = state.get();
                        crate::devices::control(&entity, now.as_ref(), false, controls)
                    }}
                </span>
                {move || {
                    let times = pressed.get();
                    (times > 0).then(|| view! {
                        <span class="builder-pressed" data-beat=if times % 2 == 0 { "b" } else { "a" }>
                            {format!("Pressed {times}×")}
                        </span>
                    })
                }}
            </div>
        </section>
    }
    .into_any()
}

/// A group of lights, switches or fans can be tried on its members before it exists: the same
/// call the group would pass on, sent to each.
fn group_try(draft: Draft) -> Option<AnyView> {
    let kind = draft
        .value
        .with_untracked(|value| value.get("kind").and_then(Value::as_str).map(str::to_owned))?;
    let words = match kind.as_str() {
        "light" => "lights",
        "switch" => "switches",
        "fan" => "fans",
        _ => return None,
    };
    let controls = expect_context::<Controls>();
    let send = move |on: bool| {
        for member in listed(draft, "members") {
            if let Ok(id) = irori_types::EntityId::try_from(member.as_str()) {
                controls.set_on.run((id, on));
            }
        }
    };
    Some(
        view! {
            <div class="builder-group-try">
                <span class="muted small">{format!("Try it on the real {words}:")}</span>
                <button type="button" class="press" on:click=move |_| send(true)>"All on"</button>
                <button type="button" class="press" on:click=move |_| send(false)>"All off"</button>
            </div>
        }
        .into_any(),
    )
}

/// What the helper will be, or why it can't be yet. One line that changes in place.
fn check_line(checked: RwSignal<Checked>) -> impl IntoView {
    let state = move || match checked.get() {
        Checked::Quiet => "quiet",
        Checked::Checking => "checking",
        Checked::Will(_) => "will",
        Checked::Wont(_) => "wont",
    };
    view! {
        <p class="builder-check" data-state=state aria-live="polite">
            <span class="builder-check-mark" aria-hidden="true"></span>
            {move || match checked.get() {
                Checked::Quiet => view! { <span class="muted">"Give it a name to see what it makes."</span> }.into_any(),
                Checked::Checking => view! { <span class="muted">"Checking…"</span> }.into_any(),
                Checked::Will(will) => {
                    let taken = !will.taken.is_empty();
                    view! {
                        <span>
                            "Makes "
                            {will.entities.iter().enumerate().map(|(at, entity)| view! {
                                {(at > 0).then_some(" and ")}
                                <code>{entity.clone()}</code>
                            }).collect_view()}
                            {taken.then(|| view! {
                                <span class="muted">
                                    {format!(
                                        " — {} is taken, so it gets the next free id",
                                        will.taken.join(", "),
                                    )}
                                </span>
                            })}
                        </span>
                    }
                    .into_any()
                }
                Checked::Wont(why) => view! { <span>{why}</span> }.into_any(),
            }}
        </p>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choices_options_are_read_a_comma_apart() {
        assert_eq!(
            split_options("home\naway, asleep\n\n away "),
            ["home", "away", "asleep"]
        );
        assert!(split_options(" \n , ").is_empty());
    }

    #[test]
    fn a_number_box_keeps_what_it_says() {
        assert_eq!(number(""), None);
        assert_eq!(number("40"), Some(json!(40)));
        assert_eq!(number("0.5"), Some(json!(0.5)));
        // Kept as typed, for the check to say what's wrong with it.
        assert_eq!(number("lots"), Some(json!("lots")));
        assert_eq!(loose("heat"), Some(json!("heat")));
        assert_eq!(loose("true"), Some(json!(true)));
        assert_eq!(loose("3"), Some(json!(3)));
    }

    #[test]
    fn a_length_of_time_reads_in_its_best_unit() {
        assert_eq!(best_unit(7200.0), 3600.0);
        assert_eq!(best_unit(600.0), 60.0);
        assert_eq!(best_unit(90.0), 1.0);
        assert_eq!(minutes("24:00"), Some(1440));
        assert_eq!(minutes("7:3x"), None);
        assert_eq!(clock_time(390), "06:30");
    }

    /// A group has a `kind` of its own; written out and read back, it must keep it.
    #[test]
    fn a_helper_written_as_json_reads_back_the_same() {
        let draft = Draft {
            kind: RwSignal::new(None),
            value: RwSignal::new(Map::new()),
        };
        let groups = HelperKind::of_table("groups").expect("groups");
        draft.pick(groups);
        draft.put("name", Some(json!("Downstairs")));
        draft.put("members", Some(json!(["light.a", "light.b"])));
        let written = draft.code();
        assert!(
            written.starts_with("{\n  \"helper\": \"groups\",\n  \"name\""),
            "{written}"
        );
        let before = draft.value.get_untracked();
        draft.take_code(&written).expect("reads back");
        assert_eq!(draft.value.get_untracked(), before);
        assert_eq!(draft.value.get_untracked()["kind"], "light");
        assert_eq!(draft.kind.get_untracked(), Some(groups));
        assert!(draft.take_code("{\"helper\": \"nothing\"}").is_err());
        assert!(draft.take_code("[1]").is_err());
    }

    #[test]
    fn every_kind_starts_as_an_object_and_has_its_icon() {
        for kind in HELPER_KINDS {
            assert!(
                serde_json::from_str::<Map<String, Value>>(kind.start).is_ok(),
                "{}",
                kind.table
            );
            assert_ne!(
                irori_ui_kit::entity_icon::helper(kind.table),
                irori_ui_kit::entity_icon::helper("nothing"),
                "{}",
                kind.table
            );
        }
    }
}
