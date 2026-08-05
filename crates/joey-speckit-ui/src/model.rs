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

// =====================================================================
// Feature 010: Spec-Kit Development IDE — additive entity types.
//
// All types below are strictly additive (Constitution VII). Existing types
// above are untouched. See specs/010-speckit-development-ide/data-model.md.
// =====================================================================

/// Which kind of feature artifact a path represents (data-model §2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    #[default]
    Spec,
    Plan,
    Tasks,
    Checklist,
    Research,
    DataModel,
    Contract,
    Quickstart,
    Constitution,
    Supporting,
}

impl ArtifactKind {
    /// Which lifecycle phase owns this artifact kind — used for explorer
    /// grouping (FR-003).
    pub fn workflow_phase(&self) -> WorkflowPhase {
        match self {
            ArtifactKind::Spec => WorkflowPhase::Specify,
            ArtifactKind::Plan => WorkflowPhase::Plan,
            ArtifactKind::Tasks => WorkflowPhase::Tasks,
            ArtifactKind::Checklist => WorkflowPhase::Checklist,
            ArtifactKind::Research => WorkflowPhase::Plan,
            ArtifactKind::DataModel => WorkflowPhase::Plan,
            ArtifactKind::Contract => WorkflowPhase::Plan,
            ArtifactKind::Quickstart => WorkflowPhase::Implement,
            ArtifactKind::Constitution => WorkflowPhase::Constitution,
            ArtifactKind::Supporting => WorkflowPhase::Supporting,
        }
    }
}

/// Lifecycle phase for explorer grouping (data-model §2 `workflow_phase`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    #[default]
    Constitution,
    Specify,
    Clarify,
    Plan,
    Checklist,
    Tasks,
    Analyze,
    Implement,
    Supporting,
}

/// Save/sync state of an artifact (FR-005).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveState {
    Clean,
    Dirty,
    Saving,
    Saved,
    Invalid,
    ExternallyChanged,
    ReadOnly,
    #[default]
    Unparsed,
}

/// A repository-backed feature document (data-model §2). Extends the
/// implicit notion in specs/001 to every authorable artifact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Artifact {
    pub path: String,
    pub kind: ArtifactKind,
    pub exists: bool,
    pub content_hash: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    pub save_state: SaveState,
    pub validity: Vec<ValidationFinding>,
    pub workflow_phase: WorkflowPhase,
    #[serde(default)]
    pub stale: bool,
    pub stale_reason: Option<String>,
}

/// A located validation issue/warning/info (data-model §8, FR-007).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidationFinding {
    pub finding_id: String,
    pub severity: Severity,
    pub code: String,
    pub description: String,
    pub location: ArtifactLocation,
    pub remediation: Option<String>,
}

/// Where a finding or reference points (data-model §8, FR-023).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactLocation {
    pub path: String,
    pub line_or_section: String,
}

// ---------------------------------------------------------------------
// Workflow step, run config, attempt (data-model §3/4/5)
// ---------------------------------------------------------------------

/// A lifecycle stage, derived from the installed spec-kit + skills (FR-008).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: String,
    pub order: i32,
    pub purpose: String,
    pub inputs: Vec<ArtifactRef>,
    pub outputs: Vec<ArtifactRef>,
    pub prerequisites: Vec<String>,
    pub available: bool,
    pub state: StepState,
    pub blocking_reason: Option<String>,
    pub latest_attempt_id: Option<String>,
    pub installed_definition_ref: String,
}

/// Lightweight artifact reference (path + kind) for step inputs/outputs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub path: String,
    #[serde(default)]
    pub kind: Option<ArtifactKind>,
}

/// Derived readiness of a step (FR-022). `attention_needed` is a
/// presentation aggregate, never persisted (spec US2 note).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Ready,
    Blocked,
    Running,
    AttentionNeeded,
    Succeeded,
    Failed,
    Stale,
    Unavailable,
    #[default]
    Unparsed,
}

/// The effective inputs for one attempt — immutable after prepare (data-model §4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunConfiguration {
    pub step_id: String,
    pub effective_instructions: String,
    pub scope: Scope,
    #[serde(default)]
    pub options: Option<AgentOptions>,
    pub option_catalog_rev: String,
    pub change_mode: Option<ChangeMode>,
    pub override_id: Option<String>,
    pub prepared_at: Option<String>,
}

