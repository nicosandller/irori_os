//! One task per device: log in over KLAP, learn what it has, and read it every poll.
//!
//! What it reads becomes a [`Snapshot`]; the protocol loop turns that into descriptions and
//! state. A device that misses two readings in a row is reported offline. One that refuses the
//! account is reported once and retried slowly, so a wrong password doesn't flood it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::discover::Found;
use crate::klap::{AuthHash, KlapError, Session};
use crate::smart::{self, Info, Meter};

/// Misses before a device is reported offline.
const MISSES: u32 = 2;
/// The longest wait between attempts at a device that isn't answering.
const MAX_BACKOFF: Duration = Duration::from_secs(120);
/// How long before trying an account the device refused again.
const REFUSED_RETRY: Duration = Duration::from_secs(600);
/// A strip with more outlets than this isn't a strip.
const MAX_CHILDREN: usize = 32;
/// Polls in a row with no meter reading before a device is taken not to meter.
const EMPTY_METERS: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub info: Info,
    pub meter: Option<Meter>,
    pub led: Option<bool>,
    pub children: Vec<Child>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Child {
    pub info: Info,
    pub meter: Option<Meter>,
}

#[derive(Debug)]
pub enum Event {
    Read {
        generation: u64,
        mac: String,
        snapshot: Box<Snapshot>,
    },
    Offline {
        generation: u64,
        mac: String,
    },
    Refused {
        generation: u64,
        mac: String,
    },
}

/// What a device has, learned once per session from its component list.
#[derive(Debug, Default)]
struct Plan {
    energy: bool,
    led: bool,
    children: bool,
    child_energy: BTreeSet<String>,
    /// Polls in a row where neither meter method answered.
    empty_meters: u32,
}

pub fn spawn(
    found: Found,
    auth: AuthHash,
    poll: Duration,
    generation: u64,
    events: mpsc::Sender<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mac = found.mac.clone();
        let mut session: Option<(Session, Plan)> = None;
        let mut misses = 0u32;
        loop {
            // Only a session from an earlier poll can have expired; a fresh one that's refused is
            // a miss like any other, so it waits instead of logging in again at once.
            let reused = session.is_some();
            let outcome = match session.as_mut() {
                Some((session, plan)) => read(session, plan).await,
                None => match Session::connect(found.address, &auth).await {
                    Ok(mut fresh) => match learn(&mut fresh).await {
                        Ok(plan) => {
                            let (fresh, plan) = session.insert((fresh, plan));
                            read(fresh, plan).await
                        }
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                },
            };
            match outcome {
                Ok(snapshot) => {
                    misses = 0;
                    let event = Event::Read {
                        generation,
                        mac: mac.clone(),
                        snapshot: Box::new(snapshot),
                    };
                    if events.send(event).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(poll).await;
                }
                Err(KlapError::Expired) if reused => {
                    // A session ends after a day; log in again straight away.
                    session = None;
                }
                Err(KlapError::Account) => {
                    session = None;
                    let event = Event::Refused {
                        generation,
                        mac: mac.clone(),
                    };
                    if events.send(event).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(REFUSED_RETRY).await;
                }
                Err(error) => {
                    session = None;
                    misses = misses.saturating_add(1);
                    tracing::debug!("a TP-Link device didn't answer: {error}");
                    if misses == MISSES {
                        let event = Event::Offline {
                            generation,
                            mac: mac.clone(),
                        };
                        if events.send(event).await.is_err() {
                            return;
                        }
                    }
                    let wait = poll.saturating_mul(misses).min(MAX_BACKOFF);
                    tokio::time::sleep(wait).await;
                }
            }
        }
    })
}

async fn call(
    session: &mut Session,
    request: &Value,
) -> Result<BTreeMap<String, smart::Answer>, KlapError> {
    let response = session.request(request).await?;
    smart::answers(&response)
        .map_err(|code| KlapError::Device(format!("the device refused the request ({code})")))
}

