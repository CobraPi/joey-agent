//! Append-only JSONL history store for workflow attempts (FR-018/019/031/033).
//!
//! Stored at `~/.joey/speckit-ui/history/<feature-id>.jsonl`. Each line is a
//! self-contained `WorkflowAttempt` record with mandatory `schema_version: 1`.
//! O(1) append, streamed lazy read, 90-day expiry via file-mtime, tolerant
//! skip of partial last line.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::WorkflowAttempt;

/// The mandatory schema version stamp on every JSONL record (Constitution VII).
pub const SCHEMA_VERSION: u32 = 1;

/// A JSONL history record: the on-disk wire shape for one attempt.
/// This wraps `WorkflowAttempt` with the mandatory `schema_version` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub schema_version: u32,
    #[serde(flatten)]
    pub attempt: WorkflowAttempt,
}

impl HistoryRecord {
    pub fn new(attempt: WorkflowAttempt) -> Self {
        HistoryRecord {
            schema_version: SCHEMA_VERSION,
            attempt,
        }
    }
}

/// Errors from history operations.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Resolve the history file path for a feature under `joey_home` (typically
/// `~/.joey/`).
pub fn history_file(joey_home: &Path, feature_id: &str) -> PathBuf {
    joey_home
        .join("speckit-ui")
        .join("history")
        .join(format!("{feature_id}.jsonl"))
}

/// Append a single attempt record to the feature's JSONL history file.
/// Creates the file and parent directories if needed. O(1) — a single
/// `writeln!` to end-of-file (FR-018).
pub fn append(joey_home: &Path, attempt: &WorkflowAttempt) -> Result<(), HistoryError> {
    let path = history_file(joey_home, &attempt.feature_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let record = HistoryRecord::new(attempt.clone());
    let line = serde_json::to_string(&record)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{line}")?;

    Ok(())
}

/// Rewrite an in-progress attempt line in place (atomic temp + rename of the
/// whole file). Used when a checkpoint advances during a run.
pub fn update_in_place(joey_home: &Path, attempt: &WorkflowAttempt) -> Result<(), HistoryError> {
    let path = history_file(joey_home, &attempt.feature_id);
    if !path.exists() {
        return append(joey_home, attempt);
    }

    let records = read_all(&path)?;
    let updated: Vec<HistoryRecord> = records
        .into_iter()
        .map(|r| {
            if r.attempt.attempt_id == attempt.attempt_id {
                HistoryRecord::new(attempt.clone())
            } else {
                r
            }
        })
        .collect();

    write_all(&path, &updated)
}

/// Read all records from a history file, newest-first (most recently appended
/// last in the file, so reverse). Partial last lines are tolerated and
/// skipped (crash safety). Records with unknown `schema_version` are skipped
/// with a warning (tolerant parser, Constitution VII migration safety).
pub fn read_all(path: &Path) -> Result<Vec<HistoryRecord>, HistoryError> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    line = line_num,
                    error = %e,
                    "skipping malformed line in history file"
                );
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<HistoryRecord>(trimmed) {
            Ok(r) if r.schema_version == SCHEMA_VERSION => records.push(r),
            Ok(r) => {
                tracing::warn!(
                    schema_version = r.schema_version,
                    line = line_num,
                    "skipping record with unknown schema_version"
                );
            }
            Err(e) => {
                tracing::warn!(
                    line = line_num,
                    error = %e,
                    "skipping unparseable line in history file (partial write?)"
                );
            }
        }
    }

    // Reverse to newest-first.
    records.reverse();
    Ok(records)
}

