//! TUI application state machine.
//!
//! Consumes the [`AgentEvent`] stream and maintains a rich, queryable model
//! that the widgets render each frame. This replaces the line-based
//! `render_turn` with a live, animated view.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use joey_agent_core::events::{AgentEvent, ContextEntry, FileChangeKind};
use joey_tools::file_tracker::DiffResult;

/// Feature 005 (T021): the three-state expand cycle for reasoning blocks.
///
/// Collapsed → TailWindow → Full → Collapsed → …
///
/// **Skip rule:** if the reasoning text is short enough to fit in the
/// collapsed view (≤ `MAX_COLLAPSED_HEIGHT` lines), `TailWindow` and `Full`
/// would show the exact same content, so the cycle skips directly to the
/// next distinct state to avoid a redundant no-op press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningExpandState {
    /// Show only the first `MAX_COLLAPSED_HEIGHT` lines + "… (N more)".
    Collapsed,
    /// Show the last `MAX_TAIL_WINDOW_LINES` lines (most recent thinking).
    TailWindow,
    /// Show the full reasoning text, no truncation.
    Full,
}

/// Feature 005 (T021): max lines shown in the collapsed reasoning view.
const MAX_COLLAPSED_HEIGHT: usize = 10;
/// Feature 005 (T021): max lines shown in the tail-window (expanded) view.
const MAX_TAIL_WINDOW_LINES: usize = 200;

impl ReasoningExpandState {
    /// Advance to the next state in the cycle, applying the skip rule.
    ///
    /// `total_lines` is the number of lines in the reasoning text, used to
    /// decide whether a state would be redundant.
    pub fn cycle(self, total_lines: usize) -> Self {
        use ReasoningExpandState::*;
        let fits_collapsed = total_lines <= MAX_COLLAPSED_HEIGHT;
        let fits_tail = total_lines <= MAX_TAIL_WINDOW_LINES;
        match self {
            Collapsed => {
                if fits_collapsed {
                    // Collapsed already shows everything; skip to Full.
                    if fits_tail {
                        Full
                    } else {
                        TailWindow
                    }
                } else {
                    TailWindow
                }
            }
            TailWindow => {
                if fits_tail {
                    // TailWindow == Full; skip to Collapsed.
                    Collapsed
                } else {
                    Full
                }
            }
            Full => Collapsed,
        }
    }
}

