//! Regression test: JSONL history round-trip (T011, Constitution VII).
//!
//! Asserts that `WorkflowAttempt` → JSONL line → record preserves the
//! `schema_version`, that partial lines are tolerated, and stubs a
//! v1→v2 migration path test (Constitution VII public-format gate).

mod common;

use joey_speckit_ui::{
    history::{self, HistoryRecord, SCHEMA_VERSION},
    model::{AttemptStatus, ChangeMode, RunConfiguration, WorkflowAttempt},
};
use tempfile::tempdir;

fn make_attempt(id: &str, feature: &str, step: &str) -> WorkflowAttempt {
    WorkflowAttempt {
        attempt_id: id.to_string(),
        feature_id: feature.to_string(),
        step_id: step.to_string(),
        initiator: "test".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        ended_at: Some("2026-01-01T00:05:00Z".to_string()),
        status: AttemptStatus::Succeeded,
        run_config: RunConfiguration {
            step_id: step.to_string(),
            effective_instructions: "Do the thing".to_string(),
            change_mode: Some(ChangeMode::Staged),
            ..Default::default()
        },
        expires_at: Some("2026-04-01T00:00:00Z".to_string()),
        ..Default::default()
    }
}

#[test]
fn roundtrip_preserves_all_fields() {
    let attempt = make_attempt("rt1", "001-test", "plan");
    let record = HistoryRecord::new(attempt.clone());

    let json = serde_json::to_string(&record).unwrap();
    let restored: HistoryRecord = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.schema_version, SCHEMA_VERSION);
    assert_eq!(restored.attempt.attempt_id, "rt1");
    assert_eq!(restored.attempt.feature_id, "001-test");
    assert_eq!(restored.attempt.step_id, "plan");
    assert_eq!(restored.attempt.status, AttemptStatus::Succeeded);
    assert!(restored.attempt.run_config.effective_instructions.contains("Do the thing"));
}

#[test]
fn append_then_read_preserves_schema_version() {
    let dir = tempdir().unwrap();
    let attempt = make_attempt("rt2", "001", "tasks");

    history::append(dir.path(), &attempt).unwrap();
    let records = history::read_all(&history::history_file(dir.path(), "001")).unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].schema_version, SCHEMA_VERSION);
}

#[test]
fn partial_last_line_does_not_corrupt_valid_records() {
    let dir = tempdir().unwrap();
    let path = history::history_file(dir.path(), "001");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    // Valid record followed by a truncated line.
    let valid = HistoryRecord::new(make_attempt("good", "001", "plan"));
    let valid_json = serde_json::to_string(&valid).unwrap();
    std::fs::write(&path, format!("{valid_json}\n{{\"schema_version\":1,\"incomplete")).unwrap();

    let records = history::read_all(&path).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attempt.attempt_id, "good");
}

#[test]
fn unknown_schema_version_is_skipped_not_crashed() {
    let dir = tempdir().unwrap();
    let path = history::history_file(dir.path(), "001");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    // A v999 record (unknown future version) + a valid v1 record.
    let v1 = HistoryRecord::new(make_attempt("v1", "001", "plan"));
    let v1_json = serde_json::to_string(&v1).unwrap();
    let v999 = format!(
        "{{\"schema_version\":999,\"attempt_id\":\"future\",\"feature_id\":\"001\",\"step_id\":\"plan\"}}"
    );
    std::fs::write(&path, format!("{v1_json}\n{v999}\n")).unwrap();

    let records = history::read_all(&path).unwrap();
    // Only the v1 record survives.
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attempt.attempt_id, "v1");
}

#[test]
fn update_in_place_preserves_other_records() {
    let dir = tempdir().unwrap();
    let a1 = make_attempt("keep1", "001", "plan");
    let a2 = make_attempt("update", "001", "tasks");
    let a3 = make_attempt("keep2", "001", "implement");

    history::append(dir.path(), &a1).unwrap();
    history::append(dir.path(), &a2).unwrap();
    history::append(dir.path(), &a3).unwrap();

    let mut updated = a2.clone();
    updated.status = AttemptStatus::Failed;
    history::update_in_place(dir.path(), &updated).unwrap();

    let records = history::read_all(&history::history_file(dir.path(), "001")).unwrap();
    assert_eq!(records.len(), 3);
    // The updated one should be Failed, others unchanged.
    let updated_rec = records.iter().find(|r| r.attempt.attempt_id == "update").unwrap();
    assert_eq!(updated_rec.attempt.status, AttemptStatus::Failed);
    assert!(records.iter().any(|r| r.attempt.attempt_id == "keep1"));
    assert!(records.iter().any(|r| r.attempt.attempt_id == "keep2"));
}

/// Stub: a v1→v2 migration path test. When a breaking schema change occurs
/// (MAJOR bump), this test would assert the migration function transforms
/// v1 records correctly. For now it documents the contract (Constitution VII).
#[test]
fn migration_stub_v1_to_future_version() {
    // When schema_version bumps to 2, a migration function must exist:
    //   fn migrate_v1_to_v2(record: HistoryRecord) -> HistoryRecordV2
    // and this test would exercise it. For v1 (current), the identity
    // migration is a no-op.
    let record = HistoryRecord::new(make_attempt("mig", "001", "plan"));
    assert_eq!(record.schema_version, 1);
    // Future: assert_eq!(migrate(record).schema_version, 2);
}

