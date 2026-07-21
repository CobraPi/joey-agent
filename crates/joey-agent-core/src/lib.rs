//! `joey-agent-core` — the agent runtime and turn loop.
//!
//! Port of `run_agent.py` + `agent/conversation_loop.py` + `agent/prompt_builder.py`.
//! Wires the provider layer to the tool system: assembles messages, builds the
//! system prompt, calls the model, dispatches tool calls, and loops until the
//! assistant stops requesting tools.

pub mod agent;
pub mod events;
pub mod prompt;

pub use agent::{Agent, AgentConfig, TurnResult};
pub use events::AgentEvent;
