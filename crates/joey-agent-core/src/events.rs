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

/// One message in a live context-window snapshot (see
/// [`AgentEvent::ContextSnapshot`]). A display-oriented projection of a
/// `Message`: role, rough size, and a bounded single-line preview.
#[derive(Debug, Clone)]
pub struct ContextEntry {
    /// Message role: user / assistant / tool (provider-neutral string).
    pub role: String,
    /// Rough token estimate for this message.
    pub tokens: u64,
    /// Bounded preview (first line, ~80 chars).
    pub preview: String,
    /// Whether this message carries tool_calls (assistant tool-request).
    pub has_tool_calls: bool,
    /// Whether this entry is a context-compaction summary (compressed).
    pub is_compressed_summary: bool,
    /// The FULL text content of the message (expandable-stats feature).
    /// Populated from the message's text content; assistant tool-request
    /// messages (empty text) carry the tool_calls rendered as indented
    /// JSON instead. UIs that only need the one-line preview can ignore
    /// this — it is purely additive.
    pub full_content: String,
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
    /// A chunk of RAW output streamed live from a running tool (feature:
    /// realtime terminal output). Emitted between `ToolStart` and `ToolEnd`
    /// for the same tool name, carrying lossy-UTF-8 text chunks exactly as
    /// the subprocess produced them (throttled to the same 50ms window as
    /// `ToolProgress`). UIs that want a live terminal view accumulate these
    /// per tool call; UIs that don't can ignore them (additive variant —
    /// `ToolEnd.full_result` still carries the complete output).
    ToolOutput { name: String, chunk: String },
    /// A complete live snapshot of the agent's context window (feature:
    /// realtime agent-stats page). Emitted at every history mutation the
    /// turn loop makes (user message appended, assistant/tool messages
    /// flushed, post-compaction) plus turn start, so a UI can render a
    /// live-streaming view of exactly what will be sent to the model.
    /// Additive: UIs that don't care can ignore it — nothing about the
    /// request itself changes.
    ContextSnapshot {
        /// Entries in send order (oldest first), one per history message.
        entries: Vec<ContextEntry>,
        /// Rough token estimate for the system prompt (stable per session).
        system_tokens: u64,
        /// Rough token estimate for the whole history.
        history_tokens: u64,
        /// The compressor's context window (0 = unknown).
        context_window: u64,
        /// The effective compression threshold in tokens (0 = unknown).
        compression_threshold: u64,
        /// Number of prior compactions this session.
        compactions: u32,
        /// Model id the next request will use.
        model: String,
    },
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
    /// A subagent was spawned (per child). `id` is stable for the child's
    /// whole lifetime and correlates the [`AgentEvent::SubagentEvent`]
    /// stream, completion, and the TUI's per-subagent pane.
    SubagentSpawn {
        id: u64,
        goal: String,
        model: String,
        toolset_summary: String,
        depth: usize,
    },
    /// A subagent completed successfully.
    SubagentComplete {
        id: u64,
        goal: String,
        success: bool,
        summary_preview: String,
        token_usage: Usage,
        duration_secs: f64,
    },
    /// A subagent failed with an error.
    SubagentFailed {
        id: u64,
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
    /// A live event produced by a running subagent, tagged with the child's
    /// stable id (parallel-subagent feature). The orchestration layer wraps
    /// EVERY event the child `Agent` emits — `ContentDelta`, `ToolStart`,
    /// `ContextSnapshot`, even the child's own `Done` — so a UI can render a
    /// dedicated per-subagent view without those events contaminating the
    /// parent's transcript or turn state. Consumers that don't care about
    /// per-subagent detail can ignore this variant entirely; the plain
    /// `SubagentSpawn`/`SubagentComplete`/`SubagentFailed` lifecycle events
    /// still arrive unwrapped alongside it.
    SubagentEvent {
        /// Stable child id (matches `SubagentSpawn.id`).
        id: u64,
        /// The child's own event.
        event: Box<AgentEvent>,
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
    /// Feature 015 follow-up (interactive context visualization): the
    /// structured node/edge snapshot of the assembled context graph, emitted
    /// right after [`AgentEvent::NeuroCodeContext`]. UIs use this to render
    /// an interactive graph view of the expansion window. Only fired when
    /// the engine produced a snapshot (populated graph); the CLI renderer
    /// consumes it silently.
    NeuroCodeGraph {
        /// Pure-data projection of primary + expanded nodes and the edges
        /// among them (no graph-store handle inside).
        snapshot: joey_neurocode::ContextGraphSnapshot,
    },
    /// Feature 015 (NeuroCode): the engine's active/inactive state changed
    /// (wired + enabled). Lets UIs show/hide the NeuroCode indicator without
    /// polling config.
    NeuroCodeActive { active: bool },
    /// Feature 015 follow-up (auto re-index): the structural graph was
    /// rebuilt after large edits, so subsequent assemblies reflect the
    /// current codebase. Emitted after the re-index completes at turn end;
    /// UIs show a brief notice. The turn's already-assembled context is
    /// NOT retroactively rewritten — the next user turn re-assembles
    /// against the fresh index.
    NeuroCodeReindexed {
        /// Distinct source files re-scanned.
        files_scanned: usize,
        /// Edit-pressure that triggered the pass (files / lines tracked
        /// before the reset).
        files_edited: usize,
        lines_edited: usize,
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
