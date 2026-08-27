//! Subagent types: DelegationRequest, TaskSpec, DelegationResult, SubagentRole,
//! plus feature-020 async-delegation types (StopReason, Budgets, WorkHandle,
//! RunningUsage, ChildHandle, DelegationOverview).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use joey_providers::{ReasoningEffort, Usage};
use serde::{Deserialize, Serialize};

/// Whether a subagent can delegate further (Leaf) or spawn its own children
/// (Orchestrator, requires `max_spawn_depth > 1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SubagentRole {
    #[default]
    Leaf,
    Orchestrator,
}


/// Why a running subagent was stopped before natural completion (FR-010).
/// The manager records the reason on the child handle before setting its
/// interrupt flag; it surfaces on [`DelegationResult::stop_reason`], in
/// `AgentEvent::SubagentStopped`, and in completion notices (FR-016).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The orchestrating agent requested the stop (`subagent_control stop`).
    OrchestratorRequested,
    /// The human operator requested the stop (TUI keybinding).
    OperatorRequested,
    /// A resource budget (turns/tokens/wall-clock) was exceeded.
    BudgetExceeded,
    /// Session wind-down (`SubagentManager::shutdown`).
    SessionEnd,
}

/// Optional per-child resource budgets (feature 020, FR-011).
///
/// Any present value must be `> 0`; deserialization rejects invalid values
/// at parse time with a clear error naming the offending field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Budgets {
    /// Max child iterations. Unset falls back to `delegation.default_max_turns`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Max cumulative tokens (prompt + completion). Unset = unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Max wall-clock seconds. Unset = unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_clock_secs: Option<u64>,
}

impl Budgets {
    /// Validate that every present budget is `> 0` (FR-011).
    ///
    /// Returns a clear error naming the first offending field.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_turns == Some(0) {
            return Err("budgets.max_turns must be > 0".to_string());
        }
        if self.max_tokens == Some(0) {
            return Err("budgets.max_tokens must be > 0".to_string());
        }
        if self.max_wall_clock_secs == Some(0) {
            return Err("budgets.max_wall_clock_secs must be > 0".to_string());
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Budgets {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Parse through a raw mirror so invalid (zero) values are rejected
        // at parse time with a clear error (FR-011), wherever a Budgets
        // value is deserialized (tool params, TaskSpec payloads).
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            max_turns: Option<u32>,
            #[serde(default)]
            max_tokens: Option<u64>,
            #[serde(default)]
            max_wall_clock_secs: Option<u64>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let budgets = Budgets {
            max_turns: raw.max_turns,
            max_tokens: raw.max_tokens,
            max_wall_clock_secs: raw.max_wall_clock_secs,
        };
        budgets.validate().map_err(serde::de::Error::custom)?;
        Ok(budgets)
    }
}

/// Serde skip helper: keeps `TaskSpec`'s serialized form byte-identical to
/// pre-feature output when `background` is unset (false).
fn is_false(b: &bool) -> bool {
    !*b
}

/// Per-task specification within a batch delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub goal: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub toolsets: Vec<String>,
    /// HyperCode role routing ("explorer" | "implementor"): fills toolsets/
    /// model/turns from the role's config table (gaps only) and injects the
    /// role directive. Optional.
    #[serde(default)]
    pub role: Option<String>,
    /// Background delegation (feature 020, FR-001): when true the dispatch
    /// returns a handle immediately instead of blocking for the result.
    /// Default `false` preserves blocking behavior byte-for-byte (FR-002).
    #[serde(default, skip_serializing_if = "is_false")]
    pub background: bool,
    /// Optional per-child resource budgets (feature 020, FR-011), validated
    /// `> 0` at parse time. `None` = no caps beyond delegation defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budgets: Option<Budgets>,
}

