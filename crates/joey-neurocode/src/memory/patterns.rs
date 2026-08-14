//! Learned patterns and anti-patterns (FR-011, T040/T041).

use serde::{Deserialize, Serialize};

use crate::graph::NodeId;

/// A recorded successful generation (data-model.md Entity 6).
/// Stored in the `patterns` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedPattern {
    pub id: u64,
    pub prompt_signature: String,
    pub generation_summary: String,
    pub verify_result: String,
    pub artifact_ids: Vec<NodeId>,
    pub tier: String,
    pub created_at: String,
}

/// A recorded failure with its fix (data-model.md Entity 7).
/// Surfaced as a warning when the same area is edited again (FR-011).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedAntiPattern {
    pub id: u64,
    pub error_signature: String,
    pub error_output: String,
    pub resolution: String,
    pub artifact_ids: Vec<NodeId>,
    pub created_at: String,
    pub hit_count: u32,
    pub status: AntiPatternStatus,
}

/// Lifecycle status of an anti-pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AntiPatternStatus {
    Active,
    Resolved,
}

impl Default for AntiPatternStatus {
    fn default() -> Self {
        AntiPatternStatus::Active
    }
}