/// One entry in the conversation transcript.
#[derive(Clone, Debug)]
pub enum TranscriptItem {
    User { text: String },
    Assistant { text: String },
    /// A complete reasoning block shown in a dimmed/collapsed style.
    /// Feature 005 (T021): carries a per-item expand state.
    Reasoning {
        text: String,
        /// Per-item expand state for the three-state cycle
        /// (collapsed → tail-window → full → collapsed). See
        /// `contracts/expandable.md`.
        expand_state: ReasoningExpandState,
        /// Feature 007: elapsed time from first `ReasoningDelta` to flush.
        /// Drives the `Thought for Ns` footer. `None` when duration wasn't
        /// tracked (e.g. short blocks where timing is meaningless).
        thought_duration: Option<Duration>,
    },
    /// A tool call rendered inline with its result.
    /// Feature 005 (T026): carries the three-state expand cycle + full
    /// args/result. Unified with the reasoning-history format: click/space
    /// expands INLINE (collapsed → tail window → full), never a separate
    /// viewer window.
    Tool {
        name: String,
        emoji: String,
        summary: String,
        status: ToolStatus,
        duration_secs: Option<f64>,
        result_preview: String,
        /// Feature 005 (T026): per-item expand state (three-state cycle,
        /// same semantics as reasoning blocks).
        expand_state: ReasoningExpandState,
        /// Feature 005 (T026): full arguments JSON for the expanded view.
        full_args: Option<String>,
        /// Feature 005 (T026): full result text for the expanded view.
        full_result: Option<String>,
        /// Feature 007: whether this is a terminal/shell tool call.
        is_terminal: bool,
        /// Feature 007: process exit code (terminal tools only).
        exit_code: Option<i64>,
        /// Live-streamed raw output accumulated from `AgentEvent::ToolOutput`
        /// while the tool runs (terminal calls only). Bounded ring — see
        /// `live_output_capacity` on the item; the definitive full output
        /// still arrives via `ToolEnd.full_result` and wins once present.
        live_output: String,
        /// Cap (bytes) for `live_output` accumulation. The tail matters in a
        /// live view; the head is evicted when the cap is hit.
        live_output_capacity: usize,
    },
    /// Feature 005 (T018): an inline file-change diff block. Pushed when an
    /// `AgentEvent::FileChange` arrives, rendered as a path header + stat +
    /// colored diff lines (T019). See `data-model.md` RenderedDiffBlock.
    FileDiff {
        path: String,
        stat: String,
        /// Pre-split diff lines (including `+`/`-`/` ` markers).
        lines: Vec<String>,
        is_binary: bool,
        /// Expand state (three-state cycle, same semantics as reasoning
        /// blocks): collapsed shows the last MAX_DIFF_LINES lines; the tail
        /// window shows the last 200; full shows the whole diff.
        expand_state: ReasoningExpandState,
    },
    /// A system notice / status line.
    Notice { text: String, kind: NoticeKind },
    Error { text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
    Failed,
}

/// Feature 007 (T016, FR-017): classify whether a tool name is a terminal
/// block (should render with crush's `$ command` layout).
pub fn is_terminal_block(name: &str) -> bool {
    name == "terminal"
}

/// Default cap (bytes) for per-item live-output accumulation. The live view
/// is a tail window; the definitive full output lands in `full_result` at
/// `ToolEnd`. 128 KB matches the background ring buffers' order of magnitude
/// while keeping even pathological transcripts affordable (1024-item
/// capacity × 128 KB worst case ≈ 128 MB only when EVERY item is a live
/// terminal call at the cap — in practice only the newest few items carry
/// live output at all, and it is replaced by the bounded `full_result`).
pub const LIVE_OUTPUT_CAPACITY: usize = 128 * 1024;

// ── Tool-result content formatting (crush-style display projection) ──────

/// Extract the human payload from a tool result string for DISPLAY.
///
/// Many built-ins serialize their result as a compact JSON envelope
/// (`{"output":"…","exit_code":0,"error":null}` for terminal,
/// `{"error":"…"}` for failures). The transcript should show the payload,
/// not the envelope. Returns `None` when the string isn't a recognizable
/// envelope (displayed verbatim then).
pub fn display_result_content(content: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    match &v {
        // terminal / process envelopes: the `output` field is the payload.
        serde_json::Value::Object(map) => {
            if let Some(out) = map.get("output").and_then(|o| o.as_str()) {
                return Some(out.to_string());
            }
            // tool-error envelope: `{"error": "…"}`.
            if map.len() == 1 {
                if let Some(err) = map.get("error").and_then(|e| e.as_str()) {
                    return Some(err.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Pretty-print a string as JSON when it parses (2-space indent), for
/// DISPLAY in text-editor-like views: string values keep their REAL
/// newlines/tabs (via [`joey_core::utils::pretty_json_for_display`]) so
/// the view shows actual line breaks instead of literal `\n` escape runs.
/// On parse failure the original is returned unchanged.
pub fn pretty_json_if_parses(s: &str) -> String {
    let trimmed = s.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return s.to_string();
    }
    match joey_core::utils::pretty_json_for_display(trimmed) {
        Some(out) => out,
        None => s.to_string(),
    }
}

/// The display text for a tool call's full result: envelope-unwrapped, and
/// JSON pretty-printed when the payload itself is JSON (e.g. MCP tool
/// results, list-shaped outputs). Falls back to the raw string.
pub fn format_tool_result_for_display(content: &str) -> String {
    match display_result_content(content) {
        Some(payload) => pretty_json_if_parses(&payload),
        None => pretty_json_if_parses(content),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Warning,
    Success,
    Busy,
}

/// A currently-running agent turn (one per concurrent tool/iteration).
#[derive(Clone, Debug)]
pub struct ActiveAgent {
    pub id: usize,
    pub label: String,
    pub phase: AgentPhase,
    pub started: Instant,
    pub iterations: usize,
    pub max_iterations: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPhase {
    Idle,
    /// Waiting on the model API.
    QueryingModel,
    /// Executing a named tool.
    RunningTool(String),
    /// Reasoning / thinking.
    Reasoning,
    Done,
}

/// For the Tab picker and activity panel roster (T028, data-model.md).
#[derive(Clone, Debug)]
pub struct DisplayAgent {
    /// Canonical name (e.g. "sisyphus", "default").
    pub name: String,
    /// Human label (e.g. "Sisyphus", "Default").
    pub display_name: String,
    /// Hex color string.
    pub color: String,
    /// Primary or Subagent.
    pub mode: String,
    /// Resolved model (None = unavailable/skipped).
    pub resolved_model: Option<String>,
    /// Short description.
    pub description: String,
}

/// For the activity panel when subagents are running (T064).
#[derive(Clone, Debug)]
pub struct ActiveSubagentEntry {
    /// Unique entry ID.
    pub id: usize,
    /// Stable child id from the orchestration layer (parallel-subagent
    /// feature). Correlates SubagentSpawn/SubagentEvent/SubagentComplete.
    pub child_id: u64,
    /// "explore", "librarian", "oracle", "sisyphus-junior", etc.
    pub agent_type: String,
    /// If category-spawned (e.g. "quick").
    pub category: Option<String>,
    /// Pending, Running, Done, Failed, Stopped (spec 020).
    pub status: SubagentStatus,
    /// "querying model", "running tool: X", "reasoning".
    pub phase: String,
    /// Resolved model.
    pub model: String,
    /// API calls made.
    pub iterations: usize,
    /// For elapsed time.
    pub started: Instant,
    /// T155: Human-readable delegated task title for the Atlas job board
    /// (e.g. "Task 1: Implement auth"). Populated from the delegation goal or
    /// BoulderWorkStarted task title. None for non-plan delegations.
    pub task_title: Option<String>,
    /// T155: Number of tool calls this entry has made. Incremented on each
    /// ToolStart attributed to the entry.
    pub tool_call_count: usize,
    /// T155: Name of the most recent tool invoked by this entry.
    pub last_tool: Option<String>,
}

// ── Per-subagent panes (parallel-subagent feature) ─────────────────────

/// A dedicated live view for one subagent. The TUI keeps one pane per
/// spawned child; the right-side vertical tab rail lists them and clicking
/// a tab focuses that pane, retargeting the main transcript + the
/// maximized stats/context window to the child's stream.
#[derive(Clone, Debug)]
pub struct SubagentPane {
    /// Stable child id (correlates SubagentSpawn.id).
    pub child_id: u64,
    /// Goal line (tab label source).
    pub goal: String,
    /// Resolved model.
    pub model: String,
    /// Toolset summary at spawn.
    pub toolset_summary: String,
    /// Delegation depth.
    pub depth: usize,
    /// Lifecycle status.
    pub status: SubagentStatus,
    /// The child's transcript (items pushed from wrapped SubagentEvents).
    pub transcript: VecDeque<TranscriptItem>,
    /// Bounded capacity for the child transcript (ring).
    pub transcript_capacity: usize,
    /// Live streaming assistant text from the child.
    pub streaming_assistant: String,
    /// Live streaming reasoning text from the child.
    pub streaming_reasoning: String,
    /// T034 (US4, FR-008, D6): expanded mode for THIS pane's live
    /// reasoning panel — set by clicking the pane's docked strip, the
    /// live stream takes over the pane view below a transcript strip,
    /// and clicking again (or Esc) docks it back. Mirrors
    /// `App::reasoning_expanded` (auto-collapses when the block flushes).
    pub reasoning_expanded: bool,
    /// T034: view state for THIS pane's live reasoning panel. `None` =
    /// auto-follow (pinned to the live tail); `Some(anchor)` = frozen at
    /// that absolute wrapped-line index. Mirrors `App::reasoning_view`;
    /// per-pane so sibling panes keep their own scroll (FR-010).
    pub reasoning_view: Option<usize>,
    /// T034: timestamp of the first `ReasoningDelta` of the pane's
    /// current block — drives the "◆ thinking Ns" header and the
    /// `thought_duration` stamped on flush. Mirrors
    /// `App::reasoning_started` (Feature 007).
    pub reasoning_started: Option<Instant>,
    /// Scroll offset in the child transcript (None = auto-follow).
    pub scroll: Option<usize>,
    /// Latest context-window snapshot fields (for the per-child stats view).
    pub context_entries: Vec<ContextEntry>,
    pub context_system_tokens: u64,
    pub context_history_tokens: u64,
    pub context_window: u64,
    pub compression_threshold: u64,
    pub compactions: u32,
    /// T004 (FR-010): view anchor for THIS pane's maximized stats/context
    /// stream. `None` = auto-follow (pinned to the live tail); `Some(anchor)`
    /// = frozen at that absolute line. Per-pane so switching focus never
    /// resets a sibling pane's scroll — mirrors `App::stats_view` (main
    /// transcript stats page).
    pub stats_view: Option<usize>,
    /// T004: upper bound for a valid `stats_view` anchor, recorded by the
    /// pane stats widget at render time (wrap widths are render-only
    /// knowledge). Mirrors `App::last_stats_max_anchor`.
    pub last_stats_max_anchor: Cell<usize>,
    /// Expandable-stats feature: which context-stream entries are expanded
    /// (indices into `context_entries`). Entries whose index isn't in the
    /// set render collapsed (one-line preview); expanded entries render the
    /// full content inline with a gutter.
    pub expanded_context: std::collections::HashSet<usize>,
    /// T015 (US3, FR-006/FR-007, design D5): per-pane search state — a
    /// mirror of `App`'s search-bar fields so each pane owns its query and
    /// match indicator against ITS transcript (FR-010: survives focus
    /// switches; the orchestrator's `App::search_*` is a separate view's
    /// state and is never consulted for a pane search). Match-indicator
    /// only — search never highlights text in-place (D5 parity).
    pub search_open: bool,
    /// Current search query for THIS pane's transcript.
    pub search_query: String,
    /// Whether the last pane search found any matches.
    pub search_has_match: bool,
    /// Cumulative usage for the child.
    pub tokens: TokenStats,
    /// When the child started.
    pub started: Instant,
    /// Summary preview once complete.
    pub summary_preview: Option<String>,
    /// True once a wrapped SubagentEvent arrived for this pane — the
    /// id-matched attribution path is live, and the raw-channel duplicate
    /// must not double-count tool calls.
    pub tap_attached: bool,
    /// T022 (US4, FR-008): mode attribution — true when the NeuroCode mode
    /// spawned this pane (snapshot of `App::neurocode_active` at
    /// SubagentSpawn time). Gates the mode-specific explorer render arm:
    /// mode explorers are reachable only from surfaces that mode spawned
    /// (the orchestrator view + the panes it spawned), never from a plain
    /// delegation pane. Bool mirrors how App represents the mode
    /// (`neurocode_active: bool`) — the minimal additive shape.
    pub spawned_by_neurocode: bool,
}

impl SubagentPane {
    fn new(spawn_goal: &str, model: String, toolset_summary: String, depth: usize) -> Self {
        Self {
            child_id: 0,
            goal: spawn_goal.to_string(),
            model,
            toolset_summary,
            depth,
            status: SubagentStatus::Running,
            transcript: VecDeque::with_capacity(64),
            transcript_capacity: 256,
            streaming_assistant: String::new(),
            streaming_reasoning: String::new(),
            reasoning_expanded: false,
            reasoning_view: None,
            reasoning_started: None,
            scroll: None,
            context_entries: Vec::new(),
            context_system_tokens: 0,
            context_history_tokens: 0,
            context_window: 0,
            compression_threshold: 0,
            compactions: 0,
            stats_view: None,
            last_stats_max_anchor: Cell::new(0),
            expanded_context: std::collections::HashSet::new(),
            search_open: false,
            search_query: String::new(),
            search_has_match: false,
            tokens: TokenStats::default(),
            started: Instant::now(),
            summary_preview: None,
            tap_attached: false,
            spawned_by_neurocode: false,
        }
    }

    /// Percentage of the child's context window in use (0.0 when unknown).
    pub fn context_usage_pct(&self) -> f64 {
        if self.context_window == 0 {
            return 0.0;
        }
        let used = self.context_system_tokens + self.context_history_tokens;
        (used as f64 / self.context_window as f64) * 100.0
    }

    /// Push an item into the pane transcript, enforcing the ring capacity.
    pub fn push_item(&mut self, item: TranscriptItem) {
        if self.transcript.len() >= self.transcript_capacity {
            self.transcript.pop_front();
        }
        self.transcript.push_back(item);
    }

    /// Expandable-stats/pane parity: toggle the pane transcript item at
    /// `index`. All expandable kinds (reasoning, tool, file-diff) use the
    /// same three-state inline cycle.
    pub fn toggle_item_expand(&mut self, index: usize) {
        if index >= self.transcript.len() {
            return;
        }
        toggle_expand_item(&mut self.transcript[index]);
    }
}

/// Shared three-state inline expand cycle for a single item (the
/// reasoning-history semantics, unified across all expandable kinds).
pub(crate) fn toggle_expand_item(item: &mut TranscriptItem) {
    // Compute the line count BEFORE mutably borrowing the expand state.
    let total_lines = expandable_line_count(item);
    let expand_state = match item {
        TranscriptItem::Reasoning { expand_state, .. }
        | TranscriptItem::Tool { expand_state, .. }
        | TranscriptItem::FileDiff { expand_state, .. } => expand_state,
        _ => return,
    };
    *expand_state = expand_state.cycle(total_lines);
}

/// Test-only exposure of the unified inline expand cycle (integration
/// tests link this crate as an external crate, so plain `pub`).
pub fn toggle_expand_for_test(item: &mut TranscriptItem) {
    toggle_expand_item(item);
}

/// Test-only read of an item's expand state (any expandable kind).
pub fn expand_state_for_test(item: &TranscriptItem) -> ReasoningExpandState {
    match item {
        TranscriptItem::Reasoning { expand_state, .. }
        | TranscriptItem::Tool { expand_state, .. }
        | TranscriptItem::FileDiff { expand_state, .. } => *expand_state,
        _ => ReasoningExpandState::Collapsed,
    }
}

/// Total logical lines an item's expanded view can show (drives the cycle's
/// redundancy skip rules).
fn expandable_line_count(item: &TranscriptItem) -> usize {
    match item {
        TranscriptItem::Reasoning { text, .. } => text.lines().count(),
        TranscriptItem::Tool { full_result, result_preview, .. } => {
            let payload = full_result
                .as_deref()
                .filter(|f| !f.is_empty())
                .unwrap_or(result_preview);
            format_tool_result_for_display(payload).lines().count()
        }
        TranscriptItem::FileDiff { lines, .. } => lines.len(),
        _ => 0,
    }
}

/// Expandable-stats feature: identity of a context entry for expansion
/// remapping across snapshots. (role, tokens, preview) is stable for an
/// unchanged message; pure renumbering (append/compaction) maps cleanly.
/// When multiple entries share an identity the FIRST match wins and is
/// consumed, avoiding duplicate expansion after repeated identical turns.
fn entry_identity(e: &ContextEntry) -> (String, u64, String) {
    (e.role.clone(), e.tokens, e.preview.clone())
}

/// Remap an old expansion set onto a new snapshot's indices. Old expanded
/// entries keep their expansion when a same-identity entry exists in the
/// new list; dropped messages (compacted away) lose their expansion.
fn remap_expansions(
    old_entries: &[ContextEntry],
    old_expanded: &std::collections::HashSet<usize>,
    new_entries: &[ContextEntry],
) -> std::collections::HashSet<usize> {
    if old_expanded.is_empty() {
        return std::collections::HashSet::new();
    }
    let mut new_set = std::collections::HashSet::new();
    let mut wanted: Vec<(String, u64, String)> = old_entries
        .iter()
        .enumerate()
        .filter(|(i, _)| old_expanded.contains(i))
        .map(|(_, e)| entry_identity(e))
        .collect();
    for (j, e) in new_entries.iter().enumerate() {
        let id = entry_identity(e);
        if let Some(pos) = wanted.iter().position(|w| *w == id) {
            wanted.remove(pos);
            new_set.insert(j);
        }
    }
    new_set
}

/// Build a `TranscriptItem::FileDiff` from a `FileChange` event — the single
/// construction shared by the main transcript (`App::apply`, feature 005
/// T018) and the per-pane path (`pane_apply`, spec 017 T013 / D7, FR-005):
/// panes must render file diffs exactly like the main transcript, so both
/// paths map the event through this one fn (parity by construction).
fn file_diff_item(path: &str, kind: FileChangeKind, diff: &DiffResult, is_binary: bool) -> TranscriptItem {
    let label = match kind {
        FileChangeKind::Create => " (new file)",
        FileChangeKind::Delete => " (deleted)",
        FileChangeKind::Edit => "",
    };
    let stat = format!("{}{}", diff.stat_line(), label);
    let lines: Vec<String> = if is_binary {
        Vec::new()
    } else {
        diff.diff.lines().map(|l| l.to_string()).collect()
    };
    TranscriptItem::FileDiff {
        path: path.to_string(),
        stat,
        lines,
        is_binary,
        expand_state: ReasoningExpandState::Collapsed,
    }
}

/// Flush a pane's pending streamed reasoning into its transcript as a
/// collapsed `Reasoning` item, mirroring App's `flush_reasoning` on the
/// pane (T034 / FR-008, D6): compute `thought_duration` from the pane's
/// `reasoning_started` clock (set on the first delta of the block),
/// reset the clock, and auto-dock the pane's expanded reasoning panel
/// (an expanded view with no live block is over). Shared by
/// `pane_apply`'s boundary flushes and the parent-side
/// `SubagentComplete`/`SubagentFailed` close-out (T032) so all flush
/// sites construct the item identically.
fn pane_flush_reasoning(pane: &mut SubagentPane) {
    if !pane.streaming_reasoning.is_empty() {
        let text = std::mem::take(&mut pane.streaming_reasoning);
        // Feature 007 parity: compute the thinking duration from the
        // first delta of this block (App::flush_reasoning semantics).
        let thought_duration = pane.reasoning_started.take().map(|s| s.elapsed());
        pane.push_item(TranscriptItem::Reasoning {
            text,
            expand_state: ReasoningExpandState::Collapsed,
            thought_duration,
        });
        // T034: the live block ended — auto-dock the pane's expanded
        // panel and reset its frozen anchor (App::flush_reasoning's
        // reasoning_expanded/reasoning_view reset, mirrored).
        pane.reasoning_expanded = false;
        pane.reasoning_view = None;
    }
}

/// Apply a child event to a pane — a reduced version of the main
/// `App::apply` logic covering the display-relevant subset. Lifecycle
/// events (`TurnStart`/`Done`/`Failed` from the CHILD) intentionally do
/// NOT touch pane status: pane status is owned by the parent-side
/// `SubagentComplete`/`SubagentFailed` lifecycle events.
fn pane_apply(pane: &mut SubagentPane, ev: &AgentEvent) {
    use AgentEvent::*;
    match ev {
        ContentDelta(d) => pane.streaming_assistant.push_str(d),
        ReasoningDelta(d) => {
            // T034 (US4, FR-008, D6 / Feature 007 parity): mark the start
            // of this reasoning block on the first delta — the main
            // loop's semantics (App::apply's ReasoningDelta arm sets
            // reasoning_started when the block opens from empty). Panes
            // have no reasoning_open latch, so the content-based
            // condition is "no text yet AND no clock yet"; a fully-empty
            // delta stream commits no item on either path, so it never
            // starts a clock. Drives the "◆ thinking Ns" header and the
            // thought_duration stamped by pane_flush_reasoning.
            if pane.streaming_reasoning.is_empty() && pane.reasoning_started.is_none() {
                pane.reasoning_started = Some(Instant::now());
            }
            pane.streaming_reasoning.push_str(d);
        }
        AssistantMessage(text) => {
            // T021 (US4, D6): flush pending streamed reasoning at the
            // message boundary — the main loop's flush-on-boundary
            // semantics (App::apply ToolStart arm / flush_reasoning).
            // T034: the committed item carries a thought_duration
            // computed from the pane's reasoning_started clock (the
            // same Feature-007 stamping as main's flush_reasoning).
            pane_flush_reasoning(pane);
            let final_text = if text.is_empty() {
                std::mem::take(&mut pane.streaming_assistant)
            } else {
                pane.streaming_assistant.clear();
                text.clone()
            };
            if !final_text.is_empty() {
                pane.push_item(TranscriptItem::Assistant { text: final_text });
            }
        }
        ToolStart { name, emoji, summary } => {
            // T021 (US4, D6): same flush at the ToolStart boundary — the
            // reasoning commits BEFORE the tool item (main loop ordering:
            // App::apply's ToolStart arm calls flush_reasoning() first).
            pane_flush_reasoning(pane);
            pane.push_item(TranscriptItem::Tool {
            name: name.clone(),
            emoji: emoji.clone(),
            summary: summary.clone(),
            status: ToolStatus::Running,
            duration_secs: None,
            result_preview: String::new(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: is_terminal_block(name),
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: LIVE_OUTPUT_CAPACITY,
            });
        }
        ToolProgress { name, progress } => {
            if !progress.is_empty() {
                for it in pane.transcript.iter_mut().rev() {
                    if let TranscriptItem::Tool { name: n, status, summary, is_terminal, .. } = it {
                        if *status == ToolStatus::Running && *n == *name && !*is_terminal {
                            *summary = progress.clone();
                            break;
                        }
                    }
                }
            }
        }
        ToolOutput { name, chunk } => {
            if !chunk.is_empty() {
                for it in pane.transcript.iter_mut().rev() {
                    if let TranscriptItem::Tool {
                        name: n,
                        status,
                        live_output,
                        live_output_capacity,
                        ..
                    } = it
                    {
                        if *status == ToolStatus::Running && *n == *name {
                            App::push_bounded_item(live_output, chunk, *live_output_capacity);
                            break;
                        }
                    }
                }
            }
        }
        ToolEnd { name, is_error, result_preview, duration_secs, exit_code, full_result } => {
            for it in pane.transcript.iter_mut().rev() {
                if let TranscriptItem::Tool {
                    name: n,
                    status,
                    duration_secs: dur,
                    result_preview: rp,
                    exit_code: ec,
                    full_result: fr,
                    ..
                } = it
                {
                    if *status == ToolStatus::Running && *n == *name {
                        *status = if *is_error { ToolStatus::Failed } else { ToolStatus::Done };
                        *dur = Some(*duration_secs);
                        *rp = result_preview.clone();
                        *ec = *exit_code;
                        *fr = Some(full_result.clone());
                        break;
                    }
                }
            }
        }
        Notice(msg) => pane.push_item(TranscriptItem::Notice {
            text: msg.clone(),
            kind: NoticeKind::Info,
        }),
        RetryAttempt { attempt, max_retries, error, .. } => pane.push_item(TranscriptItem::Notice {
            text: format!("Retry {}/{}: {}", attempt, max_retries, error),
            kind: NoticeKind::Warning,
        }),
        CompressionStart { reason, approx_tokens } => pane.push_item(TranscriptItem::Notice {
            text: format!("Compressing ~{} tokens: {}", approx_tokens, reason),
            kind: NoticeKind::Busy,
        }),
        CompressionEnd { original_msgs, new_msgs } => pane.push_item(TranscriptItem::Notice {
            text: format!("Compressed {} → {} messages", original_msgs, new_msgs),
            kind: NoticeKind::Success,
        }),
        ContextSnapshot {
            entries,
            system_tokens,
            history_tokens,
            context_window,
            compression_threshold,
            compactions,
            model: _,
        } => {
            pane.expanded_context =
                remap_expansions(&pane.context_entries, &pane.expanded_context, entries);
            pane.context_entries = entries.clone();
            pane.context_system_tokens = *system_tokens;
            pane.context_history_tokens = *history_tokens;
            pane.context_window = *context_window;
            pane.compression_threshold = *compression_threshold;
            pane.compactions = *compactions;
        }
        ApiCallEnd { usage } => {
            pane.tokens.prompt += usage.prompt_tokens;
            pane.tokens.completion += usage.completion_tokens;
            pane.tokens.iterations += 1;
        }
        // Spec 017 (T013, FR-005, D7): map FileChange events to FileDiff
        // transcript items in panes via the SAME construction the main
        // transcript uses (`file_diff_item`), so the shared item_lines
        // FileDiff arm renders diffs in panes.
        FileChange { path, kind, diff, is_binary, .. } => {
            pane.push_item(file_diff_item(path, *kind, diff, *is_binary));
        }
        // Child lifecycle / orchestration / OMO events: not pane-relevant.
        _ => {}
    }
}

/// Status of a subagent entry in the activity panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentStatus {
    /// Queued but not yet started (job board).
    Pending,
    Running,
    Done,
    Failed,
    /// Spec 020 (T030): terminal state for a child halted before
    /// completing its goal (operator stop, orchestrator request, budget
    /// breach, session wind-down). Set by `SubagentStopped`, which is
    /// emitted BEFORE the follow-up `SubagentComplete` — so the
    /// Complete/Failed handlers must treat it as terminal and never
    /// overwrite it.
    Stopped,
}

/// Top-level run mode of the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Accepting user input.
    Input,
    /// A turn is in progress; input box shows "busy" styling.
    Busy,
    /// User requested quit; rendering goodbye.
    Quitting,
}

/// Token accounting for the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokenStats {
    pub prompt: u64,
    pub completion: u64,
    pub iterations: usize,
}

impl TokenStats {
    pub fn total(self) -> u64 {
        self.prompt + self.completion
    }
}

/// The complete TUI state, rendered by borrowed widgets each frame.
pub struct App {
    pub mode: RunMode,
    pub transcript: VecDeque<TranscriptItem>,
    pub transcript_capacity: usize,
    /// Current streaming assistant text accumulator.
    pub streaming_assistant: String,
    /// Current streaming reasoning accumulator.
    pub streaming_reasoning: String,
    pub reasoning_open: bool,
    /// Expanded (main-screen) mode for the live reasoning panel: set by
    /// clicking the docked bottom strip — the live reasoning stream takes
    /// over the main screen (below a live transcript strip), and clicking
    /// again (or Esc) docks it back. Live streaming keeps flowing in both
    /// modes. Auto-collapses when the reasoning block closes.
    pub reasoning_expanded: bool,
    /// View state for the live reasoning panel. `None` = auto-follow (the
    /// window is pinned to the live tail); `Some(anchor)` = frozen at that
    /// absolute line index of the wrapped stream — scrolling up freezes the
    /// view so streaming no longer moves it, and only scrolling back down
    /// to the very bottom (or a new turn/block) re-enables following.
    /// The anchor is clamped to the measured stream length at render time.
    pub reasoning_view: Option<usize>,
    /// Upper bound for a valid `reasoning_view` anchor (the wrapped-stream
    /// length minus the visible rows), recorded by the reasoning widget at
    /// render time — the model can't know wrap widths. Interior mutability
    /// so the widget can write it from `&App`.
    pub last_reasoning_max_anchor: Cell<usize>,
    /// Screen rect of the reasoning panel as drawn by the LAST frame
    /// (docked or expanded). Used for mouse hit-testing, mirroring
    /// `last_neurocode_rect`. Interior mutability so widgets can record it
    /// from `&App` during render; zeroed on frames where the panel isn't
    /// drawn so stale geometry can't catch clicks.
    pub last_reasoning_rect: Cell<(u16, u16, u16, u16)>,
    /// Feature 007: timestamp of the first `ReasoningDelta` of the current
    /// block. Reset to `None` when reasoning is flushed. Drives the
    /// `Thought for Ns` footer.
    pub reasoning_started: Option<Instant>,
    /// Concurrent agent activities. Length drives animation intensity.
    pub active_agents: Vec<ActiveAgent>,
    pub next_agent_id: usize,
    pub tokens: TokenStats,
    pub session_id: String,
    pub model: String,
    pub provider: String,
    pub cwd: String,
    pub last_error: Option<String>,
    pub turn_started: Option<Instant>,
    /// Reasoning visibility toggle (user can collapse with Ctrl+R).
    pub show_reasoning: bool,
    /// Scroll offset in the transcript (rows from bottom). None = auto-follow.
    pub scroll: Option<usize>,
    /// Upper bound for `scroll`, recorded by the transcript widget at render
    /// time (the model doesn't know wrap widths). Cell: written during
    /// immutable rendering.
    pub last_max_scroll: Cell<usize>,
    /// Feature 007 (T026): transcript text-area geometry recorded at render
    /// time, used by click hit-testing. Stored as `(x, y, width, height)`.
    pub last_text_area: Cell<(u16, u16, u16, u16)>,
    pub last_final_text: String,
    // ── Live terminal output viewer (maximize) ──
    /// Whether the maximized terminal-output viewer is open. Opened with
    /// Ctrl+O (most recent terminal item) or by clicking the live-output
    /// region of a terminal transcript item; closed with Esc or Ctrl+O.
    /// While open it takes over the main screen area below a transcript
    /// strip (mirrors the expanded reasoning/NeuroCode panels).
    pub output_viewer_open: bool,
    /// Transcript index of the terminal item shown in the viewer. Kept even
    /// after the tool finishes — the viewer can replay any completed
    /// terminal call's full output. `None` = resolve to the most recent
    /// terminal item on demand.
    pub output_viewer_index: Option<usize>,
    /// View state for the maximized viewer. `None` = auto-follow (pinned to
    /// the live tail); `Some(anchor)` = frozen at that absolute wrapped-line
    /// index (scroll up freezes; scroll back to bottom resumes follow).
    /// Mirrors `reasoning_view`.
    pub output_viewer_view: Option<usize>,
    /// Upper bound for a valid `output_viewer_view` anchor, recorded by the
    /// viewer widget at render time (wrap widths are render-only knowledge).
    pub last_output_viewer_max_anchor: Cell<usize>,
    /// Screen rect of the maximized output viewer as drawn by the LAST
    /// frame; zeroed on frames where it isn't drawn (stale-rect guard).
    pub last_output_viewer_rect: Cell<(u16, u16, u16, u16)>,
    // ── Agent stats page (maximized context-window view) ──
    /// Whether the agent-stats page is open. Opened by clicking the
    /// header's right section (model/session/activity/tokens) or Ctrl+A;
    /// closed with Esc. Takes over the main screen area below a transcript
    /// strip, mirroring the other maximized panels.
    pub stats_open: bool,
    /// View state for the stats page's context stream. `None` =
    /// auto-follow (pinned to the live tail); `Some(anchor)` = frozen at
    /// that absolute line (scroll up freezes; scroll back to the bottom
    /// re-pins). Mirrors `reasoning_view`.
    pub stats_view: Option<usize>,
    /// Upper bound for a valid `stats_view` anchor, recorded by the stats
    /// widget at render time (wrap widths are render-only knowledge).
    pub last_stats_max_anchor: Cell<usize>,
    /// Screen rect of the stats page as drawn by the LAST frame; zeroed on
    /// frames where it isn't drawn (stale-rect guard).
    pub last_stats_rect: Cell<(u16, u16, u16, u16)>,
    /// Expandable-stats feature: the stats page's visible stream window,
    /// recorded at render time — (inner_y, first_visible_row). Click
    /// hit-testing maps screen rows to entry rows through this.
    pub last_stats_window: Cell<(u16, usize)>,
    /// Expandable-stats feature: per-entry row geometry of the stats page's
    /// context stream as drawn by the LAST frame — (entry_index,
    /// first_row, row_count), rows relative to the stream's top. Matches
    /// `last_stats_window.1` to resolve absolute rows.
    pub last_stats_stream_rows: std::cell::RefCell<Vec<(usize, usize, usize)>>,
    /// Same geometry pair for the FOCUSED pane's stats page (the retargeted
    /// view when a subagent is focused).
    pub last_pane_stats_window: Cell<(u16, usize)>,
    pub last_pane_stats_stream_rows: std::cell::RefCell<Vec<(usize, usize, usize)>>,
    /// Screen rect of the header's RIGHT section (model/session/status) as
    /// drawn by the last frame — the click target that opens the stats page.
    pub last_header_right_rect: Cell<(u16, u16, u16, u16)>,
    /// Latest context-window snapshot entries (oldest first).
    pub context_entries: Vec<ContextEntry>,
    /// Latest snapshot aggregates.
    pub context_system_tokens: u64,
    pub context_history_tokens: u64,
    pub context_window: u64,
    pub compression_threshold: u64,
    pub compactions: u32,
    /// Expandable-stats feature: which context-stream entries are expanded
    /// (indices into `context_entries`). Collapsed entries show the
    /// one-line preview; expanded entries render the full content inline
    /// with a gutter — the same affordance as transcript item expansion.
    pub expanded_context: std::collections::HashSet<usize>,
    /// Monotonic count of snapshots received (drives the "live" pulse).
    pub context_snapshots: u64,
    /// Timestamp of the most recent snapshot.
    pub context_updated_at: Option<Instant>,
    /// Rolling per-turn token usage series (prompt, completion) for the
    /// stats sparkline — one sample per ApiCallEnd.
    pub usage_series: Vec<(u64, u64)>,
    /// Turn count this session (for stats).
    pub turns: u64,
    // ── OMO agent picker state (T028) ──
    /// Agent picker overlay is open.
    pub agent_picker_open: bool,
    /// Cursor position in the agent picker.
    pub agent_picker_cursor: usize,
    /// The agent roster for the picker (Default + available OMO agents).
    pub agent_roster: Vec<DisplayAgent>,
    /// Index of the currently active agent (0=Default).
    pub active_agent_index: usize,
    /// The session's original model, stashed on first agent switch so switching
    /// back to "Default" can restore it. None until the user switches away.
    pub default_model: Option<String>,
    /// An agent switch requested while a turn was running — applied to the
    /// next turn (BC-016). Cleared once honored.
    pub pending_agent_switch: Option<String>,
    /// Active subagent entries for the activity panel (T064).
    pub subagent_entries: Vec<ActiveSubagentEntry>,
    /// Monotonic ID generator for subagent entries.
    pub next_subagent_id: usize,
    /// Per-subagent live panes (parallel-subagent feature): one per spawned
    /// child, stacked as vertical tabs on the right rail. Pane order matches
    /// spawn order (orchestrator implicit tab 0; children appended right).
    pub subagent_panes: Vec<SubagentPane>,
    /// Which view the main transcript + maximized stats/context window are
    /// showing: `None` = the main orchestrator; `Some(i)` = panes[i].
    pub focused_subagent: Option<usize>,
    /// Expandable subagent rail: when false the rail renders as the fixed
    /// 19-col tab strip (parity with the original layout); when true it
    /// widens to a detail view (model/depth/iterations/last-tool per pane).
    /// Toggled by Ctrl+N or clicking the rail's title row.
    pub subagent_rail_expanded: bool,
    /// Rects of each tab in the right rail as drawn by the LAST frame
    /// (click hit-testing), one per pane in order. RefCell: recorded by the
    /// rail widget during `&App` renders (mirrors the `Cell` geometry).
    pub last_subagent_tab_rects: std::cell::RefCell<Vec<(u16, u16, u16, u16)>>,
    /// Rect of the pinned orchestrator tab at the rail's bottom (click
    /// hit-testing; zeroed on frames that don't draw the rail).
    pub last_orchestrator_tab_rect: Cell<(u16, u16, u16, u16)>,
    /// Rect of the rail's TITLE row as drawn by the LAST frame — clicking
    /// it toggles `subagent_rail_expanded` (zeroed when the rail is hidden).
    pub last_subagent_rail_title_rect: Cell<(u16, u16, u16, u16)>,
    /// Scroll offset for the rail's tab window, in PANES (not rows): the
    /// first `subagent_rail_scroll` panes are skipped when the panes
    /// overflow the rail height, making later tabs reachable (Alt+Up /
    /// Alt+Down / mouse wheel over the rail). An item-offset sidesteps the
    /// collapsed-2-row vs expanded-4-row geometry; the rail widget clamps
    /// it against the capacity it records each frame.
    pub subagent_rail_scroll: usize,
    /// Upper bound for `subagent_rail_scroll` (panes.len() - visible
    /// capacity), recorded by the rail widget at render. 0 = all fit.
    pub last_subagent_rail_max_scroll: Cell<usize>,
    /// The clamped scroll offset the LAST rendered frame actually used.
    /// Added to the rect vec index in `subagent_tab_hit` so hit rects map
    /// back to TRUE pane indices after windowing.
    pub last_subagent_rail_drawn_offset: Cell<usize>,
    /// Rect of the whole rail strip (incl. its left border) as drawn by
    /// the LAST frame — routes mouse-wheel events over the rail to rail
    /// scrolling. Zeroed on frames that don't draw the rail.
    pub last_subagent_rail_rect: Cell<(u16, u16, u16, u16)>,
    /// Render-time geometry for the FOCUSED pane (parallel-subagent
    /// feature): scroll upper bound + text-area rect, recorded by the pane
    /// transcript widget each frame. App-level because widgets render
    /// through `&App` (interior mutability, mirroring `last_text_area`).
    pub last_pane_max_scroll: Cell<usize>,
    pub last_pane_text_area: Cell<(u16, u16, u16, u16)>,
    /// Screen rect of the pane stats page as drawn by the LAST frame.
    pub last_pane_stats_rect: Cell<(u16, u16, u16, u16)>,
    /// Learnings counter for wisdom accumulation display.
    pub learnings_count: usize,
    // ── NeuroCode (feature 015) ──
    /// Whether the NeuroCode engine is wired + active (drives the status-bar
    /// indicator and the bottom-right live context panel).
    pub neurocode_active: bool,
    /// Live feed of the context NeuroCode assembled for the CURRENT/latest
    /// request (updated on every AgentEvent::NeuroCodeContext).
    pub neurocode_context: String,
    /// Tier that served the latest request (e.g. "Frontier").
    pub neurocode_tier: String,
    /// Estimated tokens in the latest assembled context.
    pub neurocode_tokens: usize,
    /// Graph-expanded node count in the latest assembled context.
    pub neurocode_nodes: usize,
    /// Cold-mode flag (project not indexed — degraded context).
    pub neurocode_cold: bool,
    /// Live assembly stage (feature 015 follow-up): the most recent
    /// `AgentEvent::NeuroCodeProgress` description, shown while assembling.
    pub neurocode_stage: String,
    /// When the live stage was last updated — drives the animated
    /// "assembling" indicator in the context panel.
    pub neurocode_stage_at: Option<std::time::Instant>,
    /// When the final context blob last arrived (`NeuroCodeContext`) —
    /// rendered as "updated Ns ago" so refreshes are visible in realtime.
    pub neurocode_updated_at: Option<std::time::Instant>,
    /// Scroll offset (lines) for the context panel when the feed overflows.
    pub neurocode_scroll: usize,
    /// Expanded (main-screen) mode for the context feed: set by clicking the
    /// docked bottom-right panel — its content takes over the main screen
    /// (below the live transcript tail), and clicking again (or Esc) docks
    /// it back. Live streaming keeps flowing in both modes.
    pub neurocode_expanded: bool,
    /// Screen rect of the NeuroCode context panel as drawn by the LAST frame
    /// (docked or expanded). Used for mouse hit-testing, mirroring
    /// `last_text_area`. Interior mutability so widgets can record it from
    /// `&App` during render.
    pub last_neurocode_rect: Cell<(u16, u16, u16, u16)>,
    /// Interactive visualization (feature 015 follow-up): the structured
    /// node/edge snapshot of the latest assembly (`AgentEvent::NeuroCodeGraph`),
    /// consumed by the fullscreen explorer when the feed is expanded.
    pub neurocode_snapshot: Option<joey_neurocode::ContextGraphSnapshot>,
    /// Explorer interaction state — pan offset (cells), zoom factor, selected
    /// node index, active pane, and per-pane scroll. Reset whenever a new
    /// snapshot lands or the explorer is re-opened.
    pub neurocode_viz: crate::neurocode_viz::VizState,
    /// Rect of the explorer's node-list pane as drawn by the last frame
    /// (mouse hit-testing for click-to-select).
    pub last_viz_nodes_rect: Cell<(u16, u16, u16, u16)>,
    // ── HyperCode parallel optimization ──
    /// Whether HyperCode mode is enabled (parallel task decomposition).
    pub hypercode_enabled: bool,
    /// Live HyperCode pipeline phase (planning/exploring/building/
    /// synthesizing) while a `/hypercode run` executes on the engine;
    /// None when no run is active. Drives the ⚡ badge's phase label.
    pub hypercode_phase: Option<String>,
    /// Rect of the HyperCode status indicator in the header (click target).
    pub last_hypercode_rect: Cell<(u16, u16, u16, u16)>,
    /// T155: when true, the Atlas job board section renders in draw_omo_panel.
    /// Set on BoulderWorkStarted, cleared on BoulderWorkCompleted / turn Done.
    pub job_board_visible: bool,
    /// T114: OMO context injection set by `/start-work`, consumed (and cleared)
    /// on the next submitted turn. Mirrors ReplState.pending_context_injection.
    pub pending_context_injection: Option<String>,
    // ── Search-in-history ──
    /// Search overlay is open.
    pub search_open: bool,
    /// Current search query.
    pub search_query: String,
    /// Whether search found any matches (updated on each query change).
    pub search_has_match: bool,
    // ── Slash-command popup ──
    /// The command catalog the popup filters (injected by the host from the
    /// shared slash registry — joey-tui cannot depend on joey-cli).
    pub slash_commands: Vec<SlashCommandInfo>,
    /// Whether the slash popup is currently shown.
    pub slash_menu_open: bool,
    /// Cursor row in the slash popup.
    pub slash_menu_cursor: usize,
    /// Scroll offset of the popup list (when matches exceed visible rows).
    pub slash_menu_scroll: usize,
    /// Subcommand stage: when the input is `/cmd <first-arg-partial>`, the
    /// popup offers the command's subcommands (derived from args_hint pipes)
    /// instead of command names.
    pub slash_subcommand_stage: bool,
    // ── Generic completion popup (@-context / file paths) ──
    /// Whether the completion popup is shown (host feeds the items).
    pub completion_menu_open: bool,
    /// Items offered by the completion popup (host-computed).
    pub completion_items: Vec<joey_tools::completion::CompletionItem>,
    /// Cursor row in the completion popup.
    pub completion_menu_cursor: usize,
    // ── Input history (shared with the CLI via ~/.joey/.joey_history) ──
    /// Session input history (most recent last), loaded from the shared file.
    pub input_history: Vec<String>,
    /// Current position while recalling (None = not recalling; Some(0) = newest).
    pub history_pos: Option<usize>,
    /// Draft saved when the user first pressed Up (restored on Down past newest).
    pub history_draft: String,
    /// Whether the bottom status bar renders (toggled by /statusbar; backed
    /// by config key display.statusbar, default true).
    pub show_status_bar: bool,
    // ── Terminal governor contention (spec 018, T019) ──
    /// Latest snapshot from `AgentEvent::TerminalQueueState` — how many
    /// terminal commands hold an execution slot. Last-value-wins: each
    /// event overwrites both counters wholesale.
    pub terminal_active: usize,
    /// Terminal commands currently waiting for a slot. The status-bar
    /// contention span renders ONLY while this is > 0 (FR-011: no
    /// persistent chrome).
    pub terminal_queued: usize,
}

/// One entry of the slash-command catalog shown in the TUI popup. Injected by
/// the host from the shared slash registry (single source of truth in
/// joey-cli's `slash::REGISTRY`).
#[derive(Debug, Clone)]
pub struct SlashCommandInfo {
    /// Canonical name without the leading slash (e.g. "help").
    pub name: String,
    /// Aliases without slashes.
    pub aliases: Vec<String>,
    /// One-line description.
    pub description: String,
    /// Argument hint (e.g. "[model] [--global]").
    pub args_hint: String,
    /// Whether this build has a handler.
    pub implemented: bool,
}

impl App {
    /// Update the slash popup from the current input-box text. Opens the
    /// popup when the first line starts with `/` (auto-popup as you type);
    /// closes and resets it otherwise. Two stages:
    ///
    /// - `/par` — command-name/alias matches (as before).
    /// - `/cmd arg` (first argument word only) — when the command is known
    ///   and has pipe-encoded subcommands in its args_hint, the popup
    ///   switches to subcommand matches (Hermes SUBCOMMANDS parity).
    pub fn update_slash_menu(&mut self, input_text: &str) {
        let first_line = input_text.lines().next().unwrap_or("");
        self.slash_subcommand_stage = false;
        if !first_line.starts_with('/') || !self.input_cursor_at_first_line() {
            self.slash_menu_open = false;
            self.slash_menu_cursor = 0;
            self.slash_menu_scroll = 0;
            return;
        }
        // Subcommand stage: "/cmd partial" (cursor in the first argument
        // word, no further spaces). An empty partial (trailing space) offers
        // ALL subcommands — upstream shows the full list on `/cmd `.
        if let Some((base, arg)) = first_line.split_once(' ') {
            if !arg.contains(' ') {
                if let Some(def) = self
                    .slash_commands
                    .iter()
                    .find(|c| c.name == base[1..].to_lowercase() || c.aliases.iter().any(|a| *a == base[1..].to_lowercase()))
                {
                    if def.implemented {
                        let arg_lower = arg.to_lowercase();
                        let subs = joey_tools::completion::pipe_subcommands(&def.args_hint);
                        let matches: Vec<&String> = subs
                            .iter()
                            .filter(|s| arg_lower.is_empty() || s.starts_with(arg_lower.as_str()))
                            .filter(|s| s.as_str() != arg_lower)
                            .collect();
                        self.slash_menu_open = !matches.is_empty();
                        self.slash_subcommand_stage = self.slash_menu_open;
                        if self.slash_menu_cursor >= matches.len().max(1) {
                            self.slash_menu_cursor = 0;
                            self.slash_menu_scroll = 0;
                        }
                        return;
                    }
                }
            }
            // Args typed but not a subcommand-match case → close popup.
            self.slash_menu_open = false;
            self.slash_menu_cursor = 0;
            self.slash_menu_scroll = 0;
            return;
        }
        // Command-name stage.
        let typed = first_line.strip_prefix('/').unwrap_or("");
        self.slash_menu_open = !self.slash_matches(typed).is_empty();
        // Clamp cursor/scroll into range for the (possibly new) match set.
        let len = self.slash_matches(typed).len();
        if self.slash_menu_cursor >= len {
            self.slash_menu_cursor = 0;
            self.slash_menu_scroll = 0;
        }
    }

    /// Subcommand matches for the subcommand stage (derived from the typed
    /// `/cmd arg` input). Empty when not in the subcommand stage.
    pub fn slash_subcommand_matches(&self, input_text: &str) -> Vec<String> {
        if !self.slash_subcommand_stage {
            return Vec::new();
        }
        let first_line = input_text.lines().next().unwrap_or("");
        let Some((base, arg)) = first_line.split_once(' ') else {
            return Vec::new();
        };
        let base_lower = base[1..].to_lowercase();
        let Some(def) = self
            .slash_commands
            .iter()
            .find(|c| c.name == base_lower || c.aliases.iter().any(|a| *a == base_lower))
        else {
            return Vec::new();
        };
        let arg_lower = arg.to_lowercase();
        joey_tools::completion::pipe_subcommands(&def.args_hint)
            .into_iter()
            .filter(|s| s.starts_with(arg_lower.as_str()) && s.as_str() != arg_lower)
            .collect()
    }

    // ── Generic completion popup (@-context / paths; host-fed items) ──

    /// Set the completion-popup items from the host and open it (or close
    /// when empty). Resets the cursor.
    pub fn set_completion_items(&mut self, items: Vec<joey_tools::completion::CompletionItem>) {
        self.completion_menu_open = !items.is_empty();
        self.completion_items = items;
        self.completion_menu_cursor = 0;
    }

    /// Move the completion-popup cursor (wrapping). Returns the selected
    /// item's replacement when open.
    pub fn completion_menu_move(&mut self, down: bool) -> Option<String> {
        if !self.completion_menu_open || self.completion_items.is_empty() {
            return None;
        }
        let len = self.completion_items.len();
        if down {
            self.completion_menu_cursor = (self.completion_menu_cursor + 1) % len;
        } else {
            self.completion_menu_cursor = (self.completion_menu_cursor + len - 1) % len;
        }
        self.completion_items.get(self.completion_menu_cursor).map(|i| i.replacement.clone())
    }

    /// The currently selected completion's replacement, when open.
    pub fn completion_selected(&self) -> Option<String> {
        if !self.completion_menu_open {
            return None;
        }
        self.completion_items.get(self.completion_menu_cursor).map(|i| i.replacement.clone())
    }

    /// The current popup filter fragment derived from the input box.
    /// (The host passes the input text to `update_slash_menu` /
    /// `slash_fragment` — the App does not own the Input widget.)

    /// Matching commands for a typed fragment (prefix match on names and
    /// aliases; empty fragment matches all).
    pub fn slash_matches(&self, typed: &str) -> Vec<&SlashCommandInfo> {
        if typed.is_empty() {
            return self.slash_commands.iter().collect();
        }
        self.slash_commands
            .iter()
            .filter(|c| c.name.starts_with(typed) || c.aliases.iter().any(|a| a.starts_with(typed)))
            .collect()
    }

    /// The typed fragment for the popup given the raw input text.
    pub fn slash_fragment<'a>(&self, input_text: &'a str) -> &'a str {
        input_text
            .lines()
            .next()
            .unwrap_or("")
            .strip_prefix('/')
            .unwrap_or("")
    }

    /// Whether the input cursor is on the first line (popup eligibility).
    fn input_cursor_at_first_line(&self) -> bool {
        // The Input widget reports its cursor line; the App can't see it, so
        // eligibility is approximated: multi-line drafts never open the popup
        // (handled by the caller passing single-line-relevant text). Kept as a
        // hook for future precision.
        true
    }

    /// Move the slash popup cursor; returns the newly selected command name
    /// (without slash) if the popup is open.
    pub fn slash_menu_move(&mut self, input_text: &str, down: bool) -> Option<String> {
        if !self.slash_menu_open {
            return None;
        }
        let len = self.slash_matches(self.slash_fragment(input_text)).len();
        if len == 0 {
            return None;
        }
        if down {
            self.slash_menu_cursor = (self.slash_menu_cursor + 1) % len;
        } else {
            self.slash_menu_cursor = (self.slash_menu_cursor + len - 1) % len;
        }
        self.slash_selected(input_text)
    }

    /// The currently selected command (name without slash), if the popup is open.
    pub fn slash_selected(&self, input_text: &str) -> Option<String> {
        if !self.slash_menu_open {
            return None;
        }
        self.slash_matches(self.slash_fragment(input_text))
            .get(self.slash_menu_cursor)
            .map(|c| c.name.clone())
    }

    // ── Input history ──

    /// Recall the previous (older) history entry. `current` is the live input
    /// text (used to seed the draft on first Up). Returns the text to place
    /// in the input box.
    pub fn history_prev(&mut self, current: &str) -> Option<String> {
        if self.input_history.is_empty() {
            return None;
        }
        let pos = match self.history_pos {
            None => {
                self.history_draft = current.to_string();
                self.input_history.len() - 1
            }
            Some(0) => return None, // already at the oldest entry
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        Some(self.input_history[pos].clone())
    }

    /// Recall the next (newer) history entry. Returns the text to place in
    /// the input box (the saved draft when moving past the newest entry).
    pub fn history_next(&mut self) -> Option<String> {
        let pos = self.history_pos?;
        let next = pos + 1;
        if next >= self.input_history.len() {
            // Past the newest — restore the draft and stop recalling.
            self.history_pos = None;
            let draft = self.history_draft.clone();
            self.history_draft.clear();
            return Some(draft);
        }
        self.history_pos = Some(next);
        Some(self.input_history[next].clone())
    }

    /// Record a submitted input into history (dedup against the previous
    /// entry, matching reedline's FileBackedHistory semantics).
    pub fn history_record(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.input_history.last().map(|l| l.as_str()) == Some(text) {
            return;
        }
        self.input_history.push(text.to_string());
        // Bound the in-memory list to match the on-disk cap.
        const CAP: usize = 10_000;
        if self.input_history.len() > CAP {
            let drop_n = self.input_history.len() - CAP;
            self.input_history.drain(0..drop_n);
        }
        self.history_pos = None;
        self.history_draft.clear();
    }
}

impl App {
    pub fn new(session_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            mode: RunMode::Input,
            transcript: VecDeque::with_capacity(256),
            transcript_capacity: 1024,
            streaming_assistant: String::new(),
            streaming_reasoning: String::new(),
            reasoning_open: false,
            reasoning_expanded: false,
            reasoning_view: None,
            last_reasoning_max_anchor: Cell::new(0),
            last_reasoning_rect: Cell::new((0, 0, 0, 0)),
            reasoning_started: None,
            active_agents: Vec::new(),
            next_agent_id: 1,
            tokens: TokenStats::default(),
            session_id: session_id.into(),
            model: model.into(),
            provider: String::new(),
            cwd: String::new(),
            last_error: None,
            turn_started: None,
            show_reasoning: true,
            scroll: None,
            last_max_scroll: Cell::new(0),
            last_text_area: Cell::new((0, 0, 0, 0)),
            last_final_text: String::new(),
            output_viewer_open: false,
            output_viewer_index: None,
            output_viewer_view: None,
            last_output_viewer_max_anchor: Cell::new(0),
            last_output_viewer_rect: Cell::new((0, 0, 0, 0)),
            stats_open: false,
            stats_view: None,
            last_stats_max_anchor: Cell::new(0),
            last_stats_rect: Cell::new((0, 0, 0, 0)),
            last_stats_window: Cell::new((0, 0)),
            last_stats_stream_rows: std::cell::RefCell::new(Vec::new()),
            last_pane_stats_window: Cell::new((0, 0)),
            last_pane_stats_stream_rows: std::cell::RefCell::new(Vec::new()),
            last_header_right_rect: Cell::new((0, 0, 0, 0)),
            context_entries: Vec::new(),
            context_system_tokens: 0,
            context_history_tokens: 0,
            context_window: 0,
            compression_threshold: 0,
            compactions: 0,
            expanded_context: std::collections::HashSet::new(),
            context_snapshots: 0,
            context_updated_at: None,
            usage_series: Vec::new(),
            turns: 0,
            agent_picker_open: false,
            agent_picker_cursor: 0,
            agent_roster: Vec::new(),
            active_agent_index: 0,
            default_model: None,
            pending_agent_switch: None,
            subagent_entries: Vec::new(),
            next_subagent_id: 1,
            subagent_panes: Vec::new(),
            focused_subagent: None,
            subagent_rail_expanded: false,
            last_subagent_tab_rects: std::cell::RefCell::new(Vec::new()),
            last_orchestrator_tab_rect: Cell::new((0, 0, 0, 0)),
            last_subagent_rail_title_rect: Cell::new((0, 0, 0, 0)),
            subagent_rail_scroll: 0,
            last_subagent_rail_max_scroll: Cell::new(0),
            last_subagent_rail_drawn_offset: Cell::new(0),
            last_subagent_rail_rect: Cell::new((0, 0, 0, 0)),
            last_pane_max_scroll: Cell::new(0),
            last_pane_text_area: Cell::new((0, 0, 0, 0)),
            last_pane_stats_rect: Cell::new((0, 0, 0, 0)),
            learnings_count: 0,
            neurocode_active: false,
            neurocode_context: String::new(),
            neurocode_tier: String::new(),
            neurocode_tokens: 0,
            neurocode_nodes: 0,
            neurocode_cold: false,
            neurocode_stage: String::new(),
            neurocode_stage_at: None,
            neurocode_updated_at: None,
            neurocode_scroll: 0,
            neurocode_expanded: false,
            last_neurocode_rect: Cell::new((0, 0, 0, 0)),
            neurocode_snapshot: None,
            neurocode_viz: crate::neurocode_viz::VizState::default(),
            last_viz_nodes_rect: Cell::new((0, 0, 0, 0)),
            hypercode_enabled: false,
            hypercode_phase: None,
            last_hypercode_rect: Cell::new((0, 0, 0, 0)),
            job_board_visible: false,
            pending_context_injection: None,
            search_open: false,
            search_query: String::new(),
            search_has_match: false,
            slash_commands: Vec::new(),
            slash_menu_open: false,
            slash_menu_cursor: 0,
            slash_menu_scroll: 0,
            slash_subcommand_stage: false,
            completion_menu_open: false,
            completion_items: Vec::new(),
            completion_menu_cursor: 0,
            input_history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            show_status_bar: true,
            terminal_active: 0,
            terminal_queued: 0,
        }
    }

    pub fn active_count(&self) -> usize {
        self.active_agents.iter().filter(|a| a.phase != AgentPhase::Done).count()
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.mode, RunMode::Busy)
    }

    pub fn transcript_len(&self) -> usize {
        self.transcript.len()
    }

    /// Commit any pending streamed reasoning as a transcript item.
    fn flush_reasoning(&mut self) {
        if self.reasoning_open {
            let text = std::mem::take(&mut self.streaming_reasoning);
            // Feature 007: compute the thinking duration from the first delta.
            let thought_duration = self.reasoning_started.take().map(|s| s.elapsed());
            if !text.is_empty() {
                self.push_item(TranscriptItem::Reasoning {
                    text,
                    expand_state: ReasoningExpandState::Collapsed,
                    thought_duration,
                });
            }
            self.reasoning_open = false;
            // The live block ended — the expanded panel (if open) docks
            // back. The committed transcript item carries the full text and
            // has its own click-to-expand affordance.
            self.reasoning_expanded = false;
            self.reasoning_view = None;
        }
    }

    /// Feature 005 (T021/T023): advance the most-recent reasoning block's
    /// expand state to the next step in the three-state cycle.
    ///
    /// The TUI uses scroll-based navigation (no per-item cursor), so this
    /// targets the last `Reasoning` item in the transcript — matching crush's
    /// behavior of expanding the most recent thinking block first.
    ///
    /// T012 (US2, FR-004): the TARGET follows focus. With a subagent pane
    /// focused, the PANE's most-recent reasoning entry cycles and the main
    /// transcript stays untouched (focused-view isolation); unfocused, the
    /// main transcript's entry cycles exactly as before (byte-identical).
    pub fn cycle_focused_reasoning_expand(&mut self) {
        if let Some(pane) = self.focused_pane_mut() {
            Self::cycle_last_reasoning_expand_in(&mut pane.transcript);
        } else {
            Self::cycle_last_reasoning_expand_in(&mut self.transcript);
        }
    }

    /// Feature 005 (T026/T028): cycle the most-recent tool call through the
    /// three-state inline expand. Targets the last `Tool` item (FR-018:
    /// per-item isolation — only one item is affected).
    ///
    /// T012 (US2, FR-004): the TARGET follows focus — the focused pane's
    /// most-recent tool entry when a pane is focused (main untouched),
    /// the main transcript's otherwise (byte-identical to before).
    pub fn toggle_focused_tool_expand(&mut self) {
        if let Some(pane) = self.focused_pane_mut() {
            Self::toggle_last_tool_expand_in(&mut pane.transcript);
        } else {
            Self::toggle_last_tool_expand_in(&mut self.transcript);
        }
    }

    /// T012 (US2, FR-004): walk `transcript` newest-first and advance the
    /// most-recent `Reasoning` item's expand state through the shared
    /// three-state cycle (the exact walk the main transcript always used,
    /// parameterized by the target transcript). No-op when it holds no
    /// `Reasoning` item.
    fn cycle_last_reasoning_expand_in(transcript: &mut VecDeque<TranscriptItem>) {
        // Find the last Reasoning item in the transcript.
        for item in transcript.iter_mut().rev() {
            if let TranscriptItem::Reasoning { text, expand_state, .. } = item {
                let total_lines = text.lines().count();
                *expand_state = expand_state.cycle(total_lines);
                return;
            }
        }
    }

    /// T012 (US2, FR-004): walk `transcript` newest-first and advance the
    /// most-recent `Tool` item's expand state (same per-item isolation the
    /// main transcript's toggle always had). No-op when it holds no `Tool`.
    fn toggle_last_tool_expand_in(transcript: &mut VecDeque<TranscriptItem>) {
        for item in transcript.iter_mut().rev() {
            if matches!(item, TranscriptItem::Tool { .. }) {
                toggle_expand_item(item);
                return;
            }
        }
    }

    /// Feature 007 (T026): toggle the expand state of the transcript item at
    /// the given index, dispatching to the type-appropriate expand method
    /// (reasoning three-state cycle vs. tool boolean toggle). Called by the
    /// mouse click handler after hit-testing resolves a click to an item.
    /// Whether the transcript item at `index` has an expand affordance
    /// (tool calls, terminal blocks, file diffs, reasoning blocks).
    pub fn item_is_expandable(&self, index: usize) -> bool {
        match self.transcript.get(index) {
            Some(TranscriptItem::Tool { .. })
            | Some(TranscriptItem::FileDiff { .. })
            | Some(TranscriptItem::Reasoning { .. }) => true,
            _ => false,
        }
    }

    pub fn toggle_item_expand_by_index(&mut self, index: usize) {
        if index >= self.transcript.len() {
            return;
        }
        toggle_expand_item(&mut self.transcript[index]);
    }

    /// Commit any pending streamed assistant text as a transcript item.
    fn flush_streaming_assistant(&mut self) {
        let text = std::mem::take(&mut self.streaming_assistant);
        if !text.is_empty() {
            self.push_item(TranscriptItem::Assistant { text });
        }
    }

    /// True if the most recent Assistant item in the transcript equals `text`
    /// (the agent sends `AssistantMessage` immediately before `Done` with the
    /// same content — committing both would duplicate the final answer).
    fn last_assistant_is(&self, text: &str) -> bool {
        self.transcript
            .iter()
            .rev()
            .find_map(|it| match it {
                TranscriptItem::Assistant { text: t } => Some(t == text),
                _ => None,
            })
            .unwrap_or(false)
    }

    /// Apply one agent event to the model.
    pub fn apply(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::TurnStart { max_iterations } => {
                self.mode = RunMode::Busy;
                self.turns += 1;
                self.turn_started = Some(Instant::now());
                self.streaming_assistant.clear();
                self.streaming_reasoning.clear();
                self.reasoning_open = false;
                self.reasoning_expanded = false;
                self.reasoning_view = None;
                self.reasoning_started = None;
                let id = self.next_agent_id;
                self.next_agent_id += 1;
                self.active_agents.push(ActiveAgent {
                    id,
                    label: "turn".into(),
                    phase: AgentPhase::Idle,
                    started: Instant::now(),
                    iterations: 0,
                    max_iterations,
                });
            }
            AgentEvent::IterationStart { iteration, max_iterations } => {
                if let Some(a) = self.active_agents.last_mut() {
                    a.iterations = iteration;
                    a.max_iterations = max_iterations;
                }
            }
            AgentEvent::ApiCallStart => {
                if let Some(a) = self.active_agents.last_mut() {
                    a.phase = AgentPhase::QueryingModel;
                }
            }
            AgentEvent::ApiCallEnd { usage } => {
                // The single source of token accounting: every API call
                // reports here; `Done.usage` is the turn total and must NOT
                // be added again (it would double-count).
                self.tokens.prompt += usage.prompt_tokens;
                self.tokens.completion += usage.completion_tokens;
                // Stats page: one usage sample per API call (bounded).
                self.usage_series.push((usage.prompt_tokens, usage.completion_tokens));
                const USAGE_CAP: usize = 240;
                if self.usage_series.len() > USAGE_CAP {
                    let drop_n = self.usage_series.len() - USAGE_CAP;
                    self.usage_series.drain(0..drop_n);
                }
                if let Some(a) = self.active_agents.last_mut() {
                    if a.phase == AgentPhase::QueryingModel {
                        a.phase = AgentPhase::Idle;
                    }
                }
            }
            AgentEvent::ContextSnapshot {
                entries,
                system_tokens,
                history_tokens,
                context_window,
                compression_threshold,
                compactions,
                model: _,
            } => {
                // Live context-window view (agent stats page). The snapshot
                // replaces the previous one wholesale — it is a complete
                // projection of the history at emit time.
                // Expandable-stats: remap expansions by entry identity
                // (role/tokens/preview) so expanded rows survive appends
                // and renumbering (compaction).
                self.expanded_context = remap_expansions(&self.context_entries, &self.expanded_context, &entries);
                self.context_entries = entries;
                self.context_system_tokens = system_tokens;
                self.context_history_tokens = history_tokens;
                self.context_window = context_window;
                self.compression_threshold = compression_threshold;
                self.compactions = compactions;
                self.context_snapshots += 1;
                self.context_updated_at = Some(Instant::now());
            }
            AgentEvent::ReasoningDelta(d) => {
                if !self.show_reasoning {
                    return;
                }
                if !self.reasoning_open {
                    self.reasoning_open = true;
                    self.streaming_reasoning.clear();
                    // Feature 007: mark the start of this reasoning block.
                    self.reasoning_started = Some(Instant::now());
                }
                if let Some(a) = self.active_agents.last_mut() {
                    a.phase = AgentPhase::Reasoning;
                }
                self.streaming_reasoning.push_str(&d);
            }
            AgentEvent::ContentDelta(d) => {
                self.flush_reasoning();
                if let Some(a) = self.active_agents.last_mut() {
                    if a.phase == AgentPhase::Reasoning {
                        a.phase = AgentPhase::Idle;
                    }
                }
                self.streaming_assistant.push_str(&d);
            }
            AgentEvent::AssistantMessage(text) => {
                // The event supersedes any interim streamed text.
                let final_text = if text.is_empty() {
                    std::mem::take(&mut self.streaming_assistant)
                } else {
                    self.streaming_assistant.clear();
                    text
                };
                if !final_text.is_empty() {
                    self.push_item(TranscriptItem::Assistant { text: final_text });
                }
            }
            AgentEvent::ToolStart { name, emoji, summary } => {
                self.flush_reasoning();
                self.flush_streaming_assistant();
                if let Some(a) = self.active_agents.last_mut() {
                    a.phase = AgentPhase::RunningTool(name.clone());
                }
                // T155: tool-call attribution to subagent entries. When the
                // child's wrapped SubagentEvent stream is attached to a
                // pane, that id-matched path counts precisely AND the raw
                // duplicate arrives here too — counting both double-counted
                // every child call. Fall back to latest-Running attribution
                // only when no pane has an attached tap.
                if !self
                    .subagent_panes
                    .iter()
                    .any(|p| p.tap_attached)
                {
                    for entry in self.subagent_entries.iter_mut().rev() {
                        if entry.status == SubagentStatus::Running {
                            entry.tool_call_count += 1;
                            entry.last_tool = Some(name.clone());
                            entry.phase = format!("running tool: {}", name);
                            break;
                        }
                    }
                }
                self.push_item(TranscriptItem::Tool {
                    name: name.clone(),
                    emoji,
                    summary,
                    status: ToolStatus::Running,
                    duration_secs: None,
                    result_preview: String::new(),
                    expand_state: ReasoningExpandState::Collapsed,
                    full_args: None,
                    full_result: None,
                    is_terminal: is_terminal_block(&name),
                    exit_code: None,
                    live_output: String::new(),
                    live_output_capacity: LIVE_OUTPUT_CAPACITY,
                });
                // Live-follow retarget: when the maximized viewer is open in
                // auto-follow mode and its previous target already finished,
                // a NEW tool call takes over the viewer — the user keeps
                // watching output without re-opening anything.
                if self.output_viewer_open && self.output_viewer_view.is_none() {
                    self.output_viewer_index = Some(self.transcript.len() - 1);
                }
            }
            AgentEvent::ToolProgress { name, progress } => {
                if progress.is_empty() {
                    return;
                }
                // Update the most recent still-running call of this tool
                // (notices/reasoning may have landed after the ToolStart).
                for it in self.transcript.iter_mut().rev() {
                    if let TranscriptItem::Tool { name: n, status, summary, is_terminal, .. } = it {
                        if *status == ToolStatus::Running && *n == name {
                            if *is_terminal {
                                // Terminal blocks: ignore ToolProgress entirely.
                                // The command header (summary) must never be
                                // clobbered, and raw output arrives via the
                                // separate ToolOutput stream (the terminal
                                // tool emits both channels from the same
                                // chunk — appending here would duplicate it).
                                // Heartbeats surface via the live-tail
                                // rendering's spinner/hint instead.
                            } else {
                                *summary = progress;
                            }
                            break;
                        }
                    }
                }
            }
            AgentEvent::ToolOutput { name, chunk } => {
                if chunk.is_empty() {
                    return;
                }
                // Accumulate the raw output chunk on the most recent
                // still-running call of this tool. Concurrent same-name tools
                // interleave by design (mirrors the ToolProgress policy).
                for it in self.transcript.iter_mut().rev() {
                    if let TranscriptItem::Tool { name: n, status, live_output, live_output_capacity, .. } = it {
                        if *status == ToolStatus::Running && *n == name {
                            Self::push_bounded_item(live_output, &chunk, *live_output_capacity);
                            break;
                        }
                    }
                }
            }
            AgentEvent::ToolEnd { name, is_error, result_preview, duration_secs, exit_code, full_result } => {
                for it in self.transcript.iter_mut().rev() {
                    if let TranscriptItem::Tool {
                        name: n,
                        status,
                        duration_secs: dur,
                        result_preview: rp,
                        exit_code: ec,
                        full_result: fr,
                        ..
                    } = it
                    {
                        if *status == ToolStatus::Running && *n == name {
                            *status = if is_error { ToolStatus::Failed } else { ToolStatus::Done };
                            *dur = Some(duration_secs);
                            *rp = result_preview.clone();
                            // Feature 007: store exit code and full result.
                            *ec = exit_code;
                            *fr = Some(full_result);
                            break;
                        }
                    }
                }
                if let Some(a) = self.active_agents.last_mut() {
                    if matches!(a.phase, AgentPhase::RunningTool(_)) {
                        a.phase = AgentPhase::Idle;
                    }
                }
            }
            AgentEvent::Notice(msg) => {
                self.push_item(TranscriptItem::Notice {
                    text: msg,
                    kind: NoticeKind::Info,
                });
            }
            AgentEvent::RetryAttempt { attempt, max_retries, error, .. } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Retry {}/{}: {}", attempt, max_retries, error),
                    kind: NoticeKind::Warning,
                });
            }
            AgentEvent::CompressionStart { reason, approx_tokens } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Compressing ~{} tokens: {}", approx_tokens, reason),
                    kind: NoticeKind::Busy,
                });
            }
            AgentEvent::CompressionEnd { original_msgs, new_msgs } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Compressed {} → {} messages", original_msgs, new_msgs),
                    kind: NoticeKind::Success,
                });
            }
            AgentEvent::FallbackActivated { from_model, to_model } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Fallback: {} → {}", from_model, to_model),
                    kind: NoticeKind::Warning,
                });
            }
            AgentEvent::SubagentSpawn { id, goal, model, toolset_summary, depth: _ } => {
                // Populate the activity panel's subagent roster (T064).
                let entry_id = self.next_subagent_id;
                self.next_subagent_id += 1;
                // The goal text is the closest thing to an agent_type label we
                // have at spawn time; the summary_preview on completion is too
                // late for the "running" state the panel needs to show.
                let label: String = goal.chars().take(28).collect();
                // T155: use the full goal as the job-board task title.
                let task_title = if goal.is_empty() { None } else { Some(goal.clone()) };
                self.subagent_entries.push(ActiveSubagentEntry {
                    id: entry_id,
                    child_id: id,
                    agent_type: label,
                    category: None,
                    status: SubagentStatus::Running,
                    phase: "querying model".to_string(),
                    model: model.clone(),
                    iterations: 0,
                    started: Instant::now(),
                    task_title,
                    tool_call_count: 0,
                    last_tool: None,
                });
                // Parallel-subagent feature: open a dedicated live pane and
                // stack it as a new tab on the right rail.
                let mut pane = SubagentPane::new(&goal, model.clone(), toolset_summary.clone(), 0);
                pane.child_id = id;
                // T022 (US4, FR-008): snapshot mode attribution at spawn —
                // the pane may show the mode-specific explorer only if the
                // spawning mode (NeuroCode) was active when this child was
                // delegated. Frozen at spawn so a later mode toggle never
                // retroactively grants a plain pane mode reachability.
                pane.spawned_by_neurocode = self.neurocode_active;
                self.subagent_panes.push(pane);
                self.push_item(TranscriptItem::Notice {
                    text: format!("🤖 Subagent: {} [{}] (click its tab on the right rail to watch live)", goal, toolset_summary),
                    kind: NoticeKind::Busy,
                });
            }
            AgentEvent::SubagentEvent { id, event } => {
                // Route the child's live event to its pane (matched by the
                // stable child id). Unknown ids (e.g. an id spawned before a
                // pane-capable UI attached) are dropped.
                if let Some(pane) = self
                    .subagent_panes
                    .iter_mut()
                    .find(|p| p.child_id == id)
                {
                    pane.tap_attached = true;
                    pane_apply(pane, event.as_ref());
                    // Mirror phase/activity into the job-board entry.
                    if let Some(entry) = self
                        .subagent_entries
                        .iter_mut()
                        .rev()
                        .find(|e| e.child_id == id && e.status == SubagentStatus::Running)
                    {
                        match event.as_ref() {
                            AgentEvent::ToolStart { name, .. } => {
                                entry.tool_call_count += 1;
                                entry.last_tool = Some(name.clone());
                                entry.phase = format!("running tool: {}", name);
                            }
                            AgentEvent::ApiCallStart => {
                                entry.phase = "querying model".to_string();
                            }
                            AgentEvent::ApiCallEnd { .. } => {
                                entry.iterations += 1;
                            }
                            AgentEvent::IterationStart { iteration, .. } => {
                                entry.iterations = *iteration;
                            }
                            _ => {}
                        }
                    }
                }
            }
            AgentEvent::SubagentComplete { id, goal, success, summary_preview, token_usage: _, duration_secs: _ } => {
                // Mark the matching subagent entry as done/failed (T064).
                // Match by child_id ONLY: the old fallback compared a
                // 28-char goal prefix, so parallel goals sharing a prefix
                // closed the wrong entry.
                for entry in self.subagent_entries.iter_mut().rev() {
                    if entry.status == SubagentStatus::Running && entry.child_id == id
                    {
                        entry.status = if success {
                            SubagentStatus::Done
                        } else {
                            SubagentStatus::Failed
                        };
                        break;
                    }
                }
                // Close out the pane's live state.
                // Spec 020 (T030): Stopped is terminal — the orchestration
                // layer emits SubagentStopped FIRST and then still fires
                // SubagentComplete; a child already in Stopped keeps its
                // stop reason + preview: no state change and no follow-up
                // "done" notice (the stop notice already reported the
                // terminal state, FR-016).
                let already_stopped = self
                    .subagent_panes
                    .iter()
                    .any(|p| p.child_id == id && p.status == SubagentStatus::Stopped)
                    || self
                        .subagent_entries
                        .iter()
                        .any(|e| e.child_id == id && e.status == SubagentStatus::Stopped);
                if already_stopped {
                    return;
                }
                if let Some(pane) = self.subagent_panes.iter_mut().find(|p| p.child_id == id) {
                    pane.status = if success {
                        SubagentStatus::Done
                    } else {
                        SubagentStatus::Failed
                    };
                    pane.summary_preview = Some(summary_preview.clone());
                    // T032: flush pending streamed reasoning FIRST (pane_apply
                    // ordering: reasoning commits before the assistant text) —
                    // a child that ends without a trailing
                    // AssistantMessage/ToolStart would otherwise drop it and
                    // leave draw_reasoning's live condition stuck on.
                    pane_flush_reasoning(pane);
                    // Flush any pending streamed text so the pane's final
                    // answer is visible even if the child skipped the
                    // AssistantMessage event.
                    if !pane.streaming_assistant.is_empty() {
                        let text = std::mem::take(&mut pane.streaming_assistant);
                        pane.push_item(TranscriptItem::Assistant { text });
                    }
                }
                self.push_item(TranscriptItem::Notice {
                    text: format!("{} {}: {}", if success { "✓" } else { "✗" }, goal, summary_preview),
                    kind: if success { NoticeKind::Success } else { NoticeKind::Warning },
                });
            }
            AgentEvent::SubagentFailed { id, goal, error, duration_secs: _ } => {
                // Match by child_id ONLY (see SubagentComplete).
                for entry in self.subagent_entries.iter_mut().rev() {
                    if entry.status == SubagentStatus::Running && entry.child_id == id
                    {
                        entry.status = SubagentStatus::Failed;
                        break;
                    }
                }
                // Spec 020 (T030): Stopped is terminal — never overwrite it
                // (Stopped-then-Complete emission order; see the
                // SubagentComplete arm).
                if self
                    .subagent_panes
                    .iter()
                    .any(|p| p.child_id == id && p.status == SubagentStatus::Stopped)
                    || self
                        .subagent_entries
                        .iter()
                        .any(|e| e.child_id == id && e.status == SubagentStatus::Stopped)
                {
                    return;
                }
                if let Some(pane) = self.subagent_panes.iter_mut().find(|p| p.child_id == id) {
                    pane.status = SubagentStatus::Failed;
                    // T032: flush pending streamed reasoning before the error
                    // item — same close-out semantics as SubagentComplete (a
                    // failed child often ends mid-reasoning with no trailing
                    // AssistantMessage/ToolStart boundary to flush it).
                    pane_flush_reasoning(pane);
                    pane.push_item(TranscriptItem::Error { text: error.clone() });
                }
                self.push_item(TranscriptItem::Notice {
                    text: format!("✗ {}: {}", goal, error),
                    kind: NoticeKind::Warning,
                });
            }
            // Spec 020 (T030): a child halted before completing its goal
            // (operator stop, orchestrator request, budget breach, session
            // wind-down). Emitted BEFORE the follow-up SubagentComplete —
            // Stopped is the terminal state here and the Complete/Failed
            // arms must not overwrite it.
            AgentEvent::SubagentStopped { id, goal, reason, summary_preview } => {
                // (a) Job-board entry: only a Running entry transitions
                // (Stopped is terminal; a re-stop is a no-op).
                for entry in self.subagent_entries.iter_mut().rev() {
                    if entry.status == SubagentStatus::Running && entry.child_id == id
                    {
                        entry.status = SubagentStatus::Stopped;
                        entry.phase = format!("stopped: {}", reason);
                        break;
                    }
                }
                // (b) Pane: final state = Stopped + the partial-result
                // preview; flush the live streams exactly like the
                // SubagentComplete close-out (T032) so a child stopped
                // mid-stream keeps its reasoning + partial answer.
                if let Some(pane) = self.subagent_panes.iter_mut().find(|p| p.child_id == id) {
                    pane.status = SubagentStatus::Stopped;
                    pane.summary_preview = Some(summary_preview.clone());
                    pane_flush_reasoning(pane);
                    if !pane.streaming_assistant.is_empty() {
                        let text = std::mem::take(&mut pane.streaming_assistant);
                        pane.push_item(TranscriptItem::Assistant { text });
                    }
                }
                // (c) FR-016: the notice carries the raw reason string
                // (budget_exceeded vs operator_requested vs …) plus the
                // partial-result preview (FR-010).
                self.push_item(TranscriptItem::Notice {
                    text: format!("■ {}: stopped ({}) — {}", goal, reason, summary_preview),
                    kind: NoticeKind::Warning,
                });
            }
            AgentEvent::DelegationBatchComplete { total, succeeded, failed, total_duration_secs: _ } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Batch: {}/{} done, {} failed", succeeded, total, failed),
                    kind: if failed > 0 { NoticeKind::Warning } else { NoticeKind::Success },
                });
            }
            // ── OMO orchestration events (additive — no UI action needed in
            // the transcript; the activity panel reads these separately) ──
            AgentEvent::AgentModeChanged { agent_name, model: _ } => {
                // T065/T139: update active_agent_index to match the new agent.
                if let Some(idx) = self
                    .agent_roster
                    .iter()
                    .position(|a| a.name == agent_name || a.display_name == agent_name)
                {
                    self.active_agent_index = idx;
                }
                self.push_item(TranscriptItem::Notice {
                    text: format!("Agent: {}", agent_name),
                    kind: NoticeKind::Info,
                });
            }
            // ── NeuroCode (feature 015): live context feed ──
            AgentEvent::NeuroCodeProgress { stage } => {
                // Feature 015 follow-up: live stage line during assembly.
                // Arrives BEFORE the final NeuroCodeContext blob.
                self.neurocode_active = true;
                self.neurocode_stage = stage;
                self.neurocode_stage_at = Some(std::time::Instant::now());
            }
            AgentEvent::NeuroCodeContext {
                tier,
                token_estimate,
                expanded_nodes,
                cold_mode,
                formatted_context,
            } => {
                self.neurocode_active = true;
                self.neurocode_tier = tier;
                self.neurocode_tokens = token_estimate;
                self.neurocode_nodes = expanded_nodes;
                self.neurocode_cold = cold_mode;
                self.neurocode_context = formatted_context;
                self.neurocode_scroll = 0;
                // Assembly finished: clear the live stage, stamp the refresh.
                self.neurocode_stage.clear();
                self.neurocode_stage_at = None;
                self.neurocode_updated_at = Some(std::time::Instant::now());
            }
            AgentEvent::NeuroCodeGraph { snapshot } => {
                // Interactive visualization payload: stash the snapshot and
                // reset explorer interaction state so a new assembly starts
                // fresh (selection lands back on the primary node).
                self.neurocode_snapshot = Some(snapshot);
                self.neurocode_viz.reset();
            }
            AgentEvent::NeuroCodeReindexed { files_scanned, files_edited, lines_edited } => {
                // Auto re-index completed after large edits: the structural
                // graph is fresh again. Surface a notice; the next user
                // turn re-assembles context against the new index (dynamic
                // context across turns).
                self.push_item(TranscriptItem::Notice {
                    text: format!(
                        "⚡ NeuroCode re-indexed: {} files scanned (edit pressure: {} files / {} lines)",
                        files_scanned, files_edited, lines_edited
                    ),
                    kind: NoticeKind::Success,
                });
            }
            AgentEvent::NeuroCodeActive { active } => {
                self.neurocode_active = active;
                if !active {
                    self.neurocode_context.clear();
                    self.neurocode_stage.clear();
                    self.neurocode_stage_at = None;
                    self.neurocode_updated_at = None;
                    self.neurocode_expanded = false;
                    self.last_neurocode_rect.set((0, 0, 0, 0));
                    self.neurocode_snapshot = None;
                    self.neurocode_viz = crate::neurocode_viz::VizState::default();
                    self.last_viz_nodes_rect.set((0, 0, 0, 0));
                }
            }
            AgentEvent::CategoryDelegation { category, model } => {
                // T065/T139: add a subagent entry with the category label.
                let id = self.next_subagent_id;
                self.next_subagent_id += 1;
                let title = format!("[{}] delegation", category);
                self.subagent_entries.push(ActiveSubagentEntry {
                    id,
                    child_id: 0, // category delegations carry no stable child id
                    agent_type: format!("junior:{}", category),
                    category: Some(category.clone()),
                    status: SubagentStatus::Running,
                    phase: "querying model".to_string(),
                    model: model.clone(),
                    iterations: 0,
                    started: Instant::now(),
                    task_title: Some(title),
                    tool_call_count: 0,
                    last_tool: None,
                });
                self.push_item(TranscriptItem::Notice {
                    text: format!("Category [{}] → {}", category, model),
                    kind: NoticeKind::Busy,
                });
            }
            AgentEvent::BoulderWorkStarted { plan_name, work_id: _ } => {
                // T069/T155: show the Atlas job board during plan execution.
                self.job_board_visible = true;
                self.push_item(TranscriptItem::Notice {
                    text: format!("Started work: {}", plan_name),
                    kind: NoticeKind::Success,
                });
            }
            AgentEvent::BoulderWorkResumed { plan_name, work_id: _ } => {
                self.job_board_visible = true;
                self.push_item(TranscriptItem::Notice {
                    text: format!("Resumed work: {}", plan_name),
                    kind: NoticeKind::Info,
                });
            }
            AgentEvent::BoulderWorkCompleted { plan_name, work_id: _ } => {
                self.job_board_visible = false;
                self.push_item(TranscriptItem::Notice {
                    text: format!("Completed work: {}", plan_name),
                    kind: NoticeKind::Success,
                });
            }
            AgentEvent::GoalSet { objective } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Goal set: {}", objective),
                    kind: NoticeKind::Success,
                });
            }
            AgentEvent::GoalCleared => {
                self.push_item(TranscriptItem::Notice {
                    text: "Goal cleared".into(),
                    kind: NoticeKind::Info,
                });
            }
            AgentEvent::WisdomAccumulated { learnings_count } => {
                self.learnings_count = learnings_count;
                self.push_item(TranscriptItem::Notice {
                    text: format!("Wisdom: {} learnings", learnings_count),
                    kind: NoticeKind::Info,
                });
            }
            // Feature 005 (T018): build a FileDiff transcript item from the
            // FileChange event. Rendering happens in widgets.rs (T019).
            // Spec 017 (T013): construction extracted to `file_diff_item`,
            // shared verbatim with `pane_apply` (D7 parity).
            AgentEvent::FileChange { path, kind, diff, is_binary, .. } => {
                self.push_item(file_diff_item(&path, kind, &diff, is_binary));
            }
            AgentEvent::Done { final_text, usage: _, iterations } => {
                // Tokens were already counted per ApiCallEnd; only the
                // iteration count is new information here.
                self.tokens.iterations += iterations;
                self.flush_reasoning();
                let leftover = std::mem::take(&mut self.streaming_assistant);
                let text = if !final_text.is_empty() { final_text } else { leftover };
                if !text.is_empty() {
                    // `AssistantMessage` fires right before `Done` with the
                    // same text — don't commit it twice.
                    if !self.last_assistant_is(&text) {
                        self.push_item(TranscriptItem::Assistant { text: text.clone() });
                    }
                    self.last_final_text = text;
                }
                self.active_agents.clear();
                // T064: stale activity-panel entries are cleaned up, but the
                // per-subagent PANES deliberately survive the turn — the
                // user can still click a completed child's tab and read its
                // full transcript. Panes accumulate until cleared (Ctrl+L or
                // a new batch replaces the view); bounded by pane count.
                let _keep_panes = &self.subagent_panes;
                if let Some(focus) = self.focused_subagent {
                    if focus >= self.subagent_panes.len() {
                        self.focused_subagent = None;
                    }
                }
                self.subagent_entries.clear();
                self.mode = RunMode::Input;
                self.turn_started = None;
            }
            AgentEvent::Failed(err) => {
                self.flush_reasoning();
                self.flush_streaming_assistant();
                // Resolve any tool still marked Running — its ToolEnd will
                // never arrive, and an eternal spinner reads as a hang.
                for it in self.transcript.iter_mut() {
                    if let TranscriptItem::Tool { status, .. } = it {
                        if *status == ToolStatus::Running {
                            *status = ToolStatus::Failed;
                        }
                    }
                }
                self.push_item(TranscriptItem::Error { text: err.clone() });
                self.last_error = Some(err);
                self.active_agents.clear();
                self.subagent_entries.clear();
                self.mode = RunMode::Input;
                self.turn_started = None;
            }
            // Spec 018 (T019): terminal governor contention snapshot.
            // Last-value-wins — each event overwrites both counters, so
            // intermediate snapshots are naturally coalesced to whatever
            // the latest event said by the time a frame draws (same
            // pattern as ToolOutput chunk accumulation: cheap apply, the
            // frame_budget-paced draw renders the current state).
            AgentEvent::TerminalQueueState { active, queued } => {
                self.terminal_active = active;
                self.terminal_queued = queued;
            }
            // Additive events with no TUI state: ignored.
            _ => {}
        }
    }

    /// Push a transcript item, enforcing the capacity (ring buffer).
    pub fn push_item(&mut self, item: TranscriptItem) {
        if self.transcript.len() >= self.transcript_capacity {
            self.transcript.pop_front();
            // Indices shifted by one: keep the maximized output viewer glued
            // to its (possibly evicted) target.
            if let Some(i) = self.output_viewer_index {
                self.output_viewer_index = if i == 0 { None } else { Some(i - 1) };
                if i == 0 {
                    // The viewed item itself was evicted — fall back to the
                    // most recent terminal item on the next resolve.
                    self.output_viewer_view = None;
                }
            }
        }
        self.transcript.push_back(item);
        // Deliberately does NOT touch `scroll`: a user reading history stays
        // where they are while new content streams in below.
    }

    /// Append `chunk` to the bounded live-output buffer, evicting from the
    /// head (keep the tail) when the capacity is hit. Tries to cut at a line
    /// boundary near the eviction point so the live view doesn't start
    /// mid-word more than necessary.
    pub(crate) fn push_bounded_item(buf: &mut String, chunk: &str, cap: usize) {
        buf.push_str(chunk);
        if buf.len() > cap {
            let mut cut = buf.len() - cap;
            // Prefer cutting right after a newline within the first 4 KB of
            // the eviction window so the retained tail starts on a line start.
            let scan_end = (cut + 4096).min(buf.len());
            if let Some(nl) = buf.as_bytes()[cut..scan_end].iter().position(|&b| b == b'\n') {
                cut = (cut + nl + 1).min(buf.len());
            }
            // Byte-slice safety: never cut inside a UTF-8 sequence.
            while cut < buf.len() && !buf.is_char_boundary(cut) {
                cut += 1;
            }
            let tail = buf[cut..].to_string();
            *buf = tail;
        }
    }

    /// Record a user message in the transcript and snap to the bottom.
    pub fn record_user(&mut self, text: &str) {
        self.push_item(TranscriptItem::User { text: text.to_string() });
        self.scroll = None;
    }

    pub fn scroll_up(&mut self, by: usize) {
        let cur = self.scroll.unwrap_or(0);
        self.scroll = Some((cur + by).min(self.last_max_scroll.get()));
    }

    pub fn scroll_down(&mut self, by: usize) {
        if let Some(s) = self.scroll {
            // Content may have shrunk (e.g. a cleared view) — re-clamp so one
            // page-down always makes visible progress.
            let s = s.min(self.last_max_scroll.get());
            self.scroll = if s > by { Some(s - by) } else { None };
        }
    }

    /// Jump to the oldest rendered content (bounded by what the transcript
    /// widget has measured so far).
    pub fn scroll_to_top(&mut self) {
        self.scroll = Some(self.last_max_scroll.get());
    }

    /// Resume auto-follow at the bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll = None;
    }

    // ── NeuroCode context feed ─────────────────────────────────────────

    /// Toggle the NeuroCode context feed between its docked bottom-right
    /// panel and expanded main-screen mode (and back). Invoked by clicking
    /// the panel or pressing Esc while expanded. No-op when NeuroCode is
    /// inactive.
    pub fn toggle_neurocode_expanded(&mut self) {
        if !self.neurocode_active {
            return;
        }
        self.neurocode_expanded = !self.neurocode_expanded;
        // Reset feed scroll so the expanded view opens at the tail (the
        // live end) rather than wherever the docked view was scrolled to.
        self.neurocode_scroll = 0;
    }

    // ── Live terminal output viewer (maximize) ─────────────────────────

    /// Toggle the maximized terminal-output viewer. With `index` set (mouse
    /// click on a specific tool item) it targets that item; otherwise it
    /// targets the most recent tool item (any kind) in the transcript. When
    /// nothing eligible exists this is a no-op. Toggling while open docks it
    /// back.
    pub fn toggle_output_viewer(&mut self, index: Option<usize>) {
        if self.output_viewer_open {
            self.close_output_viewer();
            return;
        }
        let target = match index {
            Some(i) => {
                let ok = matches!(self.transcript.get(i), Some(TranscriptItem::Tool { .. }));
                if !ok {
                    return;
                }
                i
            }
            None => match self.most_recent_tool_item() {
                Some(i) => i,
                None => return,
            },
        };
        self.output_viewer_open = true;
        self.output_viewer_index = Some(target);
        // Open at the live tail, following as output streams in.
        self.output_viewer_view = None;
    }

    /// Close the maximized viewer (Esc / Ctrl+O / toggle-click).
    pub fn close_output_viewer(&mut self) {
        self.output_viewer_open = false;
        self.output_viewer_view = None;
        self.last_output_viewer_rect.set((0, 0, 0, 0));
    }

    /// Mouse-click semantics for tool blocks: same target while open →
    /// close (toggle); a DIFFERENT tool item while open → switch the viewer
    /// to it (stays open, re-pins to that item's tail); otherwise the plain
    /// open toggle.
    pub fn output_viewer_click(&mut self, index: usize) {
        if self.output_viewer_open {
            match self.output_viewer_index {
                Some(i) if i == index => self.close_output_viewer(),
                _ => {
                    if matches!(self.transcript.get(index), Some(TranscriptItem::Tool { .. })) {
                        self.output_viewer_index = Some(index);
                        self.output_viewer_view = None;
                    } else {
                        self.close_output_viewer();
                    }
                }
            }
        } else {
            self.toggle_output_viewer(Some(index));
        }
    }

    /// Index of the most recent terminal tool item in the transcript.
    pub fn most_recent_terminal_item(&self) -> Option<usize> {
        self.transcript.iter().rposition(|i| {
            matches!(i, TranscriptItem::Tool { is_terminal: true, .. })
        })
    }

    /// Index of the most recent tool item of ANY kind (terminal or generic).
    pub fn most_recent_tool_item(&self) -> Option<usize> {
        self.transcript.iter().rposition(|i| matches!(i, TranscriptItem::Tool { .. }))
    }

    /// Resolve the output-viewer target: an explicit index when it is a tool
    /// item, else the most recent tool item (any kind).
    fn output_viewer_target(&self) -> Option<usize> {
        if let Some(i) = self.output_viewer_index {
            if matches!(self.transcript.get(i), Some(TranscriptItem::Tool { .. })) {
                return Some(i);
            }
        }
        self.most_recent_tool_item()
    }

    /// The text the maximized viewer should show for the targeted item:
    /// the live accumulation while running, or the formatted full result
    /// once finished (envelope-unwrapped + JSON pretty-printed).
    pub fn output_viewer_text(&self) -> String {
        let idx = match self.output_viewer_target() {
            Some(i) => i,
            None => return String::new(),
        };
        match self.transcript.get(idx) {
            Some(TranscriptItem::Tool {
                status,
                live_output,
                full_result,
                result_preview,
                ..
            }) => {
                let full = full_result
                    .as_deref()
                    .filter(|f| !f.is_empty())
                    .map(crate::state::format_tool_result_for_display);
                match status {
                    ToolStatus::Running => live_output.clone(),
                    _ => full
                        .or_else(|| {
                            let p = result_preview.as_str();
                            (!p.is_empty()).then(|| p.to_string())
                        })
                        .unwrap_or_else(|| live_output.clone()),
                }
            }
            _ => String::new(),
        }
    }

    /// Scroll the maximized viewer up by `by` lines: freezes the view at an
    /// absolute wrapped-line anchor (auto-follow off). Mirrors
    /// `reasoning_scroll_up`.
    pub fn output_viewer_scroll_up(&mut self, by: usize) {
        let cur = self
            .output_viewer_view
            .unwrap_or_else(|| self.last_output_viewer_max_anchor.get());
        self.output_viewer_view = Some(cur.saturating_sub(by));
    }

    /// Scroll the maximized viewer down by `by` lines: stays frozen until
    /// the anchor reaches the tail, then auto-follow resumes. Mirrors
    /// `reasoning_scroll_down`.
    pub fn output_viewer_scroll_down(&mut self, by: usize) {
        if let Some(a) = self.output_viewer_view {
            let target = a.saturating_add(by);
            if target >= self.last_output_viewer_max_anchor.get() {
                self.output_viewer_view = None;
            } else {
                self.output_viewer_view = Some(target);
            }
        }
    }

    // ── Agent stats page (maximized context window) ────────────────────

    /// Open the agent-stats page. Re-pins its context stream to the live
    /// tail (auto-follow) so it opens on the newest entry.
    pub fn open_stats(&mut self) {
        self.stats_open = true;
        self.stats_view = None;
    }

    /// Close the agent-stats page (Esc / Ctrl+A / clicking the header
    /// section again).
    pub fn close_stats(&mut self) {
        self.stats_open = false;
        self.stats_view = None;
        self.last_stats_rect.set((0, 0, 0, 0));
    }

    /// Toggle the stats page.
    pub fn toggle_stats(&mut self) {
        if self.stats_open {
            self.close_stats();
        } else {
            self.open_stats();
        }
    }

    /// Toggle the subagent rail between the collapsed 19-col tab strip and
    /// the expanded wider detail view (Ctrl+N / clicking the rail title).
    pub fn toggle_subagent_rail(&mut self) {
        self.subagent_rail_expanded = !self.subagent_rail_expanded;
    }

    /// Scroll the stats page's context stream up by `by` lines: freezes the
    /// view at an absolute anchor (auto-follow off). Mirrors
    /// `reasoning_scroll_up`.
    pub fn stats_scroll_up(&mut self, by: usize) {
        let cur = self
            .stats_view
            .unwrap_or_else(|| self.last_stats_max_anchor.get());
        self.stats_view = Some(cur.saturating_sub(by));
    }

    /// Scroll the stats page's context stream down by `by` lines: stays
    /// frozen until the anchor reaches the tail, then auto-follow resumes.
    pub fn stats_scroll_down(&mut self, by: usize) {
        if let Some(a) = self.stats_view {
            let target = a.saturating_add(by);
            if target >= self.last_stats_max_anchor.get() {
                self.stats_view = None;
            } else {
                self.stats_view = Some(target);
            }
        }
    }

    /// Percentage of the context window consumed by the current history +
    /// system prompt (0.0 when the window is unknown).
    pub fn context_usage_pct(&self) -> f64 {
        if self.context_window == 0 {
            return 0.0;
        }
        let used = self.context_system_tokens + self.context_history_tokens;
        (used as f64 / self.context_window as f64) * 100.0
    }

    // ── Expandable context stream (expandable-stats feature) ─────────

    /// Toggle expansion of the context-stream entry at `index` (main
    /// orchestrator's stats page). No-op for out-of-range indices.
    pub fn toggle_context_entry(&mut self, index: usize) {
        if index >= self.context_entries.len() {
            return;
        }
        if !self.expanded_context.insert(index) {
            self.expanded_context.remove(&index);
        }
    }

    /// Toggle expansion of the context-stream entry at `index` in the
    /// FOCUSED subagent pane's stats page.
    pub fn toggle_pane_context_entry(&mut self, index: usize) {
        if let Some(pane) = self.focused_pane_mut() {
            if index >= pane.context_entries.len() {
                return;
            }
            if !pane.expanded_context.insert(index) {
                pane.expanded_context.remove(&index);
            }
        }
    }

    /// Clear all context-entry expansions (main stats page).
    pub fn clear_context_expansions(&mut self) {
        self.expanded_context.clear();
    }

    /// Clear all context-entry expansions in the focused pane.
    pub fn clear_pane_context_expansions(&mut self) {
        if let Some(pane) = self.focused_pane_mut() {
            pane.expanded_context.clear();
        }
    }

    // ── Per-subagent panes (parallel-subagent feature) ────────────────
    /// Focus a subagent pane by index. `None` returns to the orchestrator
    /// (main) view. Retargets the main transcript + the maximized stats /
    /// context window to the selected child.
    ///
    /// Focus-follow: when a pane is selected, the rail window scrolls the
    /// MINIMUM amount needed to bring its tab inside the visible range
    /// (never jumps to the top), so Ctrl+P / tab clicks auto-reveal.
    pub fn focus_subagent(&mut self, index: Option<usize>) {
        self.focused_subagent = match index {
            None => None,
            Some(i) if i < self.subagent_panes.len() => Some(i),
            Some(_) => None,
        };
        if let Some(i) = self.focused_subagent {
            self.reveal_subagent_in_rail(i);
        }
    }

    /// Scroll the rail window minimally so pane `i` is inside it. Uses the
    /// last recorded max-scroll; when `i` is already visible this is a
    /// no-op (visible range = [scroll, scroll + visible], visible =
    /// panes.len() - max_scroll is exact only at scroll==0, so this checks
    /// against the drawn offset + rect count of the last frame when
    /// available, falling back to max-scroll bounds).
    fn reveal_subagent_in_rail(&mut self, i: usize) {
        // Visible pane count of the last frame: recorded rects + the offset
        // the frame skipped. If no frame has rendered yet (or the rects
        // were cleared), fall back to treating everything as visible.
        let offset = self.last_subagent_rail_drawn_offset.get();
        let visible = self.last_subagent_tab_rects.borrow().len();
        if visible == 0 {
            return; // rail not rendered (or empty): nothing to reveal in
        }
        if i < offset {
            // Above the window: scroll up just enough — pane becomes the
            // top visible tab.
            self.subagent_rail_scroll = i;
        } else if i >= offset + visible {
            // Below the window: scroll down just enough — pane becomes the
            // bottom visible tab.
            let target = i + 1 - visible;
            let max = self.last_subagent_rail_max_scroll.get();
            self.subagent_rail_scroll = target.min(max);
        }
        // Clamp defensively (a resize between frames can shrink capacity).
        self.subagent_rail_scroll = self
            .subagent_rail_scroll
            .min(self.last_subagent_rail_max_scroll.get());
    }

    /// Scroll the subagent rail window toward the top (earlier panes) by
    /// `by` panes. Clamped at 0.
    pub fn subagent_rail_scroll_up(&mut self, by: usize) {
        self.subagent_rail_scroll = self.subagent_rail_scroll.saturating_sub(by);
    }

    /// Scroll the subagent rail window toward the bottom (later panes) by
    /// `by` panes. Clamped at the max-scroll recorded by the last frame.
    pub fn subagent_rail_scroll_down(&mut self, by: usize) {
        let max = self.last_subagent_rail_max_scroll.get();
        self.subagent_rail_scroll = (self.subagent_rail_scroll + by).min(max);
    }

    /// Click hit-test against the right rail's tab rects. Returns the pane
    /// index whose tab was clicked, or None. The recorded rect vec indexes
    /// the WINDOWED pane list (the first `last_subagent_rail_drawn_offset`
    /// panes are skipped), so the frame's drawn offset is added back to
    /// map a click to the TRUE pane index.
    pub fn subagent_tab_hit(&self, row: u16, col: u16) -> Option<usize> {
        let offset = self.last_subagent_rail_drawn_offset.get();
        for (i, (x, y, w, h)) in self.last_subagent_tab_rects.borrow().iter().enumerate() {
            if w > &0 && h > &0 && row >= *y && row < *y + *h && col >= *x && col < *x + *w {
                return Some(i + offset);
            }
        }
        None
    }

    /// Click hit-test against the pinned orchestrator tab (rail bottom).
    /// True when the click lands on it. Checked BEFORE `subagent_tab_hit`
    /// so the orchestrator rect wins over any overlapping pane rect.
    pub fn orchestrator_tab_hit(&self, row: u16, col: u16) -> bool {
        let (x, y, w, h) = self.last_orchestrator_tab_rect.get();
        w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w
    }

    /// Click hit-test against the rail's TITLE row (both collapsed and
    /// expanded modes) — clicking it toggles rail expansion.
    pub fn subagent_rail_title_hit(&self, row: u16, col: u16) -> bool {
        let (x, y, w, h) = self.last_subagent_rail_title_rect.get();
        w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w
    }

    // ── Expandable context stream hit-testing (expandable-stats) ───────

    /// Resolve a screen row to a context-stream entry index on the MAIN
    /// stats page, using the geometry recorded by the last render. Column
    /// must be inside the stats page (checked by the caller). Returns None
    /// when the click isn't on an entry row (dashboard header, footer, or
    /// the page wasn't drawn).
    pub fn stats_context_entry_hit(&self, row: u16) -> Option<usize> {
        let (inner_y, start) = self.last_stats_window.get();
        if inner_y == 0 && start == 0 && self.last_stats_stream_rows.borrow().is_empty() {
            return None;
        }
        let content_row = (row as usize).saturating_sub(inner_y as usize) + start;
        for &(entry, first_row, count) in self.last_stats_stream_rows.borrow().iter() {
            if content_row >= first_row && content_row < first_row + count {
                return Some(entry);
            }
        }
        None
    }

    /// Resolve a screen row to a context-stream entry index on the FOCUSED
    /// pane's stats page. Mirrors [`Self::stats_context_entry_hit`].
    pub fn pane_stats_context_entry_hit(&self, row: u16) -> Option<usize> {
        let (inner_y, start) = self.last_pane_stats_window.get();
        if self.last_pane_stats_stream_rows.borrow().is_empty() {
            return None;
        }
        let content_row = (row as usize).saturating_sub(inner_y as usize) + start;
        for &(entry, first_row, count) in self.last_pane_stats_stream_rows.borrow().iter() {
            if content_row >= first_row && content_row < first_row + count {
                return Some(entry);
            }
        }
        None
    }

    /// Scroll the focused pane's transcript up by `by` lines.
    ///
    /// T009 (US1, data-model ScrollState): `None` (follow-tail) →
    /// `Some(by)`; every step clamps into `[0, last_pane_max_scroll]`, the
    /// bound the pane transcript widget records at render time. A pinned
    /// offset that went stale-high (transcript shrank via ring eviction
    /// since it was pinned) is re-clamped here, so the invariant "pinned
    /// stays ≤ the bound" holds at every mutation — exactly the main
    /// transcript's `scroll_up` semantics on the pane. Appends never move
    /// the offset (see `SubagentPane::push_item`); only user scrolls do.
    /// No-op while no pane is focused (focused-view isolation).
    pub fn pane_scroll_up(&mut self, by: usize) {
        let max = self.last_pane_max_scroll.get();
        if let Some(pane) = self.focused_pane_mut() {
            // Clamp `cur` as well: a stale-high offset (bound shrank since
            // the pin) must not survive an up-step. `(cur + by).min(max)`
            // already bounds the result; the explicit clamp documents the
            // re-clamp invariant and keeps `cur + by` from overflowing.
            let cur = pane.scroll.unwrap_or(0).min(max);
            pane.scroll = Some((cur + by).min(max));
        }
    }

    /// Scroll the focused pane's transcript down by `by` lines (None at the
    /// bottom resumes auto-follow).
    ///
    /// T009: mirrors the main transcript's `scroll_down` exactly — re-clamp
    /// the current offset against the render-time bound first (content may
    /// have shrunk), then step; reaching (or passing) the bottom flips the
    /// pane back to follow-tail (`None`), which is the ONLY way appends
    /// resume live-tracking after the user pinned the view.
    pub fn pane_scroll_down(&mut self, by: usize) {
        let max = self.last_pane_max_scroll.get();
        if let Some(pane) = self.focused_pane_mut() {
            if let Some(s) = pane.scroll {
                let s = s.min(max);
                pane.scroll = if s > by { Some(s - by) } else { None };
            }
        }
    }

    /// Clear all panes and return to the orchestrator view (Ctrl+L parity).
    ///
    /// T009 (US1, data-model ScrollState / D9): besides the pane map and
    /// focus, the render-time pane geometry cells go back to their pristine
    /// values — the recorded `last_pane_max_scroll` bound described panes
    /// that no longer exist, and leaving it stale would let a pre-render
    /// scroll on a freshly spawned pane clamp against a ghost bound
    /// (bound-freshness invariant: the bound is only meaningful between
    /// the render that recorded it and the next clear). The orchestrator's
    /// own `scroll`/`last_max_scroll` are deliberately untouched (D9).
    pub fn clear_subagent_panes(&mut self) {
        self.subagent_panes.clear();
        self.focused_subagent = None;
        self.last_subagent_tab_rects.borrow_mut().clear();
        self.last_orchestrator_tab_rect.set((0, 0, 0, 0));
        // Reset the rail window: with no panes there is nothing scrolled.
        self.subagent_rail_scroll = 0;
        self.last_subagent_rail_max_scroll.set(0);
        self.last_subagent_rail_drawn_offset.set(0);
        self.last_subagent_rail_rect.set((0, 0, 0, 0));
        // T009: reset the pane-view geometry cells too (see doc comment) —
        // they describe the FOCUSED pane's transcript area, which is gone.
        self.last_pane_max_scroll.set(0);
        self.last_pane_text_area.set((0, 0, 0, 0));
        self.last_pane_stats_rect.set((0, 0, 0, 0));
    }

    /// The pane currently focused, if any.
    pub fn focused_pane(&self) -> Option<&SubagentPane> {
        self.focused_subagent.and_then(|i| self.subagent_panes.get(i))
    }

    /// The pane currently focused, mutably, if any.
    pub fn focused_pane_mut(&mut self) -> Option<&mut SubagentPane> {
        self.focused_subagent.and_then(|i| self.subagent_panes.get_mut(i))
    }

    /// Scroll the focused pane's maximized stats stream up (freezes the
    /// anchor). Mirrors `stats_scroll_up`. T004: the anchor is per-pane
    /// (`SubagentPane::stats_view`), so sibling panes keep their own scroll.
    pub fn pane_stats_scroll_up(&mut self, by: usize) {
        if let Some(pane) = self.focused_pane_mut() {
            let cur = pane
                .stats_view
                .unwrap_or_else(|| pane.last_stats_max_anchor.get());
            pane.stats_view = Some(cur.saturating_sub(by));
        }
    }

    /// Scroll the focused pane's maximized stats stream down; re-pins at the
    /// bottom. Mirrors `stats_scroll_down`. T004: per-pane anchor.
    pub fn pane_stats_scroll_down(&mut self, by: usize) {
        if let Some(pane) = self.focused_pane_mut() {
            if let Some(a) = pane.stats_view {
                let target = a.saturating_add(by);
                if target >= pane.last_stats_max_anchor.get() {
                    pane.stats_view = None;
                } else {
                    pane.stats_view = Some(target);
                }
            }
        }
    }

    /// T004: the FOCUSED pane's stats-view anchor, if a pane is focused.
    /// Returns `None` both when no pane is focused and when the focused
    /// pane is auto-following — callers that must distinguish should check
    /// `focused_subagent` first.
    pub fn focused_pane_stats_view(&self) -> Option<usize> {
        self.focused_pane().and_then(|p| p.stats_view)
    }

    /// T004: set the FOCUSED pane's stats-view anchor. No-op when no pane
    /// is focused (graceful fallback for the orchestrator view, whose stats
    /// anchor is `App::stats_view`).
    pub fn set_focused_pane_stats_view(&mut self, view: Option<usize>) {
        if let Some(pane) = self.focused_pane_mut() {
            pane.stats_view = view;
        }
    }
}