// =====================================================================
// Feature 012: Overlay-record extension coverage (T025).
// The existing `WorkflowAttempt` record still round-trips unchanged
// (Constitution VII); the two new variants (`AcceptedClarify`,
// `CommentThread`) are added below.
// =====================================================================

use joey_speckit_ui::history::{append_accepted_clarify, append_comment_thread, read_overlay_records, OverlayRecord};
use joey_speckit_ui::ui_state::{AcceptedClarifyRecord, CommentMessage, CommentThreadRecord};

#[test]
fn accepted_clarify_record_round_trips() {
    let dir = tempdir().unwrap();
    let record = AcceptedClarifyRecord::new(
        "2026-08-05T10:00:00Z".to_string(),
        "requirement:FR-016".to_string(),
        "What about external changes?".to_string(),
        "Three-way merge handles it.".to_string(),
        "sha256:abc123".to_string(),
    );
    append_accepted_clarify(dir.path(), "012-feature", record).unwrap();

    let path = history::history_file(dir.path(), "012-feature");
    let records = read_overlay_records(&path).unwrap();
    assert_eq!(records.len(), 1);
    match &records[0] {
        OverlayRecord::AcceptedClarify(r) => {
            assert_eq!(r.schema_version, 1);
            assert_eq!(r.record_type, "accepted_clarify");
            assert_eq!(r.marker_node, "requirement:FR-016");
            assert_eq!(r.question, "What about external changes?");
            assert_eq!(r.patch_revision, "sha256:abc123");
        }
        other => panic!("expected AcceptedClarify, got {other:?}"),
    }
}

#[test]
fn comment_thread_record_round_trips() {
    let dir = tempdir().unwrap();
    let record = CommentThreadRecord::new(
        "thread-42".to_string(),
        "requirement:FR-016".to_string(),
        "requirement/FR-016".to_string(),
        vec![
            CommentMessage {
                author: "alice".to_string(),
                text: "Is this still valid?".to_string(),
                at: "2026-08-05T10:00:00Z".to_string(),
            },
            CommentMessage {
                author: "bob".to_string(),
                text: "Yes, per the merge contract.".to_string(),
                at: "2026-08-05T10:05:00Z".to_string(),
            },
        ],
    );
    append_comment_thread(dir.path(), "012-feature", record).unwrap();

    let path = history::history_file(dir.path(), "012-feature");
    let records = read_overlay_records(&path).unwrap();
    assert_eq!(records.len(), 1);
    match &records[0] {
        OverlayRecord::CommentThread(r) => {
            assert_eq!(r.schema_version, 1);
            assert_eq!(r.record_type, "comment_thread");
            assert_eq!(r.thread_id, "thread-42");
            assert_eq!(r.anchor_fingerprint, "requirement/FR-016");
            assert_eq!(r.messages.len(), 2);
            assert_eq!(r.messages[0].author, "alice");
        }
        other => panic!("expected CommentThread, got {other:?}"),
    }
}

#[test]
fn existing_workflow_attempt_record_unchanged() {
    // Constitution VII regression: the existing specs/010 WorkflowAttempt
    // record must round-trip unchanged alongside the new variants.
    let dir = tempdir().unwrap();
    let attempt = make_attempt("regress-1", "001", "plan");
    history::append(dir.path(), &attempt).unwrap();

    let path = history::history_file(dir.path(), "001");
    let records = read_overlay_records(&path).unwrap();
    assert_eq!(records.len(), 1);
    match &records[0] {
        OverlayRecord::Attempt(a) => {
            assert_eq!(a.attempt_id, "regress-1");
            assert_eq!(a.step_id, "plan");
        }
        other => panic!("expected Attempt, got {other:?}"),
    }

    // Also: the existing read_all path still works unchanged.
    let legacy_records = history::read_all(&path).unwrap();
    assert_eq!(legacy_records.len(), 1);
    assert_eq!(legacy_records[0].attempt.attempt_id, "regress-1");
}

#[test]
fn mixed_record_types_coexist_in_one_file() {
    let dir = tempdir().unwrap();
    let attempt = make_attempt("a1", "012", "plan");
    history::append(dir.path(), &attempt).unwrap();

    let clarify = AcceptedClarifyRecord::new(
        "2026-08-05T10:00:00Z".to_string(),
        "requirement:FR-001".to_string(),
        "Q".to_string(),
        "A".to_string(),
        "sha256:x".to_string(),
    );
    append_accepted_clarify(dir.path(), "012", clarify).unwrap();

    let thread = CommentThreadRecord::new(
        "t1".to_string(),
        "task:T001".to_string(),
        "task/T001".to_string(),
        vec![CommentMessage {
            author: "x".to_string(),
            text: "y".to_string(),
            at: "2026-08-05T10:00:00Z".to_string(),
        }],
    );
    append_comment_thread(dir.path(), "012", thread).unwrap();

    let path = history::history_file(dir.path(), "012");
    let records = read_overlay_records(&path).unwrap();
    // Newest-first order.
    assert_eq!(records.len(), 3);
    assert!(matches!(records[0], OverlayRecord::CommentThread(_)));
    assert!(matches!(records[1], OverlayRecord::AcceptedClarify(_)));
    assert!(matches!(records[2], OverlayRecord::Attempt(_)));
}
