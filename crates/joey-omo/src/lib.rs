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
pub mod plan_parser;
pub mod team;

// ── Public API re-exports ───────────────────────────────────────────
pub use agents::{registry::AgentRegistry, OmoAgent};
pub use categories::{resolve_category, validate_delegation, CategoryConfig};
pub use intent_gate::{detect_keyword, KeywordType};
pub use mode::{AgentMode, ToolPermissions};
pub use models::{
    resolve_model, AvailableModelSet, FallbackEntry, ModelFamily, ModelRequirement,
};
pub use boulder::{BoulderState, BoulderWork, BoulderWorkStatus};
pub use goal::{parse_goal_command, GoalAction, GoalState, GoalStatus};