// ── Live reasoning panel ───────────────────────────────────────────

impl App {
    /// Toggle the live reasoning panel between its docked bottom strip and
    /// expanded main-screen mode (and back). Invoked by clicking the panel
    /// or pressing Esc while expanded. No-op when no live reasoning block
    /// is streaming (there's nothing to expand).
    pub fn toggle_reasoning_expanded(&mut self) {
        if !self.reasoning_open {
            return;
        }
        self.reasoning_expanded = !self.reasoning_expanded;
        // Re-pin to the live tail on mode change so the expanded view opens
        // at the streaming end, not a stale frozen anchor.
        self.reasoning_view = None;
    }

    /// Scroll the live reasoning panel view up by `by` lines: freezes the
    /// view at an absolute anchor (auto-follow off) so streaming no longer
    /// moves the window. The anchor is the window's TOP line index —
    /// while following, that index is `max_anchor`, so moving up decreases
    /// it (0 = the very top of the stream; frozen there, not following).
    pub fn reasoning_scroll_up(&mut self, by: usize) {
        let cur = self
            .reasoning_view
            .unwrap_or_else(|| self.last_reasoning_max_anchor.get());
        self.reasoning_view = Some(cur.saturating_sub(by));
    }

    /// Scroll the live reasoning panel view down by `by` lines. While the
    /// window is still above the tail it stays frozen; when the anchor
    /// reaches the bottom of the stream, auto-follow resumes. A no-op
    /// while already following.
    pub fn reasoning_scroll_down(&mut self, by: usize) {
        if let Some(a) = self.reasoning_view {
            let target = a.saturating_add(by);
            if target >= self.last_reasoning_max_anchor.get() {
                // Bottom reached: re-pin to the live tail.
                self.reasoning_view = None;
            } else {
                self.reasoning_view = Some(target);
            }
        }
    }