/// Write all records to a file atomically (temp + rename).
fn write_all(path: &Path, records: &[HistoryRecord]) -> Result<(), HistoryError> {
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        for record in records {
            let line = serde_json::to_string(record)?;
            writeln!(file, "{line}")?;
        }
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Streamed, paginated read of history records (newest first). Returns up to
/// `limit` records starting after `before` (an attempt_id cursor, exclusive).
pub fn read_paginated(
    path: &Path,
    limit: usize,
    before: Option<&str>,
) -> Result<(Vec<WorkflowAttempt>, Option<String>), HistoryError> {
    let records = read_all(path)?;
    let mut attempts: Vec<WorkflowAttempt> = records.into_iter().map(|r| r.attempt).collect();

    // Apply cursor.
    if let Some(cursor) = before {
        let idx = attempts.iter().position(|a| a.attempt_id == cursor);
        if let Some(idx) = idx {
            attempts = attempts.into_iter().skip(idx + 1).collect();
        }
    }

    // Apply limit.
    let next_cursor = if attempts.len() > limit {
        Some(attempts[limit - 1].attempt_id.clone())
    } else {
        None
    };
    attempts.truncate(limit);

    Ok((attempts, next_cursor))
}

/// Sweep history files and remove records whose `expires_at` has passed
/// (FR-018, 90-day retention). Rewrites files atomically; removes empty files.
/// Returns the number of records removed.
pub fn sweep_expired(joey_home: &Path, now: chrono::DateTime<chrono::Utc>) -> Result<usize, HistoryError> {
    let history_dir = joey_home.join("speckit-ui").join("history");
    if !history_dir.is_dir() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in std::fs::read_dir(&history_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let records = read_all(&path)?;
        let kept: Vec<HistoryRecord> = records
            .into_iter()
            .filter(|r| {
                if let Some(ref expires_str) = r.attempt.expires_at {
                    if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_str) {
                        return expires.with_timezone(&chrono::Utc) > now;
                    }
                }
                // No expiry → keep (shouldn't happen, but tolerant).
                true
            })
            .collect();

        removed += {
            // We read newest-first; the original file had them oldest-first.
            // Count is the same either way.
            let original_count = {
                let file = std::fs::File::open(&path).map(|f| BufReader::new(f).lines().count());
                file.unwrap_or(0)
            };
            original_count.saturating_sub(kept.len())
        };

        if kept.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            // read_all preserves file order (oldest-first); keep it that way
            // for append-order on disk. (The old double-reverse was an
            // identity — confusing but correct.)
            write_all(&path, &kept)?;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AttemptStatus, RunConfiguration};
    use tempfile::tempdir;

    fn make_attempt(id: &str, feature: &str, step: &str) -> WorkflowAttempt {
        WorkflowAttempt {
            attempt_id: id.to_string(),
            feature_id: feature.to_string(),
            step_id: step.to_string(),
            initiator: "test".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            status: AttemptStatus::Succeeded,
            run_config: RunConfiguration::default(),
            expires_at: Some("2026-04-01T00:00:00Z".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn append_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let attempt = make_attempt("a1", "001-test", "plan");

        append(dir.path(), &attempt).unwrap();
        let records = read_all(&history_file(dir.path(), "001-test")).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].schema_version, SCHEMA_VERSION);
        assert_eq!(records[0].attempt.attempt_id, "a1");
        assert_eq!(records[0].attempt.step_id, "plan");
    }

    #[test]
    fn multiple_appends_read_newest_first() {
        let dir = tempdir().unwrap();
        append(dir.path(), &make_attempt("a1", "001", "plan")).unwrap();
        append(dir.path(), &make_attempt("a2", "001", "tasks")).unwrap();

        let records = read_all(&history_file(dir.path(), "001")).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].attempt.attempt_id, "a2"); // newest first
        assert_eq!(records[1].attempt.attempt_id, "a1");
    }

    #[test]
    fn partial_last_line_is_tolerated() {
        let dir = tempdir().unwrap();
        let path = history_file(dir.path(), "001");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Write a valid record followed by a partial line.
        let attempt = make_attempt("a1", "001", "plan");
        let record = HistoryRecord::new(attempt);
        let valid_line = serde_json::to_string(&record).unwrap();
        let content = format!("{valid_line}\n{{\"schema_version\":1,\"attempt_id\":\"partial");
        std::fs::write(&path, &content).unwrap();

        let records = read_all(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].attempt.attempt_id, "a1");
    }

    #[test]
    fn update_in_place_rewrites_matching_record() {
        let dir = tempdir().unwrap();
        let mut attempt = make_attempt("a1", "001", "plan");
        append(dir.path(), &attempt).unwrap();

        attempt.status = AttemptStatus::Failed;
        update_in_place(dir.path(), &attempt).unwrap();

        let records = read_all(&history_file(dir.path(), "001")).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].attempt.status, AttemptStatus::Failed);
    }

    #[test]
    fn sweep_removes_expired_records() {
        let dir = tempdir().unwrap();
        let mut old = make_attempt("old", "001", "plan");
        old.expires_at = Some("2020-01-01T00:00:00Z".to_string());
        let mut new = make_attempt("new", "001", "plan");
        new.expires_at = Some("2099-01-01T00:00:00Z".to_string());

        append(dir.path(), &old).unwrap();
        append(dir.path(), &new).unwrap();

        let now = chrono::Utc::now();
        let removed = sweep_expired(dir.path(), now).unwrap();
        assert!(removed >= 1);

        let records = read_all(&history_file(dir.path(), "001")).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].attempt.attempt_id, "new");
    }

    #[test]
    fn read_paginated_respects_limit() {
        let dir = tempdir().unwrap();
        for i in 0..5 {
            append(dir.path(), &make_attempt(&format!("a{i}"), "001", "plan")).unwrap();
        }

        let (page, next) = read_paginated(&history_file(dir.path(), "001"), 2, None).unwrap();
        assert_eq!(page.len(), 2);
        assert!(next.is_some());
    }

    #[test]
    fn schema_version_is_present_in_serialized_record() {
        let attempt = make_attempt("a1", "001", "plan");
        let record = HistoryRecord::new(attempt);
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"schema_version\":1"));
    }
}

