//! The home as a built-in protocol reads it ([`irori_protocol::home::Home`]), over [`Core`],
//! within the API scopes its manifest asks for.

use irori_protocol::home::{Answer, Home};
use irori_types::{
    ApiScope, ContextId, Entity, EntityId, EntityState, ExtensionId, HomeSettings, Origin,
    Timestamp,
};
use tokio::sync::{broadcast, mpsc};

use crate::{Command, Core, Event};

/// A built-in's view of the home.
#[derive(Debug, Clone)]
pub(crate) struct CoreHome {
    pub core: Core,
    pub extension: ExtensionId,
    pub scopes: Vec<ApiScope>,
}

impl CoreHome {
    fn may(&self, scope: ApiScope) -> bool {
        let may = self.scopes.contains(&scope);
        if !may {
            tracing::warn!(
                extension = %self.extension,
                %scope,
                "a built-in asked for what its manifest doesn't let it have"
            );
        }
        may
    }
}

impl Home for CoreHome {
    fn entities(&self) -> Vec<Entity> {
        if self.may(ApiScope::RegistryRead) {
            self.core.entities()
        } else {
            Vec::new()
        }
    }

    fn states(&self) -> Vec<EntityState> {
        if self.may(ApiScope::StatesRead) {
            self.core.states()
        } else {
            Vec::new()
        }
    }

    fn watch(&self) -> mpsc::UnboundedReceiver<EntityState> {
        let (tx, rx) = mpsc::unbounded_channel();
        if !self.may(ApiScope::StatesRead) {
            return rx;
        }
        let mut events = self.core.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(Event::StateChanged { new_state, .. }) => {
                        if tx.send(*new_state).is_err() {
                            return;
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        rx
    }

    fn history(&self, entity_id: &EntityId, since: Timestamp) -> Vec<EntityState> {
        if self.may(ApiScope::HistoryRead) {
            self.core.history(entity_id, since)
        } else {
            Vec::new()
        }
    }

    fn place(&self) -> HomeSettings {
        self.core.place()
    }

    fn call(
        &self,
        entity_id: EntityId,
        action: String,
        data: serde_json::Map<String, serde_json::Value>,
        caused_by: Option<ContextId>,
    ) -> Answer {
        let allowed = self.may(ApiScope::ServicesCall);
        let core = self.core.clone();
        let extension = self.extension.clone();
        Box::pin(async move {
            if !allowed {
                return Err(format!("`{extension}` may not call services"));
            }
            // As this extension, on behalf of whatever made it call: a group passing a press
            // on keeps the press's context as its parent, so a trace can follow it.
            let run = core.new_context(Origin::System).id;
            let mut context = core.new_context(Origin::Automation {
                extension,
                run_id: run,
            });
            context.parent_id = caused_by;
            core.call_service(&entity_id, Command { action, data }, context)
                .await
                .map_err(|e| e.to_string())
        })
    }
}