/// A request from the parent agent (or user) to dispatch one or more
/// subagents. When `tasks` is non-empty, batch (parallel) mode is triggered.
#[derive(Debug, Clone)]
pub struct DelegationRequest {
    /// Task goal (single-task mode). Ignored when `tasks` is non-empty.
    pub goal: String,
    /// Additional context passed to the subagent.
    pub context: Option<String>,
    /// Batch mode: parallel dispatch of independent tasks.
    pub tasks: Vec<TaskSpec>,
    /// Model override for the subagent(s).
    pub model: Option<String>,
    /// Restrict subagent toolset to named toolsets.
    pub toolsets: Vec<String>,
    /// Override iteration budget.
    pub max_turns: Option<usize>,
    /// Override the child's reasoning effort (HyperCode per-role levels).
    /// None inherits the parent's `AgentConfig.reasoning`.
    pub reasoning: Option<ReasoningEffort>,
    /// Override the child's max output tokens. None inherits the parent's.
    pub max_tokens: Option<u32>,
    /// Persist subagent trace to session DB.
    pub persist: bool,
    /// Leaf (default) or Orchestrator.
    pub role: SubagentRole,
    /// Per-subagent working directory override.
    pub workdir: Option<std::path::PathBuf>,
    /// OMO category name (e.g. "quick"). When set, the model should be
    /// resolved from the category's fallback chain and `prompt_append`
    /// injected into the subagent's system prompt. Mutually exclusive with
    /// `subagent_type` (BC-011).
    pub category: Option<String>,
    /// OMO subagent type (e.g. "oracle", "explore"). When set, the model
    /// should be resolved from that agent's requirement. Mutually exclusive
    /// with `category` (BC-011).
    pub subagent_type: Option<String>,
    /// Skill names to load and prepend to the subagent's system prompt.
    pub load_skills: Vec<String>,
    /// Text prepended to the subagent's system prompt (from category config).
    pub prompt_append: Option<String>,
}

impl DelegationRequest {
    /// Single-task constructor.
    pub fn single(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            context: None,
            tasks: Vec::new(),
            model: None,
            toolsets: Vec::new(),
            max_turns: None,
            reasoning: None,
            max_tokens: None,
            persist: false,
            role: SubagentRole::Leaf,
            workdir: None,
            category: None,
            subagent_type: None,
            load_skills: Vec::new(),
            prompt_append: None,
        }
    }
}

/// The outcome of a completed subagent execution.
#[derive(Debug, Clone)]
pub struct DelegationResult {
    /// The original goal (for correlation).
    pub goal: String,
    /// Concise result summary (<500 tokens target).
    pub summary: String,
    /// Whether the subagent completed without fatal error.
    pub success: bool,
    /// Error detail if `success == false`.
    pub error: Option<String>,
    /// Total tokens consumed by this subagent.
    pub token_usage: Usage,
    /// Total wall-clock execution time.
    pub wall_clock: Duration,
    /// Model that was used.
    pub model: String,
    /// Number of API calls made.
    pub iterations: usize,
    /// If `persist == true`, the child session ID.
    pub persisted_session_id: Option<String>,
    /// Why the child was stopped before natural completion (feature 020).
    /// `None` for natural completion/failure.
    pub stop_reason: Option<StopReason>,
}

/// Handle returned by a background `delegate_task` dispatch (feature 020,
/// FR-001): identifies running work the orchestrator can steer/stop/wait on.
#[derive(Debug, Clone)]
pub struct WorkHandle {
    /// Global child id (from the manager's process-global child counter).
    pub child_id: String,
    /// The delegated goal (for correlation in notices and overview records).
    pub goal: String,
    /// When the child was spawned.
    pub started_at: Instant,
}

/// Live per-child resource consumption, accumulated by the parent from tap
/// events (feature 020, FR-012).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunningUsage {
    /// Child iterations completed so far.
    pub iterations: u64,
    /// Cumulative prompt tokens.
    pub prompt_tokens: u64,
    /// Cumulative completion tokens.
    pub completion_tokens: u64,
    /// Cumulative total tokens.
    pub total_tokens: u64,
}

/// Internal manager-registry record for a live child (feature 020, D4).
/// Created at spawn, removed at completion (the result is archived into a
/// [`DelegationOverview`] record).
#[derive(Debug, Clone)]
pub struct ChildHandle {
    /// Clone of the child Agent's interrupt flag (set by `stop_child`).
    pub interrupt: Arc<AtomicBool>,
    /// The child Agent's steer slot (from `Agent::steer_handle`).
    pub steer: Arc<Mutex<String>>,
    /// The task the child was spawned for.
    pub task: TaskSpec,
    /// Effective budgets for this child (taken from the spec).
    pub budgets: Option<Budgets>,
    /// Accumulated usage so far.
    pub usage: RunningUsage,
    /// When the child was spawned.
    pub started_at: Instant,
    /// Stop reason recorded before the interrupt flag is set (research D5);
    /// `None` while the child is running.
    pub pending_stop: Option<StopReason>,
}

