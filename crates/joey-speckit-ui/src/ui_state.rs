//! Per-repo+branch UI-state JSON (FR-032 overlay layer, data-model.md §4).
//!
//! A small JSON file at `~/.joey/speckit-ui/ui-state/<repo-hash>-<branch>.json`
//! holds mutable UI state (board positions, filters, panel layout, open
//! artifacts). Rewritten atomically on layout/filter/selection changes (rare).
//! Carries `schema_version: 1`. Explicitly excludes unsaved artifact content,
//! secrets, anything not belonging to this repo+branch.
//!
//! STUB types only — full `OverlayStore` impl lands in Phase 2 (T020).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The mandatory schema version stamp on the UI-state JSON (Constitution VII).
pub const UI_STATE_SCHEMA_VERSION: u16 = 1;

/// The per-repo+branch UI-state blob (data-model.md §4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiState {
    pub schema_version: u16,
    pub repo_hash: String,
    pub branch: String,
    #[serde(default)]
    pub selected_feature: Option<String>,
    #[serde(default)]
    pub open_artifacts: Vec<String>,
    #[serde(default)]
    pub active_view: Option<String>,
    #[serde(default)]
    pub pane_layout: PaneLayout,
    #[serde(default)]
    pub board_filters: BoardFilters,
    #[serde(default)]
    pub scroll_positions: HashMap<String, f32>,
    #[serde(default)]
    pub selection: Option<String>,
}

impl UiState {
    pub fn new(repo_hash: String, branch: String) -> Self {
        UiState {
            schema_version: UI_STATE_SCHEMA_VERSION,
            repo_hash,
            branch,
            selected_feature: None,
            open_artifacts: Vec::new(),
            active_view: None,
            pane_layout: PaneLayout::default(),
            board_filters: BoardFilters::default(),
            scroll_positions: HashMap::new(),
            selection: None,
        }
    }
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            schema_version: UI_STATE_SCHEMA_VERSION,
            repo_hash: String::new(),
            branch: String::new(),
            selected_feature: None,
            open_artifacts: Vec::new(),
            active_view: None,
            pane_layout: PaneLayout::default(),
            board_filters: BoardFilters::default(),
            scroll_positions: HashMap::new(),
            selection: None,
        }
    }
}

/// Pane sizes + collapse state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaneLayout {
    #[serde(default)]
    pub sizes: Vec<f32>,
    #[serde(default)]
    pub collapsed: Vec<String>,
}

/// Board phase/story/parallel/completion filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardFilters {
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub story: Option<String>,
    #[serde(default)]
    pub parallel_only: bool,
    #[serde(default)]
    pub completion: Option<String>,
}

/// Errors from overlay-store operations.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------
// Overlay-record extension (data-model.md §4). The existing
// `WorkflowAttempt` record and `schema_version: 1` gate are preserved
// (Constitution VII); two new variants are added.
// ---------------------------------------------------------------------

/// The on-disk record-type tag for the JSONL history (Constitution VII —
/// additive over specs/010's single `workflow_attempt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    /// Existing, unchanged (specs/010).
    WorkflowAttempt,
    /// NEW — accepted clarify answer (FR-024).
    AcceptedClarify,
    /// NEW — anchored comment thread (FR-026).
    CommentThread,
}

/// One anchored comment message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommentMessage {
    pub author: String,
    pub text: String,
    pub at: String,
}

/// An accepted clarify answer (FR-024), stored as a JSONL record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptedClarifyRecord {
    pub schema_version: u32,
    pub record_type: String,
    pub timestamp: String,
    pub marker_node: String,
    pub question: String,
    pub answer: String,
    pub patch_revision: String,
}

/// An anchored comment thread (FR-026), stored as a JSONL record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentThreadRecord {
    pub schema_version: u32,
    pub record_type: String,
    pub thread_id: String,
    pub anchor_node: String,
    pub anchor_fingerprint: String,
    pub messages: Vec<CommentMessage>,
}

impl AcceptedClarifyRecord {
    pub fn new(
        timestamp: String,
        marker_node: String,
        question: String,
        answer: String,
        patch_revision: String,
    ) -> Self {
        Self {
            schema_version: 1,
            record_type: "accepted_clarify".to_string(),
            timestamp,
            marker_node,
            question,
            answer,
            patch_revision,
        }
    }
}

