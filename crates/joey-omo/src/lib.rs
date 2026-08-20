//! `joey-omo` — Oh My OpenAgent orchestration system for joey-agent.
//!
//! A 1-to-1 Rust port of oh-my-openagent's multi-agent orchestration:
//! 11 built-in agents, 11 delegate-task categories, model fallback chains
//! with family-level fuzzy matching, IntentGate/ultrawork working modes,
//! the three-layer plan→execute→worker pipeline, and Tab-based agent switching.
//!
//! The existing joey-agent default agent is prepended to the Tab cycle
//! (5 entries) for backward compatibility. This crate builds on top of
//! the existing `joey-orchestration` delegation engine.
//!
//! **Constitution**: strictly additive, workspace-first, narrow public API.

pub mod agents;
pub mod boulder;
pub mod categories;
pub mod goal;
pub mod intent_gate;
pub mod mode;
pub mod models;
pub mod notepad;
pub mod orchestrator;
pub mod plan_parser;
pub mod team;

// ── Public API re-exports ───────────────────────────────────────────
pub use agents::{registry::AgentRegistry, OmoAgent};
pub use categories::{resolve_category, validate_delegation, CategoryConfig};
pub use intent_gate::{check_ultrawork_activation, detect_keyword, KeywordType};
pub use orchestrator::{
    accumulate_wisdom, build_task_delegation_prompt, boulder_push_reminder,
    extract_wisdom, is_prometheus_write_allowed, prepare_plan_execution,
    route_delegation, start_work, wisdom_context_block,
    AtlasPlanConfig, DelegationRoute, ExtractedWisdom, OmoDelegationRequest,
    StartWorkResult, TaskExecutionResult,
};

/// Ultrawork-mode prompt overlay for the active agent (OMO FR-022/FR-024).
/// Returns the model-family-specific variant for the resolved model.
pub fn ultrawork_prompt(model: &str) -> String {
    agents::prompts::ultrawork_prompt(model)
}

/// Resolve the OMO system prompt for a named agent + model (BC-004).
/// Used when Tab-switching to inject the agent's identity.
pub fn dispatch_system_prompt(agent_name: &str, model: &str) -> String {
    agents::prompts::dispatch_system_prompt(agent_name, model)
}
pub use mode::{AgentMode, ToolPermissions};
pub use models::{
    resolve_model, AvailableModelSet, FallbackEntry, ModelFamily, ModelRequirement,
};
pub use boulder::{BoulderState, BoulderWork, BoulderWorkStatus};
pub use goal::{parse_goal_command, parse_subgoal_command, GoalAction, GoalState, GoalStatus, Subgoal, SubgoalAction};
pub use team::{
    activate_team, MemberActivity, TeamActivationError, TeamMailbox, TeamMember, TeamMemberKind,
    TeamModeConfig, TeamSpec, TeamTask, TeamTaskList, TeamTaskStatus, TmuxVisualizer,
};