// =====================================================================
// Feature 012: Overlay Store extension — new JSONL record kinds
// (AcceptedClarify, CommentThread). The existing WorkflowAttempt record
// and `schema_version: 1` gate are preserved (Constitution VII).
// =====================================================================

use crate::ui_state::{AcceptedClarifyRecord, CommentThreadRecord};

/// Append an accepted-clarify record to the feature's JSONL history.
/// O(1) — a single `writeln!` to end-of-file (FR-024).
pub fn append_accepted_clarify(
    joey_home: &Path,
    feature_id: &str,
    record: AcceptedClarifyRecord,
) -> Result<(), HistoryError> {
    let path = history_file(joey_home, feature_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(&record)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Append a comment-thread record to the feature's JSONL history (FR-026).
pub fn append_comment_thread(
    joey_home: &Path,
    feature_id: &str,
    record: CommentThreadRecord,
) -> Result<(), HistoryError> {
    let path = history_file(joey_home, feature_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(&record)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// A raw JSONL line as read from disk — used to deserialize mixed record
/// types without knowing the variant upfront.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawRecord {
    schema_version: u32,
    record_type: Option<String>,
    #[serde(flatten)]
    rest: serde_json::Value,
}

/// One of the known overlay record kinds (additive over specs/010).
#[derive(Debug, Clone)]
pub enum OverlayRecord {
    /// Existing, unchanged (specs/010).
    Attempt(WorkflowAttempt),
    /// NEW — accepted clarify answer (FR-024).
    AcceptedClarify(AcceptedClarifyRecord),
    /// NEW — anchored comment thread (FR-026).
    CommentThread(CommentThreadRecord),
    /// Unknown record type (forward-compatible — skipped with a warning).
    Unknown,
}

/// Read all overlay records from a feature's JSONL history, newest-first.
/// Records with unknown `record_type` or wrong `schema_version` are skipped
/// with a warning (tolerant parser, Constitution VII).
pub fn read_overlay_records(path: &Path) -> Result<Vec<OverlayRecord>, HistoryError> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(line = line_num, error = %e, "skipping malformed overlay line");
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let raw: RawRecord = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(line = line_num, error = %e, "skipping unparseable overlay line");
                continue;
            }
        };

        if raw.schema_version != SCHEMA_VERSION {
            tracing::warn!(line = line_num, "skipping record with wrong schema_version");
            continue;
        }

        let record = match raw.record_type.as_deref() {
            None | Some("workflow_attempt") => {
                // Legacy specs/010 records have no record_type field; treat
                // them as workflow attempts.
                match serde_json::from_str::<HistoryRecord>(trimmed) {
                    Ok(r) => OverlayRecord::Attempt(r.attempt),
                    Err(e) => {
                        tracing::warn!(line = line_num, error = %e, "skipping malformed attempt");
                        continue;
                    }
                }
            }
            Some("accepted_clarify") => {
                match serde_json::from_str::<AcceptedClarifyRecord>(trimmed) {
                    Ok(r) => OverlayRecord::AcceptedClarify(r),
                    Err(e) => {
                        tracing::warn!(line = line_num, error = %e, "skipping malformed accepted_clarify");
                        continue;
                    }
                }
            }
            Some("comment_thread") => {
                match serde_json::from_str::<CommentThreadRecord>(trimmed) {
                    Ok(r) => OverlayRecord::CommentThread(r),
                    Err(e) => {
                        tracing::warn!(line = line_num, error = %e, "skipping malformed comment_thread");
                        continue;
                    }
                }
            }
            Some(other) => {
                tracing::warn!(line = line_num, record_type = other, "skipping unknown record_type");
                OverlayRecord::Unknown
            }
        };
        records.push(record);
    }

    records.reverse();
    Ok(records)
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn append_and_read_accepted_clarify() {
        let dir = tempfile::tempdir().unwrap();
        let record = AcceptedClarifyRecord::new(
            "2026-08-05T10:00:00Z".to_string(),
            "requirement:FR-016".to_string(),
            "What about concurrency?".to_string(),
            "Per-node locking.".to_string(),
            "sha256:abc".to_string(),
        );
        append_accepted_clarify(dir.path(), "001-test", record).unwrap();

        let path = history_file(dir.path(), "001-test");
        let records = read_overlay_records(&path).unwrap();
        assert_eq!(records.len(), 1);
        match &records[0] {
            OverlayRecord::AcceptedClarify(r) => {
                assert_eq!(r.marker_node, "requirement:FR-016");
                assert_eq!(r.schema_version, 1);
            }
            other => panic!("expected AcceptedClarify, got {other:?}"),
        }
    }

    #[test]
    fn append_and_read_comment_thread() {
        let dir = tempfile::tempdir().unwrap();
        let record = CommentThreadRecord::new(
            "thread-1".to_string(),
            "requirement:FR-016".to_string(),
            "requirement/FR-016".to_string(),
            vec![crate::ui_state::CommentMessage {
                author: "joe".to_string(),
                text: "Check this.".to_string(),
                at: "2026-08-05T10:00:00Z".to_string(),
            }],
        );
        append_comment_thread(dir.path(), "001-test", record).unwrap();

        let path = history_file(dir.path(), "001-test");
        let records = read_overlay_records(&path).unwrap();
        assert_eq!(records.len(), 1);
        match &records[0] {
            OverlayRecord::CommentThread(r) => {
                assert_eq!(r.thread_id, "thread-1");
                assert_eq!(r.messages.len(), 1);
            }
            other => panic!("expected CommentThread, got {other:?}"),
        }
    }

    #[test]
    fn mixed_records_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        // A workflow attempt (specs/010 form).
        let attempt = WorkflowAttempt {
            attempt_id: "a1".to_string(),
            feature_id: "001".to_string(),
            step_id: "plan".to_string(),
            initiator: "test".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            status: crate::model::AttemptStatus::Succeeded,
            run_config: crate::model::RunConfiguration::default(),
            expires_at: Some("2026-04-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        append(dir.path(), &attempt).unwrap();

        // An accepted clarify (new).
        let clarify = AcceptedClarifyRecord::new(
            "2026-08-05T10:00:00Z".to_string(),
            "requirement:FR-001".to_string(),
            "Q?".to_string(),
            "A.".to_string(),
            "sha256:x".to_string(),
        );
        append_accepted_clarify(dir.path(), "001", clarify).unwrap();

        let path = history_file(dir.path(), "001");
        let records = read_overlay_records(&path).unwrap();
        // Newest-first: clarify, then attempt.
        assert_eq!(records.len(), 2);
        assert!(matches!(records[0], OverlayRecord::AcceptedClarify(_)));
        assert!(matches!(records[1], OverlayRecord::Attempt(_)));
    }
}
