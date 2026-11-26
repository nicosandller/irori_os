//! Bringing a Home Assistant automation over as a flow (issue #198).
//!
//! The Home Assistant extension reads Home Assistant for this one (its manifest names this
//! extension in `answers`): the automations, and one automation's setup as Home Assistant keeps
//! it. Here that setup is translated into a flow: each trigger a trigger node, the conditions
//! gates one after another, the actions in order. The flow arrives switched off, remembering its
//! twin (`Flow::home_assistant`), and Home Assistant's automation goes on running.
//!
//! **All or nothing.** When anything in the automation can't be said as a flow yet, nothing is
//! brought over, and every such part is listed, so the owner sees why. Half an automation in
//! Irori would run differently from the whole one in Home Assistant.
//!
//! Nothing here asks Home Assistant to change anything, and only the home's owner may ask.

use std::collections::BTreeMap;
use std::time::Duration;

use irori_flow_types::Flow;
use irori_protocol::engine::{AppRequest, EngineClient};
use irori_rules::{CallData, MapRegistry, RuleService};
use irori_types::{Entity, ExtensionId, RuleId, ValueShape};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

/// The extension that reads Home Assistant.
const BRIDGE: &str = "homeassistant";
/// The page's questions.
pub const AUTOMATIONS: &str = "home_assistant.automations";
pub const BRING_OVER: &str = "home_assistant.bring_over";

/// Whether the page's question is one of these.
pub fn asks(method: &str) -> bool {
    method == AUTOMATIONS || method == BRING_OVER
}

/// What came back from Home Assistant, for the service's own loop to finish with: the reading
/// happens beside it, so flows keep running while Home Assistant answers.
#[derive(Debug)]
pub enum Fetched {
    List {
        request: AppRequest,
        list: Result<Value, String>,
    },
    Setup {
        request: AppRequest,
        list: Value,
        automation: Listed,
        setup: Result<Value, String>,
    },
}

/// One of Home Assistant's automations, as the Home Assistant extension lists it.
#[derive(Debug, Clone, Deserialize)]
pub struct Listed {
    pub entity_id: String,
    pub name: String,
    #[serde(default)]
    pub on: bool,
    /// Where its setup is kept; absent for one written in YAML without an `id`.
    #[serde(default)]
    pub id: Option<String>,
}

/// Starts answering one of the page's questions about Home Assistant: the reading goes on
/// beside the service's loop and comes back through `fetched`.
pub fn start(
    request: AppRequest,
    client: &EngineClient,
    bridged: bool,
    fetched: &mpsc::UnboundedSender<Fetched>,
) {
    if !request.caller.owner {
        request.answer(Err(
            "only the home's owner can bring Home Assistant's automations over".to_owned(),
        ));
        return;
    }
    // Asked only of a home Home Assistant's devices came into: anywhere else there is nobody to
    // ask, and a core older than the Home Assistant extension wouldn't know the question.
    if !bridged {
        request.answer(Err("this home has no Home Assistant".to_owned()));
        return;
    }
    let client = client.clone();
    let fetched = fetched.clone();
    tokio::spawn(async move {
        let bridge = ExtensionId::try_from(BRIDGE).expect("a valid id");
        let deadline = tokio::time::Instant::now() + WITHIN;
        let late = || "Home Assistant didn't answer in time; nothing was brought over".to_owned();
        let list = tokio::time::timeout_at(
            deadline,
            client.ask_extension(bridge.clone(), "automations.list", json!({})),
        )
        .await
        .map_err(|_| late())
        .and_then(|answer| answer.map_err(|e| e.to_string()));
        if request.method == AUTOMATIONS {
            let _ = fetched.send(Fetched::List { request, list });
            return;
        }
        let list = match list {
            Ok(list) => list,
            Err(why) => {
                request.answer(Err(why));
                return;
            }
        };
        let wanted = request.params["entity_id"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let automation = listed(&list)
            .into_iter()
            .find(|automation| automation.entity_id == wanted);
        let Some(automation) = automation else {
            request.answer(Err(format!("Home Assistant has no automation `{wanted}`")));
            return;
        };
        let setup = match &automation.id {
            Some(id) => tokio::time::timeout_at(
                deadline,
                client.ask_extension(bridge, "automation.setup", json!({ "id": id })),
            )
            .await
            .map_err(|_| late())
            .and_then(|answer| answer.map_err(|e| e.to_string())),
            None => Err(NO_ID.to_owned()),
        };
        let _ = fetched.send(Fetched::Setup {
            request,
            list,
            automation,
            setup,
        });
    });
}

/// What a device action may say besides which entity and what to do.
const DEVICE_ACTION_KEYS: &[&str] = &[
    "device_id",
    "domain",
    "type",
    "entity_id",
    "alias",
    "enabled",
    "continue_on_error",
    "metadata",
];

/// What a service call may say.
const CALL_KEYS: &[&str] = &[
    "action",
    "service",
    "target",
    "data",
    "service_data",
    "data_template",
    "entity_id",
    "alias",
    "enabled",
    "continue_on_error",
    "metadata",
    "response_variable",
];

/// The first key of `item` not in `known`.
fn unknown_key<'v>(item: &'v Value, known: &[&str]) -> Option<&'v str> {
    item.as_object()?
        .keys()
        .map(String::as_str)
        .find(|key| !known.contains(key))
}

