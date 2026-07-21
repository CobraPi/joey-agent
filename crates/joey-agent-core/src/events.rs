//! Agent events streamed to the UI (port of the callback surface in
//! `run_agent.py` — stream_delta, thinking, tool_progress, notice, …).

use joey_providers::Usage;

/// An event emitted during a turn. The CLI/gateway renders these live.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A chunk of assistant text.
    ContentDelta(String),
    /// A chunk of reasoning/thinking text.
    ReasoningDelta(String),
    /// A tool call is about to run (name, pretty args).
    ToolStart { name: String, emoji: String, summary: String },
    /// A tool call finished (name, whether it errored).
    ToolEnd { name: String, is_error: bool },
    /// The assistant produced a complete message this iteration.
    AssistantMessage(String),
    /// A one-line status/notice for the user.
    Notice(String),
    /// The turn finished; carries the final text and cumulative usage.
    Done { final_text: String, usage: Usage },
    /// The turn failed with an error message.
    Failed(String),
}