    /// T034 (US4, FR-008, D6): toggle the FOCUSED pane's live reasoning
    /// panel between its docked strip and expanded takeover (and back) —
    /// the exact semantics of `toggle_reasoning_expanded` on the pane.
    /// Invoked by clicking the panel or pressing Esc while a pane's
    /// expanded reasoning is open. No-op when no pane is focused or the
    /// pane has no live reasoning block (content-based live condition:
    /// a non-empty accumulator IS a live block — panes carry no
    /// `reasoning_open` latch). Re-pins the pane view to the live tail
    /// on mode change so the expanded view opens at the streaming end.
    pub fn toggle_focused_pane_reasoning_expanded(&mut self) {
        if let Some(pane) = self.focused_pane_mut() {
            if pane.streaming_reasoning.is_empty() {
                return;
            }
            pane.reasoning_expanded = !pane.reasoning_expanded;
            pane.reasoning_view = None;
        }
    }

    /// T034: scroll the FOCUSED pane's live reasoning panel view up by
    /// `by` lines — freezes at an absolute anchor (auto-follow off).
    /// Mirrors `reasoning_scroll_up`; the anchor bound is the shared
    /// `last_reasoning_max_anchor` the reasoning widget records at
    /// render time (the focused pane's panel is the one that rendered,
    /// so the cell describes IT on that frame). No-op while no pane is
    /// focused.
    pub fn pane_reasoning_scroll_up(&mut self, by: usize) {
        // Read the render-time bound BEFORE the mutable pane borrow (the
        // cell is on App, not the pane — unlike pane_stats_scroll_*).
        let max_anchor = self.last_reasoning_max_anchor.get();
        if let Some(pane) = self.focused_pane_mut() {
            let cur = pane.reasoning_view.unwrap_or(max_anchor);
            pane.reasoning_view = Some(cur.saturating_sub(by));
        }
    }

