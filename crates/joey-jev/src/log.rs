//! Decision log: one JSONL row per delegated decision, appended to
//! `~/.joey/logs/jev-decisions.jsonl` (the `assembly.jsonl` side-log
//! precedent, research.md R4 / FR-010 / SC-006).
//!
//! Constant-cost append; never read on the hot path; write failures are
//! traced, never propagated — the decision loop must not fail on logging.

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

/// One decision record (data-model.md). `answer`/`confidence` carry the raw
/// gated answer; raw page state is never logged (cross-cutting invariant).
#[derive(Debug, Clone, Serialize)]
pub struct DecisionRecord {
    pub ts: String,
    pub point: String,
    pub question_kind: String,
    pub answer: serde_json::Value,
    pub confidence: Option<f32>,
    pub latency_ms: u64,
    pub fallback_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The log file path under the user's joey home (JOEY_HOME/profile aware).
pub fn decision_log_path() -> PathBuf {
    joey_core::logging::logs_dir().join("jev-decisions.jsonl")
}

/// Append one record as a JSON line. Best-effort: `Err` maps to a debug
/// trace and `Ok(())` — callers ignore the result on the hot path.
pub fn append_record(dir: &std::path::Path, record: &DecisionRecord) {
    let line = match serde_json::to_string(record) {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!(error = %e, "jev decision log serialize");
            return;
        }
    };
    if let Err(e) = try_append(dir, &line) {
        tracing::debug!(dir = %dir.display(), error = %e, "jev decision log append");
    }
}

fn try_append(dir: &std::path::Path, line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("jev-decisions.jsonl"))?;
    writeln!(file, "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_one_json_line_per_record() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = DecisionRecord {
            ts: "2026-09-25T00:00:00+00:00".into(),
            point: "target_resolution".into(),
            question_kind: "choice".into(),
            answer: "e2".into(),
            confidence: Some(0.9),
            latency_ms: 42,
            fallback_used: false,
            error: None,
        };
        append_record(tmp.path(), &rec);
        append_record(tmp.path(), &rec);
        let content = std::fs::read_to_string(tmp.path().join("jev-decisions.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["point"], "target_resolution");
        assert_eq!(parsed["fallback_used"], false);
        // error is skipped when absent (7-field row)
        assert!(parsed.get("error").is_none());
        assert_eq!(parsed["confidence"], 0.9);
    }
}
