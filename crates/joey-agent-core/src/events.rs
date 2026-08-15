//! Agent events streamed to the UI (port of the callback surface in
//! `run_agent.py` — stream_delta, thinking, tool_progress, notice, …).
//!
//! Enhanced with rich orchestration events for maximum TUI verbosity:
//! iteration tracking, usage reporting, API call lifecycle, tool arguments.

use joey_providers::Usage;

// Feature 005: supporting enums for FileChange events. Lives at the event
// layer so both the CLI renderer and the TUI state machine can consume them
// from a single stream (constitution Principle II: CLI/TUI parity).
// See specs/005-expandable-diff-ui/contracts/agent-event.md.

/// What kind of file change a `FileChange` event represents (FR-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    /// A brand-new file was created (all additions, no baseline).
    Create,
    /// An existing file was edited.
    Edit,
    /// A file was deleted (all removals).
    Delete,
}

/// What produced a `FileChange` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeSource {
    /// An explicit joey file tool (`write_file`, `patch`).
    FileTool,
    /// A terminal command mutated the file (snapshot-diff detected).
    Terminal,
    /// Diff text detected in a tool's textual output (FR-005).
    Detected,
}

/// An event emitted during a turn. The CLI/gateway renders these live.
///
/// Ordering guarantees: `ContentDelta`/`ReasoningDelta` stream during a
/// provider call; `AssistantMessage` fires when a complete assistant message
/// is recorded (interim messages during tool loops are deduped against the
/// previous interim — conversation_loop.py:4997-5013); `ToolStart` /
/// `ToolProgress` / `ToolEnd` bracket tool execution; exactly one of
/// `Done`/`Failed` ends the turn.
///
/// `FileChange` (feature 005) is emitted by the tool execution path,
/// positioned within the stream as `ToolStart` → (`FileChange`)* → `ToolEnd`
/// for the same tool call. Only the tool layer (in `joey-tools`) produces
/// these; the render layer consumes them. See
/// `specs/005-expandable-diff-ui/contracts/agent-event.md`.
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
        /// Process exit code for `terminal` tool calls (feature 007).
        /// `None` for non-terminal tools, errors, and any tool whose result
        /// does not carry an exit code. Sourced by a guarded JSON parse at
        /// the agent-loop boundary. Drives the `(exit N)` badge in the TUI.
        exit_code: Option<i64>,
        /// The full result text for the tool call (feature 007 convergence).
        /// Backs the crush-style "expand to reveal full content" affordance:
        /// the TUI stores this in the transcript item's `full_result` and
        /// renders it when expanded, instead of the one-line `result_preview`.
        /// `result_preview` stays as the always-shown collapsed summary.
        full_result: String,
    },

    // ── File changes (feature 005) ────────────────────────────────────
    /// A file was created, edited, or deleted by the agent during a turn.
    /// Emitted inline with the tool execution that caused the change, so the
    /// renderer can draw an inline diff attributed to that tool call.
    /// Ordering: `ToolStart` → (`FileChange`)* → `ToolEnd` for one tool call.
    /// Producer surface: only the tool layer (`joey-tools`).
    FileChange {
        /// Display-normalized file path.
        path: String,
        /// What kind of change this is (drives the new-file / deleted-file label).
        kind: FileChangeKind,
        /// Baseline content (from `FileTracker::get_original`). Empty for Create.
        before: String,
        /// Post-write on-disk content. Empty for Delete.
        after: String,
        /// The computed unified diff + counts. Reuses
        /// `joey_tools::file_tracker::DiffResult`. Empty `.diff` when
        /// `is_binary` is true.
        diff: joey_tools::file_tracker::DiffResult,
        /// True when before/after could not be decoded as UTF-8. When true the
        /// renderer MUST show a "binary file changed" placeholder (FR-016) and
        /// MUST NOT attempt to render `.diff`.
        is_binary: bool,
        /// What produced this event: an explicit file tool, a terminal
        /// snapshot, or diff-text detection (FR-005).
        source: FileChangeSource,
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
    /// Feature 015 (NeuroCode): the pre-dispatch intercept assembled a
    /// dependency-aware context graph for the upcoming request. Emitted
    /// BEFORE the model call so UIs can show a live feed of exactly what
    /// context NeuroCode is feeding the agent. Only fired when the engine
    /// is wired AND active (byte-identical when off).
    NeuroCodeContext {
        /// The complexity tier that served the request (e.g. "frontier").
        tier: String,
        /// Estimated tokens in the assembled context.
        token_estimate: usize,
        /// Number of graph-expanded nodes included.
        expanded_nodes: usize,
        /// Whether the project was cold/un-indexed (degraded mode).
        cold_mode: bool,
        /// The full formatted context text fed to the model (the live feed
        /// payload; UIs truncate for display).
        formatted_context: String,
    },
    /// Feature 015 (NeuroCode) follow-up: live assembly progress emitted
    /// DURING `assemble_context_with_progress` (before the final
    /// `NeuroCodeContext` blob) so UIs can stream the context feed in
    /// realtime — one event per assembly stage (locate → expand → format →
    /// anti-patterns → domain knowledge). Only fired when the engine is
    /// wired AND active (byte-identical when off).
    NeuroCodeProgress {
        /// Short human-readable stage description (e.g. "expanded graph: 7 nodes pulled in").
        stage: String,
    },
    /// Feature 015 (NeuroCode): the engine's active/inactive state changed
    /// (wired + enabled). Lets UIs show/hide the NeuroCode indicator without
    /// polling config.
    NeuroCodeActive { active: bool },
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

#[cfg(test)]
mod tests {
    //! Feature 005 regression coverage for the new public `FileChange`
    //! variant (constitution Principle VII). Asserts the variant is
    //! constructible and carries through `Clone`/`Debug`, and that the
    //! supporting enums derive their intended traits.

    use super::*;

    #[test]
    fn file_change_variant_constructs_and_clones() {
        let diff = joey_tools::file_tracker::DiffResult {
            path: "src/main.rs".to_string(),
            diff: "--- a/src/main.rs\n+++ b/src/main.rs\n".to_string(),
            added: 1,
            removed: 1,
        };
        let ev = AgentEvent::FileChange {
            path: "src/main.rs".to_string(),
            kind: FileChangeKind::Edit,
            before: "old\n".to_string(),
            after: "new\n".to_string(),
            diff: diff.clone(),
            is_binary: false,
            source: FileChangeSource::FileTool,
        };
        // Clone round-trips.
        let cloned = ev.clone();
        // Debug formats without panicking (covers all fields).
        let _dbg = format!("{:?}", cloned);
        // The variant is a FileChange and kind/source match what we set.
        match ev {
            AgentEvent::FileChange { kind, source, .. } => {
                assert_eq!(kind, FileChangeKind::Edit);
                assert_eq!(source, FileChangeSource::FileTool);
            }
            _ => panic!("expected FileChange variant"),
        }
    }

    #[test]
    fn file_change_kinds_are_distinct() {
        assert_ne!(FileChangeKind::Create, FileChangeKind::Edit);
        assert_ne!(FileChangeKind::Edit, FileChangeKind::Delete);
        assert_ne!(FileChangeKind::Create, FileChangeKind::Delete);
    }

    #[test]
    fn file_change_sources_are_distinct() {
        assert_ne!(FileChangeSource::FileTool, FileChangeSource::Terminal);
        assert_ne!(FileChangeSource::Terminal, FileChangeSource::Detected);
    }
}
