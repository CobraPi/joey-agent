//! Agent events streamed to the UI (port of the callback surface in
//! `run_agent.py` — stream_delta, thinking, tool_progress, notice, …).

use joey_providers::Usage;

/// An event emitted during a turn. The CLI/gateway renders these live.
///
/// Ordering guarantees: `ContentDelta`/`ReasoningDelta` stream during a
/// provider call; `AssistantMessage` fires when a complete assistant message
/// is recorded (interim messages during tool loops are deduped against the
/// previous interim — conversation_loop.py:4997-5013); `ToolStart` /
/// `ToolProgress` / `ToolEnd` bracket tool execution; exactly one of
/// `Done`/`Failed` ends the turn.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A chunk of assistant text.
    ContentDelta(String),
    /// A chunk of reasoning/thinking text.
    ReasoningDelta(String),
    /// A tool call is about to run (name, pretty args).
    ToolStart { name: String, emoji: String, summary: String },
    /// Incremental progress from a running tool (upstream `tool_progress`).
    /// Nothing emits this yet in the port — the tool layer has no progress
    /// channel — but the variant keeps the event surface upstream-shaped.
    ToolProgress { name: String, progress: String },
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
