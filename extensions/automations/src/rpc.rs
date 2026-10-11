//! What the page asks (`docs/specs/flows.md` §8).

use irori_flow_types::api::{
    Armed, Backtest, FlowDetail, FlowSummary, Holding, Live, Problem, Saved, Severity, TestRequest,
    Timeline,
};
use irori_flow_types::trace::{RunRecord, TestKind};
use std::collections::BTreeMap;

use irori_flow_types::{Flow, Node, Trigger};
use irori_flows::engine::next_moment;
use irori_flows::{sim, validate};
use irori_types::{ContextId, RuleId, Timestamp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{HISTORY_CAP, Service, a_day_before, brief, history, now};

fn params<T: DeserializeOwned>(params: Value) -> Result<T, String> {
    serde_json::from_value(params).map_err(|e| format!("the request didn't make sense: {e}"))
}

fn answer(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct ById {
    id: RuleId,
}

#[derive(Deserialize)]
struct WithFlow {
    flow: Value,
}

/// A draft from the page: a flow, or the reason it isn't one yet, as a problem the page shows.
fn draft(value: Value) -> Result<Flow, Problem> {
    serde_json::from_value::<Flow>(value).map_err(|error| Problem {
        severity: Severity::Error,
        node: None,
        wire: None,
        field: None,
        message: error.to_string(),
    })
}

/// Answers one question from the page.
pub async fn handle(service: &mut Service, method: &str, raw: Value) -> Result<Value, String> {
    match method {
        "flows.list" => answer(json!({
            "flows": list(service),
            "file_problems": service.store.problems().iter().map(|p| json!({
                "file": p.file, "reason": p.reason,
            })).collect::<Vec<_>>(),
        })),
        // One flow written out for the assistant. The core never opens the flow file.
        "flow.brief" => {
            #[derive(Deserialize)]
            struct Brief {
                id: RuleId,
                /// How long it may be, in characters.
                #[serde(default)]
                budget: Option<usize>,
            }
            let Brief { id, budget } = params(raw)?;
            let flow = service
                .store
                .flow(&id)
                .cloned()
                .ok_or_else(|| format!("there's no flow `{id}`"))?;
            let problems = validate::check(&flow, &service.registry);
            let armed = service
                .engine
                .armed()
                .remove(&id)
                .map(|(armed, _)| armed)
                .unwrap_or(Armed::Disabled);
            let mut involved = validate::watched(&flow, &service.registry);
            involved.extend(flow.nodes.values().filter_map(|node| match node {
                Node::Call { entity, .. } => Some(entity.clone()),
                _ => None,
            }));
            let going = service
                .engine
                .active(Some(&id))
                .into_iter()
                .next()
                .map(|run| run.record);
            let kept = service.store.runs(&id);
            let near_misses = service.store.near_misses(&id);
            let text = brief::write(
                &brief::Picture {
                    flow: &flow,
                    armed: &armed,
                    problems: &problems,
                    registry: &service.registry,
                    states: &service.engine.states(),
                    involved: involved.into_iter().collect(),
                    going: going.as_ref(),
                    last: kept.iter().find(|run| run.test.is_none()),
                    ran_as_test: !kept.is_empty(),
                    near_misses: &near_misses,
                },
                budget.unwrap_or(brief::BUDGET),
            );
            answer(json!({
                "text": text,
                "name": flow.name,
                "enabled": flow.enabled,
                "problems": problems.len(),
            }))
        }
        "flows.get" => {
            let ById { id } = params(raw)?;
            let flow = service
                .store
                .flow(&id)
                .cloned()
                .ok_or_else(|| format!("there's no flow `{id}`"))?;
            let armed = service
                .engine
                .armed()
                .remove(&id)
                .map(|(armed, _)| armed)
                .unwrap_or(Armed::Disabled);
            answer(FlowDetail {
                version: flow.version(),
                problems: validate::check(&flow, &service.registry),
                armed,
                flow,
            })
        }
        "flows.validate" => {
            let WithFlow { flow } = params(raw)?;
            answer(match draft(flow) {
                Ok(flow) => validate::check(&flow, &service.registry),
                Err(problem) => vec![problem],
            })
        }
        "flows.save" => {
            let WithFlow { flow } = params(raw)?;
            let flow = draft(flow).map_err(|problem| problem.message)?;
            service.store.save(&flow, now())?;
            service.arm();
            answer(Saved {
                version: flow.version(),
                problems: validate::check(&flow, &service.registry),
            })
        }
        "flows.enable" => {
            #[derive(Deserialize)]
            struct Enable {
                id: RuleId,
                enabled: bool,
            }
            let Enable { id, enabled } = params(raw)?;
            let mut flow = service
                .store
                .flow(&id)
                .cloned()
                .ok_or_else(|| format!("there's no flow `{id}`"))?;
            flow.enabled = enabled;
            service.store.save(&flow, now())?;
            service.arm();
            answer(Value::Null)
        }
        "flows.delete" => {
            let ById { id } = params(raw)?;
            service.store.delete(&id)?;
            service.arm();
            answer(Value::Null)
        }
        "versions.list" => {
            let ById { id } = params(raw)?;
            answer(service.store.versions(&id))
        }
        "versions.get" | "versions.restore" => {
            #[derive(Deserialize)]
            struct Which {
                id: RuleId,
                version: String,
            }
            let Which { id, version } = params(raw)?;
            let entry = service
                .store
                .version(&id, &version)
                .ok_or_else(|| format!("`{id}` has no version {version}"))?;
            if method == "versions.get" {
                return answer(entry);
            }
            let flow = entry
                .flow
                .ok_or_else(|| "that version's definition wasn't kept".to_owned())?;
            service.store.save(&flow, now())?;
            service.arm();
            answer(Saved {
                version: flow.version(),
                problems: validate::check(&flow, &service.registry),
            })
        }
        "runs.list" => {
            let ById { id } = params(raw)?;
            let mut runs: Vec<_> = service
                .engine
                .active(Some(&id))
                .into_iter()
                .map(|run| run.record.summary())
                .collect();
            runs.extend(service.store.runs(&id).iter().map(RunRecord::summary));
            answer(runs)
        }
        "runs.get" => {
            #[derive(Deserialize)]
            struct Which {
                id: RuleId,
                run_id: ContextId,
            }
            let Which { id, run_id } = params(raw)?;
            answer(find_run(service, &id, &run_id).ok_or("that run isn't kept any more")?)
        }
        "runs.active" => {
            #[derive(Deserialize)]
            struct Maybe {
                #[serde(default)]
                id: Option<RuleId>,
            }
            let Maybe { id } = params(raw)?;
            answer(service.engine.active(id.as_ref()))
        }
        "runs.cancel" => {
            #[derive(Deserialize)]
            struct Which {
                run_id: ContextId,
            }
            let Which { run_id } = params(raw)?;
            let cancelled = service.engine.cancel(&run_id, now());
            service.apply_effects();
            answer(cancelled)
        }
        "tests.get" => {
            let ById { id } = params(raw)?;
            answer(service.store.test_settings(&id))
        }
        "tests.save" => {
            #[derive(Deserialize)]
            struct Settings {
                id: RuleId,
                settings: Value,
            }
            let Settings { id, settings } = params(raw)?;
            // Only for a flow there is: nothing would ever clean up after one there isn't.
            if service.store.flow(&id).is_none() {
                return Err(format!("there's no flow `{id}`"));
            }
            service.store.keep_test_settings(&id, &settings)?;
            answer(json!({}))
        }
        "live" => {
            #[derive(Deserialize)]
            struct Since {
                id: RuleId,
                #[serde(default)]
                after: Option<Timestamp>,
            }
            /// Most runs one answer brings: the canvas plays them, and more would only queue up.
            const MOST: usize = 5;
            let Since { id, after } = params(raw)?;
            let now = now();
            let newer = |at: &Timestamp| after.is_none_or(|after| *at > after);
            // With nothing to go on, what already happened is history, not news.
            let (runs, near_misses) = if after.is_none() {
                (Vec::new(), Vec::new())
            } else {
                let mut runs: Vec<RunRecord> = service
                    .store
                    .runs(&id)
                    .into_iter()
                    .filter(|run| run.finished_at.as_ref().is_some_and(newer))
                    .take(MOST)
                    .collect();
                runs.reverse();
                let mut misses: Vec<_> = service
                    .store
                    .near_misses(&id)
                    .into_iter()
                    .filter(|miss| newer(&miss.at))
                    .take(MOST)
                    .collect();
                misses.reverse();
                (runs, misses)
            };
            let holding = service
                .engine
                .holding(&id)
                .into_iter()
                .map(|(node, until)| Holding { node, until })
                .collect();
            answer(Live {
                now,
                runs,
                near_misses,
                holding,
            })
        }
        "nearmiss.list" => {
            let ById { id } = params(raw)?;
            answer(service.store.near_misses(&id))
        }
        "timeline" => {
            #[derive(Deserialize)]
            struct Window {
                id: RuleId,
                from: Timestamp,
                to: Timestamp,
            }
            let Window { id, from, to } = params(raw)?;
            let flow = service.flow_or_draft(&id, None)?;
            let mut changes: Vec<_> = history(service, &flow, from)
                .await?
                .into_values()
                .flatten()
                .filter(|state| state.last_updated <= to)
                .collect();
            changes.sort_by_key(|state| state.last_updated);
            let in_window = |at: Timestamp| at >= from && at <= to;
            answer(Timeline {
                from,
                to,
                changes,
                near_misses: service
                    .store
                    .near_misses(&id)
                    .into_iter()
                    .filter(|miss| in_window(miss.at))
                    .collect(),
                runs: service
                    .store
                    .runs(&id)
                    .iter()
                    .filter(|run| in_window(run.started_at))
                    .map(RunRecord::summary)
                    .collect(),
            })
        }
        "test" => {
            let request: TestRequest = params(raw)?;
            let at = now();
            if request.dry {
                let flow = service.flow_or_draft(&request.id, request.flow)?;
                answer(sim::dry_run(
                    &flow,
                    &request.trigger,
                    service.engine.states(),
                    &request.overrides,
                    service.engine.place().cloned(),
                    at,
                )?)
            } else {
                let run_id = service
                    .engine
                    .fire(&request.id, &request.trigger, Some(TestKind::Live), at)?
                    .ok_or("the flow's mode didn't start a run: one is already going")?;
                service.apply_effects();
                answer(find_run(service, &request.id, &run_id).ok_or("the run went missing")?)
            }
        }
        "backtest" => {
            #[derive(Deserialize)]
            struct Ask {
                id: RuleId,
                #[serde(default)]
                flow: Option<Value>,
            }
            let Ask { id, flow } = params(raw)?;
            let flow = match flow {
                Some(flow) => draft(flow).map_err(|problem| problem.message)?,
                None => service.flow_or_draft(&id, None)?,
            };
            let to = now();
            let window = a_day_before(to);
            let history = history(service, &flow, window).await?;
            // An entity the core kept its most changes for may reach back less than a day; the
            // backtest only covers as far back as every entity does.
            // History only reaches back as far as the core has been keeping it, and an entity
            // the core kept its most changes for reaches back less than a day: the backtest
            // covers only what every entity covers.
            let capped = history
                .values()
                .filter(|states| states.len() >= HISTORY_CAP)
                .filter_map(|states| states.first().map(|state| state.last_updated))
                .max();
            let kept_since = history
                .values()
                .filter_map(|states| states.first().map(|state| state.last_updated))
                .min();
            let from = [Some(window), capped, kept_since]
                .into_iter()
                .flatten()
                .max()
                .unwrap_or(window);
            let (would, changes) = sim::backtest(
                &flow,
                &history,
                &service.engine.states(),
                service.engine.place().cloned(),
                to,
            );
            answer(Backtest {
                from,
                to,
                would: would
                    .into_iter()
                    .filter(|run| run.started_at >= from)
                    .collect(),
                did: service
                    .store
                    .runs(&id)
                    .iter()
                    .filter(|run| run.started_at >= from && run.test.is_none())
                    .map(RunRecord::summary)
                    .collect(),
                changes,
            })
        }
        "clock.next" => {
            #[derive(Deserialize)]
            struct Ask {
                trigger: Value,
            }
            let Ask { trigger } = params(raw)?;
            answer(clock_next(service, trigger))
        }
        "clock.holds" => {
            #[derive(Deserialize)]
            struct Ask {
                conditions: Vec<Value>,
            }
            let Ask { conditions } = params(raw)?;
            answer(clock_holds(service, conditions))
        }
        other => Err(format!("the Automations engine has no `{other}`")),
    }
}

fn list(service: &Service) -> Vec<FlowSummary> {
    let armed = service.engine.armed();
    service
        .store
        .flows()
        .map(|flow| {
            let problems = validate::check(flow, &service.registry);
            let last_run = service
                .engine
                .active(Some(&flow.id))
                .first()
                .map(|run| run.record.summary())
                .or_else(|| service.store.runs(&flow.id).first().map(RunRecord::summary));
            FlowSummary {
                id: flow.id.clone(),
                name: flow.name.clone(),
                enabled: flow.enabled,
                armed: armed
                    .get(&flow.id)
                    .map(|(armed, _)| armed.clone())
                    .unwrap_or(Armed::Disabled),
                problems: problems.len(),
                version: flow.version(),
                last_run,
                near_misses: service.store.near_misses(&flow.id).len(),
                active_runs: service.engine.running(&flow.id),
                triggers: flow.nodes.values().filter(|n| n.is_trigger()).count(),
                nodes: flow.nodes.len(),
            }
        })
        .collect()
}

fn find_run(service: &Service, id: &RuleId, run_id: &ContextId) -> Option<RunRecord> {
    service
        .engine
        .active(Some(id))
        .into_iter()
        .map(|run| run.record)
        .chain(service.store.runs(id))
        .find(|run| &run.run_id == run_id)
}

/// What the page is told about a time or sun trigger: when it next fires, and today's sun.
#[derive(Debug, Serialize)]
struct ClockNext {
    /// The home's time zone, if it has been set.
    #[serde(skip_serializing_if = "Option::is_none")]
    time_zone: Option<String>,
    /// Whether the home's location has been set.
    location: bool,
    /// When the trigger next fires.
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<Timestamp>,
    /// The same, as a person says it: "tomorrow at 06:52".
    #[serde(skip_serializing_if = "Option::is_none")]
    spoken: Option<String>,
    /// Why there's no next time, when the trigger itself is what's wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
    /// The time on the wall at home now, "14:05".
    #[serde(skip_serializing_if = "Option::is_none")]
    now: Option<String>,
    /// Today's sun at home, each as "HH:MM": only the events that happen today.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    sun: BTreeMap<&'static str, String>,
}

/// When `trigger` next fires. The page asks instead of working it out, so the zone and the sun
/// are only ever read in one place.
fn clock_next(service: &Service, trigger: Value) -> ClockNext {
    use irori_rules::clock;

    let at = now();
    let Some(place) = service.engine.place() else {
        return ClockNext {
            time_zone: None,
            location: false,
            next: None,
            spoken: None,
            problem: None,
            now: None,
            sun: BTreeMap::new(),
        };
    };
    let sun = sun_today(place, at);
    let (next, problem) = match serde_json::from_value::<Trigger>(trigger) {
        Ok(trigger) => match trigger.validate() {
            Ok(()) => (next_moment(place, &trigger, at), None),
            Err(error) => (None, Some(error.to_string())),
        },
        Err(error) => (None, Some(error.to_string())),
    };
    ClockNext {
        time_zone: Some(place.time_zone().to_owned()),
        location: place.has_location(),
        spoken: next.map(|next| clock::spoken(place, next, at)),
        next,
        problem,
        now: Some(clock::wall(place, at)),
        sun,
    }
}

/// Today's sun at home, each event that happens today as "HH:MM".
fn sun_today(place: &irori_rules::clock::Place, at: Timestamp) -> BTreeMap<&'static str, String> {
    use irori_rules::SunEvent;
    use irori_rules::clock;

    let mut sun = BTreeMap::new();
    for (name, event) in [
        ("dawn", SunEvent::Dawn),
        ("sunrise", SunEvent::Sunrise),
        ("noon", SunEvent::Noon),
        ("sunset", SunEvent::Sunset),
        ("dusk", SunEvent::Dusk),
        ("midnight", SunEvent::Midnight),
    ] {
        if let Ok(Some(when)) = clock::sun_today(place, event, at) {
            sun.insert(name, clock::wall(place, when));
        }
    }
    sun
}

/// What the page is told about time and sun windows: whether each holds right now.
#[derive(Debug, Serialize)]
struct ClockHolds {
    #[serde(skip_serializing_if = "Option::is_none")]
    time_zone: Option<String>,
    location: bool,
    /// The time on the wall at home now, "14:05".
    #[serde(skip_serializing_if = "Option::is_none")]
    now: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    sun: BTreeMap<&'static str, String>,
    /// One answer per condition asked about, in the order asked.
    holds: Vec<Holds>,
}

/// Whether one window holds now. Neither `holds` nor a reason means it can't be told yet.
#[derive(Debug, Default, Serialize)]
struct Holds {
    #[serde(skip_serializing_if = "Option::is_none")]
    holds: Option<bool>,
    /// Why it can't be told: no time zone, no location, no sunset at home today.
    #[serde(skip_serializing_if = "Option::is_none")]
    why: Option<String>,
}

/// Whether each time or sun window holds right now. The page asks instead of working it out,
/// so a dot on the canvas and a run agree: both are the engine's own reading of the clock.
fn clock_holds(service: &Service, conditions: Vec<Value>) -> ClockHolds {
    use irori_rules::{CompactDuration, Condition, clock};

    let at = now();
    let place = service.engine.place();
    let holds = conditions
        .into_iter()
        .map(|condition| {
            let told = |result: Result<bool, String>| match result {
                Ok(holds) => Holds {
                    holds: Some(holds),
                    why: None,
                },
                Err(why) => Holds {
                    holds: None,
                    why: Some(why),
                },
            };
            let condition = match serde_json::from_value::<Condition>(condition) {
                Ok(condition) => condition,
                Err(error) => return told(Err(error.to_string())),
            };
            if let Err(error) = condition.validate(0) {
                return told(Err(error.to_string()));
            }
            let Some(place) = place else {
                return told(Err("the home has no time zone yet".to_owned()));
            };
            match &condition {
                Condition::Time {
                    after,
                    before,
                    weekday,
                } => told(Ok(clock::in_time_window(
                    place,
                    after.as_ref(),
                    before.as_ref(),
                    weekday.as_deref(),
                    at,
                ))),
                Condition::Sun {
                    after,
                    before,
                    offset,
                    after_offset,
                    before_offset,
                } => {
                    let millis = |own: &Option<CompactDuration>| {
                        own.as_ref()
                            .or(offset.as_ref())
                            .map_or(0, CompactDuration::millis)
                    };
                    told(clock::in_sun_window(
                        place,
                        *after,
                        *before,
                        millis(after_offset),
                        millis(before_offset),
                        at,
                    ))
                }
                _ => told(Err(
                    "only a time or a sun window is told by the clock".to_owned()
                )),
            }
        })
        .collect();
    ClockHolds {
        time_zone: place.map(|place| place.time_zone().to_owned()),
        location: place.is_some_and(clock::Place::has_location),
        now: place.map(|place| clock::wall(place, at)),
        sun: place.map(|place| sun_today(place, at)).unwrap_or_default(),
        holds,
    }
}
