//! Data model for the SpecKit Visual UI backend.
//!
//! Types mirror `specs/001-speckit-visual-ui/data-model.md`. Parsing is
//! tolerant: malformed entries become `Status::Unparsed` (or otherwise
//! degrade gracefully) rather than panicking or being silently dropped.

use serde::{Deserialize, Serialize};

/// Shared status enum used across UserStory/Requirement/Task nodes.
///
/// `Unparsed` represents a malformed or unrecognized status marker found in
/// a source Markdown file; it is surfaced to the UI (Edge Cases in
/// data-model.md) rather than dropped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Status {
    Draft,
    InProgress,
    Completed,
    Approved,
    #[default]
    Unparsed,
}

/// Status specific to a single checkbox-backed Task line in tasks.md.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Done,
    /// Checkbox / line present but could not be confidently parsed.
    #[default]
    Unparsed,
}

/// One directory under `specs/<NNN-name>/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub directory: String,
    pub branch_name: Option<String>,
    pub specification: Option<Specification>,
    pub plan: Option<Plan>,
    pub tasks: Vec<Task>,
    /// Present when `plan.md` and/or `tasks.md` do not yet exist for this
    /// feature (Edge Cases: "not yet created" empty state).
    pub missing: Vec<String>,
    pub spec_content_hash: Option<String>,
    pub plan_content_hash: Option<String>,
    pub tasks_content_hash: Option<String>,
}

/// Parsed `spec.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Specification {
    pub title: String,
    pub created: Option<String>,
    pub status: Status,
    pub user_stories: Vec<UserStory>,
    pub requirements: Vec<Requirement>,
    pub clarifications: Vec<ClarificationEntry>,
    pub key_entities: Vec<String>,
    pub success_criteria: Vec<String>,
}

/// One `### User Story N` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserStory {
    pub id: String,
    pub title: String,
    pub priority: Option<String>,
    pub acceptance_scenarios: Vec<String>,
    pub status: Status,
}

/// One `- **FR-NNN**: ...` requirement line.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub text: String,
    pub user_story_ref: Option<String>,
}

/// One clarification session Q/A entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClarificationEntry {
    pub session_date: Option<String>,
    pub question: String,
    pub answer: Option<String>,
}

/// Parsed `plan.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub summary: String,
    pub technical_context: Option<String>,
    pub constitution_gates: Vec<ConstitutionGate>,
}

/// One row of the Constitution Check table in `plan.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConstitutionGate {
    pub principle: String,
    pub result: GateResult,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum GateResult {
    Pass,
    Fail,
    #[default]
    Unparsed,
}

/// One task line in `tasks.md` (a single markdown checkbox entry).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub parallel_eligible: bool,
    pub description: String,
    pub target_files: Vec<String>,
    pub status: TaskStatus,
    pub user_story_ref: Option<String>,
}

/// Result of `/speckit-analyze`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisFinding {
    pub severity: Severity,
    pub description: String,
    pub target_file: Option<String>,
    pub target_line_or_section: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
    #[default]
    Unparsed,
}
