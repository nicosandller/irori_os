//! The core: registry, live state, events, service calls, and the extension host. Contains no
//! protocol code; devices arrive through integrations (`docs/specs/integrations.md`).

mod clock;
mod context_id;
mod events;
mod home;
mod host;
mod services;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use irori_integration::host::{Op, incoming_call};
use irori_integration::{IncomingCall, Rejected, ServiceErrorCode};
use irori_types::{
    Context, ContextId, Device, Entity, EntityId, EntityKind, EntityState, ExtensionId,
    IntegrationId, ServiceCall, StateReport, Timestamp, Version,
};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

pub use clock::{Clock, SystemClock};
pub use events::Event;
pub use host::{ExtensionHost, Timing};
pub use services::{CallError, Command};

use home::{Home, Stamp};

/// How long a service call may take before the caller gets [`CallError::Timeout`].
pub const SERVICE_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Events waiting for a slow listener before it starts missing them.
const EVENT_BUFFER: usize = 1024;

/// What the UI and CLI show for an extension (`docs/specs/extensions.md` §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExtensionStatus {
    Disabled,
    Starting,
    Running,
    Degraded {
        reason: String,
    },
    Failed {
        reason: String,
        /// When the core will try again; absent when it won't (e.g. incompatible version).
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_at: Option<Timestamp>,
    },
}

/// An extension as the Extensions page shows it: its status, and how many of its state reports
/// were lost since Irori started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionOverview {
    #[serde(flatten)]
    pub status: ExtensionStatus,
    /// Reports the core refused, e.g. a value that doesn't fit the entity. Each is logged.
    pub rejected_reports: u64,
    /// Reports dropped because too many entities were waiting for the core.
    pub dropped_reports: u64,
}

/// Drops an entity's call lock from the map once nobody else is waiting for it, so the map
/// doesn't grow with every entity ever called.
struct ReleaseWhenIdle<'a> {
    core: &'a Core,
    entity_id: EntityId,
}

impl Drop for ReleaseWhenIdle<'_> {
    fn drop(&mut self) {
        let mut busy = self
            .core
            .0
            .busy
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // 2 = the map's copy and this call's. Anything more means another call is waiting.
        if busy
            .get(&self.entity_id)
            .is_some_and(|lock| Arc::strong_count(lock) <= 2)
        {
            busy.remove(&self.entity_id);
        }
    }
}

/// Forgets a recorded call unless it was delivered, whatever ends the call: an error, a timeout,
/// or the caller dropping the future.
struct Delivery<'a> {
    core: &'a Core,
    integration: IntegrationId,
    context_id: ContextId,
    entity_id: EntityId,
    delivered: bool,
}

impl Drop for Delivery<'_> {
    fn drop(&mut self) {
        if !self.delivered {
            self.core
                .forget_call(&self.integration, &self.context_id, &self.entity_id);
        }
    }
}

/// A handle to the core. Cheap to clone; all clones share the same home.
#[derive(Debug, Clone)]
pub struct Core(Arc<Shared>);

#[derive(Debug)]
struct Shared {
    clock: Arc<dyn Clock>,
    version: Version,
    // Never held across an `.await`: every change is a short synchronous step.
    home: RwLock<Home>,
    events: broadcast::Sender<Event>,
    links: RwLock<HashMap<IntegrationId, mpsc::Sender<IncomingCall>>>,
    /// One lock per entity, so calls on the same entity happen one after another.
    busy: Mutex<HashMap<EntityId, Arc<tokio::sync::Mutex<()>>>>,
    extensions: RwLock<BTreeMap<ExtensionId, ExtensionOverview>>,
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

impl Core {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let version = Version::try_from(env!("CARGO_PKG_VERSION"))
            .expect("the workspace version is a valid semantic version");
        Self::with_version(clock, version)
    }

    /// For tests of version compatibility.
    pub fn with_version(clock: Arc<dyn Clock>, version: Version) -> Self {
        Self(Arc::new(Shared {
            clock,
            version,
            home: RwLock::default(),
            events: broadcast::channel(EVENT_BUFFER).0,
            links: RwLock::default(),
            busy: Mutex::default(),
            extensions: RwLock::default(),
        }))
    }

    pub fn version(&self) -> &Version {
        &self.0.version
    }

    pub fn now(&self) -> Timestamp {
        self.0.clock.now()
    }

