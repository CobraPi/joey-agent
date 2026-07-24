//! TUI application state machine.
//!
//! Consumes the [`AgentEvent`] stream and maintains a rich, queryable model
//! that the widgets render each frame. This replaces the line-based
//! `render_turn` with a live, animated view.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Instant;

use joey_agent_core::AgentEvent;

/// One entry in the conversation transcript.
#[derive(Clone, Debug)]
pub enum TranscriptItem {
    User { text: String },
    Assistant { text: String },
    /// A complete reasoning block shown in a dimmed/collapsed style.
    Reasoning { text: String },
    /// A tool call rendered inline with its result.
    Tool {
        name: String,
        emoji: String,
        summary: String,
        status: ToolStatus,
        duration_secs: Option<f64>,
        result_preview: String,
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
    /// Running, Done, Failed.
    pub status: SubagentStatus,
    /// "querying model", "running tool: X", "reasoning".
    pub phase: String,
    /// Resolved model.
    pub model: String,
    /// API calls made.
    pub iterations: usize,
    /// For elapsed time.
    pub started: Instant,
}

/// Status of a subagent entry in the activity panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentStatus {
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
    /// Learnings counter for wisdom accumulation display.
    pub learnings_count: usize,
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
            learnings_count: 0,
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
                self.push_item(TranscriptItem::Reasoning { text });
            }
            self.reasoning_open = false;
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
                self.push_item(TranscriptItem::Tool {
                    name,
                    emoji,
                    summary,
                    status: ToolStatus::Running,
                    duration_secs: None,
                    result_preview: String::new(),
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
                self.push_item(TranscriptItem::Notice {
                    text: format!("🤖 Subagent: {} ({}) [{}]", goal, model, toolset_summary),
                    kind: NoticeKind::Busy,
                });
            }
            AgentEvent::SubagentComplete { goal, success, summary_preview, token_usage: _, duration_secs: _ } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("{} {}: {}", if success { "✓" } else { "✗" }, goal, summary_preview),
                    kind: if success { NoticeKind::Success } else { NoticeKind::Warning },
                });
            }
            AgentEvent::SubagentFailed { goal, error, duration_secs: _ } => {
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
                self.push_item(TranscriptItem::Notice {
                    text: format!("Agent: {}", agent_name),
                    kind: NoticeKind::Info,
                });
            }
            AgentEvent::CategoryDelegation { category, model } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Category [{}] → {}", category, model),
                    kind: NoticeKind::Busy,
                });
            }
            AgentEvent::BoulderWorkStarted { plan_name, work_id: _ } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Started work: {}", plan_name),
                    kind: NoticeKind::Success,
                });
            }
            AgentEvent::BoulderWorkResumed { plan_name, work_id: _ } => {
                self.push_item(TranscriptItem::Notice {
                    text: format!("Resumed work: {}", plan_name),
                    kind: NoticeKind::Info,
                });
            }
            AgentEvent::BoulderWorkCompleted { plan_name, work_id: _ } => {
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
                self.push_item(TranscriptItem::Notice {
                    text: format!("Wisdom: {} learnings", learnings_count),
                    kind: NoticeKind::Info,
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
}
