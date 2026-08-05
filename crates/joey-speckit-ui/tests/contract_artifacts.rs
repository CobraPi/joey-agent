//! Contract tests: feature-010 artifact endpoints (T013/T014/T015).

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn get_artifacts_lists_existing_and_missing() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features/001-test/artifacts")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let artifacts = json["artifacts"].as_array().unwrap();
    assert!(!artifacts.is_empty());

    // spec.md exists in the fixture.
    let spec = artifacts
        .iter()
        .find(|a| a["kind"] == "spec")
        .expect("spec artifact should be listed");
    assert_eq!(spec["exists"], true);

    // plan.md exists in the fixture too.
    let plan = artifacts
        .iter()
        .find(|a| a["kind"] == "plan")
        .expect("plan artifact should be listed");
    assert_eq!(plan["exists"], true);

    // research.md does not exist.
    let research = artifacts
        .iter()
        .find(|a| a["kind"] == "research")
        .expect("research artifact should still be listed (createable)");
    assert_eq!(research["exists"], false);
}

#[tokio::test]
async fn get_artifact_returns_text_and_outline() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features/001-test/artifacts/specs/001-test/plan.md")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["text"].as_str().unwrap().contains("Implementation Plan"));
    assert!(json["content_hash"].as_str().unwrap().starts_with("sha256:"));
    assert!(json["outline"].as_array().is_some());
}

#[tokio::test]
async fn get_artifact_404_for_nonexistent() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features/001-test/artifacts/specs/001-test/research.md")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_artifact_whole_scope_succeeds() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let current =
        std::fs::read_to_string(dir.path().join("specs/001-test/plan.md")).unwrap();
    let hash = joey_speckit_ui::conflict::content_hash(&current);

    let new_text = "# Implementation Plan: Updated\n\n## Summary\nNew summary.\n\n## Technical Context\nRust.\n\n## Constitution Check\nPass.\n";

    let body = serde_json::json!({
        "new_text": new_text,
        "based_on_hash": hash,
        "scope": { "whole": true },
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/artifacts/specs/001-test/plan.md")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let after = std::fs::read_to_string(dir.path().join("specs/001-test/plan.md")).unwrap();
    assert!(after.contains("New summary."));
}

#[tokio::test]
async fn patch_artifact_conflict_leaves_file_untouched() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let original =
        std::fs::read_to_string(dir.path().join("specs/001-test/plan.md")).unwrap();
    let stale_hash = joey_speckit_ui::conflict::content_hash("stale");

    let body = serde_json::json!({
        "new_text": "completely different",
        "based_on_hash": stale_hash,
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/artifacts/specs/001-test/plan.md")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let after =
        std::fs::read_to_string(dir.path().join("specs/001-test/plan.md")).unwrap();
    assert_eq!(after, original);
}

#[tokio::test]
async fn patch_artifact_section_scope_preserves_rest() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let current =
        std::fs::read_to_string(dir.path().join("specs/001-test/plan.md")).unwrap();
    let hash = joey_speckit_ui::conflict::content_hash(&current);

    let body = serde_json::json!({
        "new_text": "A completely rewritten summary that replaces the old one.",
        "based_on_hash": hash,
        "scope": { "section": "Summary" },
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/artifacts/specs/001-test/plan.md")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let after =
        std::fs::read_to_string(dir.path().join("specs/001-test/plan.md")).unwrap();
    assert!(after.contains("completely rewritten summary"));
    // Constitution Check section should still be there.
    assert!(after.contains("Constitution Check"));
}

#[tokio::test]
async fn get_workflow_returns_step_catalog() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features/001-test/workflow")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let steps = json["steps"].as_array().unwrap();
    assert!(!steps.is_empty());
    assert!(steps.iter().any(|s| s["id"] == "constitution"));
    assert!(steps.iter().any(|s| s["id"] == "implement"));
}

#[tokio::test]
async fn get_options_returns_catalog_with_revision() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/options")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["revision"].as_str().unwrap().starts_with("sha256:"));
    assert!(json["models"].as_array().unwrap().len() > 0);
}

#[tokio::test]
async fn post_run_rejects_missing_change_mode() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let body = serde_json::json!({
        "option_catalog_rev": "sha256:dummy",
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/workflow/plan/run")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn post_run_rejects_stale_option_catalog() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let body = serde_json::json!({
        "option_catalog_rev": "sha256:stale",
        "change_mode": "staged",
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/workflow/plan/run")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"], "stale_option_catalog");
}

#[tokio::test]
async fn get_health_returns_connectivity_status() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["backend_reachable"], true);
}

#[tokio::test]
async fn override_lifecycle_works() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    // PUT override.
    let body = serde_json::json!({ "instructions": "custom plan instructions" });
    let req = Request::builder()
        .method(Method::PUT)
        .uri("/api/features/001-test/workflow/plan/override")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET config shows override.
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features/001-test/workflow/plan/config")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["override"].is_object());
    assert_eq!(
        json["effective_instructions"],
        "custom plan instructions"
    );

    // DELETE override.
    let req = Request::builder()
        .method(Method::DELETE)
        .uri("/api/features/001-test/workflow/plan/override")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}
