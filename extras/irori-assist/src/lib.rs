//! The assistant: which model is configured, what a conversation may keep, and how a provider's
//! stream becomes text or a tool call.
//!
//! The binary owns the files, the HTTP routes, and the Ollama process. This crate is the part
//! that can be checked without a server. The core does not depend on it.

mod config;
mod prompt;
mod stream;
mod tools;
mod transcript;

pub use config::{
    AssistantFile, CONTEXT_MAX, CONTEXT_MIN, CloudPreset, DEFAULT_TAG, INSTRUCTIONS_MAX,
    LOCAL_CONTEXT, Mode, Shape, cloud_ready, context_scale, library_page, local_context,
    local_fits, local_needs, model_tag,
};
pub use prompt::{
    AutomationLine, BriefLine, ExtensionLine, HOME_CAP, Place, SettingsPicture, automations_brief,
    device_brief, home_brief, settings_brief,
};
pub use stream::{AnthropicParser, Lines, OllamaParser, OpenAiParser, Piece};
pub use tools::{
    LIMIT_CALLS, TOOL_ROUNDS, ToolCall, anthropic_tools, assemble, execute_round, openai_tools,
    tool_offered,
};
pub use transcript::{LIMIT_BYTES, LIMIT_GLOBAL_BYTES, LIMIT_MESSAGES, Role, Turn, append_capped};
