//! Git-backed staging area contract (FR-010/015/016/017/020/033).
//!
//! Defines the `StagingArea` trait — the contract boundary US2's runner mocks
//! and US3 implements with a concrete Git-backed staging area. See
//! `contracts/staging-api.md`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::model::{ChangeMode, ChangeSet, Checkpoint, Scope};

/// Error from staging operations.
#[derive(Debug, thiserror::Error)]
pub enum StagingError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git operation failed: {0}")]
    Git(String),
    /// An in-flight attempt's change set overlaps the candidate's scope (FR-015).
    #[error("conflicting run: {0}")]
    ConflictingRun(String),
    #[error("other: {0}")]
    Other(String),
}

/// The root of a staging area — the worktree directory the agent runs inside.
#[derive(Debug, Clone)]
pub struct StagingRoot {
    /// Absolute path to the worktree directory.
    pub worktree: PathBuf,
    /// The mode this staging area was opened with.
    pub mode: ChangeMode,
    /// The attempt id this staging area belongs to.
    pub attempt_id: String,
}

/// A selection of hunks/files to apply (FR-016).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Selection {
    pub entries: Vec<SelectionEntry>,
    #[serde(default)]
    pub apply_all_accepted: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SelectionEntry {
    pub path: String,
    pub hunks: Vec<String>,
}

/// Outcome of an apply operation (FR-016/SC-016).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ApplyOutcome {
    pub applied: Vec<String>,
    pub warnings: Vec<DependencyWarning>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DependencyWarning {
    pub hunk_id: String,
    pub depends_on: Vec<String>,
    pub message: String,
}

/// The staging area contract: open/checkpoint/diff/apply/discard.
///
/// - US2 uses a mock/no-op implementation for testing the runner.
/// - US3 (T028) provides the concrete Git-backed implementation.
#[async_trait]
pub trait StagingArea: Send + Sync {
    /// Create a staging area for an attempt in `mode`.
    /// - direct: returns the primary worktree root (agent writes live).
    /// - staged: creates a temp worktree and returns it.
    async fn open(
        &self,
        repo_root: &Path,
        attempt_id: &str,
        mode: ChangeMode,
        scope: &Scope,
    ) -> Result<StagingRoot, StagingError>;

    /// Snapshot the current staging state as a Git tree-ish (checkpoint).
    async fn checkpoint(&self, root: &StagingRoot) -> Result<Checkpoint, StagingError>;

    /// Compute the change set (files + hunks) vs the primary tree.
    async fn diff(&self, root: &StagingRoot) -> Result<ChangeSet, StagingError>;

    /// Apply a selection of accepted hunks/files into the primary tree;
    /// emit dependency warnings for partial selections before application.
    async fn apply(
        &self,
        root: &StagingRoot,
        selection: &Selection,
    ) -> Result<ApplyOutcome, StagingError>;

    /// Discard staging (reject all). Safe recovery (FR-017).
    async fn discard(&self, root: &StagingRoot) -> Result<(), StagingError>;
}