    /// Every event from now on. A listener that falls more than 1024 events behind skips ahead
    /// and is told how many it missed.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    pub fn devices(&self) -> Vec<Device> {
        read(&self.0.home).devices().cloned().collect()
    }

    pub fn entities(&self) -> Vec<Entity> {
        read(&self.0.home).entities().cloned().collect()
    }

    pub fn states(&self) -> Vec<EntityState> {
        read(&self.0.home).states().cloned().collect()
    }

    pub fn state(&self, entity_id: &EntityId) -> Option<EntityState> {
        read(&self.0.home).state(entity_id).cloned()
    }

    pub fn extensions(&self) -> BTreeMap<ExtensionId, ExtensionOverview> {
        read(&self.0.extensions).clone()
    }

    /// Asks an entity to do something, and waits for its integration's answer (at most
    /// [`SERVICE_CALL_TIMEOUT`]). `context` says who's asking.
    pub async fn call_service(
        &self,
        entity_id: &EntityId,
        command: Command,
        context: Context,
    ) -> Result<(), CallError> {
        // The whole call, queueing included, fits in SERVICE_CALL_TIMEOUT.
        let deadline = tokio::time::Instant::now() + SERVICE_CALL_TIMEOUT;

        // One call at a time per entity: two toggles arriving together must not both read the
        // same "off" and both turn it on. Held until the integration answers, so the second
        // toggle sees the result of the first.
        let busy = Arc::clone(
            self.0
                .busy
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(entity_id.clone())
                .or_default(),
        );
        let _release = ReleaseWhenIdle {
            core: self,
            entity_id: entity_id.clone(),
        };
        let _turn = tokio::time::timeout_at(deadline, busy.lock())
            .await
            .map_err(|_| CallError::Timeout)?;

        let resolved = read(&self.0.home).resolve(entity_id, command)?;
        let sender = read(&self.0.links)
            .get(&resolved.integration)
            .cloned()
            .ok_or_else(|| CallError::NotRunning(resolved.integration.clone()))?;
        // Both recorded before sending, because the integration may answer and report before
        // this task runs again. The guard undoes them unless the call is delivered, including
        // when the caller drops this future mid-send.
        {
            let mut home = write(&self.0.home);
            home.record_call(&resolved.integration, context.id.clone(), self.now());
            home.record_command(entity_id, &resolved.service);
        }
        let mut delivery = Delivery {
            core: self,
            integration: resolved.integration.clone(),
            context_id: context.id.clone(),
            entity_id: entity_id.clone(),
            delivered: false,
        };

        let called = Event::ServiceCalled {
            entity_id: entity_id.clone(),
            service: resolved.service.name(),
            context: context.clone(),
        };
        let (incoming, result) = incoming_call(ServiceCall {
            unique_id: resolved.unique_id.clone(),
            service: resolved.service.clone(),
            context,
        });
        let integration = resolved.integration.clone();
        // The registry can change while a call is being prepared (an integration re-describing
        // or removing the entity), so check again right before sending. A change after this
        // point reaches the integration, which answers with an error like any other device
        // trouble (`docs/specs/integrations.md` §7.3).
        if !read(&self.0.home).still_dispatchable(entity_id, &resolved) {
            return Err(CallError::NotSupported(format!(
                "`{entity_id}` changed while the call was being prepared; try again"
            )));
        }
        let sent = tokio::time::timeout_at(deadline, sender.send(incoming)).await;
        if !matches!(sent, Ok(Ok(()))) {
            return Err(match sent {
                Err(_) => CallError::Timeout,
                _ => CallError::NotRunning(integration),
            });
        }
        delivery.delivered = true;
        // Only once the integration has it: a call that never went out wasn't made.
        self.publish(vec![called]);

        let outcome = match tokio::time::timeout_at(deadline, result).await {
            Err(_) => Err(CallError::Timeout),
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(match e.code {
                ServiceErrorCode::Unavailable => CallError::Unavailable(e.message),
                ServiceErrorCode::Failed => CallError::Failed(e.message),
            }),
            Ok(Err(_)) => Err(CallError::Failed(
                "the integration dropped the call without answering".into(),
            )),
        };
        if outcome.is_err() {
            // It didn't happen, so the entity is whatever it last reported.
            write(&self.0.home).forget_command(entity_id);
        }
        outcome
    }

    fn forget_call(
        &self,
        integration: &IntegrationId,
        context_id: &ContextId,
        entity_id: &EntityId,
    ) {
        let mut home = write(&self.0.home);
        home.forget_call(integration, context_id);
        home.forget_command(entity_id);
    }

    fn stamp(&self) -> Stamp {
        let now = self.now();
        Stamp {
            now,
            context_id: context_id::new_context_id(now),
        }
    }

    fn publish(&self, events: Vec<Event>) {
        for event in events {
            // No listeners is fine.
            let _ = self.0.events.send(event);
        }
    }

    /// Applies one change to the home and publishes what it caused.
    fn change(
        &self,
        f: impl FnOnce(&mut Home, &Stamp) -> Result<Vec<Event>, Rejected>,
    ) -> Result<(), Rejected> {
        let stamp = self.stamp();
        let events = f(&mut write(&self.0.home), &stamp)?;
        self.publish(events);
        Ok(())
    }

    fn apply_op(
        &self,
        extension: &ExtensionId,
        integration: &IntegrationId,
        kinds: &[EntityKind],
        op: Op,
    ) {
        let (result, reply) = match op {
            Op::DescribeDevice(device, reply) => (
                self.change(|home, _| home.describe_device(integration, device)),
                reply,
            ),
            Op::DescribeEntity(entity, reply) => (
                self.change(|home, stamp| home.describe_entity(integration, kinds, entity, stamp)),
                reply,
            ),
            Op::RemoveDevice(unique_id, reply) => (
                self.change(|home, _| home.remove_device(integration, &unique_id)),
                reply,
            ),
            Op::RemoveEntity(unique_id, reply) => (
                self.change(|home, _| home.remove_entity(integration, &unique_id)),
                reply,
            ),
            Op::SetAvailability(target, availability, reply) => (
                self.change(|home, stamp| {
                    home.set_availability(integration, target, availability, stamp)
                }),
                reply,
            ),
            Op::SetHealth(health) => {
                self.set_status(
                    extension,
                    match health {
                        irori_integration::Health::Running => ExtensionStatus::Running,
                        irori_integration::Health::Degraded(reason) => {
                            ExtensionStatus::Degraded { reason }
                        }
                    },
                );
                return;
            }
        };
        if let Err(rejected) = &result {
            tracing::warn!(%extension, %rejected, "rejected an operation from an integration");
        }
        let _ = reply.send(result);
    }

    fn apply_reports(
        &self,
        extension: &ExtensionId,
        integration: &IntegrationId,
        reports: Vec<StateReport>,
        dropped: u64,
    ) {
        if dropped > 0 {
            tracing::warn!(
                %extension,
                dropped,
                "dropped state reports: too many entities were waiting for the core"
            );
        }
        let mut rejected_count = 0;
        for report in reports {
            if let Err(rejected) =
                self.change(|home, stamp| home.report_state(integration, report, stamp))
            {
                tracing::warn!(%extension, %rejected, "rejected a state report");
                rejected_count += 1;
            }
        }
        if (dropped > 0 || rejected_count > 0)
            && let Some(overview) = write(&self.0.extensions).get_mut(extension)
        {
            overview.dropped_reports += dropped;
            overview.rejected_reports += rejected_count;
        }
    }

    fn mark_unavailable(&self, integration: &IntegrationId) {
        let stamp = self.stamp();
        let events = write(&self.0.home).mark_unavailable(integration, &stamp);
        self.publish(events);
    }

    fn link(&self, integration: &IntegrationId, calls: mpsc::Sender<IncomingCall>) {
        write(&self.0.links).insert(integration.clone(), calls);
    }

    fn unlink(&self, integration: &IntegrationId) {
        write(&self.0.links).remove(integration);
    }

    fn set_status(&self, extension: &ExtensionId, status: ExtensionStatus) {
        let changed = {
            let mut extensions = write(&self.0.extensions);
            match extensions.get_mut(extension) {
                Some(overview) if overview.status == status => false,
                Some(overview) => {
                    overview.status = status.clone();
                    true
                }
                None => {
                    extensions.insert(
                        extension.clone(),
                        ExtensionOverview {
                            status: status.clone(),
                            rejected_reports: 0,
                            dropped_reports: 0,
                        },
                    );
                    true
                }
            }
        };
        if changed {
            self.publish(vec![Event::ExtensionStatusChanged {
                extension_id: extension.clone(),
                status,
            }]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever ends a call before it's delivered (an error, a timeout, or the caller walking
    /// away mid-send), the recorded context must not stay valid for `caused_by`.
    #[test]
    fn an_undelivered_call_is_forgotten() {
        let core = Core::new(Arc::new(SystemClock));
        let integration = IntegrationId::try_from("demo").expect("valid");
        let context_id = context_id::new_context_id(core.now());
        let record = || {
            write(&core.0.home).record_call(&integration, context_id.clone(), core.now());
            assert!(read(&core.0.home).knows_call(&integration, &context_id));
        };
        let guard = |delivered| Delivery {
            core: &core,
            integration: integration.clone(),
            context_id: context_id.clone(),
            entity_id: EntityId::try_from("light.lamp").expect("valid"),
            delivered,
        };

        record();
        drop(guard(false));
        assert!(!read(&core.0.home).knows_call(&integration, &context_id));

        record();
        drop(guard(true));
        assert!(read(&core.0.home).knows_call(&integration, &context_id));
    }
}