impl CommentThreadRecord {
    pub fn new(
        thread_id: String,
        anchor_node: String,
        anchor_fingerprint: String,
        messages: Vec<CommentMessage>,
    ) -> Self {
        Self {
            schema_version: 1,
            record_type: "comment_thread".to_string(),
            thread_id,
            anchor_node,
            anchor_fingerprint,
            messages,
        }
    }
}

// =====================================================================
// T020: UI-state JSON save/load — per-repo+branch atomic persistence.
// =====================================================================

use std::path::{Path, PathBuf};

/// Resolve the UI-state JSON path for a (repo_hash, branch) pair under
/// `joey_home` (typically `~/.joey/`).
pub fn ui_state_file(joey_home: &Path, repo_hash: &str, branch: &str) -> PathBuf {
    joey_home
        .join("speckit-ui")
        .join("ui-state")
        .join(format!("{repo_hash}-{branch}.json"))
}

/// Load the UI-state blob for a (repo, branch). Missing file → default.
pub fn load_ui_state(joey_home: &Path, repo_hash: &str, branch: &str) -> Result<UiState, OverlayError> {
    let path = ui_state_file(joey_home, repo_hash, branch);
    if !path.exists() {
        return Ok(UiState::new(repo_hash.to_string(), branch.to_string()));
    }
    let content = std::fs::read_to_string(&path)?;
    let mut state: UiState = serde_json::from_str(&content)?;
    // Enforce schema_version gate (Constitution VII).
    if state.schema_version != UI_STATE_SCHEMA_VERSION {
        tracing::warn!(
            schema_version = state.schema_version,
            "ui-state file has unexpected schema_version; returning default"
        );
        return Ok(UiState::new(repo_hash.to_string(), branch.to_string()));
    }
    state.repo_hash = repo_hash.to_string();
    state.branch = branch.to_string();
    Ok(state)
}

/// Atomically save the UI-state blob (write-temp + rename).
pub fn save_ui_state(joey_home: &Path, state: &UiState) -> Result<(), OverlayError> {
    let path = ui_state_file(joey_home, &state.repo_hash, &state.branch);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(state)?;
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Verify that a candidate UI-state path is NOT inside any `specs/` directory
/// (FR-032 write-tree isolation). Returns true if the path is safe (outside
/// any feature directory).
pub fn is_write_tree_isolated(candidate: &Path) -> bool {
    let mut is_isolated = true;
    for component in candidate.components() {
        if let std::path::Component::Normal(os_str) = component {
            if os_str.to_string_lossy() == "specs" {
                is_isolated = false;
                break;
            }
        }
    }
    is_isolated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = UiState::new("repo-hash-123".to_string(), "main".to_string());
        state.selected_feature = Some("012-spec-studio".to_string());
        state.active_view = Some("atlas".to_string());
        state.open_artifacts = vec!["spec.md".to_string(), "tasks.md".to_string()];

        save_ui_state(dir.path(), &state).unwrap();
        let loaded = load_ui_state(dir.path(), "repo-hash-123", "main").unwrap();

        assert_eq!(loaded.schema_version, UI_STATE_SCHEMA_VERSION);
        assert_eq!(loaded.selected_feature.as_deref(), Some("012-spec-studio"));
        assert_eq!(loaded.active_view.as_deref(), Some("atlas"));
        assert_eq!(loaded.open_artifacts.len(), 2);
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let state = load_ui_state(dir.path(), "no-such-repo", "main").unwrap();
        assert_eq!(state.schema_version, UI_STATE_SCHEMA_VERSION);
        assert!(state.selected_feature.is_none());
    }

    #[test]
    fn write_tree_isolation_check() {
        assert!(is_write_tree_isolated(Path::new("/home/user/.joey/speckit-ui/ui-state/r-b.json")));
        assert!(!is_write_tree_isolated(Path::new("/repo/specs/012-foo/ui-state.json")));
        assert!(!is_write_tree_isolated(Path::new("/repo/specs/012-foo")));
    }

    #[test]
    fn ui_state_file_path_is_under_joey_home() {
        let path = ui_state_file(Path::new("/home/user/.joey"), "abc123", "main");
        assert!(path.starts_with("/home/user/.joey/speckit-ui/ui-state"));
        assert!(path.to_string_lossy().contains("abc123-main"));
    }
}
