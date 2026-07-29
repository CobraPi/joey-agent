//! Subagent types: DelegationRequest, TaskSpec, DelegationResult, SubagentRole.

use std::time::Duration;

use joey_providers::Usage;
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
}
