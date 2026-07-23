//! Contract test: PATCH /api/features/{id}/spec — T015.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn patch_spec_success_updates_content_and_returns_new_hash() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let current = std::fs::read_to_string(dir.path().join("specs/001-test/spec.md")).unwrap();
    let hash = joey_speckit_ui::conflict::content_hash(&current);

    let target_line = current
        .lines()
        .find(|l| l.contains("FR-001"))
        .expect("fixture spec.md must contain an FR-001 line")
        .to_string();
    let new_line = target_line.replace("Must do a thing.", "Must do an updated thing.");

    let body = serde_json::json!({
        "target": { "type": "requirement", "id": "FR-001" },
        "new_text": new_line,
        "based_on_hash": hash,
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/spec")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json.get("content_hash").is_some());

    let updated = std::fs::read_to_string(dir.path().join("specs/001-test/spec.md")).unwrap();
    assert!(updated.contains("Must do an updated thing."));

    // Critical regression guard: PATCH /spec must only ever change the single
    // targeted line, never wholesale-replace the file (this is exactly the
    // bug class that once destroyed a real spec.md during manual testing).
    let original_lines: Vec<&str> = current.lines().collect();
    let updated_lines: Vec<&str> = updated.lines().collect();
    assert_eq!(
        original_lines.len(),
        updated_lines.len(),
        "PATCH /spec changed the total line count — it must only replace the single targeted line"
    );
    for (orig, upd) in original_lines.iter().zip(updated_lines.iter()) {
        if *orig == target_line {
            continue;
        }
        assert_eq!(orig, upd, "PATCH /spec modified a line other than the targeted one");
    }
}

#[tokio::test]
async fn patch_spec_conflict_leaves_file_untouched() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let original = std::fs::read_to_string(dir.path().join("specs/001-test/spec.md")).unwrap();
    let stale_hash = joey_speckit_ui::conflict::content_hash("stale content");

    let body = serde_json::json!({
        "target": { "type": "requirement", "id": "FR-001" },
        "new_text": "whatever new content",
        "based_on_hash": stale_hash,
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/001-test/spec")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"], "conflict");
    assert!(json.get("current_hash").is_some());

    let after = std::fs::read_to_string(dir.path().join("specs/001-test/spec.md")).unwrap();
    assert_eq!(after, original);
}

#[tokio::test]
async fn patch_spec_not_found_for_missing_feature() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let body = serde_json::json!({
        "target": { "type": "requirement", "id": "FR-001" },
        "new_text": "x",
        "based_on_hash": "sha256:deadbeef",
    });

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/features/does-not-exist/spec")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
