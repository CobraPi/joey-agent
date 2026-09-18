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
pub mod context_assembly;
pub mod events;
pub mod guardrails;
pub mod guidance;
pub mod hooks;
pub mod image_model;
pub mod loop_detection;
pub mod memory_hook;
pub mod prompt;
pub mod state_block;
pub mod threat_scan;
pub mod verification;

/// Feature 034 integration tests (context assembly improvements).
#[cfg(test)]
mod feature034;

pub use agent::{Agent, AgentConfig, TurnResult, Transport};
pub use compression::ContextCompressor;
pub use events::AgentEvent;
pub use hooks::PreToolUseRunner;
pub use loop_detection::LoopDetector;
pub use memory_hook::{MemoryRuntime, MemoryTurnSummary};
pub use prompt::{build_system_prompt, PromptInputs};

/// Serializes tests that override the process-global joey home. Aliases
/// joey-core's process-wide `TEST_HOME_OVERRIDE_LOCK` — a second,
/// crate-local mutex here would NOT serialize against the feature034
/// fixtures (support.rs), which lock the same process-global home via
/// the joey-core lock; the split lock let concurrent home overrides race
/// (assembly_log_records_gauge_fields flake).
#[cfg(test)]
pub(crate) use joey_core::constants::TEST_HOME_OVERRIDE_LOCK as TEST_HOME_LOCK;
