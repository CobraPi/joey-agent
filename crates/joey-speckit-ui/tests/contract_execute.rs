//! Contract test: POST /api/features/{id}/tasks/{taskId}/execute — T033.
//!
//! Per Clarifications Q3, execution must be single-task only. This test
//! confirms the endpoint accepts one task id and returns a run_id without
//! touching any other tasks synchronously in the HTTP response.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn post_task_execute_returns_202_with_run_id() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/tasks/T001/execute")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json.get("run_id").is_some());
    assert!(!json["run_id"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn post_task_execute_does_not_modify_tasks_md_synchronously() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let before = std::fs::read_to_string(dir.path().join("specs/001-test/tasks.md")).unwrap();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/tasks/T002/execute")
        .body(Body::empty())
        .unwrap();

    let _response = app.oneshot(req).await.unwrap();

    // The HTTP response returns immediately (202); tasks.md updates happen
    // asynchronously via the run/WebSocket flow, not inline in this call.
    let after = std::fs::read_to_string(dir.path().join("specs/001-test/tasks.md")).unwrap();
    assert_eq!(before, after);
}
