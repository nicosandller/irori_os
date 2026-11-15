//! Sending commands: a car's sessions, waking it when it sleeps, sending again when that's safe,
//! and what the car or site shows once a command is done.
//!
//! A command to a sleeping car is answered "asleep" (and billed). Irori then wakes the car once
//! ($0.02) and sends the command again every few seconds until it lands, which costs a command's
//! price a try, a tenth of a wake: Home Assistant woke the car again and again instead, and
//! skipped the wake when its last reading said "online" (#182023). Every command waiting for the
//! same car shares one wake.
//!
//! A command is sent again only when the car surely didn't act on it: it was asleep, or turned
//! the message away unread (a session out of step). One that went unanswered may have happened,
//! so honk and flash are never sent twice, and nothing is when Tesla doesn't answer.
//!
//! Each car has one session per computer, and its commands go one at a time per computer: two
//! signed with counters 7 and 8 could reach the car 8 first, and 7 would be turned away.

use std::sync::Arc;
use std::time::Duration;

use irori_protocol::{IncomingCall, ServiceError};
use irori_types::{ContextId, State};
use p256::SecretKey;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::api::{Api, ApiError};
use crate::commands::{CarCommand, Check, SiteCommand};
use crate::reading::report_caused;
use crate::signed::{self, Channel, Domain, Outcome, Reply};
use crate::usage::Why;
use crate::vehicle::CarReading;
use crate::{Done, World, energy, setup, vehicle};

/// How many times a session out of step is put right for one command.
const RESYNCS: u8 = 3;

/// How many times a command goes again to a car that says it's busy.
const BUSY_TRIES: u8 = 3;

/// How long sending takes. Tests shorten it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Patience {
    /// The longest a command takes, waking included: inside the 90 seconds the core waits
    /// (`answer_within_seconds`), so the person hears why rather than "timed out".
    pub deadline: Duration,
    /// Between tries while the car wakes.
    pub retry: Duration,
    /// Before sending again to a car that's busy.
    pub busy: Duration,
}

impl Patience {
    pub fn real() -> Self {
        Self {
            deadline: Duration::from_secs(80),
            retry: Duration::from_secs(5),
            busy: Duration::from_secs(2),
        }
    }
}

/// One car, as commands reach it.
pub(crate) struct Link {
    /// Where the car's answers are addressed: one per car, for as long as Irori runs.
    address: Vec<u8>,
    security: Mutex<Channel>,
    infotainment: Mutex<Channel>,
    /// When a command last woke the car.
    woken_at: Mutex<Option<Instant>>,
}

impl Link {
    pub fn new() -> Result<Self, String> {
        let mut address = vec![0u8; 16];
        getrandom::fill(&mut address).map_err(|error| format!("no randomness: {error}"))?;
        Ok(Self {
            address,
            security: Mutex::new(Channel::default()),
            infotainment: Mutex::new(Channel::default()),
            woken_at: Mutex::new(None),
        })
    }

    fn channel(&self, domain: Domain) -> &Mutex<Channel> {
        match domain {
            Domain::VehicleSecurity => &self.security,
            _ => &self.infotainment,
        }
    }
}

/// Why a command didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Irori's key isn't on the car.
    NotPaired,
    /// The account's own trouble (signed out, billing).
    Account(ApiError),
    /// Tesla or the car said no, or it couldn't be done.
    Failed(String),
    /// Tesla didn't answer: the command may or may not have reached the car.
    Unsure(String),
}

fn failure(error: ApiError) -> Failure {
    match error {
        ApiError::SignedOut(_) | ApiError::Billing(_) => Failure::Account(error),
        ApiError::Unreachable(_) | ApiError::Server(_) => Failure::Unsure(format!(
            "{}, so the command may or may not have reached the car",
            error.describe()
        )),
        error => Failure::Failed(error.describe()),
    }
}