impl ChildHandle {
    /// Create a registry entry for a freshly spawned child: budgets come
    /// from the spec, usage starts at zero, no stop is pending.
    pub fn new(task: TaskSpec, interrupt: Arc<AtomicBool>, steer: Arc<Mutex<String>>) -> Self {
        Self {
            interrupt,
            steer,
            budgets: task.budgets,
            usage: RunningUsage::default(),
            started_at: Instant::now(),
            pending_stop: None,
            task,
        }
    }
}

/// Lifecycle state of a delegated child in the session overview
/// (feature 020). `Completed`, `Failed`, and `Stopped` are terminal —
/// transitions into them are one-way (FR-019).
#[derive(Debug, Clone)]
pub enum DelegationState {
    /// Child is executing.
    Running,
    /// Child finished naturally; carries its result.
    Completed { result: DelegationResult },
    /// Child failed; carries the error message.
    Failed { error: String },
    /// Child was stopped; carries the stop reason.
    Stopped { reason: StopReason },
}

impl DelegationState {
    /// Terminal states are one-way (FR-019): a terminal record never
    /// transitions again.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, DelegationState::Running)
    }
}

/// One record in the session-lifetime delegation overview (feature 020,
/// FR-019): running children plus completed/failed/stopped history.
/// In-memory only, discarded at session end.
#[derive(Debug, Clone)]
pub struct DelegationOverview {
    /// Global child id.
    pub child_id: String,
    /// The delegated goal.
    pub goal: String,
    /// Lifecycle state (terminal states are one-way, FR-019).
    pub state: DelegationState,
    /// Wall-clock time since spawn (or until termination for records).
    pub elapsed: Duration,
    /// Cumulative total tokens consumed.
    pub tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_defaults_are_none() {
        let b = Budgets::default();
        assert_eq!(b.max_turns, None);
        assert_eq!(b.max_tokens, None);
        assert_eq!(b.max_wall_clock_secs, None);
        assert!(b.validate().is_ok(), "all-unset budgets are valid");
    }

    #[test]
    fn budgets_validation_accepts_positive_values() {
        let b = Budgets {
            max_turns: Some(1),
            max_tokens: Some(1),
            max_wall_clock_secs: Some(1),
        };
        assert!(b.validate().is_ok());
    }

    #[test]
    fn budgets_validation_rejects_zero_values() {
        // FR-011: any present value must be > 0, with a clear error naming
        // the offending field.
        let max_turns = Budgets {
            max_turns: Some(0),
            ..Default::default()
        };
        assert_eq!(
            max_turns.validate().unwrap_err(),
            "budgets.max_turns must be > 0"
        );

        let max_tokens = Budgets {
            max_tokens: Some(0),
            ..Default::default()
        };
        assert_eq!(
            max_tokens.validate().unwrap_err(),
            "budgets.max_tokens must be > 0"
        );

        let wall_clock = Budgets {
            max_wall_clock_secs: Some(0),
            ..Default::default()
        };
        assert_eq!(
            wall_clock.validate().unwrap_err(),
            "budgets.max_wall_clock_secs must be > 0"
        );
    }