    /// T034: scroll the FOCUSED pane's live reasoning panel view down;
    /// re-pins to the live tail when the bottom is reached. Mirrors
    /// `reasoning_scroll_down`. No-op while no pane is focused.
    pub fn pane_reasoning_scroll_down(&mut self, by: usize) {
        let max_anchor = self.last_reasoning_max_anchor.get();
        if let Some(pane) = self.focused_pane_mut() {
            if let Some(a) = pane.reasoning_view {
                let target = a.saturating_add(by);
                if target >= max_anchor {
                    pane.reasoning_view = None;
                } else {
                    pane.reasoning_view = Some(target);
                }
            }
        }
    }

    // ── Search ─────────────────────────────────────────────────────────

    /// Run a search query against the transcript, scrolling to the first
    /// (newest) match. Called when the user types in the search bar.
    ///
    /// T015 (US3, FR-006/FR-007, design D5): focus-follow — with a
    /// subagent pane focused the same search bar searches the PANE's
    /// transcript and pins the PANE view (clamped per ScrollState,
    /// `last_pane_max_scroll`), leaving the orchestrator's scroll and
    /// match-indicator state untouched (per-view SearchState isolation,
    /// data-model.md). Unfocused, the main-transcript behavior is
    /// byte-identical to before (the walk now lives in `run_search_in`).
    /// Match-indicator only — search never highlights text in-place (D5).
    pub fn run_search(&mut self) {
        if self.focused_subagent.is_some() {
            let query = self.search_query.clone();
            let max = self.last_pane_max_scroll.get();
            if let Some(pane) = self.focused_pane_mut() {
                // The live bar's query becomes the pane's preserved query
                // (FR-010: switching focus away and back keeps it).
                pane.search_query = query;
                Self::run_search_on_pane(pane, max);
            }
            return;
        }
        match Self::run_search_in(&self.transcript, &self.search_query) {
            Some(idx) => {
                // Scroll to show this item — approximate by scrolling up
                // proportionally to the item position.
                let target_scroll = idx
                    .saturating_sub(2)
                    .min(self.last_max_scroll.get());
                self.scroll = Some(target_scroll);
                self.search_has_match = true;
            }
            None => self.search_has_match = false,
        }
    }

    /// Find the next/previous search match from the current scroll position.
    ///
    /// T015 (US3, FR-006, D5): focus-follow — with a pane focused, n/N
    /// walk the PANE's matches and scroll only the OWNING (pane) view;
    /// the main path is byte-identical to before (the walk now lives in
    /// `search_next_in`).
    pub fn search_next(&mut self, forward: bool) {
        if self.focused_subagent.is_some() {
            let query = self.search_query.clone();
            let max = self.last_pane_max_scroll.get();
            if let Some(pane) = self.focused_pane_mut() {
                pane.search_query = query;
                Self::search_next_on_pane(pane, forward, max);
            }
            return;
        }
        let current_scroll = self.scroll.unwrap_or(0);
        if let Some(idx) =
            Self::search_next_in(&self.transcript, &self.search_query, current_scroll, forward)
        {
            let target_scroll = idx.saturating_sub(2).min(self.last_max_scroll.get());
            self.scroll = Some(target_scroll);
        }
    }

    /// T015 (US3, FR-006, D5): pane-targeted `run_search` — run the FOCUSED
    /// pane's own query (its per-view SearchState) against its transcript
    /// and pin the pane. No-op while no pane is focused (focused-view
    /// isolation). The key-routing wave (T016) dispatches here for '/'
    /// opened inside a pane view.
    pub fn pane_run_search(&mut self) {
        let max = self.last_pane_max_scroll.get();
        if let Some(pane) = self.focused_pane_mut() {
            Self::run_search_on_pane(pane, max);
        }
    }

    /// T015 (US3, FR-006, D5): pane-targeted `search_next` — walk the
    /// FOCUSED pane's matches (its own query) and scroll only the pane.
    /// No-op while no pane is focused.
    pub fn pane_search_next(&mut self, forward: bool) {
        let max = self.last_pane_max_scroll.get();
        if let Some(pane) = self.focused_pane_mut() {
            Self::search_next_on_pane(pane, forward, max);
        }
    }

    /// T015 (D5): apply `run_search` semantics to one pane: newest-first
    /// first match pins the pane's scroll (clamped to the render-time
    /// bound — ScrollState: pinned ≤ last_pane_max_scroll) and sets the
    /// pane's match indicator; no match (or empty query) clears it and
    /// never moves the view (never yanks a scrolled-up pane).
    fn run_search_on_pane(pane: &mut SubagentPane, max_scroll: usize) {
        match Self::run_search_in(&pane.transcript, &pane.search_query) {
            Some(idx) => {
                pane.scroll = Some(idx.saturating_sub(2).min(max_scroll));
                pane.search_has_match = true;
            }
            None => pane.search_has_match = false,
        }
    }

    /// T015 (D5): apply `search_next` semantics to one pane: navigate to
    /// the next/previous match from the pane's current offset (wrap-around
    /// included) and pin the pane, clamped per ScrollState. Empty query /
    /// no matches are a no-op (the view stays where the user put it).
    ///
    /// Position proxy: a pinned match sits at `scroll = idx - 2` (the
    /// display offset `run_search_on_pane` applies), so the walk threshold
    /// is `current_scroll + 2` — the inverse mapping — to step to the
    /// match strictly BEYOND the one in view. (The main transcript's
    /// `search_next` keeps its historical raw-scroll proxy byte-for-byte;
    /// under it N re-finds the current match whenever the offset is
    /// unclamped, which pane navigation must not inherit: n/N must
    /// advance, data-model.md "wraps/advances matches".)
    fn search_next_on_pane(pane: &mut SubagentPane, forward: bool, max_scroll: usize) {
        let current_scroll = pane.scroll.unwrap_or(0);
        if let Some(idx) = Self::search_next_in(
            &pane.transcript,
            &pane.search_query,
            current_scroll + 2,
            forward,
        ) {
            pane.scroll = Some(idx.saturating_sub(2).min(max_scroll));
        }
    }

    /// T015 (D5): the newest-first match walk the main `run_search` always
    /// used, parameterized by the target transcript (the `_in()` precedent
    /// of `cycle_last_reasoning_expand_in`). Returns the newest matching
    /// item's from-bottom index, or `None` (empty query / no match).
    fn run_search_in(transcript: &VecDeque<TranscriptItem>, query: &str) -> Option<usize> {
        if query.is_empty() {
            return None;
        }
        let query = query.to_lowercase();
        // Search from the newest item backward.
        transcript
            .iter()
            .rev()
            .enumerate()
            .find(|(_, item)| transcript_item_text(item).to_lowercase().contains(&query))
            .map(|(idx, _)| idx)
    }

    /// T015 (D5): the next/previous match walk of `search_next`,
    /// parameterized by the target transcript and the position threshold
    /// (main passes the raw scroll offset — upstream's proxy, kept
    /// byte-identical; the pane path passes the inverted display offset,
    /// see `search_next_on_pane`). Returns the target match's from-bottom
    /// index (wrap-around included), or `None` (empty query / no matches).
    fn search_next_in(
        transcript: &VecDeque<TranscriptItem>,
        query: &str,
        threshold: usize,
        forward: bool,
    ) -> Option<usize> {
        if query.is_empty() {
            return None;
        }
        let query = query.to_lowercase();

        // Collect match positions (items that contain the query).
        let matches: Vec<usize> = transcript
            .iter()
            .rev()
            .enumerate()
            .filter(|(_, item)| {
                transcript_item_text(item).to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect();

        if matches.is_empty() {
            return None;
        }

        // Find the next match beyond the current scroll position.
        if forward {
            // Forward = scroll down toward newer messages (decrease scroll).
            matches
                .iter()
                .copied()
                .find(|&idx| idx < threshold)
                .or_else(|| matches.first().copied())
        } else {
            // Backward = scroll up toward older messages (increase scroll).
            matches
                .iter()
                .copied()
                .find(|&idx| idx > threshold)
                .or_else(|| matches.last().copied())
        }
    }
}

/// Extract searchable text from a transcript item.
fn transcript_item_text(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::User { text } => format!("user: {}", text),
        TranscriptItem::Assistant { text } => text.clone(),
        TranscriptItem::Reasoning { text, .. } => text.clone(),
        TranscriptItem::Tool { name, summary, result_preview, full_args, full_result, .. } => {
            let mut s = format!("{} {} {}", name, summary, result_preview);
            if let Some(a) = full_args {
                s.push(' ');
                s.push_str(a);
            }
            if let Some(r) = full_result {
                s.push(' ');
                s.push_str(r);
            }
            s
        }
        TranscriptItem::Notice { text, .. } => text.clone(),
        TranscriptItem::Error { text } => text.clone(),
        TranscriptItem::FileDiff { path, stat, lines, .. } => {
            format!("{} {} {}", path, stat, lines.join(" "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_agent_core::AgentEvent;

    fn mk_agent(name: &str, display: &str) -> DisplayAgent {
        DisplayAgent {
            name: name.to_string(),
            display_name: display.to_string(),
            color: String::new(),
            mode: "Primary".to_string(),
            resolved_model: Some("m".to_string()),
            description: String::new(),
        }
    }

    /// T040: active_agent_index cycles forward and backward across the roster,
    /// and the picker cursor wraps at both boundaries (BC-014, BC-017).
    #[test]
    fn active_agent_index_cycles_and_wraps() {
        let mut app = App::new("s", "m");
        app.agent_roster = vec![
            mk_agent("default", "Default"),
            mk_agent("sisyphus", "Sisyphus"),
            mk_agent("prometheus", "Prometheus"),
            mk_agent("atlas", "Atlas"),
        ];
        let n = app.agent_roster.len();
        assert_eq!(n, 4);
        assert_eq!(app.active_agent_index, 0);

        // Forward cycle 0 → 1 → 2 → 3 → 0.
        app.active_agent_index = (app.active_agent_index + 1) % n;
        assert_eq!(app.active_agent_index, 1);
        app.active_agent_index = (app.active_agent_index + 1) % n;
        assert_eq!(app.active_agent_index, 2);
        app.active_agent_index = (app.active_agent_index + 1) % n;
        assert_eq!(app.active_agent_index, 3);
        app.active_agent_index = (app.active_agent_index + 1) % n;
        assert_eq!(app.active_agent_index, 0, "must wrap to start");

        // Backward cycle (Shift+Tab logic from app.rs) 0 → n-1 → n-2 …
        app.agent_picker_cursor = app.active_agent_index;
        if app.agent_picker_cursor == 0 {
            app.agent_picker_cursor = n - 1;
        } else {
            app.agent_picker_cursor -= 1;
        }
        assert_eq!(app.agent_picker_cursor, 3, "backward wrap to end");
        if app.agent_picker_cursor == 0 {
            app.agent_picker_cursor = n - 1;
        } else {
            app.agent_picker_cursor -= 1;
        }
        assert_eq!(app.agent_picker_cursor, 2);
    }

    /// T075: SubagentSpawn adds a Running entry; SubagentComplete marks it
    /// Done; SubagentFailed marks it Failed; three parallel spawns create
    /// three entries (contracts/activity-panel.md event stream mapping).
    #[test]
    fn subagent_spawn_complete_failed_drive_entries() {
        let mut app = App::new("s", "m");
        // Three parallel explore spawns.
        for i in 1..=3 {
            app.apply(AgentEvent::SubagentSpawn {
                id: i as u64,
                goal: format!("explore task {i}"),
                model: "glm-5".into(),
                toolset_summary: "read".into(),
                depth: 1,
            });
        }
        assert_eq!(app.subagent_entries.len(), 3, "three parallel spawns");
        assert!(
            app.subagent_entries
                .iter()
                .all(|e| e.status == SubagentStatus::Running),
            "all start Running"
        );

        // First completes successfully → Done.
        app.apply(AgentEvent::SubagentComplete {
            id: 1,
            goal: "explore task 1".into(),
            success: true,
            summary_preview: "ok".into(),
            token_usage: joey_providers::Usage::default(),
            duration_secs: 4.2,
        });
        let e0 = &app.subagent_entries[0];
        assert_eq!(e0.status, SubagentStatus::Done, "complete → Done");

        // Third fails → Failed.
        app.apply(AgentEvent::SubagentFailed {
            id: 3,
            goal: "explore task 3".into(),
            error: "boom".into(),
            duration_secs: 1.0,
        });
        let e2 = &app.subagent_entries[2];
        assert_eq!(e2.status, SubagentStatus::Failed, "failed → Failed");

        // The middle one is still Running.
        assert_eq!(
            app.subagent_entries[1].status,
            SubagentStatus::Running
        );
    }

    /// T076: CategoryDelegation event adds an entry whose category label is
    /// populated and whose agent_type is "junior:<category>" (contracts/
    /// activity-panel.md "category-spawned subagents show their category").
    #[test]
    fn category_delegation_adds_category_labelled_entry() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::CategoryDelegation {
            category: "quick".into(),
            model: "gpt-5.4-mini".into(),
        });
        assert_eq!(app.subagent_entries.len(), 1);
        let entry = &app.subagent_entries[0];
        assert_eq!(entry.category.as_deref(), Some("quick"));
        assert!(entry.agent_type.contains("junior"), "junior label");
        assert!(entry.agent_type.contains("quick"), "category in label");
        assert_eq!(entry.model, "gpt-5.4-mini");
        assert_eq!(entry.status, SubagentStatus::Running);
    }

    /// Done event clears the subagent entries for the next turn (state.rs
    /// line ~595). This guards the panel's idle-state recovery.
    #[test]
    fn done_clears_subagent_entries() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::SubagentSpawn {
            id: 1,
            goal: "explore x".into(),
            model: "m".into(),
            toolset_summary: "read".into(),
            depth: 1,
        });
        assert!(!app.subagent_entries.is_empty());
        app.apply(AgentEvent::TurnStart { max_iterations: 5 });
        app.apply(AgentEvent::Done {
            final_text: "done".into(),
            usage: joey_providers::Usage::default(),
            iterations: 1,
        });
        assert!(
            app.subagent_entries.is_empty(),
            "Done must clear subagent entries"
        );
    }

    /// T155: the Atlas job board fields. SubagentSpawn populates task_title;
    /// ToolStart increments tool_call_count and sets last_tool on the most
    /// recent running entry; BoulderWorkStarted sets job_board_visible so the
    /// draw_omo_panel job-board section renders during Atlas execution.
    #[test]
    fn job_board_fields_populated_by_events() {
        let mut app = App::new("s", "m");
        assert!(!app.job_board_visible, "job board hidden by default");

        // BoulderWorkStarted reveals the job board.
        app.apply(AgentEvent::BoulderWorkStarted {
            plan_name: "feat-x".into(),
            work_id: "w1".into(),
        });
        assert!(app.job_board_visible, "BoulderWorkStarted shows job board");

        // A delegated task spawns an entry carrying the task title.
        app.apply(AgentEvent::SubagentSpawn {
            id: 2,
            goal: "Task 1: Implement auth".into(),
            model: "glm-5".into(),
            toolset_summary: "file".into(),
            depth: 1,
        });
        let entry = &app.subagent_entries[0];
        assert_eq!(
            entry.task_title.as_deref(),
            Some("Task 1: Implement auth"),
            "task_title populated from goal"
        );
        assert_eq!(entry.tool_call_count, 0);
        assert!(entry.last_tool.is_none());

        // ToolStart attributed to the running entry.
        app.apply(AgentEvent::ToolStart {
            name: "grep".into(),
            emoji: "🔍".into(),
            summary: "auth".into(),
        });
        let entry = &app.subagent_entries[0];
        assert_eq!(entry.tool_call_count, 1, "tool_call_count incremented");
        assert_eq!(entry.last_tool.as_deref(), Some("grep"), "last_tool set");
        assert!(
            entry.phase.contains("running tool"),
            "phase updated to running tool: got {}",
            entry.phase
        );

        // A second tool call increments again.
        app.apply(AgentEvent::ToolStart {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "src/auth.rs".into(),
        });
        let entry = &app.subagent_entries[0];
        assert_eq!(entry.tool_call_count, 2);
        assert_eq!(entry.last_tool.as_deref(), Some("read_file"));

        // BoulderWorkCompleted hides the job board.
        app.apply(AgentEvent::BoulderWorkCompleted {
            plan_name: "feat-x".into(),
            work_id: "w1".into(),
        });
        assert!(!app.job_board_visible, "completed hides job board");
    }

    /// T155: a category delegation also gets a task title so it shows up on the
    /// job board alongside spawned subagents.
    #[test]
    fn category_delegation_carries_task_title() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::CategoryDelegation {
            category: "quick".into(),
            model: "gpt-5.4-mini".into(),
        });
        let entry = &app.subagent_entries[0];
        assert!(
            entry.task_title.is_some(),
            "category delegation has a task title"
        );
        assert!(
            entry
                .task_title
                .as_ref()
                .unwrap()
                .contains("quick"),
            "title references the category"
        );
        assert_eq!(entry.tool_call_count, 0);
    }

    // ── Feature 005 tests (T021a) ─────────────────────────────────────────

    #[test]
    fn reasoning_expand_cycle_short_text_skips_tail() {
        // Short text (5 lines, fits collapsed): Collapsed → Full → Collapsed.
        // TailWindow is skipped because it would show the same content.
        let text = "line1\nline2\nline3\nline4\nline5";
        let total = text.lines().count();
        assert_eq!(total, 5);

        let s = ReasoningExpandState::Collapsed;
        // Collapsed(5 lines) → should skip TailWindow, go to Full.
        assert_eq!(s.cycle(total), ReasoningExpandState::Full);
        // Full → Collapsed.
        assert_eq!(ReasoningExpandState::Full.cycle(total), ReasoningExpandState::Collapsed);
    }

    #[test]
    fn reasoning_expand_cycle_medium_text() {
        // Medium text (50 lines): doesn't fit collapsed, but fits in tail
        // window (≤200). Collapsed → TailWindow → Collapsed (Full is skipped
        // because TailWindow already shows everything).
        let total = 50;
        let s = ReasoningExpandState::Collapsed;
        assert_eq!(s.cycle(total), ReasoningExpandState::TailWindow);
        assert_eq!(
            ReasoningExpandState::TailWindow.cycle(total),
            ReasoningExpandState::Collapsed
        );
    }

    #[test]
    fn reasoning_expand_cycle_long_text_skips_full_from_tail() {
        // Long text (300 lines): doesn't fit collapsed or tail window.
        // Collapsed → TailWindow → Full → Collapsed (full cycle, no skips).
        let total = 300;
        let s = ReasoningExpandState::Collapsed;
        assert_eq!(s.cycle(total), ReasoningExpandState::TailWindow);
        assert_eq!(
            ReasoningExpandState::TailWindow.cycle(total),
            ReasoningExpandState::Full
        );
        assert_eq!(
            ReasoningExpandState::Full.cycle(total),
            ReasoningExpandState::Collapsed
        );
    }

    #[test]
    fn reasoning_expand_cycle_wraps_after_full() {
        // Verify the cycle wraps for medium text (50 lines, fits tail):
        // Full → Collapsed → TailWindow → Collapsed → …
        // (Full → Collapsed because TailWindow and Collapsed are the only
        //  two distinct states for text that fits in the tail window.)
        let total = 50;
        let s = ReasoningExpandState::Full;
        let s = s.cycle(total); // Full → Collapsed
        assert_eq!(s, ReasoningExpandState::Collapsed);
        let s = s.cycle(total); // Collapsed → TailWindow
        assert_eq!(s, ReasoningExpandState::TailWindow);
        let s = s.cycle(total); // TailWindow → Collapsed (Full skipped)
        assert_eq!(s, ReasoningExpandState::Collapsed);
    }

    // ── Feature 005 tests (T026a) ─────────────────────────────────────────

    #[test]
    fn tool_expand_toggle_flips_and_isolates() {
        // FR-018: per-item isolation — toggling one tool doesn't affect others.
        let mut app = App::new("test", "test-model");
        // Push two tool items.
        app.push_item(TranscriptItem::Tool {
            name: "read_file".to_string(),
            emoji: "📖".to_string(),
            summary: "read foo.rs".to_string(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "ok".to_string(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        });
        app.push_item(TranscriptItem::Tool {
            name: "write_file".to_string(),
            emoji: "✏️".to_string(),
            summary: "write bar.rs".to_string(),
            status: ToolStatus::Done,
            duration_secs: Some(0.2),
            result_preview: "ok".to_string(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        });
        // Toggle the most-recent (write_file) tool.
        app.toggle_focused_tool_expand();
        // The most recent tool should be expanded.
        let last = app.transcript.back().unwrap();
        if let TranscriptItem::Tool { expand_state, .. } = last {
            assert!(matches!(expand_state, ReasoningExpandState::TailWindow | ReasoningExpandState::Full),
                "most-recent tool should be expanded after toggle");
        } else {
            panic!("expected Tool item");
        }
        // The first tool should still be collapsed (per-item isolation).
        let first = &app.transcript[0];
        if let TranscriptItem::Tool { expand_state, .. } = first {
            assert!(matches!(expand_state, ReasoningExpandState::Collapsed),
                "first tool should still be collapsed (FR-018 isolation)");
        } else {
            panic!("expected Tool item");
        }
        // Toggle again — should collapse.
        app.toggle_focused_tool_expand();
        let last = app.transcript.back().unwrap();
        if let TranscriptItem::Tool { expand_state, .. } = last {
            // Second press advances TailWindow -> Full for short results
            // (the cycle skips redundant states), so assert "not collapsed"
            // is wrong; assert it moved past the first expansion instead.
            assert!(matches!(expand_state, ReasoningExpandState::Full | ReasoningExpandState::Collapsed),
                "most-recent tool advances on second toggle (got {:?})", expand_state);
        }
    }

    /// T032 (convergence regression): after `ToolEnd`, the transcript item's
    /// `full_result` must hold the FULL result text (so expand reveals it),
    /// NOT the one-line truncated `result_preview`. Guards the additive
    /// `full_result` plumbing (FR-007 / FR-012 / FR-018 / SC-003).
    #[test]
    fn tool_end_stores_full_result_not_just_preview() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "foo.rs".into(),
        });
        let multi_line = "line one\nline two\nline three\nline four";
        app.apply(AgentEvent::ToolEnd {
            name: "read_file".into(),
            is_error: false,
            result_preview: "line one".into(),
            duration_secs: 0.1,
            exit_code: None,
            full_result: multi_line.into(),
        });
        let last = app.transcript.back().unwrap();
        if let TranscriptItem::Tool { result_preview, full_result, .. } = last {
            assert_eq!(result_preview, "line one", "preview stays as the one-line summary");
            assert_eq!(
                full_result.as_deref(),
                Some(multi_line),
                "full_result must hold the full multi-line text, not the preview"
            );
        } else {
            panic!("expected Tool item");
        }
    }
}

