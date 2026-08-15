//! TUI application state machine.
//!
//! Consumes the [`AgentEvent`] stream and maintains a rich, queryable model
//! that the widgets render each frame. This replaces the line-based
//! `render_turn` with a live, animated view.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

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
        /// Feature 007: elapsed time from first `ReasoningDelta` to flush.
        /// Drives the `Thought for Ns` footer. `None` when duration wasn't
        /// tracked (e.g. short blocks where timing is meaningless).
        thought_duration: Option<Duration>,
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
        /// Feature 007: whether this is a terminal/shell tool call.
        is_terminal: bool,
        /// Feature 007: process exit code (terminal tools only).
        exit_code: Option<i64>,
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
        /// Expand toggle: collapsed shows the last MAX_DIFF_LINES lines;
        /// expanded shows the whole diff.
        expanded: bool,
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
            agent_picker_open: false,
            agent_picker_cursor: 0,
            agent_roster: Vec::new(),
            active_agent_index: 0,
            default_model: None,
            pending_agent_switch: None,
            subagent_entries: Vec::new(),
            next_subagent_id: 1,
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
            if let TranscriptItem::Reasoning { text, expand_state, .. } = &mut self.transcript[i] {
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
        match &mut self.transcript[index] {
            TranscriptItem::Reasoning { text, expand_state, .. } => {
                let total_lines = text.lines().count();
                *expand_state = expand_state.cycle(total_lines);
            }
            TranscriptItem::Tool { expanded, .. } => {
                *expanded = !*expanded;
            }
            TranscriptItem::FileDiff { expanded, .. } => {
                *expanded = !*expanded;
            }
            // Other item types are not expandable; click is a no-op for them.
            _ => {}
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
                    name: name.clone(),
                    emoji,
                    summary,
                    status: ToolStatus::Running,
                    duration_secs: None,
                    result_preview: String::new(),
                    expanded: false,
                    full_args: None,
                    full_result: None,
                    is_terminal: is_terminal_block(&name),
                    exit_code: None,
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
            AgentEvent::NeuroCodeActive { active } => {
                self.neurocode_active = active;
                if !active {
                    self.neurocode_context.clear();
                    self.neurocode_stage.clear();
                    self.neurocode_stage_at = None;
                    self.neurocode_updated_at = None;
                    self.neurocode_expanded = false;
                    self.last_neurocode_rect.set((0, 0, 0, 0));
                }
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
                    expanded: false,
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
            is_terminal: false,
            exit_code: None,
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
            is_terminal: false,
            exit_code: None,
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
