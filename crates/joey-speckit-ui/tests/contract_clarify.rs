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
async fn post_clarify_answer_returns_200_with_hash() {
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
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json.get("spec_content_hash").is_some());
}