/// Sends a command to a car and waits for the car to act on it, waking it if it must. With
/// `look_first`, the car is read before the command goes and the command is checked again
/// against what it says ([`CarCommand::check`]). Answers what that read found, to keep.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn car(
    api: &Api,
    link: &Link,
    vin: &str,
    key: &SecretKey,
    command: &CarCommand,
    why: Why,
    patience: Patience,
    look_first: bool,
) -> Result<Option<Value>, Failure> {
    let deadline = Instant::now() + patience.deadline;
    let sending = async {
        let looked = if look_first {
            let data = look(api, link, vin, command, why.clone(), patience, deadline).await?;
            let mut reading = CarReading::default();
            reading.merge(&data);
            reading.read_at = Some(0);
            match command.check(&reading, 0) {
                Check::Send => {}
                Check::AlreadyDone => return Ok(Some(data)),
                Check::LookFirst => {
                    return Err(Failure::Failed(
                        "the car didn't say whether the trunk is open, so Irori didn't send the \
                         command"
                            .into(),
                    ));
                }
            }
            Some(data)
        } else {
            None
        };
        send(api, link, vin, key, command, why, patience, deadline).await?;
        Ok(looked)
    };
    match tokio::time::timeout_at(deadline, sending).await {
        Ok(result) => result,
        Err(_) => Err(Failure::Unsure(
            "Tesla didn't answer in time, so the command may or may not have reached the car"
                .into(),
        )),
    }
}

