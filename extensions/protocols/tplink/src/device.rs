//! One task per device: log in over KLAP, learn what it has, and read it every poll.
//!
//! What it reads becomes a [`Snapshot`]; the protocol loop turns that into descriptions and
//! state. A device that misses two readings in a row is reported offline. One that refuses the
//! account is reported once and retried slowly, so a wrong password doesn't flood it.
//!
//! The same task switches the device. A session numbers its requests and a second login ends the
//! first, so a command can't have a connection of its own: it waits in this task's queue, is sent
//! between readings, and is followed at once by a reading that says what the device did.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::time::Duration;

use irori_protocol::{IncomingCall, ServiceError};
use irori_types::{ContextId, UniqueId};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;

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

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub info: Info,
    /// This poll's reading; `None` when it missed.
    pub meter: Option<Meter>,
    /// The meter's values the device has given: described even when a reading misses.
    pub metered: Metered,
    pub led: Option<bool>,
    pub children: Vec<Child>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Child {
    pub info: Info,
    pub meter: Option<Meter>,
    pub metered: Metered,
}

/// Which of a meter's values a device has given this session. They stay described while it
/// meters, so a reading that misses shows them unknown instead of holding the last number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metered {
    pub power: bool,
    pub voltage: bool,
    pub current: bool,
    pub today: bool,
    pub total: bool,
}

impl Metered {
    fn add(&mut self, meter: &Meter) {
        self.power |= meter.power.is_some();
        self.voltage |= meter.voltage.is_some();
        self.current |= meter.current.is_some();
        self.today |= meter.today.is_some();
        self.total |= meter.total.is_some();
    }
}

/// A relay to turn on or off, and the call waiting to hear how it went.
#[derive(Debug)]
pub struct Command {
    pub call: IncomingCall,
    /// The outlet's own id on a strip; `None` for a plug's relay.
    pub child: Option<String>,
    pub on: bool,
    /// When the caller stops waiting. Past this the command is answered with an error and not
    /// sent: a relay must not switch after its caller was told it didn't.
    pub deadline: Instant,
}

/// An entity a command switched and the call that asked, for the reading taken straight after.
pub type Answer = (UniqueId, ContextId);

#[derive(Debug)]
pub enum Event {
    Read {
        generation: u64,
        mac: String,
        snapshot: Box<Snapshot>,
        /// The commands this reading follows, when the device took any.
        answers: Vec<Answer>,
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
}

/// The meter values a device has given. Kept beside `misses`, not in the session: a session is
/// dropped on any error, and one bad reading right after the next login must not take the sensors
/// (and with them their history) away. Only a device saying it has no meter clears them.
#[derive(Debug, Default)]
struct Seen {
    metered: Metered,
    child_metered: BTreeMap<String, Metered>,
}

pub fn spawn(
    found: Found,
    auth: AuthHash,
    poll: Duration,
    generation: u64,
    events: mpsc::Sender<Event>,
    mut commands: mpsc::Receiver<Command>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mac = found.mac.clone();
        let mut session: Option<(Session, Plan)> = None;
        let mut misses = 0u32;
        let mut seen = Seen::default();
        let mut answers: Vec<Answer> = Vec::new();
        loop {
            // Only a session from an earlier poll can have expired; a fresh one that's refused is
            // a miss like any other, so it waits instead of logging in again at once.
            let reused = session.is_some();
            let outcome = match session.as_mut() {
                Some((session, plan)) => read(session, plan, &mut seen).await,
                None => match login(found.address, &auth).await {
                    Ok(fresh) => {
                        let (fresh, plan) = session.insert(fresh);
                        read(fresh, plan, &mut seen).await
                    }
                    Err(error) => Err(error),
                },
            };
            // Only the reading straight after a command answers it. The core forgets a call
            // after five minutes and drops a report naming one it forgot, so a reading that
            // misses lets go of the call instead of naming it later.
            let answering = std::mem::take(&mut answers);
            let wait = match outcome {
                Ok(snapshot) => {
                    misses = 0;
                    let event = Event::Read {
                        generation,
                        mac: mac.clone(),
                        snapshot: Box::new(snapshot),
                        answers: answering,
                    };
                    if events.send(event).await.is_err() {
                        return;
                    }
                    poll
                }
                Err(KlapError::Expired) if reused => {
                    // A session ends after a day; log in again straight away.
                    session = None;
                    answers = answering;
                    continue;
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
                    REFUSED_RETRY
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
                    poll.saturating_mul(misses).min(MAX_BACKOFF)
                }
            };
            // Whatever it is waiting for, a command is taken at once, and the device is read
            // again straight after: that reading, not the request, is what gets shown.
            let mut next = rest(wait, &mut commands).await;
            // Every command already waiting goes before that reading, so one behind a device
            // that has stopped answering is told so now, not after the reading gives up too.
            while let Some(command) = next {
                answers.extend(switch(command, &mut session, found.address, &auth).await);
                next = commands.try_recv().ok();
            }
        }
    })
}