/// How long both questions to Home Assistant may take together: less than the page waits for an
/// answer, so a flow is never made after the page was told it wasn't.
pub const WITHIN: Duration = Duration::from_secs(25);

/// Why one without an id can't come over.
const NO_ID: &str = "it's written in Home Assistant's YAML without an `id`, so Home Assistant \
    keeps no setup of it to read";

/// The automations in the Home Assistant extension's list.
pub fn listed(list: &Value) -> Vec<Listed> {
    list["automations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|automation| Listed::deserialize(automation).ok())
        .collect()
}

/// Home Assistant's `entity_id`s brought over, with their `unique_id`s in Irori.
pub fn brought(list: &Value) -> BTreeMap<String, String> {
    list["entities"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(ha, unique)| Some((ha.clone(), unique.as_str()?.to_owned())))
        .collect()
}

/// The flow brought over from `twin`, if there is one.
pub fn twin_of<'a>(
    flows: impl IntoIterator<Item = &'a Flow>,
    automation: &Listed,
) -> Option<RuleId> {
    flows
        .into_iter()
        .find(|flow| {
            flow.home_assistant.as_ref().is_some_and(|twin| {
                automation.id.as_deref() == Some(twin.id.as_str())
                    || twin.entity_id == automation.entity_id
            })
        })
        .map(|flow| flow.id.clone())
}

/// A free flow id for `entity_id`'s automation: its object id, made a slug.
pub fn free_id(entity_id: &str, taken: impl Fn(&RuleId) -> bool) -> Result<RuleId, String> {
    let object = entity_id.strip_prefix("automation.").unwrap_or(entity_id);
    let mut base: String = object
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while base.contains("__") {
        base = base.replace("__", "_");
    }
    let base = base.trim_matches('_');
    let base: String = if base.is_empty() {
        "home_assistant"
    } else {
        base
    }
    .chars()
    .take(56)
    .collect();
    let base = base.trim_end_matches('_');
    for n in 1..100 {
        let candidate = if n == 1 {
            base.to_owned()
        } else {
            format!("{base}_{n}")
        };
        if let Ok(id) = RuleId::try_from(candidate.as_str())
            && !taken(&id)
        {
            return Ok(id);
        }
    }
    Err(format!("no free name for a flow from `{entity_id}`"))
}

/// The Irori entities Home Assistant's are, by their `entity_id` and by their registry `id`
/// (which device triggers name).
#[derive(Debug)]
pub struct Home<'a> {
    by_ha: BTreeMap<String, String>,
    by_unique: BTreeMap<&'a str, &'a Entity>,
}

impl<'a> Home<'a> {
    pub fn new(registry: &'a MapRegistry, brought: BTreeMap<String, String>) -> Self {
        let by_unique = registry
            .entities
            .values()
            .filter(|entity| entity.protocol.as_str() == BRIDGE)
            .map(|entity| (entity.unique_id.as_str(), entity))
            .collect();
        Self {
            by_ha: brought,
            by_unique,
        }
    }

    /// The Irori entity for Home Assistant's `ha` (an `entity_id`, or a registry `id`).
    fn entity(&self, ha: &str) -> Result<&'a Entity, String> {
        let unique = self.by_ha.get(ha).map_or(ha, String::as_str);
        if let Some(entity) = self.by_unique.get(unique) {
            return Ok(entity);
        }
        if self.by_ha.contains_key(ha) {
            Err(format!(
                "`{ha}` isn't in this home yet: add its device on the Devices page first"
            ))
        } else {
            Err(format!("`{ha}` isn't brought over from Home Assistant"))
        }
    }
}

/// Translates one automation's setup into a flow, switched off and remembering its twin. Every
/// part that can't be translated is listed instead, and then no flow is made.
pub fn translate(
    setup: &Value,
    automation: &Listed,
    id: RuleId,
    home: &Home<'_>,
) -> Result<Flow, Vec<String>> {
    let Some(twin_id) = automation.id.clone() else {
        return Err(vec![NO_ID.to_owned()]);
    };
    let mut build = Build {
        home,
        nodes: Map::new(),
        wires: Vec::new(),
        missing: Vec::new(),
        count: BTreeMap::new(),
    };

    // Triggers: each one starts the chain.
    let mut tails: Vec<String> = Vec::new();
    for trigger in items(setup, &["triggers", "trigger"]) {
        if disabled(trigger) {
            continue;
        }
        for node in build.trigger(trigger) {
            let id = build.add("when", node);
            tails.push(id);
        }
    }
    if tails.is_empty() && build.missing.is_empty() {
        build
            .missing
            .push("it has no trigger, so nothing would start it".to_owned());
    }
    // Conditions: gates, one after another.
    for condition in items(setup, &["conditions", "condition"]) {
        if disabled(condition) {
            continue;
        }
        if let Some(condition) = build.condition(condition) {
            let gate = build.add("check", json!({ "type": "gate", "condition": condition }));
            build.wire(&tails, &gate);
            tails = vec![format!("{gate}:yes")];
        }
    }
    // Actions, in order.
    let actions = items(setup, &["actions", "action"]);
    if actions.iter().all(|action| disabled(action)) {
        build
            .missing
            .push("it has no actions, so it would do nothing".to_owned());
    }
    for action in actions {
        if disabled(action) {
            continue;
        }
        if tails.is_empty() {
            // After a stop: Home Assistant never gets here either.
            break;
        }
        tails = build.action(action, tails);
    }

    let mode = build.mode(setup);
    if !build.missing.is_empty() {
        build.missing.dedup();
        return Err(build.missing);
    }
    let name = setup["alias"]
        .as_str()
        .filter(|alias| !alias.trim().is_empty())
        .unwrap_or(&automation.name);
    let mut flow = json!({
        "id": id,
        "name": clip(name, 80),
        "enabled": false,
        "nodes": build.nodes,
        "wires": build.wires,
        "home_assistant": { "entity_id": automation.entity_id, "id": twin_id },
    });
    if let Some(mode) = mode {
        flow["mode"] = mode;
    }
    if let Some(description) = setup["description"]
        .as_str()
        .filter(|d| !d.trim().is_empty())
    {
        flow["description"] = json!(clip(description, 500));
    }
    serde_json::from_value::<Flow>(flow)
        .map_err(|why| vec![format!("Irori couldn't make a flow of it: {why}")])
}

