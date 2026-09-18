//! Contract test: POST /api/features/{id}/meaning/board/{taskId}/toggle —
//! checkbox must be anchored at the start of the task line (FR-018).
//!
//! Regression guard: the toggle used to locate the checkbox by searching for
//! `[ ]`/`[x]`/`[X]` ANYWHERE in the node bytes, so a DONE task whose
//! DESCRIPTION mentions the `[ ]` bracket syntax got its description
//! rewritten instead of its checkbox flipped.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn board_toggle_flips_anchored_checkbox_not_description_bracket() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    // T001 is DONE and its description documents the literal `[ ]` syntax.
    std::fs::write(
        dir.path().join("specs/001-test/tasks.md"),
        "# Tasks: Test Feature\n\n\
         - [X] T001 [P] Document the `[ ]` checkbox syntax\n\
         - [ ] T002 Do another thing\n",
    )
    .unwrap();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/features/001-test/meaning/board/T001/toggle")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let updated = std::fs::read_to_string(dir.path().join("specs/001-test/tasks.md")).unwrap();
    // The anchored checkbox flipped DONE -> TODO …
    assert!(
        updated.contains("- [ ] T001 [P] Document the `[ ]` checkbox syntax"),
        "checkbox must flip in place, description bracket untouched; got:\n{updated}"
    );
    // … and no stray second flip corrupted the description into `[x]`.
    assert!(!updated.contains("`[x]`"), "description bracket must not be rewritten");
    // The other task line is byte-identical.
    assert!(updated.contains("- [ ] T002 Do another thing"));
}