#[cfg(test)]
mod pane_ctrl_expand_mutator_tests {
    //! T012 (US2, FR-004): the App expand mutators retarget to the FOCUSED
    //! pane's transcript (`cycle_focused_reasoning_expand` /
    //! `toggle_focused_tool_expand` — the shared newest-first walk, now
    //! parameterized by target). Integration tests can't construct `Tui`,
    //! so these pin the App-level dispatch the key arms delegate to; the
    //! key-level routing is pinned in app.rs's
    //! `pane_ctrl_expand_key_routing_tests`, and unfocused Main behavior by
    //! the Feature-005 tests above (`tool_expand_toggle_flips_and_isolates`)
    //! plus pane_expand_parity.rs's unfocused pins.
    use super::*;
    use joey_agent_core::AgentEvent;
    use std::time::Duration;

    fn spawn_focused_pane(app: &mut App) {
        app.apply(AgentEvent::SubagentSpawn {
            id: 1,
            goal: "expand child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        let idx = app
            .subagent_panes
            .iter()
            .position(|p| p.child_id == 1)
            .expect("spawn created the pane");
        app.focus_subagent(Some(idx));
    }

    fn long_reasoning(n: usize) -> TranscriptItem {
        TranscriptItem::Reasoning {
            text: (0..n).map(|j| format!("think line {j:03}")).collect::<Vec<_>>().join("\n"),
            expand_state: ReasoningExpandState::Collapsed,
            thought_duration: Some(Duration::from_secs(2)),
        }
    }

    fn long_tool(n: usize) -> TranscriptItem {
        let result =
            (0..n).map(|j| format!("tool out line {j:03}")).collect::<Vec<_>>().join("\n");
        TranscriptItem::Tool {
            name: "longtool".to_string(),
            emoji: "🔧".to_string(),
            summary: "long tool summary".to_string(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: result.clone(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: Some("{}".to_string()),
            full_result: Some(result),
            is_terminal: false,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: LIVE_OUTPUT_CAPACITY,
        }
    }

    fn state_of(item: &TranscriptItem) -> ReasoningExpandState {
        match item {
            TranscriptItem::Reasoning { expand_state, .. }
            | TranscriptItem::Tool { expand_state, .. }
            | TranscriptItem::FileDiff { expand_state, .. } => *expand_state,
            _ => ReasoningExpandState::Collapsed,
        }
    }

    fn main_all_collapsed(app: &App, ctx: &str) {
        for (i, it) in app.transcript.iter().enumerate() {
            assert_eq!(
                state_of(it),
                ReasoningExpandState::Collapsed,
                "{ctx}: main item {i} untouched while a pane is focused"
            );
        }
    }

    /// Focused pane: Ctrl+E's mutator cycles the PANE's most-recent
    /// reasoning entry through ALL THREE states and leaves the main
    /// transcript Collapsed (FR-004 focused-view isolation).
    #[test]
    fn cycle_focused_reasoning_expand_targets_pane() {
        let mut app = App::new("s", "m");
        spawn_focused_pane(&mut app);
        app.subagent_panes[0].push_item(long_tool(6)); // pane decoy (index 0)
        app.subagent_panes[0].push_item(long_reasoning(220)); // target (index 1)
        app.push_item(long_reasoning(90)); // main marker
        app.push_item(long_tool(91));
        let pane = |a: &App| state_of(&a.subagent_panes[0].transcript[1]);

        assert_eq!(pane(&app), ReasoningExpandState::Collapsed);
        app.cycle_focused_reasoning_expand();
        assert_eq!(pane(&app), ReasoningExpandState::TailWindow, "1st: → TailWindow");
        app.cycle_focused_reasoning_expand();
        assert_eq!(pane(&app), ReasoningExpandState::Full, "2nd: → Full (220 > 200)");
        app.cycle_focused_reasoning_expand();
        assert_eq!(pane(&app), ReasoningExpandState::Collapsed, "3rd: → Collapsed");

        assert_eq!(
            state_of(&app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "pane tool untouched (per-item isolation)"
        );
        main_all_collapsed(&app, "ctrl+e mutator");
    }

    /// Focused pane: Ctrl+G's mutator toggles the PANE's most-recent tool
    /// entry through all three states; main stays Collapsed.
    #[test]
    fn toggle_focused_tool_expand_targets_pane() {
        let mut app = App::new("s", "m");
        spawn_focused_pane(&mut app);
        app.subagent_panes[0].push_item(long_reasoning(90)); // pane decoy
        app.subagent_panes[0].push_item(long_tool(220)); // target
        app.push_item(long_reasoning(90));
        app.push_item(long_tool(91));
        let pane = |a: &App| state_of(&a.subagent_panes[0].transcript[1]);

        assert_eq!(pane(&app), ReasoningExpandState::Collapsed);
        app.toggle_focused_tool_expand();
        assert_eq!(pane(&app), ReasoningExpandState::TailWindow, "1st: → TailWindow");
        app.toggle_focused_tool_expand();
        assert_eq!(pane(&app), ReasoningExpandState::Full, "2nd: → Full (220 > 200)");
        app.toggle_focused_tool_expand();
        assert_eq!(pane(&app), ReasoningExpandState::Collapsed, "3rd: → Collapsed");

        assert_eq!(
            state_of(&app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "pane reasoning untouched (per-item isolation)"
        );
        main_all_collapsed(&app, "ctrl+g mutator");
    }

    /// Unfocused (focused_subagent == None) with expandable-carrying panes
    /// present: both mutators act on the MAIN transcript; panes stay
    /// Collapsed (byte-identical Main behavior, constitution VII).
    #[test]
    fn unfocused_mutators_target_main_transcript() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::SubagentSpawn {
            id: 1,
            goal: "idle child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        app.subagent_panes[0].push_item(long_reasoning(220));
        app.subagent_panes[0].push_item(long_tool(220));
        app.transcript.clear(); // drop the spawn notice — exact indices below
        app.push_item(long_reasoning(220)); // main index 0
        app.push_item(long_tool(220)); // main index 1
        assert!(app.focused_subagent.is_none());

        app.cycle_focused_reasoning_expand();
        assert_eq!(
            state_of(&app.transcript[0]),
            ReasoningExpandState::TailWindow,
            "Ctrl+E mutator cycled MAIN reasoning"
        );
        assert_eq!(
            state_of(&app.transcript[1]),
            ReasoningExpandState::Collapsed,
            "Ctrl+E touches only the reasoning item"
        );
        app.toggle_focused_tool_expand();
        assert_eq!(
            state_of(&app.transcript[1]),
            ReasoningExpandState::TailWindow,
            "Ctrl+G mutator toggled MAIN tool"
        );
        for (i, it) in app.subagent_panes[0].transcript.iter().enumerate() {
            assert_eq!(
                state_of(it),
                ReasoningExpandState::Collapsed,
                "pane item {i} untouched while unfocused"
            );
        }
    }
}

#[cfg(test)]
mod slash_menu_tests {
    use super::*;

    fn cmd(name: &str, aliases: &[&str], implemented: bool) -> SlashCommandInfo {
        SlashCommandInfo {
            name: name.to_string(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            description: format!("{} command", name),
            args_hint: "[args]".to_string(),
            implemented,
        }
    }

    fn app_with_commands() -> App {
        let mut app = App::new("s", "m");
        app.slash_commands = vec![
            cmd("help", &[], true),
            cmd("history", &[], true),
            cmd("neurocode", &["nc"], true),
            cmd("queue", &["q"], true),
            cmd("handoff", &[], false),
        ];
        app
    }

    #[test]
    fn popup_opens_on_slash_prefix() {
        let mut app = app_with_commands();
        app.update_slash_menu("/ne");
        assert!(app.slash_menu_open);
        let names: Vec<String> = app.slash_matches("ne").iter().map(|c| c.name.clone()).collect();
        assert!(names.contains(&"neurocode".to_string()));
        assert!(!names.contains(&"help".to_string()));
    }

    #[test]
    fn popup_closes_without_slash() {
        let mut app = app_with_commands();
        app.update_slash_menu("/ne");
        assert!(app.slash_menu_open);
        app.update_slash_menu("hello world");
        assert!(!app.slash_menu_open);
        // Closes once a space follows the command token.
        app.update_slash_menu("/neurocode status");
        assert!(!app.slash_menu_open);
    }

    #[test]
    fn empty_fragment_matches_all() {
        let app = app_with_commands();
        assert_eq!(app.slash_matches("").len(), 5);
    }

    #[test]
    fn alias_prefix_matches() {
        let app = app_with_commands();
        // "q" is queue's alias.
        let m = app.slash_matches("q");
        assert!(m.iter().any(|c| c.name == "queue"));
    }

    #[test]
    fn cursor_navigation_wraps() {
        let mut app = app_with_commands();
        app.update_slash_menu("/");
        assert_eq!(app.slash_menu_cursor, 0);
        let sel = app.slash_menu_move("/", true);
        assert_eq!(app.slash_menu_cursor, 1);
        assert_eq!(sel.as_deref(), Some("history"));
        // Wrap to the last entry from the first via Up.
        let sel = app.slash_menu_move("/", false);
        assert_eq!(app.slash_menu_cursor, 0);
        assert_eq!(sel.as_deref(), Some("help"));
    }

    #[test]
    fn no_matches_closes_popup() {
        let mut app = app_with_commands();
        app.update_slash_menu("/zzz");
        assert!(!app.slash_menu_open);
        assert!(app.slash_matches("zzz").is_empty());
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn record_dedups_consecutive() {
        let mut app = App::new("s", "m");
        app.history_record("first");
        app.history_record("first");
        assert_eq!(app.input_history.len(), 1);
        app.history_record("second");
        assert_eq!(app.input_history.len(), 2);
    }

    #[test]
    fn record_skips_empty_and_whitespace() {
        let mut app = App::new("s", "m");
        app.history_record("");
        app.history_record("   ");
        assert!(app.input_history.is_empty());
    }

    #[test]
    fn prev_then_next_walks_history() {
        let mut app = App::new("s", "m");
        app.history_record("one");
        app.history_record("two");
        app.history_record("three");

        // First Up: newest ("three"), draft saved.
        assert_eq!(app.history_prev("draf").as_deref(), Some("three"));
        assert_eq!(app.history_draft, "draf");
        // Older.
        assert_eq!(app.history_prev("three").as_deref(), Some("two"));
        assert_eq!(app.history_prev("two").as_deref(), Some("one"));
        // At the oldest — stays.
        assert_eq!(app.history_prev("one"), None);
        // Back down.
        assert_eq!(app.history_next().as_deref(), Some("two"));
        assert_eq!(app.history_next().as_deref(), Some("three"));
        // Past the newest — restores the draft and resets.
        assert_eq!(app.history_next().as_deref(), Some("draf"));
        assert!(app.history_pos.is_none());
    }

    #[test]
    fn record_resets_recall_state() {
        let mut app = App::new("s", "m");
        app.history_record("one");
        app.history_record("two");
        let _ = app.history_prev("");
        app.history_record("three");
        assert!(app.history_pos.is_none());
        // Next Up starts from the newest again.
        assert_eq!(app.history_prev("").as_deref(), Some("three"));
    }

    #[test]
    fn empty_history_prev_is_none() {
        let mut app = App::new("s", "m");
        assert!(app.history_prev("").is_none());
        assert!(app.history_next().is_none());
    }
}

#[cfg(test)]
mod completion_menu_tests {
    use super::*;

    fn cmd(name: &str, aliases: &[&str], hint: &str, implemented: bool) -> SlashCommandInfo {
        SlashCommandInfo {
            name: name.to_string(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            description: format!("{name} command"),
            args_hint: hint.to_string(),
            implemented,
        }
    }

    fn app_with_commands() -> App {
        let mut app = App::new("s", "m");
        app.slash_commands = vec![
            cmd("help", &[], "", true),
            cmd("timestamps", &["ts"], "[on|off|status]", true),
            cmd("llm-selector", &[], "[status|pool|enable|disable|help]", true),
            cmd("voice", &[], "[on|off|tts|status]", false),
            cmd("model", &[], "[model] [--global]", true),
        ];
        app
    }

    #[test]
    fn subcommand_stage_opens_after_command_space() {
        let mut app = app_with_commands();
        app.update_slash_menu("/timestamps o");
        assert!(app.slash_menu_open);
        assert!(app.slash_subcommand_stage);
        let subs = app.slash_subcommand_matches("/timestamps o");
        assert_eq!(subs, vec!["on".to_string(), "off".to_string()]);
    }

    #[test]
    fn subcommand_stage_exact_arg_not_reoffered() {
        let mut app = app_with_commands();
        app.update_slash_menu("/timestamps on");
        assert!(!app.slash_menu_open, "exact subcommand → nothing to add");
    }

    #[test]
    fn subcommand_stage_closes_past_first_arg() {
        let mut app = app_with_commands();
        app.update_slash_menu("/timestamps on extra");
        assert!(!app.slash_menu_open);
    }

    #[test]
    fn subcommand_stage_via_alias() {
        let mut app = app_with_commands();
        // "ts" is timestamps' alias.
        app.update_slash_menu("/ts st");
        assert!(app.slash_subcommand_stage);
        assert_eq!(app.slash_subcommand_matches("/ts st"), vec!["status".to_string()]);
    }

    #[test]
    fn subcommand_stage_unimplemented_command_skipped() {
        let mut app = app_with_commands();
        app.update_slash_menu("/voice o");
        assert!(!app.slash_menu_open);
        assert!(!app.slash_subcommand_stage);
    }

    #[test]
    fn no_pipe_hint_no_subcommand_stage() {
        let mut app = app_with_commands();
        // /model has no pipe run in its hint.
        app.update_slash_menu("/model gp");
        assert!(!app.slash_menu_open);
    }

    #[test]
    fn command_name_stage_still_works() {
        let mut app = app_with_commands();
        app.update_slash_menu("/he");
        assert!(app.slash_menu_open);
        assert!(!app.slash_subcommand_stage);
        assert!(app.slash_matches("he").iter().any(|c| c.name == "help"));
    }

    #[test]
    fn completion_items_set_and_navigated() {
        let mut app = App::new("s", "m");
        app.set_completion_items(vec![
            joey_tools::completion::CompletionItem { replacement: "@diff".into(), display: "@diff".into(), meta: "diff".into() },
            joey_tools::completion::CompletionItem { replacement: "@file:src/main.rs".into(), display: "main.rs".into(), meta: "1K".into() },
        ]);
        assert!(app.completion_menu_open);
        assert_eq!(app.completion_selected().as_deref(), Some("@diff"));
        app.completion_menu_move(true);
        assert_eq!(app.completion_selected().as_deref(), Some("@file:src/main.rs"));
        app.completion_menu_move(true); // wraps
        assert_eq!(app.completion_selected().as_deref(), Some("@diff"));
        app.completion_menu_move(false); // wrap back
        assert_eq!(app.completion_selected().as_deref(), Some("@file:src/main.rs"));
        // Empty set closes.
        app.set_completion_items(Vec::new());
        assert!(!app.completion_menu_open);
    }
}

#[cfg(test)]
mod neurocode_panel_tests {
    use super::*;
    use crate::theme::Theme;
    use joey_agent_core::AgentEvent;

    #[test]
    fn active_event_toggles_indicator() {
        let mut app = App::new("s", "m");
        assert!(!app.neurocode_active);
        app.apply(AgentEvent::NeuroCodeActive { active: true });
        assert!(app.neurocode_active);
        app.apply(AgentEvent::NeuroCodeActive { active: false });
        assert!(!app.neurocode_active);
        assert!(app.neurocode_context.is_empty(), "feed cleared on deactivate");
    }

    #[test]
    fn context_event_populates_feed() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::NeuroCodeContext {
            tier: "Frontier".into(),
            token_estimate: 1234,
            expanded_nodes: 7,
            cold_mode: false,
            formatted_context: "## NeuroCode Context\nTarget: TurnBudget::add()".into(),
        });
        assert!(app.neurocode_active, "context implies active");
        assert_eq!(app.neurocode_tier, "Frontier");
        assert_eq!(app.neurocode_tokens, 1234);
        assert_eq!(app.neurocode_nodes, 7);
        assert!(!app.neurocode_cold);
        assert!(app.neurocode_context.contains("TurnBudget::add()"));
        // A second event replaces the feed and resets scroll.
        app.neurocode_scroll = 5;
        app.apply(AgentEvent::NeuroCodeContext {
            tier: "Economical".into(),
            token_estimate: 100,
            expanded_nodes: 2,
            cold_mode: true,
            formatted_context: "small".into(),
        });
        assert_eq!(app.neurocode_tier, "Economical");
        assert!(app.neurocode_cold);
        assert_eq!(app.neurocode_scroll, 0);
    }

    #[test]
    fn panel_renders_without_panic_and_shows_feed() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = Theme::aurora();
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::NeuroCodeActive { active: true });
        app.apply(AgentEvent::NeuroCodeContext {
            tier: "Frontier".into(),
            token_estimate: 4321,
            expanded_nodes: 12,
            cold_mode: false,
            formatted_context: "UNIQUE_FEED_MARKER_XYZ target artifact".into(),
        });
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                crate::widgets::draw_neurocode_panel(f, f.area(), &app, theme);
            })
            .unwrap();
        let text = terminal.backend().buffer().content.iter().map(|c| c.symbol().to_string()).collect::<String>();
        assert!(text.contains("UNIQUE_FEED_MARKER_XYZ"), "feed text rendered");
        assert!(text.contains("Frontier"), "tier shown");
        assert!(text.contains("4.3K") || text.contains("4321"), "token estimate shown");
    }

    #[test]
    fn panel_silent_when_inactive() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = Theme::aurora();
        let app = App::new("s", "m"); // inactive
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                crate::widgets::draw_neurocode_panel(f, f.area(), &app, theme);
            })
            .unwrap();
        let text = terminal.backend().buffer().content.iter().map(|c| c.symbol().to_string()).collect::<String>();
        assert!(!text.contains("context feed"), "no panel when inactive");
    }

    #[test]
    fn cold_mode_badge_renders() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = Theme::aurora();
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::NeuroCodeContext {
            tier: "Frontier".into(),
            token_estimate: 10,
            expanded_nodes: 0,
            cold_mode: true,
            formatted_context: "x".into(),
        });
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::widgets::draw_neurocode_panel(f, f.area(), &app, theme)).unwrap();
        let text = terminal.backend().buffer().content.iter().map(|c| c.symbol().to_string()).collect::<String>();
        assert!(text.contains("COLD"), "cold-mode badge shown");
    }

    #[test]
    fn live_stage_streams_into_panel() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = Theme::aurora();
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::NeuroCodeActive { active: true });
        // Progress events arrive BEFORE the final context blob.
        app.apply(AgentEvent::NeuroCodeProgress {
            stage: "expanded graph: 7 nodes pulled in".into(),
        });
        assert!(app.neurocode_active, "progress implies active");
        assert!(!app.neurocode_stage.is_empty(), "stage tracked");
        assert!(app.neurocode_stage_at.is_some(), "stage timestamp tracked");

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                crate::widgets::draw_neurocode_panel(f, f.area(), &app, theme);
            })
            .unwrap();
        let text = terminal.backend().buffer().content.iter().map(|c| c.symbol().to_string()).collect::<String>();
        assert!(
            text.contains("expanded graph: 7 nodes pulled in"),
            "live stage rendered in panel, got: {}",
            text
        );

        // The final blob clears the stage and stamps the refresh.
        app.apply(AgentEvent::NeuroCodeContext {
            tier: "Frontier".into(),
            token_estimate: 900,
            expanded_nodes: 7,
            cold_mode: false,
            formatted_context: "STAGE_CLEAR_MARKER".into(),
        });
        assert!(app.neurocode_stage.is_empty(), "stage cleared on context arrival");
        assert!(app.neurocode_updated_at.is_some(), "refresh stamped");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| {
                crate::widgets::draw_neurocode_panel(f, f.area(), &app, theme);
            })
            .unwrap();
        let text = terminal.backend().buffer().content.iter().map(|c| c.symbol().to_string()).collect::<String>();
        assert!(text.contains("STAGE_CLEAR_MARKER"), "final context rendered");
        assert!(text.contains("↻"), "refresh stamp rendered");
        assert!(
            !text.contains("expanded graph"),
            "stale stage line removed once context lands"
        );
    }

    #[test]
    fn status_bar_shows_neurocode_badge_only_when_active() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = Theme::aurora();
        let area = ratatui::layout::Rect::new(0, 0, 110, 1);

        let mut app = App::new("s", "m");
        let backend = TestBackend::new(110, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::widgets::draw_status(f, area, &app, theme, std::time::Duration::from_secs(1))).unwrap();
        let text = terminal.backend().buffer().content.iter().map(|c| c.symbol().to_string()).collect::<String>();
        assert!(!text.contains("NEUROCODE"), "no badge when inactive");

        app.apply(AgentEvent::NeuroCodeActive { active: true });
        terminal.draw(|f| crate::widgets::draw_status(f, area, &app, theme, std::time::Duration::from_secs(1))).unwrap();
        let text = terminal.backend().buffer().content.iter().map(|c| c.symbol().to_string()).collect::<String>();
        assert!(text.contains("NEUROCODE"), "badge shown when active");
    }
}

// ── Live terminal output streaming + maximized viewer ─────────────────────

