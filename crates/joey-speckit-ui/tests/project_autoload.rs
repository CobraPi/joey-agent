//! Tests for the project auto-load path: `read_active_feature_id` (reads
//! `.specify/feature.json`) and `GET /api/project` (single-call bootstrap that
//! surfaces the active feature + feature list + project flags).
//!
//! These exercise the behavior added so the frontend can auto-load a
//! project's specs on startup without forcing the user to pick a feature.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use joey_speckit_ui::read_active_feature_id;
use serde_json::Value;
use std::path::PathBuf;
use tempfile::tempdir;
use tower::ServiceExt;

// -------------------------------------------------------------------------
// read_active_feature_id (lib.rs)
// -------------------------------------------------------------------------

#[test]
fn read_active_feature_id_none_when_file_absent() {
    let dir = tempdir().unwrap();
    assert!(read_active_feature_id(dir.path()).is_none());
}

#[test]
fn read_active_feature_id_parses_specs_prefix() {
    let dir = tempdir().unwrap();
    common::write_active_feature(&dir, "012-spec-studio-visual-ide");
    assert_eq!(
        read_active_feature_id(dir.path()),
        Some("012-spec-studio-visual-ide".to_string())
    );
}

#[test]
fn read_active_feature_id_accepts_bare_feature_directory() {
    // Some spec-kit versions write just the feature id, or a path without the
    // `specs/` prefix. We only rely on the final path component.
    let dir = tempdir().unwrap();
    let specify_dir = dir.path().join(".specify");
    std::fs::create_dir_all(&specify_dir).unwrap();
    std::fs::write(
        specify_dir.join("feature.json"),
        "{\n  \"feature_directory\": \"003-omo-orchestration\"\n}\n",
    )
    .unwrap();
    assert_eq!(
        read_active_feature_id(dir.path()),
        Some("003-omo-orchestration".to_string())
    );
}

#[test]
fn read_active_feature_id_ignores_unknown_keys() {
    let dir = tempdir().unwrap();
    let specify_dir = dir.path().join(".specify");
    std::fs::create_dir_all(&specify_dir).unwrap();
    std::fs::write(
        specify_dir.join("feature.json"),
        "{\n  \"feature_directory\": \"specs/001-test\",\n  \"extra\": true\n}\n",
    )
    .unwrap();
    assert_eq!(
        read_active_feature_id(dir.path()),
        Some("001-test".to_string())
    );
}

#[test]
fn read_active_feature_id_none_when_malformed() {
    let dir = tempdir().unwrap();
    let specify_dir = dir.path().join(".specify");
    std::fs::create_dir_all(&specify_dir).unwrap();
    std::fs::write(specify_dir.join("feature.json"), "not json").unwrap();
    assert!(read_active_feature_id(dir.path()).is_none());
}

#[test]
fn read_active_feature_id_none_when_empty_directory() {
    let dir = tempdir().unwrap();
    let specify_dir = dir.path().join(".specify");
    std::fs::create_dir_all(&specify_dir).unwrap();
    std::fs::write(
        specify_dir.join("feature.json"),
        "{\n  \"feature_directory\": \"\"\n}\n",
    )
    .unwrap();
    assert!(read_active_feature_id(dir.path()).is_none());
}

// -------------------------------------------------------------------------
// GET /api/project
// -------------------------------------------------------------------------

/// Helper: send a request through the router and return (status, json body).
async fn project_response(
    dir: &tempfile::TempDir,
) -> (StatusCode, Value) {
    let app = common::router_for(dir);
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/project")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn project_endpoint_surfaces_active_feature_and_list() {
    let dir = common::make_fixture_repo("001-active");
    // Add a second feature so the list isn't a singleton.
    let other = dir.path().join("specs").join("002-other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("spec.md"),
        "# Feature Specification: Other\n\n**Status**: Draft\n\n## Requirements\n- **FR-001**: x.\n",
    )
    .unwrap();
    // Mark 001-active as the project's active feature.
    common::write_active_feature(&dir, "001-active");

    let (status, body) = project_response(&dir).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(body["active_feature_id"], "001-active");
    assert_eq!(body["has_specs_dir"], true);
    assert_eq!(body["has_specify_dir"], true);

    // The feature list should contain both features.
    let features = body["features"].as_array().expect("features array");
    let ids: Vec<&str> = features
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"001-active"));
    assert!(ids.contains(&"002-other"));

    // The active feature's full parsed model should be inlined so the frontend
    // can paint without a second round-trip.
    assert_eq!(body["active_feature"]["id"], "001-active");
    assert_eq!(
        body["active_feature"]["specification"]["title"],
        "Test Feature"
    );
}

#[tokio::test]
async fn project_endpoint_null_active_feature_when_no_feature_json() {
    let dir = common::make_fixture_repo("001-test");
    // No .specify/feature.json written.

    let (status, body) = project_response(&dir).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["active_feature_id"].is_null());
    assert!(body["active_feature"].is_null());
    // The feature list should still surface the feature on disk.
    let features = body["features"].as_array().expect("features array");
    assert_eq!(features.len(), 1);
    assert_eq!(features[0]["id"], "001-test");
}

#[tokio::test]
async fn project_endpoint_null_active_feature_when_id_not_on_disk() {
    // feature.json points at a feature that doesn't exist under specs/. We must
    // not crash and must not return a half-loaded feature; the frontend falls
    // back to its feature picker.
    let dir = common::make_fixture_repo("001-real");
    common::write_active_feature(&dir, "999-missing");

    let (status, body) = project_response(&dir).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active_feature_id"], "999-missing");
    assert!(body["active_feature"].is_null());
}

#[tokio::test]
async fn project_endpoint_reports_missing_project_dirs() {
    // Empty tempdir: no specs/, no .specify/. The endpoint should still 200 and
    // report the gaps so the frontend can offer `specify init`.
    let dir = tempdir().unwrap();
    let _ = PathBuf::from(dir.path()); // keep type in scope
    let (status, body) = project_response(&dir).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["has_specs_dir"], false);
    assert_eq!(body["has_specify_dir"], false);
    assert_eq!(body["features"].as_array().unwrap().len(), 0);
    assert!(body["active_feature_id"].is_null());
}

// -------------------------------------------------------------------------
// Regression: the existing GET /api/features endpoint must still work.
// -------------------------------------------------------------------------

#[tokio::test]
async fn get_features_still_works_alongside_project_endpoint() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
