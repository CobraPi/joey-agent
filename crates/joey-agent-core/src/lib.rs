//! `joey-agent-core` — the agent runtime and turn loop.
//!
//! Port of `run_agent.py` + `agent/conversation_loop.py` +
//! `agent/system_prompt.py` / `agent/prompt_builder.py`. Wires the provider
//! layer to the tool system: builds the session-stable system prompt, calls
//! the model, validates/repairs and dispatches tool calls, and loops until
//! the assistant stops requesting tools (with retry/fallback/interrupt
//! handling and optional session persistence).

pub mod agent;
pub mod compression;
pub mod events;
pub mod guidance;
pub mod prompt;
pub mod threat_scan;

pub use agent::{Agent, AgentConfig, TurnResult, Transport};
pub use compression::ContextCompressor;
pub use events::AgentEvent;
pub use prompt::{build_system_prompt, PromptInputs};

/// Serializes tests that override the process-global joey home.
#[cfg(test)]
pub(crate) static TEST_HOME_LOCK: once_cell::sync::Lazy<std::sync::Mutex<()>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(()));