/// Reads the car live before a command that depends on how it is now, waking it if it sleeps
/// (the wake the command needs anyway).
async fn look(
    api: &Api,
    link: &Link,
    vin: &str,
    command: &CarCommand,
    why: Why,
    patience: Patience,
    deadline: Instant,
) -> Result<Value, Failure> {
    loop {
        match api.vehicle_data(vin, false, why.clone()).await {
            Ok(data) => return Ok(data),
            Err(ApiError::Asleep) => {
                if Instant::now() + patience.retry >= deadline {
                    return Err(Failure::Failed("the car didn't wake up in time".into()));
                }
                wake(api, link, vin, &command.what(), patience).await?;
                tokio::time::sleep(patience.retry).await;
            }
            // Nothing was sent to the car yet, so nothing can have happened.
            Err(error) => {
                return Err(Failure::Failed(format!(
                    "Irori couldn't read the car before sending the command: {}",
                    error.describe()
                )));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send(
    api: &Api,
    link: &Link,
    vin: &str,
    key: &SecretKey,
    command: &CarCommand,
    why: Why,
    patience: Patience,
    deadline: Instant,
) -> Result<(), Failure> {
    let (domain, bytes) = command.message();
    let mut channel = link.channel(domain).lock().await;
    let mut resyncs = 0;
    let mut busy = 0;
    loop {
        let signed = channel
            .sign(
                domain,
                vin,
                &link.address,
                key,
                bytes.clone(),
                Instant::now(),
            )
            .map_err(Failure::Failed)?;
        let handshake = signed.is_none();
        let sent = match signed {
            Some(sent) => sent,
            None => Channel::hello(domain, &link.address, key).map_err(Failure::Failed)?,
        };
        let reply = match api.signed_command(vin, &sent.message, why.clone()).await {
            Ok(reply) => reply,
            Err(ApiError::Asleep) => {
                // The car never got it: wake it (once for every command waiting) and try again.
                if Instant::now() + patience.retry >= deadline {
                    return Err(Failure::Failed("the car didn't wake up in time".into()));
                }
                wake(api, link, vin, &command.what(), patience).await?;
                tokio::time::sleep(patience.retry).await;
                continue;
            }
            Err(error) => return Err(failure(error)),
        };
        let reply = channel.read(vin, &sent, &reply, key, Instant::now());
        let outcome = match reply {
            Reply::Answer(_) if handshake => {
                if !channel.ready() {
                    return Err(Failure::Failed("the car didn't start a session".into()));
                }
                continue;
            }
            Reply::Answer(answer) => signed::outcome(domain, &answer, command.done_anyway()),
            Reply::Resync => {
                resyncs += 1;
                if resyncs > RESYNCS {
                    return Err(Failure::Failed(
                        "the car and Irori couldn't agree on a session".into(),
                    ));
                }
                continue;
            }
            Reply::Busy => Outcome::Busy,
            Reply::NotPaired => Outcome::NotPaired,
            Reply::Refused(why) => Outcome::Refused(why),
        };
        match outcome {
            Outcome::Done => return Ok(()),
            Outcome::NotPaired => return Err(Failure::NotPaired),
            Outcome::Refused(why) => return Err(Failure::Failed(why)),
            Outcome::Busy => {
                busy += 1;
                // A handshake is always safe to repeat; a command only if repeating it is.
                if !(handshake || command.repeatable()) || busy > BUSY_TRIES {
                    return Err(Failure::Failed("the car is busy: try again".into()));
                }
                tokio::time::sleep(patience.busy).await;
            }
        }
    }
}

/// Wakes the car for a command, unless a command woke it within the time one command may take:
/// a car slow to wake is tried until the command gives up, never woken (and billed) twice.
async fn wake(
    api: &Api,
    link: &Link,
    vin: &str,
    what: &str,
    patience: Patience,
) -> Result<(), Failure> {
    let mut woken_at = link.woken_at.lock().await;
    if woken_at.is_some_and(|at| at.elapsed() < patience.deadline) {
        return Ok(());
    }
    api.wake_up(
        vin,
        Why::WakeForCommand {
            what: what.to_owned(),
        },
    )
    .await
    .map_err(failure)?;
    *woken_at = Some(Instant::now());
    Ok(())
}

/// Changes an energy site's settings.
pub(crate) async fn site(
    api: &Api,
    id: &str,
    command: &SiteCommand,
    why: Why,
    patience: Patience,
) -> Result<(), Failure> {
    let (endpoint, body) = command.request();
    let sending = api.energy_command(id, endpoint, body, why);
    match tokio::time::timeout(patience.deadline, sending).await {
        Ok(result) => result.map_err(failure),
        Err(_) => Err(Failure::Unsure(
            "Tesla didn't answer in time, so the change may or may not have been made".into(),
        )),
    }
}

/// A command on its way, as the loop hears back about it.
#[derive(Debug, Clone)]
pub(crate) enum Command {
    Car(CarCommand),
    Site(SiteCommand),
}

impl World {
    /// Sends a command to a car, unless it surely can't work: those are answered at once, and
    /// cost nothing.
    pub(crate) fn command_car(&mut self, vin: &str, command: CarCommand, call: IncomingCall) {
        if !self.signed_in {
            call.reply(Err(ServiceError::unavailable("sign in to Tesla first")));
            return;
        }
        let key = match &self.key {
            Ok(key) => key.secret().clone(),
            Err(why) => {
                call.reply(Err(ServiceError::failed(format!(
                    "Irori has no key to sign commands with: {why}"
                ))));
                return;
            }
        };
        // A car Tesla listed without Irori's key, or as taking older commands, is still tried:
        // the key may have been added since, and the handshake that would show it costs a
        // tenth of a cent.
        let check = match self.cars.get(vin) {
            Some(slot) => command.check(&slot.reading, self.now()),
            None => command.check(&CarReading::default(), self.now()),
        };
        let look_first = match check {
            Check::Send => false,
            Check::AlreadyDone => {
                call.reply(Ok(()));
                return;
            }
            Check::LookFirst => true,
        };
        let link = match self.links.get(vin) {
            Some(link) => Arc::clone(link),
            None => match Link::new() {
                Ok(link) => {
                    let link = Arc::new(link);
                    self.links.insert(vin.to_owned(), Arc::clone(&link));
                    link
                }
                Err(why) => {
                    call.reply(Err(ServiceError::failed(why)));
                    return;
                }
            },
        };
        let why = Why::Command {
            what: command.what(),
            by: crate::account::asked_by(&call.call.context.origin),
        };
        let api = self.api.clone();
        let done = self.done.clone();
        let patience = self.timing.patience;
        let device = vin.to_owned();
        tokio::spawn(async move {
            let result = car(
                &api, &link, &device, &key, &command, why, patience, look_first,
            )
            .await;
            let _ = done
                .send(Done::Commanded {
                    device,
                    command: Command::Car(command),
                    result,
                    call,
                })
                .await;
        });
    }

    pub(crate) fn command_site(&mut self, id: &str, command: SiteCommand, call: IncomingCall) {
        if !self.signed_in {
            call.reply(Err(ServiceError::unavailable("sign in to Tesla first")));
            return;
        }
        let why = Why::Command {
            what: command.what(),
            by: crate::account::asked_by(&call.call.context.origin),
        };
        let api = self.api.clone();
        let done = self.done.clone();
        let patience = self.timing.patience;
        let device = id.to_owned();
        tokio::spawn(async move {
            let result = site(&api, &device, &command, why, patience)
                .await
                .map(|()| None);
            let _ = done
                .send(Done::Commanded {
                    device,
                    command: Command::Site(command),
                    result,
                    call,
                })
                .await;
        });
    }

    /// What came of a command: the entities it changed show their new states, credited to the
    /// call, and the call is answered.
    pub(crate) async fn commanded(
        &mut self,
        device: String,
        command: Command,
        result: Result<Option<Value>, Failure>,
        call: IncomingCall,
    ) {
        match result {
            Ok(looked) => {
                self.account_worked();
                let caused_by = call.call.context.id.clone();
                match command {
                    Command::Car(command) => {
                        // It took a signed command, so Irori's key is on it.
                        if self.known.unpaired.remove(&device) | self.known.unsigned.remove(&device)
                        {
                            self.keep_known().await;
                        }
                        self.car_commanded(&device, &command, looked, caused_by)
                            .await;
                    }
                    Command::Site(command) => {
                        self.site_commanded(&device, &command, caused_by).await;
                    }
                }
                call.reply(Ok(()));
            }
            Err(Failure::NotPaired) => {
                // Tesla's list was out of date, or the key was taken off the car since.
                self.known.unpaired.insert(device.clone());
                self.keep_known().await;
                call.reply(Err(ServiceError::failed(self.not_paired(&device))));
            }
            Err(Failure::Account(error)) => {
                call.reply(Err(ServiceError::failed(error.describe())));
                self.trouble_from(error);
            }
            Err(Failure::Unsure(why)) => {
                // It may have happened: what Irori knew of what it changes can't be trusted, so
                // the next command that depends on it looks first.
                if let (Command::Car(command), Some(slot)) = (&command, self.cars.get_mut(&device))
                    && let Some(field) = command.unsure_forgets()
                {
                    slot.reading.fields.remove(field);
                }
                call.reply(Err(ServiceError::failed(why)));
            }
            Err(Failure::Failed(why)) if self.known.unsigned.contains(&device) => {
                call.reply(Err(ServiceError::failed(format!(
                    "{why}. Tesla lists this car as one that takes its older, unsigned \
                     commands, which Irori doesn't send"
                ))));
            }
            Err(Failure::Failed(why)) => call.reply(Err(ServiceError::failed(why))),
        }
    }

    async fn car_commanded(
        &mut self,
        vin: &str,
        command: &CarCommand,
        looked: Option<Value>,
        caused_by: ContextId,
    ) {
        let now = self.now();
        let Some(slot) = self.cars.get_mut(vin) else {
            return;
        };
        let before = car_states(slot);
        // What the car said just before the command, read to decide whether to send it.
        if let Some(data) = looked {
            slot.reading.merge(&data);
            slot.reading.read_at = Some(now);
        }
        for (field, value) in command.effects() {
            slot.reading.fields.insert(field.to_owned(), value);
        }
        // It answered, so it's awake.
        slot.reading.state = Some("online".into());
        slot.trouble = None;
        let after = car_states(slot);
        report_changes(&self.ctx, vin, before, after, caused_by);
        self.keep_car(vin).await;
        // Charging or warming up now: read it as often as a busy car is.
        self.replan();
    }

    async fn site_commanded(&mut self, id: &str, command: &SiteCommand, caused_by: ContextId) {
        let Some(slot) = self.sites.get_mut(id) else {
            return;
        };
        let states = |slot: &crate::reading::SiteSlot| -> Vec<(&'static str, Option<State>)> {
            slot.described
                .keys()
                .map(|key| (*key, energy::state(key, &slot.reading)))
                .collect()
        };
        let before = states(slot);
        for (field, value) in command.effects() {
            slot.reading.info.insert(field.to_owned(), value);
        }
        let after = states(slot);
        report_changes(&self.ctx, id, before, after, caused_by);
        self.keep_site(id).await;
    }

    /// Why a car without Irori's key can't be commanded, and how to put it on.
    fn not_paired(&self, vin: &str) -> String {
        format!(
            "Irori's key isn't on this car yet: open {} on a phone with the Tesla app, and \
             approve it in the car",
            setup::virtual_key_address(&self.settings.domain, vin)
        )
    }
}

fn car_states(slot: &crate::reading::CarSlot) -> Vec<(&'static str, Option<State>)> {
    slot.described
        .keys()
        .map(|key| (*key, vehicle::state(key, &slot.reading)))
        .collect()
}

/// Reports the states that changed, credited to the call that changed them.
fn report_changes(
    ctx: &irori_protocol::ProtocolContext,
    device: &str,
    before: Vec<(&'static str, Option<State>)>,
    after: Vec<(&'static str, Option<State>)>,
    caused_by: ContextId,
) {
    for ((key, was), (_, now)) in before.into_iter().zip(after) {
        if let Some(now) = now
            && Some(&now) != was.as_ref()
        {
            report_caused(ctx, device, key, now, Some(caused_by.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_command_that_surely_failed_is_sent_again() {
        assert!(matches!(
            failure(ApiError::Unreachable("no connection".into())),
            Failure::Unsure(why) if why.contains("may or may not")
        ));
        assert!(matches!(
            failure(ApiError::Billing("over".into())),
            Failure::Account(_)
        ));
        assert!(!CarCommand::Honk.repeatable());
        assert!(CarCommand::Lock(true).repeatable());
    }
}