fn clip(text: &str, most: usize) -> String {
    text.trim().chars().take(most).collect()
}

/// A list under the first of `keys` that's there: one item on its own is a list of one.
fn items<'v>(value: &'v Value, keys: &[&str]) -> Vec<&'v Value> {
    for key in keys {
        match value.get(*key) {
            Some(Value::Array(all)) => return all.iter().collect(),
            Some(Value::Null) | None => {}
            Some(one) => return vec![one],
        }
    }
    Vec::new()
}

/// A part switched off in Home Assistant: it does nothing there, so it's left out here too.
fn disabled(item: &Value) -> bool {
    item.get("enabled") == Some(&Value::Bool(false))
}

/// Whether anything in `value` is a template, which Irori doesn't run.
fn templated(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("{{") || text.contains("{%"),
        Value::Array(all) => all.iter().any(templated),
        Value::Object(map) => map.values().any(templated),
        _ => false,
    }
}

/// One text or a list of them.
fn texts(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(one) => Some(vec![one.clone()]),
        Value::Array(all) => all.iter().map(|v| v.as_str().map(str::to_owned)).collect(),
        _ => None,
    }
}

/// A number, written as one or as text.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

/// Home Assistant's duration (`"00:05:00"`, `{"minutes": 5}`, or seconds) as a compact one
/// (`5m`). `None` for none at all.
fn duration(value: &Value) -> Result<Option<String>, String> {
    let bad = || format!("the duration {value} isn't one Irori can read");
    let millis: i64 = match value {
        Value::Null => return Ok(None),
        Value::Number(n) => (n.as_f64().ok_or_else(bad)? * 1000.0).round() as i64,
        Value::String(text) => {
            if text.contains("{{") {
                return Err("a duration worked out by a template".to_owned());
            }
            let (sign, rest) = match text.trim().strip_prefix('-') {
                Some(rest) => (-1, rest),
                None => (1, text.trim().trim_start_matches('+')),
            };
            let parts: Vec<&str> = rest.split(':').collect();
            let part = |p: &str| p.parse::<f64>().ok().filter(|n| *n >= 0.0);
            let seconds = match parts.as_slice() {
                [s] => part(s).ok_or_else(bad)?,
                [h, m] => part(h).ok_or_else(bad)? * 3600.0 + part(m).ok_or_else(bad)? * 60.0,
                [h, m, s] => {
                    part(h).ok_or_else(bad)? * 3600.0
                        + part(m).ok_or_else(bad)? * 60.0
                        + part(s).ok_or_else(bad)?
                }
                _ => return Err(bad()),
            };
            sign * (seconds * 1000.0).round() as i64
        }
        Value::Object(map) => {
            let mut total = 0.0;
            for (key, amount) in map {
                let per = match key.as_str() {
                    "days" => 86_400_000.0,
                    "hours" => 3_600_000.0,
                    "minutes" => 60_000.0,
                    "seconds" => 1000.0,
                    "milliseconds" => 1.0,
                    _ => return Err(bad()),
                };
                total += number(amount).ok_or_else(bad)? * per;
            }
            total.round() as i64
        }
        _ => return Err(bad()),
    };
    if millis == 0 {
        return Ok(None);
    }
    Ok(Some(compact(millis)))
}

/// Milliseconds as `1h5m`, `-30m`, `500ms`.
fn compact(millis: i64) -> String {
    let mut out = String::new();
    if millis < 0 {
        out.push('-');
    }
    let mut left = millis.unsigned_abs();
    for (unit, size) in [
        ("d", 86_400_000),
        ("h", 3_600_000),
        ("m", 60_000),
        ("s", 1000),
        ("ms", 1),
    ] {
        if left >= size {
            out.push_str(&format!("{}{unit}", left / size));
            left %= size;
        }
    }
    out
}

