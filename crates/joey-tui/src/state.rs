//! TUI application state machine.
//!
//! Consumes the [`AgentEvent`] stream and maintains a rich, queryable model
//! that the widgets render each frame. This replaces the line-based
//! `render_turn` with a live, animated view.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Instant;

use joey_agent_core::events::{AgentEvent, FileChangeKind};

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
    },
    /// A tool call rendered inline with its result.
    /// Feature 005 (T026): carries `expanded` toggle + full args/result.
    Tool {
        name: String,
        emoji: String,
        summary: String,
        status: ToolStatus,
        duration_secs: Option<f64>,
        result_preview: String,
        /// Feature 005 (T026): per-item expand toggle for the full view.
        expanded: bool,
        /// Feature 005 (T026): full arguments JSON for the expanded view.
        full_args: Option<String>,
        /// Feature 005 (T026): full result text for the expanded view.
        full_result: Option<String>,
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
    /// "explore", "librarian", "oracle", "sisyphus-junior", etc.
    pub agent_type: String,
    /// If category-spawned (e.g. "quick").
    pub category: Option<String>,
    /// Pending, Running, Done, Failed.
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

/// Status of a subagent entry in the activity panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentStatus {
    /// Queued but not yet started (job board).
    Pending,
    Running,
    Done,
    Failed,
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
    pub last_final_text: String,
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
    /// Learnings counter for wisdom accumulation display.
    pub learnings_count: usize,
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
            last_final_text: String::new(),
            agent_picker_open: false,
            agent_picker_cursor: 0,
            agent_roster: Vec::new(),
            active_agent_index: 0,
            default_model: None,
            pending_agent_switch: None,
            subagent_entries: Vec::new(),
            next_subagent_id: 1,
            learnings_count: 0,
            job_board_visible: false,
            pending_context_injection: None,
            search_open: false,
            search_query: String::new(),
            search_has_match: false,
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
            if !text.is_empty() {
                self.push_item(TranscriptItem::Reasoning {
                    text,
                    expand_state: ReasoningExpandState::Collapsed,
                });
            }
            self.reasoning_open = false;
        }
    }

    /// Feature 005 (T021/T023): advance the most-recent reasoning block's
    /// expand state to the next step in the three-state cycle.
    ///
    /// The TUI uses scroll-based navigation (no per-item cursor), so this
    /// targets the last `Reasoning` item in the transcript — matching crush's
    /// behavior of expanding the most recent thinking block first.
    pub fn cycle_focused_reasoning_expand(&mut self) {
        // Find the last Reasoning item in the transcript.
        for i in (0..self.transcript.len()).rev() {
            if let TranscriptItem::Reasoning { text, expand_state } = &mut self.transcript[i] {
                let total_lines = text.lines().count();
                *expand_state = expand_state.cycle(total_lines);
                return;
            }
        }
    }

    /// Feature 005 (T026/T028): toggle the most-recent tool call's `expanded`
    /// field. Targets the last completed `Tool` item (FR-018: per-item
    /// isolation — only one item is affected).
    pub fn toggle_focused_tool_expand(&mut self) {
        for i in (0..self.transcript.len()).rev() {
            if let TranscriptItem::Tool { expanded, .. } = &mut self.transcript[i] {
                *expanded = !*expanded;
                return;
            }
        }
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
                self.turn_started = Some(Instant::now());
                self.streaming_assistant.clear();
                self.streaming_reasoning.clear();
                self.reasoning_open = false;
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
                if let Some(a) = self.active_agents.last_mut() {
                    if a.phase == AgentPhase::QueryingModel {
                        a.phase = AgentPhase::Idle;
                    }
                }
            }
            AgentEvent::ReasoningDelta(d) => {
                if !self.show_reasoning {
                    return;
                }
                if !self.reasoning_open {
                    self.reasoning_open = true;
                    self.streaming_reasoning.clear();
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
                // T155: attribute this tool call to the most recent running
                // subagent entry for the Atlas job board. Tool events from a
                // running subagent are forwarded to the parent's channel
                // (subagent.rs), so the latest Running entry is the best
                // attribution we have on the wire.
                for entry in self.subagent_entries.iter_mut().rev() {
                    if entry.status == SubagentStatus::Running {
                        entry.tool_call_count += 1;
                        entry.last_tool = Some(name.clone());
                        entry.phase = format!("running tool: {}", name);
                        break;
                    }
                }
                self.push_item(TranscriptItem::Tool {
                    name,
                    emoji,
                    summary,
                    status: ToolStatus::Running,
                    duration_secs: None,
                    result_preview: String::new(),
                    expanded: false,
                    full_args: None,
                    full_result: None,
                });
            }
            AgentEvent::ToolProgress { name, progress } => {
                if progress.is_empty() {
                    return;
                }
                // Update the most recent still-running call of this tool
                // (notices/reasoning may have landed after the ToolStart).
                for it in self.transcript.iter_mut().rev() {
                    if let TranscriptItem::Tool { name: n, status, summary, .. } = it {
                        if *status == ToolStatus::Running && *n == name {
                            *summary = progress;
                            break;
                        }
                    }
                }
            }
            AgentEvent::ToolEnd { name, is_error, result_preview, duration_secs } => {
                for it in self.transcript.iter_mut().rev() {
                    if let TranscriptItem::Tool {
                        name: n,
                        status,
                        duration_secs: dur,
                        result_preview: rp,
                        ..
                    } = it
                    {
                        if *status == ToolStatus::Running && *n == name {
                            *status = if is_error { ToolStatus::Failed } else { ToolStatus::Done };
                            *dur = Some(duration_secs);
                            *rp = result_preview;
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
            AgentEvent::SubagentSpawn { goal, model, toolset_summary, depth: _ } => {
                // Populate the activity panel's subagent roster (T064).
                let id = self.next_subagent_id;
                self.next_subagent_id += 1;
                // The goal text is the closest thing to an agent_type label we
                // have at spawn time; the summary_preview on completion is too
                // late for the "running" state the panel needs to show.
                let label: String = goal.chars().take(28).collect();
                // T155: use the full goal as the job-board task title.
                let task_title = if goal.is_empty() { None } else { Some(goal.clone()) };
                self.subagent_entries.push(ActiveSubagentEntry {
                    id,
                    agent_type: label,
                    category: None,
                    status: SubagentStatus::Running,
                    phase: "querying model".to_string(),
                    model,
                    iterations: 0,
                    started: Instant::now(),
                    task_title,
                    tool_call_count: 0,
                    last_tool: None,
                });
                self.push_item(TranscriptItem::Notice {
                    text: format!("🤖 Subagent: {} [{}]", goal, toolset_summary),
                    kind: NoticeKind::Busy,
                });
            }
            AgentEvent::SubagentComplete { goal, success, summary_preview, token_usage: _, duration_secs: _ } => {
                // Mark the matching subagent entry as done/failed (T064).
                // Entries are matched by goal prefix (the label we stored at
                // spawn time). Stale entries are cleaned up on turn Done.
                let label: String = goal.chars().take(28).collect();
                for entry in self.subagent_entries.iter_mut().rev() {
                    if entry.status == SubagentStatus::Running && entry.agent_type == label {
                        entry.status = if success {
                            SubagentStatus::Done
                        } else {
                            SubagentStatus::Failed
                        };
                        break;
                    }
                }
                self.push_item(TranscriptItem::Notice {
                    text: format!("{} {}: {}", if success { "✓" } else { "✗" }, goal, summary_preview),
                    kind: if success { NoticeKind::Success } else { NoticeKind::Warning },
                });
            }
            AgentEvent::SubagentFailed { goal, error, duration_secs: _ } => {
                let label: String = goal.chars().take(28).collect();
                for entry in self.subagent_entries.iter_mut().rev() {
                    if entry.status == SubagentStatus::Running && entry.agent_type == label {
                        entry.status = SubagentStatus::Failed;
                        break;
                    }
                }
                self.push_item(TranscriptItem::Notice {
                    text: format!("✗ {}: {}", goal, error),
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
            AgentEvent::CategoryDelegation { category, model } => {
                // T065/T139: add a subagent entry with the category label.
                let id = self.next_subagent_id;
                self.next_subagent_id += 1;
                let title = format!("[{}] delegation", category);
                self.subagent_entries.push(ActiveSubagentEntry {
                    id,
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
            AgentEvent::FileChange { path, kind, diff, is_binary, .. } => {
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
                self.push_item(TranscriptItem::FileDiff {
                    path,
                    stat,
                    lines,
                    is_binary,
                });
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
        }
    }

    /// Push a transcript item, enforcing the capacity (ring buffer).
    pub fn push_item(&mut self, item: TranscriptItem) {
        if self.transcript.len() >= self.transcript_capacity {
            self.transcript.pop_front();
        }
        self.transcript.push_back(item);
        // Deliberately does NOT touch `scroll`: a user reading history stays
        // where they are while new content streams in below.
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

    // ── Search ─────────────���────────────────────────────────────────────

    /// Run a search query against the transcript, scrolling to the first match.
    /// Called when the user types in the search bar.
    pub fn run_search(&mut self) {
        if self.search_query.is_empty() {
            self.search_has_match = false;
            return;
        }
        let query = self.search_query.to_lowercase();
        // Search from the newest item backward.
        for (idx, item) in self.transcript.iter().rev().enumerate() {
            let text = transcript_item_text(item);
            if text.to_lowercase().contains(&query) {
                // Scroll to show this item — approximate by scrolling up
                // proportionally to the item position.
                let total = self.transcript.len();
                let from_bottom = idx;
                let target_scroll = from_bottom.saturating_sub(2).min(
                    self.last_max_scroll.get(),
                );
                self.scroll = Some(target_scroll);
                self.search_has_match = true;
                let _ = total;
                return;
            }
        }
        self.search_has_match = false;
    }

    /// Find the next/previous search match from the current scroll position.
    pub fn search_next(&mut self, forward: bool) {
        if self.search_query.is_empty() {
            return;
        }
        let query = self.search_query.to_lowercase();
        let current_scroll = self.scroll.unwrap_or(0);

        // Collect match positions (items that contain the query).
        let matches: Vec<usize> = self
            .transcript
            .iter()
            .rev()
            .enumerate()
            .filter(|(_, item)| {
                transcript_item_text(item).to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect();

        if matches.is_empty() {
            return;
        }

        // Find the next match beyond the current scroll position.
        let target = if forward {
            // Forward = scroll down toward newer messages (decrease scroll).
            matches
                .iter()
                .find(|&&idx| idx < current_scroll)
                .or_else(|| matches.first())
        } else {
            // Backward = scroll up toward older messages (increase scroll).
            matches
                .iter()
                .find(|&&idx| idx > current_scroll)
                .or_else(|| matches.last())
        };

        if let Some(&idx) = target {
            let target_scroll = idx.saturating_sub(2).min(self.last_max_scroll.get());
            self.scroll = Some(target_scroll);
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
            expanded: false,
            full_args: None,
            full_result: None,
        });
        app.push_item(TranscriptItem::Tool {
            name: "write_file".to_string(),
            emoji: "✏️".to_string(),
            summary: "write bar.rs".to_string(),
            status: ToolStatus::Done,
            duration_secs: Some(0.2),
            result_preview: "ok".to_string(),
            expanded: false,
            full_args: None,
            full_result: None,
        });
        // Toggle the most-recent (write_file) tool.
        app.toggle_focused_tool_expand();
        // The most recent tool should be expanded.
        let last = app.transcript.back().unwrap();
        if let TranscriptItem::Tool { expanded, .. } = last {
            assert!(*expanded, "most-recent tool should be expanded after toggle");
        } else {
            panic!("expected Tool item");
        }
        // The first tool should still be collapsed (per-item isolation).
        let first = &app.transcript[0];
        if let TranscriptItem::Tool { expanded, .. } = first {
            assert!(!*expanded, "first tool should still be collapsed (FR-018 isolation)");
        } else {
            panic!("expected Tool item");
        }
        // Toggle again — should collapse.
        app.toggle_focused_tool_expand();
        let last = app.transcript.back().unwrap();
        if let TranscriptItem::Tool { expanded, .. } = last {
            assert!(!*expanded, "most-recent tool should collapse on second toggle");
        }
    }
}