    #[test]
    fn budgets_deserialization_rejects_zero_at_parse_time() {
        // FR-011: invalid values are rejected at parse with a clear error.
        let err = serde_json::from_str::<Budgets>(r#"{"max_tokens": 0}"#).unwrap_err();
        assert!(err.to_string().contains("must be > 0"), "got: {err}");

        let err = serde_json::from_str::<Budgets>(r#"{"max_turns": 0}"#).unwrap_err();
        assert!(err.to_string().contains("max_turns"), "got: {err}");

        let err =
            serde_json::from_str::<Budgets>(r#"{"max_wall_clock_secs": 0}"#).unwrap_err();
        assert!(err.to_string().contains("max_wall_clock_secs"), "got: {err}");
    }

    #[test]
    fn budgets_serde_round_trip() {
        let b = Budgets {
            max_turns: Some(5),
            max_tokens: Some(10_000),
            max_wall_clock_secs: Some(120),
        };
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(
            json,
            r#"{"max_turns":5,"max_tokens":10000,"max_wall_clock_secs":120}"#
        );
        let back: Budgets = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn budgets_empty_object_deserializes_to_all_none() {
        let b: Budgets = serde_json::from_str("{}").unwrap();
        assert_eq!(b, Budgets::default());
    }

    #[test]
    fn task_spec_deserializes_pre_feature_payloads_unchanged() {
        // Backward compat: old payloads (no background/budgets keys) still
        // deserialize, with background=false and budgets=None.
        let spec: TaskSpec = serde_json::from_str(
            r#"{"goal":"g","context":null,"model":null,"toolsets":[],"role":null}"#,
        )
        .unwrap();
        assert_eq!(spec.goal, "g");
        assert!(!spec.background);
        assert!(spec.budgets.is_none());
    }

    #[test]
    fn task_spec_unset_fields_serialize_byte_identical_to_pre_feature() {
        // FR-002 byte-parity: with background=false and budgets=None the
        // serialized form contains ONLY the pre-feature keys.
        let spec = TaskSpec {
            goal: "g".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
            role: None,
            background: false,
            budgets: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(
            json,
            r#"{"goal":"g","context":null,"model":null,"toolsets":[],"role":null}"#
        );
    }

    #[test]
    fn task_spec_background_and_budgets_round_trip() {
        let spec = TaskSpec {
            goal: "g".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
            role: None,
            background: true,
            budgets: Some(Budgets {
                max_turns: Some(2),
                max_tokens: None,
                max_wall_clock_secs: Some(60),
            }),
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains(r#""background":true"#), "got: {json}");
        assert!(
            json.contains(r#""budgets":{"max_turns":2,"max_wall_clock_secs":60}"#),
            "got: {json}"
        );
        let back: TaskSpec = serde_json::from_str(&json).unwrap();
        assert!(back.background);
        assert_eq!(back.budgets.as_ref().unwrap().max_turns, Some(2));
        assert_eq!(back.budgets.as_ref().unwrap().max_wall_clock_secs, Some(60));
    }

    #[test]
    fn task_spec_rejects_invalid_budgets_at_parse_time() {
        let err = serde_json::from_str::<TaskSpec>(
            r#"{"goal":"g","toolsets":[],"budgets":{"max_turns":0}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be > 0"), "got: {err}");
    }

    #[test]
    fn stop_reason_serde_round_trip() {
        for (reason, name) in [
            (StopReason::OrchestratorRequested, "orchestrator_requested"),
            (StopReason::OperatorRequested, "operator_requested"),
            (StopReason::BudgetExceeded, "budget_exceeded"),
            (StopReason::SessionEnd, "session_end"),
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, format!(r#""{name}""#));
            let back: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, reason);
        }
    }

    #[test]
    fn child_handle_new_initializes_from_spec() {
        let spec = TaskSpec {
            goal: "g".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
            role: None,
            background: true,
            budgets: Some(Budgets {
                max_turns: Some(3),
                ..Default::default()
            }),
        };
        let interrupt = Arc::new(AtomicBool::new(false));
        let steer = Arc::new(Mutex::new(String::new()));
        let handle = ChildHandle::new(spec, interrupt, steer);
        assert_eq!(handle.task.goal, "g");
        assert_eq!(handle.budgets.as_ref().unwrap().max_turns, Some(3));
        assert_eq!(handle.usage, RunningUsage::default());
        assert!(handle.pending_stop.is_none());
    }

    #[test]
    fn delegation_state_terminal_states_are_one_way() {
        // FR-019: only Running is non-terminal.
        assert!(!DelegationState::Running.is_terminal());
        assert!(
            DelegationState::Completed {
                result: DelegationResult {
                    goal: "g".to_string(),
                    summary: String::new(),
                    success: true,
                    error: None,
                    token_usage: Usage::default(),
                    wall_clock: Duration::ZERO,
                    model: "m".to_string(),
                    iterations: 0,
                    persisted_session_id: None,
                    stop_reason: None,
                }
            }
            .is_terminal()
        );
        assert!(DelegationState::Failed { error: String::new() }.is_terminal());
        assert!(
            DelegationState::Stopped {
                reason: StopReason::SessionEnd,
            }
            .is_terminal()
        );
    }
}