/// `HH:MM` or `HH:MM:SS`, as a time of day.
fn time_of_day(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.split(':').collect();
    let two = |p: &str, most: u32| {
        p.parse::<u32>()
            .ok()
            .filter(|n| *n <= most)
            .map(|n| format!("{n:02}"))
    };
    match parts.as_slice() {
        [h, m] => Some(format!("{}:{}", two(h, 23)?, two(m, 59)?)),
        [h, m, s] => {
            let s = two(s, 59)?;
            let hm = format!("{}:{}", two(h, 23)?, two(m, 59)?);
            Some(if s == "00" { hm } else { format!("{hm}:{s}") })
        }
        _ => None,
    }
}

/// Home Assistant's weekdays, as Irori writes them.
fn weekdays(value: &Value) -> Result<Option<Value>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let days = texts(value).ok_or("its weekdays aren't readable")?;
    for day in &days {
        if !["mon", "tue", "wed", "thu", "fri", "sat", "sun"].contains(&day.as_str()) {
            return Err(format!("the weekday `{day}`"));
        }
    }
    Ok(Some(json!(days)))
}

/// What a flow is made of, as it's built.
struct Build<'h, 'a> {
    home: &'h Home<'a>,
    nodes: Map<String, Value>,
    wires: Vec<Value>,
    /// Each part that can't be translated, said for the owner.
    missing: Vec<String>,
    count: BTreeMap<&'static str, usize>,
}

impl Build<'_, '_> {
    /// Adds a node named `<prefix>_<n>` and answers its id.
    fn add(&mut self, prefix: &'static str, node: Value) -> String {
        let n = self.count.entry(prefix).or_insert(0);
        *n += 1;
        let id = format!("{prefix}_{n}");
        self.nodes.insert(id.clone(), node);
        id
    }

    /// Wires every one of `from` (`node` or `node:port`) to `to`.
    fn wire(&mut self, from: &[String], to: &str) {
        for from in from {
            self.wires.push(json!([from, to]));
        }
    }

    fn miss(&mut self, what: impl Into<String>) {
        self.missing.push(what.into());
    }

    /// The Irori entity for `ha`, or why not (said once).
    fn entity(&mut self, ha: &str) -> Option<&Entity> {
        match self.home.entity(ha) {
            Ok(entity) => Some(entity),
            Err(why) => {
                self.miss(why);
                None
            }
        }
    }

    /// One of Home Assistant's states as the value `entity` has in Irori.
    fn value(&mut self, entity: &Entity, ha_state: &str) -> Option<Value> {
        let shape = entity.capabilities.primary_shape();
        let found = match (shape, ha_state) {
            (_, "unavailable" | "unknown") => None,
            (Some(ValueShape::Bool), "on" | "home" | "true") => Some(json!(true)),
            (Some(ValueShape::Bool), "off" | "not_home" | "false") => Some(json!(false)),
            (Some(ValueShape::Number), text) => number(&json!(text)).map(|n| json!(n)),
            (Some(ValueShape::Text), text) => match entity.capabilities.text_options() {
                Some(options) if !options.iter().any(|o| o == text) => None,
                _ => Some(json!(text)),
            },
            _ => None,
        };
        if found.is_none() {
            self.miss(format!(
                "it looks for `{}` to be `{ha_state}`, which Irori can't watch for",
                entity.id
            ));
        }
        found
    }

    /// Values for a trigger's `to` or `from`: one, any of several, or none (any change).
    fn values(&mut self, entity: &Entity, value: Option<&Value>) -> Option<Option<Value>> {
        let Some(value) = value.filter(|v| !v.is_null()) else {
            return Some(None);
        };
        let Some(wanted) = texts(value) else {
            self.miss(format!("a state of `{}` written as {value}", entity.id));
            return None;
        };
        let mut out = Vec::new();
        for text in wanted {
            out.push(self.value(entity, &text)?);
        }
        out.dedup();
        Some(Some(if out.len() == 1 {
            out.remove(0)
        } else {
            Value::Array(out)
        }))
    }

