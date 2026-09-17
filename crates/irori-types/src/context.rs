use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContextId, ExtensionId, IntegrationId, TokenId, UserId};

/// Why something happened. Every state change and service call carries one, so any change can
/// be traced back to its cause (`docs/specs/entities.md` §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub id: ContextId,
    /// The context that caused this one, e.g. the motion event that triggered a rule run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<ContextId>,
    pub origin: Origin,
}

/// Where a context started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    /// Reported by a device through an integration, e.g. someone pressed a physical switch.
    Device { integration: IntegrationId },
    /// A person, through the UI or CLI.
    User { user_id: UserId },
    /// An automation engine run (any installed engine, not a core scheduler).
    Automation {
        extension: ExtensionId,
        run_id: ContextId,
    },
    /// An API client authenticated with an access token.
    Api { token_id: TokenId },
    /// Irori itself, e.g. restoring state at startup or reloading config.
    System,
}
