//! Integration tests for the spec-023 run-state store (evidence.rs).

use joey_orchestration::evidence::{DecisionEntry, EvidenceKind, RunHandle};
use std::fs;

fn decisions_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("decisions.jsonl")
}

/// (a) Exact contract tree created by `create_at`.
#[test]
fn layout_matches_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r-1");
    RunHandle::create_at(&root, "r-1", "rev-base").unwrap();

    assert!(root.join("graph.json").is_file());
    assert!(root.join("nodes").is_dir());
    assert!(root.join("evidence").is_dir());
    assert!(root.join("patches").is_dir());
    assert!(decisions_path(&root).is_file());

    let graph: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("graph.json")).unwrap()).unwrap();
    assert_eq!(graph["run_id"], "r-1");
    assert_eq!(graph["baseline_revision"], "rev-base");
    assert_eq!(graph["nodes"], serde_json::json!({}));
}

/// (b) decisions.jsonl is append-only: 3 appends ⇒ 3 lines, earlier
/// lines untouched.
#[test]
fn decision_log_is_append_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r-1");
    let mut h = RunHandle::create_at(&root, "r-1", "rev-1").unwrap();

    h.append_decision(&DecisionEntry::new("t1", "Pending", "Ready", "dependency_completed", vec![], "dep done"))
        .unwrap();
    let first_line = fs::read_to_string(decisions_path(&root)).unwrap();

    h.append_decision(&DecisionEntry::new("t2", "Ready", "Dispatched", "dependency_completed", vec![], "dispatch"))
        .unwrap();
    h.append_decision(&DecisionEntry::new("t3", "Dispatched", "Evaluating", "worker_completed", vec!["ev-7".into()], "worker finished"))
        .unwrap();

    let all = h.read_decisions().unwrap();
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|d| d.run_id == "r-1"));

    let content = fs::read_to_string(decisions_path(&root)).unwrap();
    let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, 3);

    // First line byte-identical after later appends.
    let first_line_now = content.lines().next().unwrap();
    assert_eq!(first_line.trim_end(), first_line_now);
}

/// (c) SC-007: the persisted files alone suffice to reconstruct a task's
/// final status by folding the decision log.
#[test]
fn sc007_reconstruct_status_from_persisted_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r-1");
    let mut h = RunHandle::create_at(&root, "r-1", "rev-9").unwrap();
    let task = "task-auth";

    let transitions = [
        ("Pending", "Ready", "dependency_completed", "dependency satisfied"),
        ("Ready", "Dispatched", "dependency_completed", "dispatch"),
        ("Dispatched", "Evaluating", "worker_completed", "worker finished"),
        ("Evaluating", "Completed", "gate_passed", "gates green"),
    ];
    let mut all_ev: Vec<String> = Vec::new();
    for (from, to, cause, detail) in transitions {
        let ev = h
            .record_evidence(task, EvidenceKind::CommandOutput, serde_json::json!({"step": to}))
            .unwrap();
        all_ev.push(ev.id.clone());
        h.append_decision(&DecisionEntry::new(task, from, to, cause, vec![ev.id.clone()], detail))
            .unwrap();
        h.write_node(task, &serde_json::json!({"status": to, "evidence": all_ev}))
            .unwrap();
    }

    // Fresh read: only the persisted files.
    let decisions = h.read_decisions().unwrap();
    let node_raw = fs::read_to_string(root.join("nodes").join(format!("{task}.json"))).unwrap();
    let node: serde_json::Value = serde_json::from_str(&node_raw).unwrap();

    let final_status = decisions
        .iter()
        .filter(|d| d.task_id == task)
        .map(|d| d.to.as_str())
        .last()
        .unwrap();
    assert_eq!(final_status, "Completed");
    assert_eq!(node["status"], "Completed");
    assert_eq!(node["evidence"], serde_json::json!(["ev-1", "ev-2", "ev-3", "ev-4"]));

    // Evidence file holds an immutable, append-only array of 4 records.
    let evidence: Vec<serde_json::Value> = serde_json::from_str(
        &fs::read_to_string(root.join("evidence").join(format!("{task}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(evidence.len(), 4);
    assert_eq!(evidence[0]["id"], "ev-1");
    assert_eq!(evidence[3]["id"], "ev-4");
}

/// (d) Baseline-mismatch refusal message carries both revisions.
#[test]
fn baseline_mismatch_error_message_contains_both_revisions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r-1");
    RunHandle::create_at(&root, "r-1", "rev-persisted-42").unwrap();

    let err = RunHandle::resume_at(&root, "r-1", "rev-current-99").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("rev-persisted-42"), "message: {msg}");
    assert!(msg.contains("rev-current-99"), "message: {msg}");
    assert!(msg.contains("FR-030"), "message: {msg}");
}