/// Waits out `wait`, or until a command arrives.
async fn rest(wait: Duration, commands: &mut mpsc::Receiver<Command>) -> Option<Command> {
    let sleep = tokio::time::sleep(wait);
    tokio::pin!(sleep);
    tokio::select! {
        () = &mut sleep => None,
        // A closed queue has nothing more to say: the wait runs to its end.
        Some(command) = commands.recv() => Some(command),
    }
}

async fn login(address: SocketAddr, auth: &AuthHash) -> Result<(Session, Plan), KlapError> {
    let mut session = Session::connect(address, auth).await?;
    let plan = learn(&mut session).await?;
    Ok((session, plan))
}

/// Carries out one command and answers its call. Gives back what the next reading answers, when
/// the device took the command.
async fn switch(
    command: Command,
    session: &mut Option<(Session, Plan)>,
    address: SocketAddr,
    auth: &AuthHash,
) -> Option<Answer> {
    let Command {
        call,
        child,
        on,
        deadline,
    } = command;
    if Instant::now() >= deadline {
        call.reply(Err(ServiceError::unavailable(
            "the TP-Link device was busy for too long; nothing was sent to it",
        )));
        return None;
    }
    let sent = tokio::time::timeout_at(
        deadline,
        send_switch(session, address, auth, child.as_deref(), on),
    )
    .await;
    match sent {
        Ok(Ok(())) => {
            let answers = (call.call.unique_id.clone(), call.call.context.id.clone());
            call.reply(Ok(()));
            Some(answers)
        }
        Ok(Err(error)) => {
            call.reply(Err(error));
            None
        }
        Err(_) => {
            // Cut off mid-exchange: this session's numbering can't be trusted any more.
            *session = None;
            call.reply(Err(ServiceError::unavailable(
                "the TP-Link device didn't answer in time",
            )));
            None
        }
    }
}

async fn send_switch(
    session: &mut Option<(Session, Plan)>,
    address: SocketAddr,
    auth: &AuthHash,
    child: Option<&str>,
    on: bool,
) -> Result<(), ServiceError> {
    let request = match child {
        Some(child) => smart::switch_child(child, on),
        None => smart::switch(on),
    };
    let mut fresh = false;
    loop {
        if session.is_none() {
            fresh = true;
            *session = Some(login(address, auth).await.map_err(unanswered)?);
        }
        let Some((current, _)) = session.as_mut() else {
            continue;
        };
        match current.request(&request).await {
            Ok(response) => {
                return smart::accepted(&response, child.is_some()).map_err(|code| {
                    ServiceError::failed(format!(
                        "the TP-Link device refused to switch (its error {code})"
                    ))
                });
            }
            // The one failure that is sent again: the device turned an old session away, so it
            // didn't take the request. After any other, it may have switched, and sending twice
            // could switch a relay the caller was told had failed.
            Err(KlapError::Expired) if !fresh => *session = None,
            Err(error) => {
                *session = None;
                return Err(unanswered(error));
            }
        }
    }
}

