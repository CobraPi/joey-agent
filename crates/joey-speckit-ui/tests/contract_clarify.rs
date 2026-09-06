//! Contract test: POST /api/features/{id}/clarify (+ /answer) — T017, T031.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn post_clarify_returns_202_with_session_id() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/clarify")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json.get("session_id").is_some());
    assert!(!json["session_id"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn post_clarify_answer_returns_501_not_implemented() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let body = serde_json::json!({ "answer": "Use SHA-256 content hashing." });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/clarify/some-session-id/answer")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    // The endpoint discards the answer body (no persistence yet) — it must
    // say so instead of returning a fake 200 success.
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"], "not_implemented");
}

#[tokio::test]
async fn post_clarify_answer_rejects_traversal_feature_id() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let body = serde_json::json!({ "answer": "whatever" });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/..%2F..%2Fetc/clarify/some-session-id/answer")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"], "invalid_path");
}
