//! What the page asks (`docs/specs/flows.md` §8).

use irori_flow_types::Flow;
use irori_flow_types::api::{
    Armed, Backtest, FlowDetail, FlowSummary, Holding, Live, Problem, Saved, Severity, TestRequest,
    Timeline,
};
use irori_flow_types::trace::{RunRecord, TestKind};
use irori_flows::{sim, validate};
use irori_types::{ContextId, RuleId, Timestamp};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{HISTORY_CAP, Service, a_day_before, history, now};

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
            let (would, changes) = sim::backtest(&flow, &history, &service.engine.states(), to);
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