/// What the agent's run targets (data-model §4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    pub targets: Vec<ArtifactRef>,
    #[serde(default)]
    pub task_ids: Vec<String>,
}

/// Server-advertised agent options (FR-010).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOptions {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub max_iterations: Option<i32>,
}

/// Staged or direct change mode — mandatory explicit selection every run (FR-010).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeMode {
    Staged,
    Direct,
}

/// One persisted execution record of a workflow step run (data-model §5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowAttempt {
    pub attempt_id: String,
    pub feature_id: String,
    pub step_id: String,
    pub initiator: String,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    pub status: AttemptStatus,
    pub run_config: RunConfiguration,
    #[serde(default)]
    pub transcript: Vec<TranscriptEntry>,
    #[serde(default)]
    pub interactions: Vec<AgentInteraction>,
    #[serde(default)]
    pub changes: Option<ChangeSet>,
    #[serde(default)]
    pub validation: Vec<ValidationFinding>,
    #[serde(default)]
    pub checkpoint: Option<Checkpoint>,
    #[serde(default)]
    pub prior_attempt_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// Persisted attempt status (data-model §5). Presentation-only
/// `attention_needed` is never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Preparing,
    Running,
    AwaitingInput,
    AwaitingApproval,
    RecoverableFailure,
    Conflicted,
    RecoveryFailed,
    Succeeded,
    Failed,
    Cancelled,
    RecoveryNeeded,
    #[default]
    Unparsed,
}

/// One entry in the run transcript (data-model §5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    pub at: String,
}

// ---------------------------------------------------------------------
// Agent interaction, change set, dependency link (data-model §6/7/9)
// ---------------------------------------------------------------------

/// A question/answer, approval/decision, progress, or tool-activity event (§6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInteraction {
    pub interaction_id: String,
    pub attempt_id: String,
    pub kind: InteractionKind,
    pub payload: serde_json::Value,
    #[serde(default)]
    pub confirmed: bool,
    pub at: String,
}

/// Kind of interaction (data-model §6).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Question,
    Answer,
    ApprovalRequest,
    ApprovalDecision,
    Progress,
    ToolActivity,
    #[default]
    Unparsed,
}

/// A Git-backed change set produced by a run (data-model §7).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangeSet {
    pub attempt_id: String,
    pub files: Vec<ChangedFile>,
    pub mode: Option<ChangeMode>,
    #[serde(default)]
    pub recovery_action: Option<String>,
}

/// One changed file in a change set (data-model §7).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileChangeStatus,
    pub additions: i32,
    pub removals: i32,
    #[serde(default)]
    pub why: Option<String>,
    pub hunks: Vec<Hunk>,
    pub accept_state: AcceptState,
}

/// File-level change status.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeStatus {
    Added,
    Modified,
    Removed,
    #[default]
    Unparsed,
}

/// One hunk in a changed file (data-model §7, FR-016).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hunk {
    pub hunk_id: String,
    pub old_range: String,
    pub new_range: String,
    pub accept_state: AcceptState,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Accept/reject state for hunks and files (FR-016).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptState {
    Pending,
    Accepted,
    Rejected,
    #[default]
    Unparsed,
}

/// A traceable upstream→downstream edge (data-model §9, FR-021/023/032).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyLink {
    pub from: ArtifactLocation,
    pub to: ArtifactLocation,
    pub kind: DependencyKind,
}

/// Kind of dependency link (data-model §9).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    RequirementToPlan,
    PlanToTask,
    TaskToAttempt,
    AttemptToFinding,
    ArtifactToStepOutput,
    #[default]
    Unparsed,
}

// ---------------------------------------------------------------------
// Workspace preferences, checkpoint (data-model §10, FR-026/033)
// ---------------------------------------------------------------------

/// Non-content UI preferences (data-model §10, FR-026).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspacePreference {
    #[serde(default)]
    pub last_feature_id: Option<String>,
    #[serde(default)]
    pub open_artifacts: Vec<String>,
    #[serde(default)]
    pub active_view: Option<String>,
    #[serde(default)]
    pub pane_layout: Option<serde_json::Value>,
    #[serde(default)]
    pub filters: Option<serde_json::Value>,
}

/// Latest safe recovery point for an attempt (FR-033).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Checkpoint {
    pub tree_ish: String,
    #[serde(default)]
    pub last_confirmed_interaction_id: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
}