    /// One trigger, as one trigger node per entity it watches.
    fn trigger(&mut self, trigger: &Value) -> Vec<Value> {
        let kind = trigger
            .get("trigger")
            .or_else(|| trigger.get("platform"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if templated(trigger) {
            self.miss(format!(
                "its `{kind}` trigger uses a template, which Irori doesn't run"
            ));
            return Vec::new();
        }
        for key in [
            "variables",
            "attribute",
            "value_template",
            "not_to",
            "not_from",
        ] {
            if trigger.get(key).is_some_and(|v| !v.is_null()) {
                self.miss(format!("its `{kind}` trigger's `{key}`"));
                return Vec::new();
            }
        }
        let hold = match duration(trigger.get("for").unwrap_or(&Value::Null)) {
            Ok(hold) => hold,
            Err(why) => {
                self.miss(format!("its `{kind}` trigger waits for {why}"));
                return Vec::new();
            }
        };
        match kind {
            "state" | "numeric_state" => {
                let Some(entity_ids) = texts(trigger.get("entity_id").unwrap_or(&Value::Null))
                else {
                    self.miss(format!("a `{kind}` trigger without an entity"));
                    return Vec::new();
                };
                let mut nodes = Vec::new();
                for ha in entity_ids {
                    let Some(entity) = self.entity(&ha) else {
                        continue;
                    };
                    let entity = entity.clone();
                    let mut node = json!({ "type": "state", "entity": entity.id });
                    if kind == "state" {
                        let Some(to) = self.values(&entity, trigger.get("to")) else {
                            continue;
                        };
                        let Some(from) = self.values(&entity, trigger.get("from")) else {
                            continue;
                        };
                        if let Some(to) = to {
                            node["to"] = to;
                        }
                        if let Some(from) = from {
                            node["from"] = from;
                        }
                    } else {
                        for side in ["above", "below"] {
                            match trigger.get(side).filter(|v| !v.is_null()) {
                                None => {}
                                Some(level) => match number(level) {
                                    Some(level) => node[side] = json!(level),
                                    None => {
                                        self.miss(format!(
                                            "a `numeric_state` trigger compared with {level}"
                                        ));
                                        continue;
                                    }
                                },
                            }
                        }
                    }
                    if let Some(hold) = &hold {
                        node["for"] = json!(hold);
                    }
                    nodes.push(json!({ "type": "trigger", "trigger": node }));
                }
                nodes
            }
            "time" => {
                let at = texts(trigger.get("at").unwrap_or(&Value::Null));
                let weekday = match weekdays(trigger.get("weekday").unwrap_or(&Value::Null)) {
                    Ok(weekday) => weekday,
                    Err(why) => {
                        self.miss(format!("a time trigger on {why}"));
                        return Vec::new();
                    }
                };
                let Some(at) = at else {
                    self.miss("a time trigger at a time Irori can't read");
                    return Vec::new();
                };
                let mut nodes = Vec::new();
                for at in at {
                    match time_of_day(&at) {
                        Some(at) => {
                            let mut node = json!({ "type": "time", "at": at });
                            if let Some(weekday) = &weekday {
                                node["weekday"] = weekday.clone();
                            }
                            nodes.push(json!({ "type": "trigger", "trigger": node }));
                        }
                        None => self.miss(format!(
                            "a time trigger at `{at}`: Irori can't follow a time kept in an entity yet"
                        )),
                    }
                }
                nodes
            }
            "sun" => {
                let event = trigger["event"].as_str().unwrap_or("");
                if !["sunrise", "sunset"].contains(&event) {
                    self.miss(format!("a sun trigger at `{event}`"));
                    return Vec::new();
                }
                let mut node = json!({ "type": "sun", "event": event });
                match duration(trigger.get("offset").unwrap_or(&Value::Null)) {
                    Ok(Some(offset)) => node["offset"] = json!(offset),
                    Ok(None) => {}
                    Err(why) => {
                        self.miss(format!("a sun trigger moved by {why}"));
                        return Vec::new();
                    }
                }
                vec![json!({ "type": "trigger", "trigger": node })]
            }
            "homeassistant" if trigger["event"] == "start" => {
                vec![json!({ "type": "trigger", "trigger": { "type": "startup" } })]
            }
            "time_pattern" => match cron(trigger) {
                Ok(cron) => {
                    vec![json!({ "type": "trigger", "trigger": { "type": "time", "cron": cron } })]
                }
                Err(why) => {
                    self.miss(format!("a time pattern trigger {why}"));
                    Vec::new()
                }
            },
            "device" => {
                let domain = trigger["domain"].as_str().unwrap_or("");
                let event = trigger["type"].as_str().unwrap_or("");
                let Some(ha) = trigger["entity_id"].as_str() else {
                    self.miss(format!(
                        "a `{domain}` device trigger `{event}` on the whole device"
                    ));
                    return Vec::new();
                };
                let Some(entity) = self.entity(ha) else {
                    return Vec::new();
                };
                let entity = entity.clone();
                let to = match switched(domain, event) {
                    Some(Some(on)) => Some(json!(on)),
                    Some(None) => None,
                    None => {
                        self.miss(format!(
                            "Home Assistant's `{domain}` device trigger `{event}`"
                        ));
                        return Vec::new();
                    }
                };
                if to.is_some() && entity.capabilities.primary_shape() != Some(ValueShape::Bool) {
                    self.miss(format!(
                        "Home Assistant's `{domain}` device trigger `{event}`"
                    ));
                    return Vec::new();
                }
                let mut node = json!({ "type": "state", "entity": entity.id });
                if let Some(to) = to {
                    node["to"] = to;
                }
                if let Some(hold) = &hold {
                    node["for"] = json!(hold);
                }
                vec![json!({ "type": "trigger", "trigger": node })]
            }
            "" => {
                self.miss("a trigger Irori can't read");
                Vec::new()
            }
            other => {
                self.miss(format!(
                    "Home Assistant's `{other}` trigger, which Irori flows don't have"
                ));
                Vec::new()
            }
        }
    }

    /// One condition, as Irori's.
    fn condition(&mut self, condition: &Value) -> Option<Value> {
        // A condition left out would make the flow do more than Home Assistant's automation
        // does, so one that can't come over always says why.
        let said = self.missing.len();
        let out = self.condition_of(condition);
        if out.is_none() && self.missing.len() == said {
            self.miss(format!(
                "a `{}` condition Irori can't read",
                condition["condition"].as_str().unwrap_or("?")
            ));
        }
        out
    }

    fn condition_of(&mut self, condition: &Value) -> Option<Value> {
        if condition.is_string() || templated(condition) {
            self.miss("a condition that's a template, which Irori doesn't run");
            return None;
        }
        let kind = condition["condition"].as_str().unwrap_or("");
        for key in ["attribute", "value_template", "for", "match"] {
            if condition.get(key).is_some_and(|v| !v.is_null())
                && !(key == "match" && condition[key] == "all")
            {
                self.miss(format!("a `{kind}` condition's `{key}`"));
                return None;
            }
        }
        match kind {
            "state" => {
                let entity_ids = texts(&condition["entity_id"]);
                let states = texts(&condition["state"]);
                let (Some(entity_ids), Some(states)) = (entity_ids, states) else {
                    self.miss("a state condition Irori can't read");
                    return None;
                };
                let mut all = Vec::new();
                for ha in entity_ids {
                    let entity = self.entity(&ha)?.clone();
                    let mut any = Vec::new();
                    for state in &states {
                        let is = self.value(&entity, state)?;
                        any.push(json!({ "type": "state", "entity": entity.id, "is": is }));
                    }
                    all.push(one_or(any, "any"));
                }
                Some(one_or(all, "all"))
            }
            "numeric_state" => {
                let Some(entity_ids) = texts(&condition["entity_id"]).filter(|ids| !ids.is_empty())
                else {
                    self.miss("a `numeric_state` condition on no entity Irori can read");
                    return None;
                };
                let mut all = Vec::new();
                for ha in entity_ids {
                    let entity = self.entity(&ha)?.clone();
                    let mut parts = Vec::new();
                    for (side, op) in [("above", ">"), ("below", "<")] {
                        if let Some(level) = condition.get(side).filter(|v| !v.is_null()) {
                            let Some(level) = number(level) else {
                                self.miss(format!(
                                    "a `numeric_state` condition compared with {level}"
                                ));
                                return None;
                            };
                            parts.push(format!("num('{}') {op} {level}", entity.id));
                        }
                    }
                    if parts.is_empty() {
                        self.miss("a `numeric_state` condition with no level");
                        return None;
                    }
                    all.push(json!({ "type": "expr", "expr": parts.join(" && ") }));
                }
                Some(one_or(all, "all"))
            }
            "time" => {
                let mut out = json!({ "type": "time" });
                for side in ["after", "before"] {
                    if let Some(at) = condition.get(side).and_then(Value::as_str) {
                        let Some(at) = time_of_day(at) else {
                            self.miss(format!(
                                "a time condition {side} `{at}`: Irori can't follow a time kept in an entity yet"
                            ));
                            return None;
                        };
                        out[side] = json!(at);
                    }
                }
                match weekdays(condition.get("weekday").unwrap_or(&Value::Null)) {
                    Ok(Some(days)) => out["weekday"] = days,
                    Ok(None) => {}
                    Err(why) => {
                        self.miss(format!("a time condition on {why}"));
                        return None;
                    }
                }
                Some(out)
            }
            "sun" => {
                // In Home Assistant both sides must hold, each on the same day: `after: sunset,
                // before: sunrise` is never true. One Irori sun condition with both is a window
                // that may wrap past midnight, so each side is its own condition.
                let mut sides = Vec::new();
                for side in ["after", "before"] {
                    let mut out = json!({ "type": "sun" });
                    if let Some(event) = condition.get(side).and_then(Value::as_str) {
                        if !["sunrise", "sunset"].contains(&event) {
                            self.miss(format!("a sun condition {side} `{event}`"));
                            return None;
                        }
                        out[side] = json!(event);
                    }
                    let key = format!("{side}_offset");
                    match duration(condition.get(&key).unwrap_or(&Value::Null)) {
                        Ok(Some(offset)) => out[&key] = json!(offset),
                        Ok(None) => {}
                        Err(why) => {
                            self.miss(format!("a sun condition moved by {why}"));
                            return None;
                        }
                    }
                    if out.get(side).is_some() {
                        sides.push(out);
                    } else if out.get(&key).is_some() {
                        self.miss(format!("a sun condition with `{key}` but no `{side}`"));
                        return None;
                    }
                }
                if sides.is_empty() {
                    self.miss("a sun condition with neither `after` nor `before`");
                    return None;
                }
                Some(one_or(sides, "all"))
            }
            "and" | "or" | "not" => {
                let mut inner = Vec::new();
                for child in items(condition, &["conditions"]) {
                    if !disabled(child) {
                        inner.push(self.condition(child)?);
                    }
                }
                if inner.is_empty() {
                    self.miss(format!("an empty `{kind}` condition"));
                    return None;
                }
                Some(match kind {
                    "and" => one_or(inner, "all"),
                    "or" => one_or(inner, "any"),
                    // None of them holds.
                    _ => json!({ "type": "not", "condition": one_or(inner, "any") }),
                })
            }
            "device" => {
                let domain = condition["domain"].as_str().unwrap_or("");
                let event = condition["type"].as_str().unwrap_or("");
                let on = event
                    .strip_prefix("is_")
                    .and_then(|rest| switched(domain, rest));
                let (Some(Some(on)), Some(ha)) = (on, condition["entity_id"].as_str()) else {
                    self.miss(format!(
                        "Home Assistant's `{domain}` device condition `{event}`"
                    ));
                    return None;
                };
                let entity = self.entity(ha)?.clone();
                if entity.capabilities.primary_shape() != Some(ValueShape::Bool) {
                    self.miss(format!(
                        "Home Assistant's `{domain}` device condition `{event}`"
                    ));
                    return None;
                }
                Some(json!({ "type": "state", "entity": entity.id, "is": on }))
            }
            other => {
                self.miss(format!(
                    "Home Assistant's `{other}` condition, which Irori flows don't have"
                ));
                None
            }
        }
    }

    /// One action, after `tails`: answers where the next one goes.
    fn action(&mut self, action: &Value, tails: Vec<String>) -> Vec<String> {
        if templated(action) {
            self.miss("an action that uses a template, which Irori doesn't run");
            return tails;
        }
        if let Some(service) = action
            .get("action")
            .or_else(|| action.get("service"))
            .and_then(Value::as_str)
        {
            return self.call(action, service, tails);
        }
        if let Some(scene) = action.get("scene").and_then(Value::as_str) {
            let call = json!({ "action": "scene.turn_on", "target": { "entity_id": scene } });
            return self.call(&call, "scene.turn_on", tails);
        }
        if action.get("device_id").is_some() && action.get("domain").is_some() {
            let domain = action["domain"].as_str().unwrap_or("");
            let event = action["type"].as_str().unwrap_or("");
            let Some(ha) = action["entity_id"].as_str() else {
                self.miss(format!(
                    "a `{domain}` device action `{event}` on the whole device"
                ));
                return tails;
            };
            if !["turn_on", "turn_off", "toggle"].contains(&event) {
                self.miss(format!(
                    "Home Assistant's `{domain}` device action `{event}`"
                ));
                return tails;
            }
            // A setting beside it (a brightness, a colour) would be lost.
            if let Some(extra) = unknown_key(action, DEVICE_ACTION_KEYS) {
                self.miss(format!(
                    "the `{domain}` device action `{event}` with `{extra}`, which this import can't bring over yet"
                ));
                return tails;
            }
            let call = json!({ "target": { "entity_id": ha } });
            return self.call(&call, &format!("homeassistant.{event}"), tails);
        }
        if let Some(wait) = action.get("delay") {
            return match duration(wait) {
                Ok(Some(hold)) if !hold.starts_with('-') => {
                    let id = self.add("wait", json!({ "type": "delay", "for": hold }));
                    self.wire(&tails, &id);
                    vec![id]
                }
                Ok(_) => tails,
                Err(why) => {
                    self.miss(format!("a delay of {why}"));
                    tails
                }
            };
        }
        if action.get("condition").is_some() {
            let Some(condition) = self.condition(action) else {
                return tails;
            };
            let gate = self.add("check", json!({ "type": "gate", "condition": condition }));
            self.wire(&tails, &gate);
            return vec![format!("{gate}:yes")];
        }
        if let Some(stop) = action.get("stop") {
            let mut node = json!({ "type": "stop" });
            if let Some(reason) = stop.as_str().filter(|r| !r.trim().is_empty()) {
                node["reason"] = json!(clip(reason, 200));
            }
            let id = self.add("end", node);
            self.wire(&tails, &id);
            return Vec::new();
        }
        let what = action
            .as_object()
            .and_then(|map| {
                map.keys()
                    .find(|key| !["alias", "enabled", "continue_on_error"].contains(&key.as_str()))
            })
            .map_or("an action", String::as_str);
        self.miss(match what {
            "choose" | "if" => {
                "a choice between actions (`choose`/`if`), which this import can't bring over yet"
                    .to_owned()
            }
            "repeat" => "a repeat, which Irori flows don't have yet".to_owned(),
            "wait_for_trigger" | "wait_template" => {
                format!("a `{what}`, which this import can't bring over yet")
            }
            other => format!("Home Assistant's `{other}` action, which Irori flows don't have"),
        });
        tails
    }

    /// A service call on each entity it targets, one after another.
    fn call(&mut self, action: &Value, service: &str, tails: Vec<String>) -> Vec<String> {
        let Some((domain, name)) = service.split_once('.') else {
            self.miss(format!("the action `{service}`"));
            return tails;
        };
        if let Some(extra) = unknown_key(action, CALL_KEYS) {
            self.miss(format!(
                "`{service}` with `{extra}`, which this import can't bring over yet"
            ));
            return tails;
        }
        if action.get("response_variable").is_some() {
            self.miss(format!("`{service}` keeps its answer in a variable"));
            return tails;
        }
        let target = action.get("target").cloned().unwrap_or(Value::Null);
        for key in ["device_id", "area_id", "floor_id", "label_id"] {
            if target.get(key).is_some_and(|v| !v.is_null()) {
                self.miss(format!(
                    "`{service}` on a whole {}: say which entities in Home Assistant first",
                    key.trim_end_matches("_id")
                ));
                return tails;
            }
        }
        let mut data: Map<String, Value> = action
            .get("data")
            .or_else(|| action.get("service_data"))
            .or_else(|| action.get("data_template"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let named = target
            .get("entity_id")
            .or_else(|| action.get("entity_id"))
            .cloned()
            .or_else(|| data.remove("entity_id"));
        data.remove("entity_id");
        let Some(entity_ids) = named.as_ref().and_then(texts).filter(|ids| !ids.is_empty()) else {
            self.miss(format!("`{service}` on no entity in particular"));
            return tails;
        };
        if entity_ids.iter().any(|id| id == "all") {
            self.miss(format!("`{service}` on every entity at once"));
            return tails;
        }
        let keeps_going = action.get("continue_on_error") == Some(&Value::Bool(true));
        let mut tails = tails;
        for ha in entity_ids {
            let Some(entity) = self.entity(&ha) else {
                continue;
            };
            let entity = entity.clone();
            let kind = entity.capabilities.kind();
            if domain != "homeassistant" && domain != kind.domain() {
                self.miss(format!(
                    "`{service}` on `{ha}`, which Irori has as a {}",
                    kind.domain()
                ));
                continue;
            }
            let Some(rule_service) = RuleService::of(kind, name) else {
                self.miss(format!(
                    "`{service}`: Irori can't ask a {} to `{name}`",
                    kind.domain()
                ));
                continue;
            };
            let call_data = (!data.is_empty()).then(|| CallData(data.clone()));
            if let Err(why) = rule_service.validate_data(call_data.as_ref()) {
                self.miss(format!("`{service}`'s settings on `{ha}`: {why}"));
                continue;
            }
            let mut node = json!({ "type": "call", "service": rule_service, "entity": entity.id });
            if !data.is_empty() {
                node["data"] = Value::Object(data.clone());
            }
            let id = self.add("do", node);
            self.wire(&tails, &id);
            tails = if keeps_going {
                vec![id.clone(), format!("{id}:error")]
            } else {
                vec![id]
            };
        }
        tails
    }

    /// The automation's mode, as a flow's. `None` for the usual one, which says nothing.
    fn mode(&mut self, setup: &Value) -> Option<Value> {
        let most = setup.get("max").map_or(Some(10.0), number);
        match setup["mode"].as_str().unwrap_or("single") {
            "single" => None,
            "restart" => Some(json!("restart")),
            kind @ ("queued" | "parallel") => match most {
                Some(most) if (1.0..=32.0).contains(&most) => {
                    Some(json!({ "type": kind, "max": most as u8 }))
                }
                _ => {
                    self.miss(format!("its `{kind}` mode with more than 32 runs at once"));
                    None
                }
            },
            other => {
                self.miss(format!("its `{other}` mode"));
                None
            }
        }
    }
}

/// `items` as one condition: the one, or all/any of them.
fn one_or(mut items: Vec<Value>, combine: &str) -> Value {
    if items.len() == 1 {
        items.remove(0)
    } else {
        json!({ "type": combine, "conditions": items })
    }
}

/// What a device trigger's `type` says the entity turned to: on (`Some(Some(true))`), off, or
/// any change (`Some(None)`). `None` for one this import doesn't know.
fn switched(domain: &str, event: &str) -> Option<Option<bool>> {
    match (domain, event) {
        (_, "turned_on" | "on") => Some(Some(true)),
        (_, "turned_off" | "off") => Some(Some(false)),
        (_, "changed_states") => Some(None),
        // A binary sensor's pairs: `motion` and `no_motion`, `opened` and `not_opened`…
        ("binary_sensor", event) if event.starts_with("no_") || event.starts_with("not_") => {
            Some(Some(false))
        }
        ("binary_sensor", event) if !event.is_empty() => Some(Some(true)),
        _ => None,
    }
}

/// A `time_pattern` trigger as a cron, when it fires on whole minutes.
fn cron(trigger: &Value) -> Result<String, String> {
    let field = |key: &str| -> Result<Option<String>, String> {
        match trigger.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(Value::String(text)) => {
                let text = text.trim();
                if text == "*" {
                    Ok(Some("*".to_owned()))
                } else if let Some(every) = text.strip_prefix('/') {
                    every
                        .parse::<u32>()
                        .map(|n| Some(format!("*/{n}")))
                        .map_err(|_| format!("with {key} `{text}`"))
                } else {
                    text.parse::<u32>()
                        .map(|n| Some(n.to_string()))
                        .map_err(|_| format!("with {key} `{text}`"))
                }
            }
            Some(other) => Err(format!("with {key} {other}")),
        }
    };
    let hours = field("hours")?;
    let mut minutes = field("minutes")?;
    let mut seconds = field("seconds")?;
    // Home Assistant's own rule: a larger unit given puts the smaller ones at zero.
    if minutes.is_none() && hours.is_some() {
        minutes = Some("0".to_owned());
    }
    if seconds.is_none() && minutes.is_some() {
        seconds = Some("0".to_owned());
    }
    if seconds.as_deref() != Some("0") {
        return Err("that fires on seconds, which Irori's time triggers don't".to_owned());
    }
    Ok(format!(
        "{} {} * * *",
        minutes.as_deref().unwrap_or("*"),
        hours.as_deref().unwrap_or("*")
    ))
}

#[cfg(test)]
mod tests;
