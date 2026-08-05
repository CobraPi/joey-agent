//! Safe-checkpoint recording + restart resume (FR-017/033).
//!
//! On backend startup, scans in-progress attempts (status `running`/
//! `awaiting_*` in JSONL). For each:
//! - Valid checkpoint → resume (re-spawn agent, no replay of unconfirmed actions).
//! - No valid checkpoint → mark `recovery_failed`, preserve effects.

use std::path::Path;

use crate::history;
use crate::model::{AttemptStatus, WorkflowAttempt};

/// Scan history for in-progress attempts that need recovery on startup.
/// Returns attempts that are in a recoverable state.
pub fn find_recoverable(joey_home: &Path, feature_id: &str) -> Vec<WorkflowAttempt> {
    let path = history::history_file(joey_home, feature_id);
    match history::read_all(&path) {
        Ok(records) => records
            .into_iter()
            .map(|r| r.attempt)
            .filter(|a| needs_recovery(&a.status))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Whether an attempt status requires recovery processing.
fn needs_recovery(status: &AttemptStatus) -> bool {
    matches!(
        status,
        AttemptStatus::Running
            | AttemptStatus::AwaitingInput
            | AttemptStatus::AwaitingApproval
            | AttemptStatus::RecoverableFailure
            | AttemptStatus::RecoveryNeeded
    )
}

/// Attempt recovery for a single attempt. Returns the recovery outcome.
pub enum RecoveryOutcome {
    /// The attempt has a valid checkpoint and can be resumed.
    Resume {
        attempt_id: String,
        checkpoint_tree_ish: String,
        last_confirmed_interaction_id: Option<String>,
    },
    /// No valid checkpoint — mark recovery_failed, preserve effects.
    Failed {
        attempt_id: String,
        reason: String,
        preserved_effects: bool,
    },
}

/// Determine the recovery outcome for an attempt.
pub fn evaluate_recovery(attempt: &WorkflowAttempt) -> RecoveryOutcome {
    match &attempt.checkpoint {
        Some(checkpoint) if checkpoint.tree_ish.starts_with("sha1:") => {
            RecoveryOutcome::Resume {
                attempt_id: attempt.attempt_id.clone(),
                checkpoint_tree_ish: checkpoint.tree_ish.clone(),
                last_confirmed_interaction_id: checkpoint.last_confirmed_interaction_id.clone(),
            }
        }
        _ => RecoveryOutcome::Failed {
            attempt_id: attempt.attempt_id.clone(),
            reason: "no valid checkpoint found".to_string(),
            preserved_effects: true,
        },
    }
}

/// Mark an attempt as recovery_failed, preserving its effects.
pub fn mark_recovery_failed(
    joey_home: &Path,
    attempt: &mut WorkflowAttempt,
) -> anyhow::Result<()> {
    attempt.status = AttemptStatus::RecoveryFailed;
    attempt.ended_at = Some(chrono::Utc::now().to_rfc3339());
    history::update_in_place(joey_home, attempt)?;
    Ok(())
}

/// Mark an attempt as resumed (re-running from checkpoint).
pub fn mark_resumed(
    joey_home: &Path,
    attempt: &mut WorkflowAttempt,
) -> anyhow::Result<()> {
    attempt.status = AttemptStatus::Running;
    history::update_in_place(joey_home, attempt)?;
    Ok(())
}

/// Scan all features for recoverable attempts on startup.
/// Returns (feature_id, attempts) pairs.
pub fn scan_all_for_recovery(
    joey_home: &Path,
) -> Vec<(String, Vec<WorkflowAttempt>)> {
    let history_dir = joey_home.join("speckit-ui").join("history");
    let mut results = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&history_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(feature_id) = name_str.strip_suffix(".jsonl") {
                let recoverable = find_recoverable(joey_home, feature_id);
                if !recoverable.is_empty() {
                    results.push((feature_id.to_string(), recoverable));
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Checkpoint, RunConfiguration};
    use tempfile::tempdir;

    fn make_attempt(id: &str, status: AttemptStatus) -> WorkflowAttempt {
        WorkflowAttempt {
            attempt_id: id.to_string(),
            feature_id: "001-test".to_string(),
            step_id: "plan".to_string(),
            initiator: "test".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            status,
            run_config: RunConfiguration::default(),
            expires_at: Some("2026-04-01T00:00:00Z".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn attempt_with_valid_checkpoint_can_resume() {
        let mut attempt = make_attempt("a1", AttemptStatus::Running);
        attempt.checkpoint = Some(Checkpoint {
            tree_ish: "sha1:abc123".to_string(),
            last_confirmed_interaction_id: Some("i1".to_string()),
            at: Some("2026-01-01T00:01:00Z".to_string()),
        });

        match evaluate_recovery(&attempt) {
            RecoveryOutcome::Resume {
                checkpoint_tree_ish, ..
            } => {
                assert_eq!(checkpoint_tree_ish, "sha1:abc123");
            }
            RecoveryOutcome::Failed { .. } => panic!("expected Resume"),
        }
    }

    #[test]
    fn attempt_without_checkpoint_fails() {
        let attempt = make_attempt("a2", AttemptStatus::Running);

        match evaluate_recovery(&attempt) {
            RecoveryOutcome::Failed {
                preserved_effects, ..
            } => {
                assert!(preserved_effects);
            }
            RecoveryOutcome::Resume { .. } => panic!("expected Failed"),
        }
    }

    #[test]
    fn terminal_attempts_dont_need_recovery() {
        let succeeded = make_attempt("a3", AttemptStatus::Succeeded);
        let failed = make_attempt("a4", AttemptStatus::Failed);
        let cancelled = make_attempt("a5", AttemptStatus::Cancelled);

        assert!(!needs_recovery(&succeeded.status));
        assert!(!needs_recovery(&failed.status));
        assert!(!needs_recovery(&cancelled.status));
    }

    #[test]
    fn find_recoverable_finds_in_progress() {
        let dir = tempdir().unwrap();
        let mut running = make_attempt("r1", AttemptStatus::Running);
        running.checkpoint = Some(Checkpoint {
            tree_ish: "sha1:abc".to_string(),
            ..Default::default()
        });
        let succeeded = make_attempt("s1", AttemptStatus::Succeeded);

        history::append(dir.path(), &running).unwrap();
        history::append(dir.path(), &succeeded).unwrap();

        let recoverable = find_recoverable(dir.path(), "001-test");
        assert_eq!(recoverable.len(), 1);
        assert_eq!(recoverable[0].attempt_id, "r1");
    }

    #[test]
    fn mark_recovery_failed_updates_status() {
        let dir = tempdir().unwrap();
        let mut attempt = make_attempt("f1", AttemptStatus::Running);
        history::append(dir.path(), &attempt).unwrap();

        mark_recovery_failed(dir.path(), &mut attempt).unwrap();

        let records = history::read_all(&history::history_file(dir.path(), "001-test")).unwrap();
        let updated = records.iter().find(|r| r.attempt.attempt_id == "f1").unwrap();
        assert_eq!(updated.attempt.status, AttemptStatus::RecoveryFailed);
        assert!(updated.attempt.ended_at.is_some());
    }
}
