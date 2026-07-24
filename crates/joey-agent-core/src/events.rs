//! Agent events streamed to the UI (port of the callback surface in
//! `run_agent.py` — stream_delta, thinking, tool_progress, notice, …).
//!
//! Enhanced with rich orchestration events for maximum TUI verbosity:
//! iteration tracking, usage reporting, API call lifecycle, tool arguments.

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
    // ── Streaming deltas ───────────────────────────────────────────────
    /// A chunk of assistant text.
    ContentDelta(String),
    /// A chunk of reasoning/thinking text.
    ReasoningDelta(String),

    // ── Turn lifecycle ─────────────────────────────────────────────────
    /// The turn started — carries the max iteration budget.
    TurnStart { max_iterations: usize },
    /// A new API call iteration is starting (1-indexed).
    IterationStart {
        iteration: usize,
        max_iterations: usize,
    },
    /// The model is being queried (waiting for LLM response).
    ApiCallStart,
    /// The model responded (streaming may follow).
    ApiCallEnd { usage: Usage },

    // ── Tool execution ────────────────────────────────────────────────
    /// A tool call is about to run (name, emoji, pretty args summary).
    ToolStart {
        name: String,
        emoji: String,
        summary: String,
    },
    /// Incremental progress from a running tool (upstream `tool_progress`).
    ToolProgress { name: String, progress: String },
    /// A tool call finished (name, whether it errored, result preview).
    ToolEnd {
        name: String,
        is_error: bool,
        /// A short preview of the tool result (first line, truncated).
        result_preview: String,
        /// Execution duration in seconds.
        duration_secs: f64,
    },

    // ── Assistant messages ────────────────────────────────────────────
    /// The assistant produced a complete message this iteration.
    AssistantMessage(String),

    // ── Status / notices ──────────────────────────────────────────────
    /// A one-line status/notice for the user.
    Notice(String),
    /// A retry is happening (attempt N of M, error message).
    RetryAttempt {
        attempt: usize,
        max_retries: usize,
        error: String,
        wait_secs: f64,
    },
    /// Context compression is happening.
    CompressionStart { reason: String, approx_tokens: i64 },
    /// Context compression finished.
    CompressionEnd {
        original_msgs: usize,
        new_msgs: usize,
    },
    /// A fallback provider was activated.
    FallbackActivated {
        from_model: String,
        to_model: String,
    },

    // ── Orchestration events ──────────────────────────────────────────
    /// A subagent was spawned (per child).
    SubagentSpawn {
        goal: String,
        model: String,
        toolset_summary: String,
        depth: usize,
    },
    /// A subagent completed successfully.
    SubagentComplete {
        goal: String,
        success: bool,
        summary_preview: String,
        token_usage: Usage,
        duration_secs: f64,
    },
    /// A subagent failed with an error.
    SubagentFailed {
        goal: String,
        error: String,
        duration_secs: f64,
    },
    /// A batch delegation resolved (all children done or failed).
    DelegationBatchComplete {
        total: usize,
        succeeded: usize,
        failed: usize,
        total_duration_secs: f64,
    },

    // ── OMO orchestration events ─────────────────────────────────────
    /// The active agent mode changed via Tab picker (T035, BC-015).
    AgentModeChanged {
        agent_name: String,
        model: String,
    },
    /// A category-based delegation was dispatched (T059).
    CategoryDelegation {
        category: String,
        model: String,
    },
    /// Boulder work started (T097, BC-029).
    BoulderWorkStarted {
        plan_name: String,
        work_id: String,
    },
    /// Boulder work resumed.
    BoulderWorkResumed {
        plan_name: String,
        work_id: String,
    },
    /// Boulder work completed (all tasks done).
    BoulderWorkCompleted {
        plan_name: String,
        work_id: String,
    },
    /// A goal was set or updated (T097).
    GoalSet {
        objective: String,
    },
    /// A goal was cleared.
    GoalCleared,
    /// Wisdom accumulated during plan execution (T097).
    WisdomAccumulated {
        learnings_count: usize,
    },

    // ── Turn end ──────────────────────────────────────────────────────
    /// The turn finished; carries the final text and cumulative usage.
    Done {
        final_text: String,
        usage: Usage,
        /// Total API calls made during the turn.
        iterations: usize,
    },
    /// The turn failed with an error message.
    Failed(String),
}
