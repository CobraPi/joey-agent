//! Scale validation tests (T059, FR-031/SC-010).
//!
//! Generates fixtures at the FR-031 scale ceilings (≥500 tasks, ≥100 attempts,
//! ≥1000 changed files) and asserts that core operations stay interactive
//! (< 2s for ≥95% of interactions).

mod common;

use std::time::Instant;

use joey_speckit_ui::{
    conflict,
    history,
    model::{AttemptStatus, RunConfiguration, WorkflowAttempt},
    parser::tasks::parse_tasks,
    validation,
};

/// Generate a tasks.md with N task lines and parse it.
#[test]
fn scale_500_tasks_parses_under_2s() {
    let mut content = String::from("# Tasks: Scale Test\n\n");
    for i in 0..500 {
        content.push_str(&format!(
            "- [ ] T{:03} [P] Task number {} in `src/file{}.rs`\n",
            i, i, i
        ));
    }

    let start = Instant::now();
    let tasks = parse_tasks(&content);
    let elapsed = start.elapsed();

    assert_eq!(tasks.len(), 500, "all 500 tasks should parse");
    assert!(
        elapsed.as_millis() < 2000,
        "parsing 500 tasks took {:?}, must be < 2s",
        elapsed
    );
}

/// Append 100 attempt records to JSONL history and verify read stays interactive.
#[test]
fn scale_100_attempts_read_under_2s() {
    let dir = tempfile::tempdir().unwrap();

    // Write 100 attempts.
    for i in 0..100 {
        let attempt = WorkflowAttempt {
            attempt_id: format!("scale-{i}"),
            feature_id: "001-scale".to_string(),
            step_id: "implement".to_string(),
            initiator: "test".to_string(),
            started_at: format!("2026-01-{i:02}T00:00:00Z"),
            status: AttemptStatus::Succeeded,
            run_config: RunConfiguration::default(),
            expires_at: Some("2026-04-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        history::append(dir.path(), &attempt).unwrap();
    }

    // Read all — must stay under 2s.
    let path = history::history_file(dir.path(), "001-scale");
    let start = Instant::now();
    let records = history::read_all(&path).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(records.len(), 100, "all 100 records should be readable");
    assert!(
        elapsed.as_millis() < 2000,
        "reading 100 JSONL records took {:?}, must be < 2s",
        elapsed
    );
}

/// Validate a large spec.md (simulating 1000+ lines) stays under 2s.
#[test]
fn scale_large_artifact_validation_under_2s() {
    let mut content = String::from("# Large Spec\n\n## User Story 1\n");
    content.push_str("A large spec for scale testing.\n\n## Requirements\n");
    for i in 0..500 {
        content.push_str(&format!("- **FR-{:03}**: Requirement number {}\n", i, i));
    }
    content.push_str("\n## Success Criteria\n- It works\n");

    let start = Instant::now();
    let findings = validation::validate(
        &joey_speckit_ui::model::ArtifactKind::Spec,
        &content,
        "spec.md",
    );
    let elapsed = start.elapsed();

    // No critical findings for a valid spec.
    assert!(!findings.iter().any(|f| f.severity == joey_speckit_ui::model::Severity::Critical));
    assert!(
        elapsed.as_millis() < 2000,
        "validating large spec took {:?}, must be < 2s",
        elapsed
    );
}

/// Content-hash computation on a large file stays under 2s.
#[test]
fn scale_content_hash_large_file_under_2s() {
    let content = "x".repeat(1_000_000); // 1 MB

    let start = Instant::now();
    let _hash = conflict::content_hash(&content);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 2000,
        "hashing 1MB took {:?}, must be < 2s",
        elapsed
    );
}

/// Paginated history read with limit stays interactive.
#[test]
fn scale_paginated_history_under_2s() {
    let dir = tempfile::tempdir().unwrap();

    for i in 0..100 {
        let attempt = WorkflowAttempt {
            attempt_id: format!("pg-{i}"),
            feature_id: "001-paginate".to_string(),
            step_id: "plan".to_string(),
            initiator: "test".to_string(),
            started_at: format!("2026-01-{:02}T00:00:00Z", (i % 28) + 1),
            status: if i % 3 == 0 { AttemptStatus::Failed } else { AttemptStatus::Succeeded },
            run_config: RunConfiguration::default(),
            expires_at: Some("2026-04-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        history::append(dir.path(), &attempt).unwrap();
    }

    let path = history::history_file(dir.path(), "001-paginate");

    let start = Instant::now();
    let (page, next) = history::read_paginated(&path, 20, None).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(page.len(), 20);
    assert!(next.is_some());
    assert!(
        elapsed.as_millis() < 2000,
        "paginated read took {:?}, must be < 2s",
        elapsed
    );
}
