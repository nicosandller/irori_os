//! The core: registry, live state, events, service calls, and the extension host. Contains no
//! protocol code; devices arrive through integrations (`docs/specs/integrations.md`).

mod clock;
mod context_id;
mod events;
mod home;
mod host;
mod services;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use irori_integration::host::{Op, incoming_call};
use irori_integration::{IncomingCall, Rejected, ServiceErrorCode};
use irori_types::{
    Context, Device, Entity, EntityId, EntityKind, EntityState, ExtensionId, IntegrationId,
    ServiceCall, StateReport, Timestamp, Version,
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
    statuses: RwLock<BTreeMap<ExtensionId, ExtensionStatus>>,
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
            statuses: RwLock::default(),
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

    pub fn extensions(&self) -> BTreeMap<ExtensionId, ExtensionStatus> {
        read(&self.0.statuses).clone()
    }

    /// Asks an entity to do something, and waits for its integration's answer (at most
    /// [`SERVICE_CALL_TIMEOUT`]). `context` says who's asking.
    pub async fn call_service(
        &self,
        entity_id: &EntityId,
        command: Command,
        context: Context,
    ) -> Result<(), CallError> {
        let resolved = read(&self.0.home).resolve(entity_id, command)?;
        let sender = read(&self.0.links)
            .get(&resolved.integration)
            .cloned()
            .ok_or_else(|| CallError::NotRunning(resolved.integration.clone()))?;
        write(&self.0.home).record_call(&resolved.integration, context.id.clone());

        let called = Event::ServiceCalled {
            entity_id: entity_id.clone(),
            service: resolved.service.name(),
            context: context.clone(),
        };
        let (incoming, result) = incoming_call(ServiceCall {
            unique_id: resolved.unique_id,
            service: resolved.service,
            context,
        });
        let integration = resolved.integration;
        let call = async {
            sender
                .send(incoming)
                .await
                .map_err(|_| CallError::NotRunning(integration.clone()))?;
            // Only once the integration has it: a call that never went out wasn't made.
            self.publish(vec![called]);
            match result.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(match e.code {
                    ServiceErrorCode::Unavailable => CallError::Unavailable(e.message),
                    ServiceErrorCode::Failed => CallError::Failed(e.message),
                }),
                Err(_) => Err(CallError::Failed(
                    "the integration dropped the call without answering".into(),
                )),
            }
        };
        tokio::time::timeout(SERVICE_CALL_TIMEOUT, call)
            .await
            .unwrap_or(Err(CallError::Timeout))
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
        for report in reports {
            if let Err(rejected) =
                self.change(|home, stamp| home.report_state(integration, report, stamp))
            {
                tracing::warn!(%extension, %rejected, "rejected a state report");
            }
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
        let changed = write(&self.0.statuses).insert(extension.clone(), status.clone())
            != Some(status.clone());
        if changed {
            self.publish(vec![Event::ExtensionStatusChanged {
                extension_id: extension.clone(),
                status,
            }]);
        }
    }
}
