//! UI-state JSON round-trip + write-tree isolation test (T024, FR-032).
//!
//! Asserts the UI-state JSON round-trips, `schema_version` preserved, and the
//! store never writes inside any `specs/` directory (FR-032 write-tree
//! isolation).

use std::path::Path;

use joey_speckit_ui::ui_state::{
    is_write_tree_isolated, load_ui_state, save_ui_state, BoardFilters, PaneLayout,
    UI_STATE_SCHEMA_VERSION, UiState,
};

#[test]
fn ui_state_full_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = UiState::new("repo-hash-abc".to_string(), "feature-branch".to_string());
    state.selected_feature = Some("012-spec-studio".to_string());
    state.open_artifacts = vec!["spec.md".to_string(), "plan.md".to_string()];
    state.active_view = Some("atlas".to_string());
    state.pane_layout = PaneLayout {
        sizes: vec![0.5, 0.5],
        collapsed: vec!["left".to_string()],
    };
    state.board_filters = BoardFilters {
        phase: Some("1".to_string()),
        story: Some("US1".to_string()),
        parallel_only: true,
        completion: None,
    };
    state.selection = Some("requirement:FR-016".to_string());

    save_ui_state(dir.path(), &state).unwrap();
    let loaded = load_ui_state(dir.path(), "repo-hash-abc", "feature-branch").unwrap();

    assert_eq!(loaded.schema_version, UI_STATE_SCHEMA_VERSION);
    assert_eq!(loaded.selected_feature.as_deref(), Some("012-spec-studio"));
    assert_eq!(loaded.active_view.as_deref(), Some("atlas"));
    assert_eq!(loaded.pane_layout.sizes, vec![0.5, 0.5]);
    assert!(loaded.board_filters.parallel_only);
    assert_eq!(loaded.selection.as_deref(), Some("requirement:FR-016"));
}

#[test]
fn schema_version_preserved_through_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let state = UiState::new("h".to_string(), "b".to_string());
    save_ui_state(dir.path(), &state).unwrap();
    let loaded = load_ui_state(dir.path(), "h", "b").unwrap();
    assert_eq!(loaded.schema_version, 1);
}

#[test]
fn missing_file_returns_default_with_correct_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = load_ui_state(dir.path(), "never", "main").unwrap();
    assert_eq!(loaded.schema_version, UI_STATE_SCHEMA_VERSION);
    assert!(loaded.selected_feature.is_none());
}

#[test]
fn store_never_writes_inside_specs_directory() {
    // The canonical UI-state path is under ~/.joey/speckit-ui/ui-state/.
    let safe = Path::new("/home/user/.joey/speckit-ui/ui-state/repo-main.json");
    assert!(is_write_tree_isolated(safe));

    // A path inside specs/ must fail the isolation check.
    let unsafe_path = Path::new("/repo/specs/012-foo/ui-state.json");
    assert!(!is_write_tree_isolated(unsafe_path));

    let also_unsafe = Path::new("/repo/specs/012-foo");
    assert!(!is_write_tree_isolated(also_unsafe));
}

#[test]
fn save_then_overwrite_preserves_latest_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = UiState::new("h".to_string(), "b".to_string());
    state.active_view = Some("atlas".to_string());
    save_ui_state(dir.path(), &state).unwrap();

    state.active_view = Some("board".to_string());
    save_ui_state(dir.path(), &state).unwrap();

    let loaded = load_ui_state(dir.path(), "h", "b").unwrap();
    assert_eq!(loaded.active_view.as_deref(), Some("board"));
}