/// Why a command got no answer from the device, for the person who asked.
fn unanswered(error: KlapError) -> ServiceError {
    match error {
        KlapError::Io(error) => {
            ServiceError::unavailable(format!("the TP-Link device can't be reached: {error}"))
        }
        KlapError::Account => ServiceError::failed(
            "the device doesn't accept the TP-Link account in this extension's settings",
        ),
        KlapError::Expired => {
            ServiceError::unavailable("the TP-Link device turned the request away; try again")
        }
        KlapError::Device(message) => ServiceError::failed(message),
    }
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
            // The next page starts after every entry in this one, read or not.
            let entries = smart::entries(&list, "child_component_list");
            if entries == 0 {
                break;
            }
            seen += entries;
            let page = smart::child_component_ids(&list);
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

/// The device says it has neither meter method, rather than one reading failing.
fn no_meter(answers: &BTreeMap<String, smart::Answer>) -> bool {
    [smart::EMETER_DATA, smart::ENERGY_USAGE]
        .iter()
        .all(|method| answers.get(*method) == Some(&Err(smart::UNKNOWN_METHOD)))
}

async fn read(
    session: &mut Session,
    plan: &mut Plan,
    seen: &mut Seen,
) -> Result<Snapshot, KlapError> {
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
    // Any other miss is one bad reading: its values are unknown until the next.
    if plan.energy && meter.is_none() && no_meter(&answers) {
        plan.energy = false;
        seen.metered = Metered::default();
    }
    if let Some(meter) = &meter {
        seen.metered.add(meter);
    }
    let led = answers
        .get(smart::LED_INFO)
        .and_then(|answer| answer.as_ref().ok())
        .and_then(smart::led);

    let mut children = Vec::new();
    if let Some(Ok(list)) = answers.get(smart::CHILD_LIST) {
        let mut listed = child_list(list);
        // Pages are counted by their entries, so one that can't be read doesn't shift the next.
        let mut offset = smart::entries(list, "child_device_list");
        let total = list.get("sum").and_then(Value::as_u64).unwrap_or(0) as usize;
        while offset > 0 && offset < total.min(MAX_CHILDREN) {
            let request = smart::multiple(&[(smart::CHILD_LIST, json!({ "start_index": offset }))]);
            let Some(Ok(more)) = call(session, &request).await?.remove(smart::CHILD_LIST) else {
                break;
            };
            let entries = smart::entries(&more, "child_device_list");
            if entries == 0 {
                break;
            }
            offset += entries;
            listed.extend(child_list(&more));
        }
        let mut ids = BTreeSet::new();
        listed.retain(|info| ids.insert(info.device_id.clone()));
        listed.truncate(MAX_CHILDREN);
        // What an outlet has metered is kept only while the strip lists it: an outlet that has
        // gone takes its record with it, so the record can't grow past the strip's sockets.
        seen.child_metered
            .retain(|id, _| listed.iter().any(|info| info.device_id == *id));
        for info in listed {
            let mut meter = None;
            if plan.child_energy.contains(&info.device_id) {
                let inner = smart::multiple(&[
                    (smart::ENERGY_USAGE, Value::Null),
                    (smart::EMETER_DATA, Value::Null),
                ]);
                let response = session
                    .request(&smart::control_child(&info.device_id, inner))
                    .await?;
                let answers = smart::child_answers(&response).ok();
                meter = answers.as_ref().and_then(meter_of);
                if meter.is_none() && answers.as_ref().is_some_and(no_meter) {
                    plan.child_energy.remove(&info.device_id);
                    seen.child_metered.remove(&info.device_id);
                }
            }
            let metered = seen
                .child_metered
                .entry(info.device_id.clone())
                .or_default();
            if let Some(meter) = &meter {
                metered.add(meter);
            }
            let metered = *metered;
            children.push(Child {
                info,
                meter,
                metered,
            });
        }
    }
    children.sort_by_key(|child| child.info.position.unwrap_or(u64::MAX));

    Ok(Snapshot {
        info,
        meter,
        metered: seen.metered,
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