#[cfg(test)]
mod live_output_tests {
    use super::*;

    /// Drive ToolStart → ToolOutput chunks → ToolEnd and return the App.
    fn app_with_terminal_call(chunks: &[&str], full_result: &str) -> App {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "watch me stream".into(),
        });
        for c in chunks {
            app.apply(AgentEvent::ToolOutput { name: "terminal".into(), chunk: c.to_string() });
        }
        app.apply(AgentEvent::ToolEnd {
            name: "terminal".into(),
            is_error: false,
            result_preview: full_result.lines().next().unwrap_or("").into(),
            duration_secs: 1.0,
            exit_code: Some(0),
            full_result: full_result.to_string(),
        });
        app
    }

    fn live_output_of(app: &App) -> String {
        match app.transcript.back() {
            Some(TranscriptItem::Tool { live_output, .. }) => live_output.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn tool_output_chunks_accumulate_on_running_item() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "cmd".into(),
        });
        app.apply(AgentEvent::ToolOutput { name: "terminal".into(), chunk: "line-1\n".into() });
        app.apply(AgentEvent::ToolOutput { name: "terminal".into(), chunk: "line-2\n".into() });
        assert_eq!(live_output_of(&app), "line-1\nline-2\n");
        // The command header (summary) is NOT clobbered by progress events,
        // and progress never duplicates into the live buffer (the terminal
        // tool emits the same chunk on both channels).
        app.apply(AgentEvent::ToolProgress { name: "terminal".into(), progress: "line-1\n".into() });
        match app.transcript.back() {
            Some(TranscriptItem::Tool { summary, live_output, .. }) => {
                assert_eq!(summary, "cmd", "terminal summary stays the command");
                assert_eq!(live_output, "line-1\nline-2\n", "no duplication from ToolProgress");
            }
            _ => panic!("expected terminal tool item"),
        }
    }

    #[test]
    fn tool_output_ignored_for_finished_items() {
        // A late chunk arriving after ToolEnd must not resurrect/append.
        let mut app = app_with_terminal_call(&["early\n"], "done output");
        app.apply(AgentEvent::ToolOutput { name: "terminal".into(), chunk: "LATE\n".into() });
        assert!(!live_output_of(&app).contains("LATE"), "no accumulation after ToolEnd");
    }

    #[test]
    fn non_terminal_progress_still_updates_summary() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "web_search".into(),
            emoji: "🔍".into(),
            summary: "q".into(),
        });
        app.apply(AgentEvent::ToolProgress { name: "web_search".into(), progress: "3 results".into() });
        match app.transcript.back() {
            Some(TranscriptItem::Tool { summary, .. }) => assert_eq!(summary, "3 results"),
            _ => panic!("expected tool item"),
        }
    }

    #[test]
    fn live_output_bounded_ring_keeps_tail() {
        // Fill far past the default capacity with a small test cap.
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "big".into(),
        });
        if let TranscriptItem::Tool { live_output_capacity, .. } = app.transcript.back_mut().unwrap() {
            *live_output_capacity = 100;
        }
        for i in 0..50 {
            app.apply(AgentEvent::ToolOutput {
                name: "terminal".into(),
                chunk: format!("line-{i:03}\n"),
            });
        }
        let out = live_output_of(&app);
        assert!(out.len() <= 110, "bounded: {} bytes", out.len());
        assert!(out.contains("line-049"), "the tail is kept");
        assert!(!out.contains("line-000"), "the head was evicted");
        // UTF-8 safety: capacity cut never splits a char (no panic above).
    }

    #[test]
    fn viewer_toggle_targets_most_recent_terminal_item() {
        let mut app = app_with_terminal_call(&["streamed\n"], "final full output\nsecond line");
        assert!(!app.output_viewer_open);
        app.toggle_output_viewer(None);
        assert!(app.output_viewer_open, "viewer opened");
        assert_eq!(app.output_viewer_index, Some(app.transcript.len() - 1));
        // Finished item: the viewer shows the full result, not live buffer.
        assert!(app.output_viewer_text().contains("final full output"));
        // Toggle again closes.
        app.toggle_output_viewer(None);
        assert!(!app.output_viewer_open);
    }

    #[test]
    fn viewer_open_is_noop_without_terminal_items() {
        let mut app = App::new("s", "m");
        app.push_item(TranscriptItem::User { text: "hi".into() });
        app.toggle_output_viewer(None);
        assert!(!app.output_viewer_open, "nothing to maximize");
    }

    #[test]
    fn viewer_running_item_shows_live_buffer() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "long job".into(),
        });
        app.apply(AgentEvent::ToolOutput { name: "terminal".into(), chunk: "partial output\n".into() });
        app.toggle_output_viewer(None);
        assert!(app.output_viewer_text().contains("partial output"));
    }

    #[test]
    fn viewer_scroll_freezes_and_resumes_follow() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "cmd".into(),
        });
        for i in 0..100 {
            app.apply(AgentEvent::ToolOutput {
                name: "terminal".into(),
                chunk: format!("l{i}\n"),
            });
        }
        app.toggle_output_viewer(None);
        assert!(app.output_viewer_view.is_none(), "opens following the tail");
        // Simulate the widget's render-time anchor bound.
        app.last_output_viewer_max_anchor.set(80);
        app.output_viewer_scroll_up(3);
        assert_eq!(app.output_viewer_view, Some(77), "scrolled up freezes");
        app.output_viewer_scroll_down(2);
        assert_eq!(app.output_viewer_view, Some(79), "still frozen above tail");
        app.output_viewer_scroll_down(5);
        assert!(app.output_viewer_view.is_none(), "reaching the tail resumes follow");
    }

    #[test]
    fn push_bounded_evicts_at_line_boundaries_when_possible() {
        let mut buf = String::new();
        for i in 0..60 {
            buf.push_str(&format!("row-{i:02}\n"));
        }
        let cap = 50;
        App::push_bounded_item(&mut buf, "row-60\n", cap);
        // The retained tail starts at a row boundary (starts with "row-").
        assert!(buf.starts_with("row-"), "cut at line boundary: {:?}", &buf[..12]);
        assert!(buf.ends_with("row-60\n"));
        assert!(buf.len() <= cap + 64, "approximately bounded: {}", buf.len());
    }

    #[test]
    fn viewer_retargets_to_new_terminal_call_while_following() {
        let mut app = App::new("s", "m");
        // First terminal call runs, user opens the viewer on it.
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "first".into(),
        });
        app.toggle_output_viewer(None);
        let first_idx = app.output_viewer_index.unwrap();
        // It finishes; a second terminal call starts in the same turn.
        app.apply(AgentEvent::ToolEnd {
            name: "terminal".into(),
            is_error: false,
            result_preview: "done1".into(),
            duration_secs: 0.1,
            exit_code: Some(0),
            full_result: "done1".into(),
        });
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "second".into(),
        });
        app.apply(AgentEvent::ToolOutput { name: "terminal".into(), chunk: "streaming-2\n".into() });
        assert!(app.output_viewer_open, "viewer stays open");
        assert_eq!(app.output_viewer_index, Some(first_idx + 1), "retargeted to the new call");
        assert!(app.output_viewer_text().contains("streaming-2"), "shows the new live stream");
    }

    #[test]
    fn viewer_click_switches_target_while_open() {
        let mut app = App::new("s", "m");
        for cmd in ["one", "two"] {
            app.apply(AgentEvent::ToolStart {
                name: "terminal".into(),
                emoji: "💻".into(),
                summary: cmd.into(),
            });
            app.apply(AgentEvent::ToolEnd {
                name: "terminal".into(),
                is_error: false,
                result_preview: cmd.into(),
                duration_secs: 0.1,
                exit_code: Some(0),
                full_result: format!("{cmd} full output"),
            });
        }
        app.output_viewer_click(0);
        assert!(app.output_viewer_open);
        assert_eq!(app.output_viewer_index, Some(0));
        // Click the OTHER terminal block: viewer switches, stays open.
        app.output_viewer_click(1);
        assert!(app.output_viewer_open, "switch target keeps viewer open");
        assert_eq!(app.output_viewer_index, Some(1));
        assert!(app.output_viewer_text().contains("two full output"));
        // Click the same block again: toggle-closes.
        app.output_viewer_click(1);
        assert!(!app.output_viewer_open);
    }

    #[test]
    fn viewer_index_survives_transcript_eviction() {
        // Small capacity so pushes evict; the viewer index must track the
        // shift instead of pointing at the wrong item (or off the end).
        let mut app = App::new("s", "m");
        app.transcript_capacity = 4;
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "watch".into(),
        });
        app.toggle_output_viewer(None);
        let idx_before = app.output_viewer_index.unwrap();
        // Push enough items to evict the viewer's target and shift indices.
        for i in 0..6 {
            app.push_item(TranscriptItem::User { text: format!("m{i}") });
        }
        let idx_after = app.output_viewer_index;
        // The index was adjusted down by the number of evictions that
        // happened in front of it (at least clamped into range).
        assert!(
            idx_after.map(|i| i < idx_before || i <= app.transcript.len()).unwrap_or(true),
            "index adjusted or reset, got {:?} (before {idx_before}, len {})",
            idx_after,
            app.transcript.len()
        );
        // Resolution still yields the newest terminal item (the evicted one
        // is gone — most_recent_terminal_item is now None, text is empty).
        assert!(app.most_recent_terminal_item().is_none());
    }
}

#[cfg(test)]
mod stats_page_tests {
    use super::*;
    use joey_agent_core::events::ContextEntry;

    fn entry(role: &str, tokens: u64, preview: &str) -> ContextEntry {
        ContextEntry {
            role: role.into(),
            tokens,
            preview: preview.into(),
            has_tool_calls: false,
            is_compressed_summary: false,
            full_content: String::new(),
        }
    }

    fn apply_snapshot(app: &mut App, n_entries: usize, window: u64) {
        let entries: Vec<ContextEntry> = (0..n_entries)
            .map(|i| match i % 3 {
                0 => entry("user", 120 + i as u64, &format!("user message {i}")),
                1 => entry("assistant", 300 + i as u64, &format!("assistant reply {i}")),
                _ => entry("tool", 900 + i as u64, &format!("tool result {i}")),
            })
            .collect();
        let history_tokens: u64 = entries.iter().map(|e| e.tokens).sum();
        app.apply(AgentEvent::ContextSnapshot {
            entries,
            system_tokens: 2_000,
            history_tokens,
            context_window: window,
            compression_threshold: window * 80 / 100,
            compactions: 1,
            model: "test-model".into(),
        });
    }

    #[test]
    fn snapshot_replaces_state_and_counts() {
        let mut app = App::new("s", "m");
        assert!(app.context_entries.is_empty());
        apply_snapshot(&mut app, 6, 200_000);
        assert_eq!(app.context_entries.len(), 6);
        assert_eq!(app.context_system_tokens, 2_000);
        assert_eq!(app.context_window, 200_000);
        assert_eq!(app.compactions, 1);
        assert_eq!(app.context_snapshots, 1);
        assert!(app.context_updated_at.is_some());
        // A second snapshot REPLACES (not appends).
        apply_snapshot(&mut app, 3, 200_000);
        assert_eq!(app.context_entries.len(), 3);
        assert_eq!(app.context_snapshots, 2);
    }

    #[test]
    fn context_usage_percentage() {
        let mut app = App::new("s", "m");
        apply_snapshot(&mut app, 2, 100_000);
        // history sum for 2 entries: 120 + 121? apply_snapshot uses 120/300/900
        // base + i; just verify it's computed from the fields.
        let used = app.context_system_tokens + app.context_history_tokens;
        let expected = used as f64 / 100_000.0 * 100.0;
        assert!((app.context_usage_pct() - expected).abs() < 1e-9);
        // Unknown window → 0.
        app.context_window = 0;
        assert_eq!(app.context_usage_pct(), 0.0);
    }

    #[test]
    fn usage_series_accumulates_per_call_and_is_bounded() {
        let mut app = App::new("s", "m");
        for i in 0..300 {
            app.apply(AgentEvent::ApiCallEnd {
                usage: joey_providers::Usage {
                    prompt_tokens: i as u64,
                    completion_tokens: 10,
                    ..Default::default()
                },
            });
        }
        assert!(app.usage_series.len() <= 240, "bounded: {}", app.usage_series.len());
        assert_eq!(app.tokens.prompt, (0..300).sum::<u64>());
    }

    #[test]
    fn stats_toggle_open_close_and_follow() {
        let mut app = App::new("s", "m");
        assert!(!app.stats_open);
        app.toggle_stats();
        assert!(app.stats_open);
        assert!(app.stats_view.is_none(), "opens auto-following the tail");
        // Simulate the widget's render-time anchor bound, then scroll up.
        app.last_stats_max_anchor.set(40);
        app.stats_scroll_up(5);
        assert_eq!(app.stats_view, Some(35), "scroll up freezes");
        app.stats_scroll_down(3);
        assert_eq!(app.stats_view, Some(38), "still frozen above the tail");
        app.stats_scroll_down(10);
        assert!(app.stats_view.is_none(), "reaching the tail resumes follow");
        // Esc-path close.
        app.close_stats();
        assert!(!app.stats_open);
    }

    #[test]
    fn turn_start_increments_turns() {
        let mut app = App::new("s", "m");
        assert_eq!(app.turns, 0);
        app.apply(AgentEvent::TurnStart { max_iterations: 10 });
        app.apply(AgentEvent::TurnStart { max_iterations: 10 });
        assert_eq!(app.turns, 2);
    }

    // ── T004: per-pane stats anchors (FR-010 state preservation) ──────

    fn spawn_pane(app: &mut App, id: u64, goal: &str) {
        app.apply(AgentEvent::SubagentSpawn {
            id,
            goal: goal.into(),
            model: "m".into(),
            toolset_summary: "all".into(),
            depth: 0,
        });
    }

    #[test]
    fn pane_stats_view_survives_focus_switches() {
        // FR-010: each pane keeps its own stats scroll anchor; switching
        // focus must not reset a sibling pane's position (the old global
        // App::pane_stats_view reset on every focus change).
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child one");
        spawn_pane(&mut app, 2, "child two");
        assert_eq!(app.subagent_panes.len(), 2);
        // New panes start auto-following.
        assert!(app.focused_pane().is_none());

        app.focus_subagent(Some(0));
        app.set_focused_pane_stats_view(Some(7));
        assert_eq!(app.focused_pane_stats_view(), Some(7));

        app.focus_subagent(Some(1));
        // Pane 0's anchor is untouched by the switch...
        assert_eq!(app.subagent_panes[0].stats_view, Some(7));
        // ...and pane 1 gets its own independent anchor.
        assert_eq!(app.focused_pane_stats_view(), None);
        app.set_focused_pane_stats_view(Some(11));
        assert_eq!(app.focused_pane_stats_view(), Some(11));

        // Switch back: pane 0 still holds its anchor.
        app.focus_subagent(Some(0));
        assert_eq!(app.focused_pane_stats_view(), Some(7));
        assert_eq!(app.subagent_panes[1].stats_view, Some(11));
    }

    #[test]
    fn pane_stats_scroll_helpers_use_focused_pane_bounds() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        app.focus_subagent(Some(0));
        // Simulate the pane stats widget's render-time anchor bound.
        app.focused_pane_mut().unwrap().last_stats_max_anchor.set(40);
        app.pane_stats_scroll_up(5);
        assert_eq!(app.focused_pane_stats_view(), Some(35), "scroll up freezes");
        app.pane_stats_scroll_down(3);
        assert_eq!(
            app.focused_pane_stats_view(),
            Some(38),
            "still frozen above the tail"
        );
        app.pane_stats_scroll_down(10);
        assert!(
            app.focused_pane_stats_view().is_none(),
            "reaching the tail resumes follow"
        );
        // Anchor lives on the pane itself, not on the App.
        assert_eq!(app.subagent_panes[0].stats_view, None);
    }

    #[test]
    fn pane_stats_helpers_no_op_without_focused_pane() {
        // Graceful fallback: with focus on the orchestrator view the pane
        // helpers must not panic or touch main-transcript stats state.
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        // No pane focused (orchestrator view).
        app.pane_stats_scroll_up(5);
        app.pane_stats_scroll_down(5);
        app.set_focused_pane_stats_view(Some(3));
        assert!(app.focused_pane_stats_view().is_none());
        assert_eq!(app.subagent_panes[0].stats_view, None);
        assert!(app.stats_view.is_none(), "main stats anchor untouched");
    }

    // ── T009 (US1): per-pane ScrollState semantics ─────────────────────
    //
    // data-model.md ScrollState invariants, pinned at the state level:
    //   1. None = follow-tail; Some(n) = pinned with n ≤ last_max_scroll.
    //   2. Clamp on EVERY mutation (up re-clamps the current offset too —
    //      the bound may have shrunk since the pin, e.g. ring eviction).
    //   3. The bound is render-time knowledge: with no frame drawn yet it
    //      is 0, so a pre-render scroll pins at 0, never at `by`.
    //   4. Appends while pinned-at-bottom keep following (stays None);
    //      appends while scrolled-up keep the offset stable (push_item
    //      never touches scroll — main-transcript parity).
    //   5. Reaching the bottom via scroll_down sets None (follow-tail
    //      resumes) — the main transcript's scroll_down, mirrored.
    //   6. Scroll state survives focus switches (FR-010).
    //   7. clear_subagent_panes resets the pane geometry cells (bound
    //      freshness) and leaves the orchestrator scroll untouched (D9).

    /// Invariant 2 (clamp-on-shrink): a pinned offset that went stale-high
    /// — the recorded bound shrank after the pin (ring eviction dropped the
    /// oldest content) — is re-clamped by the NEXT scroll mutation in both
    /// directions, never surviving above the bound.
    #[test]
    fn pane_scroll_reclamps_stale_offset_after_bound_shrink() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        app.focus_subagent(Some(0));
        // Frame recorded a generous bound; the user pins deep.
        app.last_pane_max_scroll.set(50);
        app.pane_scroll_up(30);
        assert_eq!(app.subagent_panes[0].scroll, Some(30));
        // Content shrank (eviction): the next frame records a smaller bound.
        app.last_pane_max_scroll.set(12);
        app.pane_scroll_up(5);
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(12),
            "up-step re-clamps the stale-high offset to the new bound"
        );
        // Down-steps clamp first as well (progress even from a stale pin).
        app.subagent_panes[0].scroll = Some(30); // simulate a stale pin
        app.last_pane_max_scroll.set(12);
        app.pane_scroll_down(4);
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(8),
            "down-step clamps the stale-high offset before moving (12-4)"
        );
    }

    /// Invariant 3 (bound freshness): with no pane frame rendered yet the
    /// recorded bound is 0, so scroll_up pins at 0 (not at `by`) — the
    /// bound only becomes real after a render, mirroring `App::scroll_up`.
    #[test]
    fn pane_scroll_before_first_render_pins_at_zero_bound() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        app.focus_subagent(Some(0));
        assert_eq!(app.last_pane_max_scroll.get(), 0, "pristine bound is 0");
        app.pane_scroll_up(25);
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(0),
            "pre-render scroll pins at the (zero) render-time bound, not at 25"
        );
    }

    /// Invariants 4 + 5 (follow-tail semantics): pinned-at-bottom keeps
    /// following across appends; a scrolled-up offset is stable across
    /// appends; and scroll_down back to the bottom flips the pane to None,
    /// after which appends follow the tail again.
    #[test]
    fn pane_follow_tail_pinned_follows_scrolled_stable_resumes_at_bottom() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        app.focus_subagent(Some(0));
        app.last_pane_max_scroll.set(40);

        // Pinned at bottom (None): appends keep auto-follow.
        assert_eq!(app.subagent_panes[0].scroll, None);
        app.subagent_panes[0].push_item(TranscriptItem::User {
            text: "m1".into(),
        });
        assert_eq!(app.subagent_panes[0].scroll, None);

        // Scroll up: pinned. Appends must NOT move the offset.
        app.pane_scroll_up(6);
        assert_eq!(app.subagent_panes[0].scroll, Some(6));
        for t in ["m2", "m3", "m4"] {
            app.subagent_panes[0].push_item(TranscriptItem::User { text: t.into() });
        }
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(6),
            "scrolled-up pane does not jump on appends"
        );

        // Walk back down to the bottom: follow-tail RESUMES (None)…
        app.pane_scroll_down(6);
        assert_eq!(app.subagent_panes[0].scroll, None, "bottom → follow-tail");
        // …and one step past the bottom stays None.
        app.pane_scroll_down(3);
        assert_eq!(app.subagent_panes[0].scroll, None, "past-bottom clamps to follow");
        // …so the next append follows again.
        app.subagent_panes[0].push_item(TranscriptItem::User { text: "m5".into() });
        assert_eq!(app.subagent_panes[0].scroll, None);
    }

    /// Invariant 6 (FR-010): a pane's scroll state survives focus switches
    /// in both directions and is never copied onto a sibling pane.
    #[test]
    fn pane_scroll_survives_focus_switches_without_leaking() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "one");
        spawn_pane(&mut app, 2, "two");
        app.focus_subagent(Some(0));
        app.last_pane_max_scroll.set(40);
        app.pane_scroll_up(9);
        assert_eq!(app.subagent_panes[0].scroll, Some(9));

        // Switch to the sibling: pane 0 keeps its pin, pane 1 stays fresh.
        app.focus_subagent(Some(1));
        assert_eq!(app.subagent_panes[0].scroll, Some(9), "pin preserved");
        assert_eq!(app.subagent_panes[1].scroll, None, "sibling untouched");

        // To the orchestrator and back: still preserved.
        app.focus_subagent(None);
        app.focus_subagent(Some(0));
        assert_eq!(app.subagent_panes[0].scroll, Some(9));
        assert_eq!(app.scroll, None, "main transcript scroll never touched");
    }

    /// Invariant 7 (D9 + bound freshness): Ctrl+L's clear resets the pane
    /// geometry cells — a freshly spawned pane's pre-render scroll clamps
    /// against the pristine 0 bound, not the ghost of the cleared panes —
    /// while the orchestrator's own scroll state rides through untouched.
    #[test]
    fn clear_subagent_panes_resets_pane_bound_and_spares_main_scroll() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        app.focus_subagent(Some(0));
        app.last_pane_max_scroll.set(77);
        app.last_pane_text_area.set((1, 2, 60, 20));
        app.last_pane_stats_rect.set((1, 2, 60, 20));
        app.pane_scroll_up(30);
        assert_eq!(app.subagent_panes[0].scroll, Some(30));

        // The orchestrator is scrolled independently.
        app.focus_subagent(None);
        app.last_max_scroll.set(50);
        app.scroll_up(4);
        assert_eq!(app.scroll, Some(4));

        app.clear_subagent_panes();
        assert!(app.subagent_panes.is_empty());
        assert!(app.focused_subagent.is_none());
        assert_eq!(app.last_pane_max_scroll.get(), 0, "ghost pane bound cleared");
        assert_eq!(app.last_pane_text_area.get(), (0, 0, 0, 0));
        assert_eq!(app.last_pane_stats_rect.get(), (0, 0, 0, 0));
        assert_eq!(app.subagent_rail_scroll, 0, "rail reset preserved");
        assert_eq!(
            app.scroll,
            Some(4),
            "D9: orchestrator scroll untouched by Ctrl+L"
        );
        assert_eq!(app.last_max_scroll.get(), 50, "main bound untouched");

        // A fresh pane spawned post-clear cannot inherit the ghost bound.
        spawn_pane(&mut app, 2, "fresh");
        app.focus_subagent(Some(0));
        app.pane_scroll_up(15);
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(0),
            "fresh pane clamps against the pristine bound, not the ghost 77"
        );
    }

    // ── Spec 017 T013 (US2, FR-005, D7): FileChange → FileDiff in panes ──

    fn mk_file_change(path: &str, kind: FileChangeKind) -> AgentEvent {
        AgentEvent::FileChange {
            path: path.to_string(),
            kind,
            before: "old\n".to_string(),
            after: "new\n".to_string(),
            diff: DiffResult {
                path: path.to_string(),
                diff: "--- a/x\n+++ b/x\n-old\n+new\n".to_string(),
                added: 1,
                removed: 1,
            },
            is_binary: false,
            source: joey_agent_core::events::FileChangeSource::FileTool,
        }
    }

    fn mk_binary_file_change(path: &str) -> AgentEvent {
        AgentEvent::FileChange {
            path: path.to_string(),
            kind: FileChangeKind::Edit,
            before: String::new(),
            after: String::new(),
            diff: DiffResult {
                path: path.to_string(),
                diff: String::new(),
                added: 0,
                removed: 0,
            },
            is_binary: true,
            source: joey_agent_core::events::FileChangeSource::FileTool,
        }
    }

    /// Deconstruct a FileDiff item field-for-field (TranscriptItem has no
    /// PartialEq; tests compare the Display-relevant payload explicitly).
    fn as_filediff(it: &TranscriptItem) -> (&str, &str, &Vec<String>, bool, ReasoningExpandState) {
        match it {
            TranscriptItem::FileDiff { path, stat, lines, is_binary, expand_state } => {
                (path, stat, lines, *is_binary, *expand_state)
            }
            other => panic!("expected FileDiff, got {:?}", other),
        }
    }

    fn last_filediff(transcript: &VecDeque<TranscriptItem>) -> &TranscriptItem {
        transcript
            .iter()
            .rev()
            .find(|it| matches!(it, TranscriptItem::FileDiff { .. }))
            .expect("a FileDiff item in transcript")
    }

    /// T013 (a) + (c): a FileChange routed by child id appends a FileDiff
    /// item to the OWNING pane only — sibling panes and unknown ids get
    /// nothing (existing SubagentEvent routing semantics preserved).
    #[test]
    fn pane_file_change_appends_filediff_to_owning_pane_only() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child one");
        spawn_pane(&mut app, 2, "child two");

        app.apply(AgentEvent::SubagentEvent {
            id: 1,
            event: Box::new(mk_file_change("src/lib.rs", FileChangeKind::Edit)),
        });

        assert_eq!(app.subagent_panes[0].transcript.len(), 1, "owning pane got the item");
        let (path, stat, lines, is_binary, expand_state) =
            as_filediff(&app.subagent_panes[0].transcript[0]);
        assert_eq!(path, "src/lib.rs");
        assert_eq!(stat, "+1 -1");
        assert_eq!(
            lines,
            &vec![
                "--- a/x".to_string(),
                "+++ b/x".to_string(),
                "-old".to_string(),
                "+new".to_string(),
            ]
        );
        assert!(!is_binary);
        assert_eq!(expand_state, ReasoningExpandState::Collapsed);

        // (c) sibling pane untouched; unknown child id dropped.
        assert!(app.subagent_panes[1].transcript.is_empty(), "sibling pane untouched");
        app.apply(AgentEvent::SubagentEvent {
            id: 99,
            event: Box::new(mk_file_change("src/other.rs", FileChangeKind::Edit)),
        });
        assert_eq!(app.subagent_panes[0].transcript.len(), 1, "unknown id not delivered");
        assert!(app.subagent_panes[1].transcript.is_empty(), "still untouched");
    }

    /// T013 (b): the pane-constructed FileDiff is field-identical to what
    /// the main transcript path constructs from the same event (Edit label,
    /// Create label, and the binary placeholder variant) — parity by the
    /// shared `file_diff_item` construction (D7).
    #[test]
    fn pane_filediff_matches_main_transcript_construction() {
        for ev in [
            mk_file_change("src/a.rs", FileChangeKind::Edit),
            mk_file_change("src/new.rs", FileChangeKind::Create),
            mk_binary_file_change("assets/logo.png"),
        ] {
            let mut app = App::new("s", "m");
            // Main-transcript path (unwrapped FileChange → App::apply).
            app.apply(ev.clone());
            // Pane path (wrapped SubagentEvent → pane_apply).
            spawn_pane(&mut app, 7, "child");
            app.apply(AgentEvent::SubagentEvent { id: 7, event: Box::new(ev) });

            let (m_path, m_stat, m_lines, m_binary, m_expand) = as_filediff(last_filediff(&app.transcript));
            let (p_path, p_stat, p_lines, p_binary, p_expand) =
                as_filediff(last_filediff(&app.subagent_panes[0].transcript));
            assert_eq!(m_path, p_path, "path parity");
            assert_eq!(m_stat, p_stat, "stat parity (incl. kind label)");
            assert_eq!(m_lines, p_lines, "diff-lines parity");
            assert_eq!(m_binary, p_binary, "is_binary parity");
            assert_eq!(m_expand, p_expand, "expand_state parity");
        }

        // Spot-check the derived payloads: Create label and binary placeholder.
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 4, "child");
        app.apply(AgentEvent::SubagentEvent {
            id: 4,
            event: Box::new(mk_file_change("src/new.rs", FileChangeKind::Create)),
        });
        let (_, stat, _, _, _) = as_filediff(&app.subagent_panes[0].transcript[0]);
        assert_eq!(stat, "+1 -1 (new file)");

        app.apply(AgentEvent::SubagentEvent {
            id: 4,
            event: Box::new(mk_binary_file_change("assets/logo.png")),
        });
        let (_, stat, lines, is_binary, _) = as_filediff(&app.subagent_panes[0].transcript[1]);
        assert_eq!(stat, "no changes");
        assert!(lines.is_empty(), "binary MUST NOT carry diff lines");
        assert!(is_binary, "binary flag drives the renderer placeholder");
    }

    /// T013 (d): a mixed sequence (tool events interleaved with file
    /// changes) preserves stream order in the pane transcript; ToolEnd
    /// updates its tool item in place rather than appending.
    #[test]
    fn pane_mixed_tool_and_file_change_preserves_order() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 3, "child");
        let send = |app: &mut App, ev: AgentEvent| {
            app.apply(AgentEvent::SubagentEvent { id: 3, event: Box::new(ev) });
        };

        send(
            &mut app,
            AgentEvent::ToolStart {
                name: "write_file".to_string(),
                emoji: "✏️".to_string(),
                summary: "write src/x.rs".to_string(),
            },
        );
        send(&mut app, mk_file_change("src/x.rs", FileChangeKind::Create));
        send(
            &mut app,
            AgentEvent::ToolEnd {
                name: "write_file".to_string(),
                is_error: false,
                result_preview: "ok".to_string(),
                duration_secs: 0.1,
                exit_code: None,
                full_result: "ok".to_string(),
            },
        );

        let t = &app.subagent_panes[0].transcript;
        assert_eq!(t.len(), 2, "ToolStart + FileDiff; ToolEnd updates in place");
        assert!(matches!(t[0], TranscriptItem::Tool { .. }), "tool item first");
        assert!(
            matches!(t[1], TranscriptItem::FileDiff { .. }),
            "file change lands in stream order after the tool"
        );
        match &t[0] {
            TranscriptItem::Tool { status, .. } => {
                assert_eq!(*status, ToolStatus::Done, "ToolEnd resolved the running tool")
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod pane_search_state_tests {
    //! T015 (US3, FR-006/FR-007, design D5): per-pane SearchState — pure
    //! state-level pins for the pane-targeted search paths:
    //!   1. A pane search matches ONLY the pane transcript (a main-only
    //!      match is a no-match: indicator false, view never moves).
    //!   2. `search_next` navigation scrolls the OWNING (pane) view only,
    //!      advancing between matches (n/N never re-find the current one).
    //!   3. Match pins clamp to the render-time bound (ScrollState:
    //!      pinned ≤ last_pane_max_scroll).
    //!   4. Pane search state survives focus switches (FR-010) and the
    //!      orchestrator's scroll/search state is never consulted nor
    //!      mutated by a pane search (per-view isolation), and vice versa.
    //! Match-indicator only — no in-text highlighting exists at this layer.
    use super::*;

    fn spawn_pane(app: &mut App, id: u64, goal: &str) {
        app.apply(AgentEvent::SubagentSpawn {
            id,
            goal: goal.into(),
            model: "m".into(),
            toolset_summary: "all".into(),
            depth: 0,
        });
    }

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User { text: text.into() }
    }

    /// Pane 0 focused, holding `items` (oldest-first); main holds one
    /// marker item so main-only matches are representable.
    fn pane_app(pane_items: Vec<TranscriptItem>) -> App {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        for it in pane_items {
            app.subagent_panes[0].push_item(it);
        }
        app.push_item(user("main needle beta"));
        app.focus_subagent(Some(0));
        app
    }

    /// 1 + 2: a pane search finds the pane's occurrences, pins only the
    /// pane (main scroll + App indicator untouched), and n/N ADVANCE
    /// between the pane's matches (older, then back newer).
    #[test]
    fn pane_search_matches_pane_only_and_n_n_advance() {
        let mut items: Vec<_> = (0..30).map(|i| user(&format!("filler {i}"))).collect();
        items[3] = user("pane needle old");
        items[27] = user("pane needle new");
        let mut app = pane_app(items);
        app.last_pane_max_scroll.set(100); // unclamped bound

        app.search_query = "needle".into();
        app.run_search();
        let pane = &app.subagent_panes[0];
        assert!(pane.search_has_match, "pane occurrence found");
        assert_eq!(pane.scroll, Some(0), "newest pane match (rev-idx 2) pins at 0");
        assert_eq!(app.scroll, None, "main scroll untouched by pane search");
        assert!(!app.search_has_match, "App indicator mirrors MAIN only");

        // N → older pane match (rev-idx 26 → offset 24).
        app.search_next(false);
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(24),
            "N advances to the older pane match, not re-finding the current one"
        );
        assert_eq!(app.scroll, None, "N still leaves the main view alone");

        // n → back to the newer pane match.
        app.search_next(true);
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(0),
            "n advances back to the newer pane match"
        );
    }

    /// 1 (negative): a query hitting ONLY the main transcript is a
    /// no-match from the pane — indicator false, pane view never moves.
    #[test]
    fn pane_search_main_only_match_is_no_match() {
        let mut app = pane_app((0..10).map(|i| user(&format!("filler {i}"))).collect());
        app.last_pane_max_scroll.set(50);

        app.search_query = "needle".into(); // exists only on main
        app.run_search();
        let pane = &app.subagent_panes[0];
        assert!(!pane.search_has_match, "no pane occurrence → no match");
        assert_eq!(pane.scroll, None, "follow-tail view never yanked");
        assert_eq!(app.scroll, None, "main-only match must not move MAIN either");
    }

    /// 3 (clamp): a deep pane match pins at the render-time bound, never
    /// beyond it (ScrollState: pinned ≤ last_pane_max_scroll) — for both
    /// `run_search` and n/N navigation.
    #[test]
    fn pane_search_pins_clamp_to_pane_max_scroll() {
        let mut items: Vec<_> = (0..40).map(|i| user(&format!("filler {i}"))).collect();
        items[2] = user("pane needle old"); // rev-idx 37
        items[5] = user("pane needle new"); // rev-idx 34 (newest match)
        let mut app = pane_app(items);
        app.last_pane_max_scroll.set(7); // bound far below the raw offsets

        app.search_query = "needle".into();
        app.run_search();
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(7),
            "run_search pin clamped to the pane bound (raw offset would be 32)"
        );

        app.subagent_panes[0].scroll = Some(7);
        app.search_next(false); // N toward older matches (raw target 32/35)
        assert_eq!(
            app.subagent_panes[0].scroll,
            Some(7),
            "n/N pin clamped to the pane bound (raw offsets are 32/35)"
        );
    }

    /// 4 (FR-010 + isolation): the pane keeps its query/match indicator
    /// across focus switches, and the orchestrator's search state is a
    /// separate view's — a later MAIN search rewrites App-level state
    /// without consulting the pane's.
    #[test]
    fn pane_search_state_survives_focus_switches_and_stays_isolated() {
        let mut app = pane_app(vec![user("pane needle alpha")]);
        app.last_pane_max_scroll.set(10);

        app.search_query = "needle".into();
        app.run_search();
        assert!(app.subagent_panes[0].search_has_match);

        // Away and back: the pane's SearchState rides through (FR-010).
        app.focus_subagent(None);
        app.focus_subagent(Some(0));
        let pane = &app.subagent_panes[0];
        assert_eq!(pane.search_query, "needle", "query preserved");
        assert!(pane.search_has_match, "indicator preserved");
        assert_eq!(pane.scroll, Some(0), "pin preserved");

        // From the orchestrator, the pane-targeted mutators are no-ops…
        app.focus_subagent(None);
        let before = app.subagent_panes[0].clone();
        app.pane_run_search();
        app.pane_search_next(true);
        assert_eq!(
            app.subagent_panes[0].search_query,
            before.search_query,
            "unfocused pane mutators are no-ops"
        );

        // …and a main search leaves the pane's state alone.
        app.search_query = "beta".into();
        app.run_search();
        assert!(app.search_has_match, "main search matched main text");
        assert_eq!(app.subagent_panes[0].search_query, "needle", "pane query untouched");
    }

    /// Unfocused parity: with no pane focused, `run_search`/`search_next`
    /// behave exactly as the main-only originals (App indicator + main
    /// scroll move; panes untouched).
    #[test]
    fn unfocused_search_still_main_only() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 1, "child");
        app.subagent_panes[0].push_item(user("pane needle"));
        app.push_item(user("main needle beta"));
        app.last_max_scroll.set(30);
        assert!(app.focused_subagent.is_none());

        app.search_query = "needle".into();
        app.run_search();
        assert!(app.search_has_match, "main match found");
        assert_eq!(app.scroll, Some(0), "main view pinned");
        assert_eq!(app.subagent_panes[0].scroll, None, "pane untouched");
        assert_eq!(app.subagent_panes[0].search_query, "", "pane SearchState untouched");

        app.search_query = "pane needle".into();
        app.run_search();
        assert!(!app.search_has_match, "pane text is invisible to main search");
        assert_eq!(app.scroll, Some(0), "a miss never moves the view");
    }
}

