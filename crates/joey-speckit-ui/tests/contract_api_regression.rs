//! Regression test (T012, Constitution VII): assert every `specs/001`
//! REST/WS endpoint is preserved unchanged when the new feature-010 routes
//! are added.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

/// Verify all specs/001 REST endpoints are still reachable.
#[tokio::test]
async fn all_specs_001_rest_endpoints_preserved() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    // GET /api/features
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET /api/features/{id}
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/features/001-test")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // POST /api/features/{id}/clarify
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/clarify")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // POST /api/features/{id}/analyze
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/analyze")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    // analyze runs a subprocess that may fail in test env, but the route
    // itself must exist (not 404).
    assert_ne!(resp.status(), StatusCode::NOT_FOUND);

    // POST /api/init
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/init")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "integration": "claude",
                "script": "bash"
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_ne!(resp.status(), StatusCode::NOT_FOUND);
}

/// Verify PATCH /spec is preserved and still conflict-checked.
#[tokio::test]
async fn patch_spec_route_preserved() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/spec")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "target": { "type": "requirement", "id": "FR-001" },
                "new_text": "updated",
                "based_on_hash": "sha256:stale",
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // Stale hash → 409 (conflict), proving the route + conflict logic work.
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

/// Verify PATCH /tasks/{taskId} is preserved.
#[tokio::test]
async fn patch_task_route_preserved() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/tasks/T001")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "new_text": "updated",
                "based_on_hash": "sha256:stale",
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

/// Verify POST /tasks/{taskId}/execute is preserved.
#[tokio::test]
async fn task_execute_route_preserved() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/tasks/T001/execute")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}
