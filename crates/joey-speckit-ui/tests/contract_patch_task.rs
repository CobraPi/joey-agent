//! Contract test: PATCH /api/features/{id}/tasks/{taskId} — T016.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn patch_task_success_updates_content_and_returns_new_hash() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let current = std::fs::read_to_string(dir.path().join("specs/001-test/tasks.md")).unwrap();
    let hash = joey_speckit_ui::conflict::content_hash(&current);

    let body = serde_json::json!({
        "new_text": "- [X] T001 [P] Do a thing (done)",
        "based_on_hash": hash,
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/tasks/T001")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json.get("content_hash").is_some());

    let updated = std::fs::read_to_string(dir.path().join("specs/001-test/tasks.md")).unwrap();
    assert!(updated.contains("Do a thing (done)"));
}

#[tokio::test]
async fn patch_task_conflict_on_stale_hash() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let stale_hash = joey_speckit_ui::conflict::content_hash("not the real content");

    let body = serde_json::json!({
        "new_text": "- [X] T001 changed",
        "based_on_hash": stale_hash,
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/tasks/T001")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"], "conflict");

    // File must remain unmodified.
    let content = std::fs::read_to_string(dir.path().join("specs/001-test/tasks.md")).unwrap();
    assert!(content.contains("- [ ] T001 [P] Do a thing"));
}

#[tokio::test]
async fn patch_task_not_found_for_missing_task_id() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let body = serde_json::json!({
        "new_text": "irrelevant",
        "based_on_hash": "sha256:deadbeef",
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/tasks/T999")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