/// What the device has. A device that won't list its components is read for everything, and
/// whatever it doesn't answer is left out.
async fn learn(session: &mut Session) -> Result<Plan, KlapError> {
    let answers = call(
        session,
        &smart::multiple(&[(smart::COMPONENTS, Value::Null)]),
    )
    .await?;
    let ids = match answers.get(smart::COMPONENTS) {
        Some(Ok(list)) => Some(smart::component_ids(list)),
        _ => None,
    };
    let has = |id: &str| ids.as_ref().is_none_or(|ids| ids.contains(id));
    let children = ids
        .as_ref()
        .is_some_and(|ids| ids.contains("child_device") || ids.contains("control_child"));
    let mut plan = Plan {
        // A strip lists `energy_monitoring` too, but its own reading is its first outlet's
        // (seen on a Tapo strip): the outlets are read instead.
        energy: has("energy_monitoring") && !children,
        led: has("led"),
        children,
        child_energy: BTreeSet::new(),
        empty_meters: 0,
    };
    if plan.children {
        // Paged like the child list itself.
        let mut seen = 0;
        while seen < MAX_CHILDREN {
            let request =
                smart::multiple(&[(smart::CHILD_COMPONENTS, json!({ "start_index": seen }))]);
            let Some(Ok(list)) = call(session, &request)
                .await?
                .remove(smart::CHILD_COMPONENTS)
            else {
                break;
            };
            let page = smart::child_component_ids(&list);
            if page.is_empty() {
                break;
            }
            seen += page.len();
            plan.child_energy.extend(
                page.into_iter()
                    .filter(|(_, ids)| ids.contains("energy_monitoring"))
                    .map(|(id, _)| id),
            );
            let total = list.get("sum").and_then(Value::as_u64).unwrap_or(0) as usize;
            if seen >= total {
                break;
            }
        }
    }
    Ok(plan)
}

fn meter_of(answers: &BTreeMap<String, smart::Answer>) -> Option<Meter> {
    let ok = |method: &str| answers.get(method).and_then(|answer| answer.as_ref().ok());
    let meter = smart::meter(ok(smart::EMETER_DATA), ok(smart::ENERGY_USAGE));
    (!meter.is_empty()).then_some(meter)
}

async fn read(session: &mut Session, plan: &mut Plan) -> Result<Snapshot, KlapError> {
    let mut methods = vec![(smart::DEVICE_INFO, Value::Null)];
    if plan.energy {
        methods.push((smart::ENERGY_USAGE, Value::Null));
        methods.push((smart::EMETER_DATA, Value::Null));
    }
    if plan.led {
        methods.push((smart::LED_INFO, Value::Null));
    }
    if plan.children {
        methods.push((smart::CHILD_LIST, json!({ "start_index": 0 })));
    }
    let answers = call(session, &smart::multiple(&methods)).await?;

    let info = answers
        .get(smart::DEVICE_INFO)
        .and_then(|answer| answer.as_ref().ok())
        .and_then(smart::info)
        .ok_or_else(|| KlapError::Device("the device didn't say what it is".into()))?;
    let meter = if plan.energy {
        meter_of(&answers)
    } else {
        None
    };
    if plan.energy {
        if meter.is_some() {
            plan.empty_meters = 0;
        } else {
            // Stop asking once the device says it has neither method, or neither answers for a
            // few polls in a row; one bad reading isn't enough.
            plan.empty_meters += 1;
            let unknown = [smart::EMETER_DATA, smart::ENERGY_USAGE]
                .iter()
                .all(|method| answers.get(*method) == Some(&Err(smart::UNKNOWN_METHOD)));
            if unknown || plan.empty_meters >= EMPTY_METERS {
                plan.energy = false;
            }
        }
    }
    let led = answers
        .get(smart::LED_INFO)
        .and_then(|answer| answer.as_ref().ok())
        .and_then(smart::led);

    let mut children = Vec::new();
    if let Some(Ok(list)) = answers.get(smart::CHILD_LIST) {
        let mut listed = child_list(list);
        let total = list.get("sum").and_then(Value::as_u64).unwrap_or(0) as usize;
        while listed.len() < total.min(MAX_CHILDREN) {
            let request =
                smart::multiple(&[(smart::CHILD_LIST, json!({ "start_index": listed.len() }))]);
            let more = match call(session, &request).await?.remove(smart::CHILD_LIST) {
                Some(Ok(more)) => child_list(&more),
                _ => Vec::new(),
            };
            if more.is_empty() {
                break;
            }
            listed.extend(more);
        }
        for info in listed {
            let meter = if plan.child_energy.contains(&info.device_id) {
                let inner = smart::multiple(&[
                    (smart::ENERGY_USAGE, Value::Null),
                    (smart::EMETER_DATA, Value::Null),
                ]);
                let response = session
                    .request(&smart::control_child(&info.device_id, inner))
                    .await?;
                smart::child_answers(&response)
                    .ok()
                    .as_ref()
                    .and_then(meter_of)
            } else {
                None
            };
            children.push(Child { info, meter });
        }
    }
    children.sort_by_key(|child| child.info.position.unwrap_or(u64::MAX));

    Ok(Snapshot {
        info,
        meter,
        led,
        children,
    })
}

fn child_list(result: &Value) -> Vec<Info> {
    result
        .get("child_device_list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(smart::info)
        .take(MAX_CHILDREN)
        .collect()
}