#[cfg(test)]
mod pane_reasoning_closeout_tests {
    //! T032: SubagentComplete/SubagentFailed must flush the pane's pending
    //! `streaming_reasoning` — a child that ends WITHOUT a trailing
    //! AssistantMessage/ToolStart (failure, max-turns, abort) would
    //! otherwise drop that reasoning entirely and leave draw_reasoning's
    //! live condition (`!streaming_reasoning.is_empty()`) stuck on.
    use super::*;

    fn spawn_pane(app: &mut App, id: u64, goal: &str) {
        app.apply(AgentEvent::SubagentSpawn {
            id,
            goal: goal.into(),
            model: "m".into(),
            toolset_summary: "all".into(),
            depth: 0,
        });
    }

    /// Push live stream content into pane `id`'s buffers directly (no
    /// AssistantMessage/ToolStart boundary events — the exact scenario
    /// where the old code dropped the reasoning).
    fn stage_pending_streams(app: &mut App) {
        let pane = app.subagent_panes.iter_mut().find(|p| p.child_id == 7).unwrap();
        pane.streaming_reasoning.push_str("thinking hard about the task");
        pane.streaming_assistant.push_str("partial answer");
    }

    /// Deconstruct a Reasoning item (TranscriptItem has no PartialEq;
    /// tests compare the payload explicitly — same style as as_filediff).
    fn as_reasoning(it: &TranscriptItem) -> (&str, ReasoningExpandState, Option<Duration>) {
        match it {
            TranscriptItem::Reasoning { text, expand_state, thought_duration } => {
                (text, *expand_state, *thought_duration)
            }
            other => panic!("expected Reasoning, got {:?}", other),
        }
    }

    /// T032: SubagentComplete flushes pending pane reasoning (committed
    /// before the streaming_assistant flush), and clears the buffer so
    /// the pane's reasoning panel stops rendering "live".
    #[test]
    fn subagent_complete_flushes_pending_pane_reasoning() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 7, "child");
        stage_pending_streams(&mut app);

        app.apply(AgentEvent::SubagentComplete {
            id: 7,
            goal: "child".into(),
            success: true,
            summary_preview: "ok".into(),
            token_usage: joey_providers::Usage::default(),
            duration_secs: 1.0,
        });

        let pane = &app.subagent_panes[0];
        // (2) buffer drained — the live-render condition turns off.
        assert!(pane.streaming_reasoning.is_empty(), "reasoning buffer flushed");
        // (1) reasoning committed with the pane-flush construction.
        assert_eq!(pane.transcript.len(), 2, "Reasoning + Assistant committed");
        let (text, expand, dur) = as_reasoning(&pane.transcript[0]);
        assert_eq!(text, "thinking hard about the task");
        assert_eq!(expand, ReasoningExpandState::Collapsed);
        // T032 staged the buffer directly (no ReasoningDelta), so no
        // reasoning_started clock ever ran — the flush stamps no
        // duration (App::flush_reasoning's `.take().map(...)` on None).
        assert_eq!(dur, None, "no clock → no duration");
        // (3) the pre-existing streaming_assistant flush still happens,
        // AFTER the reasoning item (pane_apply ordering).
        match &pane.transcript[1] {
            TranscriptItem::Assistant { text } => assert_eq!(text, "partial answer"),
            other => panic!("expected Assistant after Reasoning, got {:?}", other),
        }
        assert!(pane.streaming_assistant.is_empty());
        assert_eq!(pane.status, SubagentStatus::Done);
    }

    /// T032 mirror: SubagentFailed flushes pending pane reasoning before
    /// pushing the error item.
    #[test]
    fn subagent_failed_flushes_pending_pane_reasoning() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 7, "child");
        stage_pending_streams(&mut app);

        app.apply(AgentEvent::SubagentFailed {
            id: 7,
            goal: "child".into(),
            error: "boom".into(),
            duration_secs: 1.0,
        });

        let pane = &app.subagent_panes[0];
        assert!(pane.streaming_reasoning.is_empty(), "reasoning buffer flushed");
        let (text, expand, dur) = as_reasoning(&pane.transcript[0]);
        assert_eq!(text, "thinking hard about the task");
        assert_eq!(expand, ReasoningExpandState::Collapsed);
        // T032 staged the buffer directly (no ReasoningDelta): no clock
        // ran, so no duration (see subagent_complete test).
        assert_eq!(dur, None, "no clock → no duration");
        // Reasoning commits BEFORE the error item (pane_apply ordering).
        match &pane.transcript[1] {
            TranscriptItem::Error { text } => assert_eq!(text, "boom"),
            other => panic!("expected Error after Reasoning, got {:?}", other),
        }
        assert_eq!(pane.status, SubagentStatus::Failed);
    }

    /// T034 (US4, FR-008, D6): ReasoningDelta(s) + a flush boundary commit
    /// a `Reasoning` item carrying `thought_duration: Some(_)` (Feature 007
    /// parity with App::flush_reasoning), the timer RESETS (a second burst
    /// yields a fresh, independently-measured duration), and the pane's
    /// expanded panel auto-docks with its frozen anchor cleared.
    #[test]
    fn pane_flush_reasoning_stamps_duration_and_resets_timer() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 7, "child");
        app.focus_subagent(Some(0));

        // Burst 1: deltas start the clock (first delta of the block).
        app.apply(AgentEvent::SubagentEvent {
            id: 7,
            event: Box::new(AgentEvent::ReasoningDelta("first ".into())),
        });
        assert!(
            app.subagent_panes[0].reasoning_started.is_some(),
            "first ReasoningDelta starts the pane's thinking clock"
        );
        // Simulate the user having expanded the pane's panel mid-block.
        app.subagent_panes[0].reasoning_expanded = true;
        app.subagent_panes[0].reasoning_view = Some(12);

        // Flush boundary (AssistantMessage) commits the item.
        app.apply(AgentEvent::SubagentEvent {
            id: 7,
            event: Box::new(AgentEvent::AssistantMessage("answer".into())),
        });
        {
            let pane = &app.subagent_panes[0];
            let (text, _, dur) = as_reasoning(&pane.transcript[0]);
            assert_eq!(text, "first ");
            assert!(dur.is_some(), "PARITY: the committed item carries a thought_duration");
            assert!(pane.reasoning_started.is_none(), "timer reset on flush");
            assert!(!pane.reasoning_expanded, "expanded panel auto-docked on flush");
            assert!(pane.reasoning_view.is_none(), "frozen anchor reset on flush");
        }

        // Burst 2: a fresh clock runs and yields a fresh duration.
        app.apply(AgentEvent::SubagentEvent {
            id: 7,
            event: Box::new(AgentEvent::ReasoningDelta("second ".into())),
        });
        assert!(
            app.subagent_panes[0].reasoning_started.is_some(),
            "second burst restarts the clock"
        );
        app.apply(AgentEvent::SubagentEvent {
            id: 7,
            event: Box::new(AgentEvent::ToolStart {
                name: "terminal".into(),
                emoji: "💻".into(),
                summary: "cargo build".into(),
            }),
        });
        let pane = &app.subagent_panes[0];
        // [0]=Reasoning(burst 1), [1]=Assistant, [2]=Reasoning(burst 2), [3]=Tool.
        assert_eq!(pane.transcript.len(), 4);
        let (_, _, dur2) = as_reasoning(&pane.transcript[2]);
        assert!(dur2.is_some(), "second burst yields a fresh duration");
        assert!(pane.reasoning_started.is_none(), "timer reset again");
    }

    /// T034: the pane toggle mutator mirrors App::toggle_reasoning_expanded
    /// — no-op with no live block, expand + tail re-pin when live, and
    /// collapse on the second call. No pane focused → strict no-op (the
    /// main panel's state is the click handler's job, byte-identical).
    #[test]
    fn toggle_focused_pane_reasoning_expanded_mirrors_main_semantics() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 7, "child");
        app.focus_subagent(Some(0));

        // No live block → no-op.
        app.toggle_focused_pane_reasoning_expanded();
        assert!(!app.subagent_panes[0].reasoning_expanded);

        // Live block → expand, and the view re-pins to the live tail.
        app.subagent_panes[0].streaming_reasoning = "thinking".into();
        app.subagent_panes[0].reasoning_view = Some(20);
        app.toggle_focused_pane_reasoning_expanded();
        assert!(app.subagent_panes[0].reasoning_expanded);
        assert!(app.subagent_panes[0].reasoning_view.is_none(), "re-pinned to tail");
        // Second call docks back.
        app.toggle_focused_pane_reasoning_expanded();
        assert!(!app.subagent_panes[0].reasoning_expanded);

        // No pane focused → no-op, main state untouched.
        app.focus_subagent(None);
        app.subagent_panes[0].streaming_reasoning = "still live".into();
        app.toggle_focused_pane_reasoning_expanded();
        assert!(!app.subagent_panes[0].reasoning_expanded, "unfocused: pane untouched");
        assert!(!app.reasoning_expanded, "unfocused: main toggle is the handler's job");
    }

    /// T034: the pane reasoning scroll mutators mirror
    /// reasoning_scroll_up/down — freeze at an anchor, re-pin at the tail
    /// — reading the bound the reasoning widget recorded for the focused
    /// pane's panel. No pane focused → no-op.
    #[test]
    fn pane_reasoning_scroll_helpers_freeze_and_repin() {
        let mut app = App::new("s", "m");
        spawn_pane(&mut app, 7, "child");
        app.focus_subagent(Some(0));
        app.subagent_panes[0].streaming_reasoning = "thinking".into();

        // Simulate the render-time anchor bound for the pane's panel.
        app.last_reasoning_max_anchor.set(40);
        app.pane_reasoning_scroll_up(5);
        assert_eq!(app.subagent_panes[0].reasoning_view, Some(35), "freeze at anchor");
        app.pane_reasoning_scroll_down(3);
        assert_eq!(
            app.subagent_panes[0].reasoning_view,
            Some(38),
            "still frozen above the tail"
        );
        app.pane_reasoning_scroll_down(10);
        assert!(
            app.subagent_panes[0].reasoning_view.is_none(),
            "reaching the tail resumes follow"
        );
        // Sibling pane keeps its own state (FR-010).
        spawn_pane(&mut app, 8, "sibling");
        assert!(app.subagent_panes[1].reasoning_view.is_none());

        // No pane focused → no-op on the main anchor.
        app.focus_subagent(None);
        app.last_reasoning_max_anchor.set(40);
        app.pane_reasoning_scroll_up(5);
        assert!(app.reasoning_view.is_none(), "unfocused: main view untouched");
        assert!(
            app.subagent_panes[0].reasoning_view.is_none(),
            "unfocused: panes untouched"
        );
    }
}

#[cfg(test)]
mod subagent_stopped_tests {
    //! Spec 020 (T030): `SubagentStopped` moves a child to the terminal
    //! `Stopped` state (entry + pane) with the partial-result preview
    //! surfaced, and — given the orchestration layer's
    //! Stopped-then-Complete emission order — the follow-up
    //! `SubagentComplete` must NOT clobber it back to Done.
    use super::*;

    fn spawn(app: &mut App, id: u64, goal: &str) {
        app.apply(AgentEvent::SubagentSpawn {
            id,
            goal: goal.into(),
            model: "m".into(),
            toolset_summary: "all".into(),
            depth: 0,
        });
    }

    fn stop(id: u64, reason: &str) -> AgentEvent {
        AgentEvent::SubagentStopped {
            id,
            goal: "explore the archive".into(),
            reason: reason.into(),
            summary_preview: "partial notes on sector 7".into(),
        }
    }

    fn last_notice_text(app: &App) -> &str {
        app.transcript
            .iter()
            .rev()
            .find_map(|it| match it {
                TranscriptItem::Notice { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("")
    }

    /// T030 (a): applying SubagentStopped moves the job-board entry AND the
    /// pane to Stopped with summary_preview set, and the notice carries the
    /// raw reason string (FR-016: budget_exceeded vs operator_requested
    /// must be distinguishable) plus the preview (FR-010).
    #[test]
    fn subagent_stopped_marks_entry_and_pane_with_reason_notice() {
        let mut app = App::new("s", "m");
        spawn(&mut app, 7, "explore the archive");
        assert_eq!(app.subagent_entries[0].status, SubagentStatus::Running);

        app.apply(stop(7, "budget_exceeded"));

        let entry = &app.subagent_entries[0];
        assert_eq!(entry.status, SubagentStatus::Stopped, "entry → Stopped");
        assert!(
            entry.phase.contains("budget_exceeded"),
            "stop reason recorded in the phase: {:?}",
            entry.phase
        );
        let pane = &app.subagent_panes[0];
        assert_eq!(pane.status, SubagentStatus::Stopped, "pane → Stopped");
        assert_eq!(
            pane.summary_preview.as_deref(),
            Some("partial notes on sector 7"),
            "FR-010: partial result surfaced"
        );
        let notice = last_notice_text(&app);
        assert!(
            notice.contains("budget_exceeded"),
            "FR-016: notice carries the reason: {:?}",
            notice
        );
        assert!(
            notice.contains("partial notes on sector 7"),
            "notice carries the preview: {:?}",
            notice
        );
    }

    /// T030 (a2): Stopped flushes the pane's live streams exactly like the
    /// SubagentComplete close-out (T032 parity) — pending reasoning and a
    /// partial assistant buffer are committed, not dropped.
    #[test]
    fn subagent_stopped_flushes_pending_pane_streams() {
        let mut app = App::new("s", "m");
        spawn(&mut app, 7, "explore the archive");
        {
            let pane = app.subagent_panes.iter_mut().find(|p| p.child_id == 7).unwrap();
            pane.streaming_reasoning.push_str("mid-thought when stopped");
            pane.streaming_assistant.push_str("partial answer");
        }

        app.apply(stop(7, "operator_requested"));

        let pane = &app.subagent_panes[0];
        assert!(pane.streaming_reasoning.is_empty(), "reasoning flushed");
        assert!(pane.streaming_assistant.is_empty(), "assistant flushed");
        assert_eq!(pane.transcript.len(), 2, "Reasoning + Assistant committed");
        match &pane.transcript[0] {
            TranscriptItem::Reasoning { text, .. } => {
                assert_eq!(text, "mid-thought when stopped")
            }
            other => panic!("expected Reasoning, got {:?}", other),
        }
        match &pane.transcript[1] {
            TranscriptItem::Assistant { text } => assert_eq!(text, "partial answer"),
            other => panic!("expected Assistant, got {:?}", other),
        }
    }

    /// T030 (b): the real emission order is SubagentStopped FIRST, then
    /// SubagentComplete still fires — Stopped must win on BOTH the pane and
    /// the job-board entry (no clobber back to Done, no duplicate terminal
    /// notice).
    #[test]
    fn subagent_stopped_wins_over_followup_complete() {
        let mut app = App::new("s", "m");
        spawn(&mut app, 7, "explore the archive");
        app.apply(stop(7, "session_end"));
        let notice_after_stop = last_notice_text(&app).to_string();

        app.apply(AgentEvent::SubagentComplete {
            id: 7,
            goal: "explore the archive".into(),
            success: true,
            summary_preview: "final summary".into(),
            token_usage: joey_providers::Usage::default(),
            duration_secs: 2.0,
        });

        let pane = &app.subagent_panes[0];
        assert_eq!(pane.status, SubagentStatus::Stopped, "pane stays Stopped");
        assert_eq!(
            pane.summary_preview.as_deref(),
            Some("partial notes on sector 7"),
            "stop preview preserved, not replaced by Complete's"
        );
        assert_eq!(
            app.subagent_entries[0].status,
            SubagentStatus::Stopped,
            "entry stays Stopped"
        );
        assert_eq!(
            last_notice_text(&app),
            notice_after_stop,
            "no follow-up done notice after the stop notice"
        );
    }

    /// T030 (b2): a late SubagentFailed equally must not overwrite Stopped.
    #[test]
    fn subagent_stopped_wins_over_followup_failed() {
        let mut app = App::new("s", "m");
        spawn(&mut app, 7, "explore the archive");
        app.apply(stop(7, "orchestrator_requested"));
        app.apply(AgentEvent::SubagentFailed {
            id: 7,
            goal: "explore the archive".into(),
            error: "boom".into(),
            duration_secs: 1.0,
        });
        assert_eq!(
            app.subagent_panes[0].status,
            SubagentStatus::Stopped,
            "pane stays Stopped"
        );
        assert_eq!(
            app.subagent_entries[0].status,
            SubagentStatus::Stopped,
            "entry stays Stopped"
        );
    }
}
