//! Shared test helpers for contract tests: build a temp `specs/<id>/`
//! fixture directory and the axum router pointed at it.

use std::path::PathBuf;

use joey_speckit_ui::{api::build_router, AppState};
use tempfile::TempDir;

#[allow(dead_code)]
pub fn make_fixture_repo(feature_id: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let feature_dir = dir.path().join("specs").join(feature_id);
    std::fs::create_dir_all(&feature_dir).unwrap();

    std::fs::write(
        feature_dir.join("spec.md"),
        "# Feature Specification: Test Feature\n\n**Created**: 2026-01-01\n**Status**: Draft\n\n## Requirements\n- **FR-001**: Must do a thing.\n",
    )
    .unwrap();

    std::fs::write(
        feature_dir.join("plan.md"),
        "# Implementation Plan: Test Feature\n\n## Summary\nA plan.\n\n## Constitution Check\n| Principle | Status | Notes |\n|---|---|---|\n| I. Test | PASS | ok |\n",
    )
    .unwrap();

    std::fs::write(
        feature_dir.join("tasks.md"),
        "# Tasks: Test Feature\n\n- [ ] T001 [P] Do a thing\n- [ ] T002 Do another thing\n",
    )
    .unwrap();

    dir
}

#[allow(dead_code)]
pub fn router_for(dir: &TempDir) -> axum::Router {
    let repo_root: PathBuf = dir.path().to_path_buf();
    build_router(AppState::new(repo_root))
}

/// Write a `.specify/feature.json` pointing at `feature_id`, mirroring what
/// `specify` / `.specify/scripts/bash/create-new-feature.sh` writes. Used by
/// tests that exercise the auto-load path.
#[allow(dead_code)]
pub fn write_active_feature(dir: &TempDir, feature_id: &str) {
    let specify_dir = dir.path().join(".specify");
    std::fs::create_dir_all(&specify_dir).unwrap();
    let json = format!(
        "{{\n  \"feature_directory\": \"specs/{feature_id}\"\n}}\n"
    );
    std::fs::write(specify_dir.join("feature.json"), json).unwrap();
}
