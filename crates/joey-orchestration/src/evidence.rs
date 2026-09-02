//! Evidence records, the append-only decision log, and the resumable
//! on-disk run-state store for the orchestration runtime (spec 023 T002).
//!
//! Run directory layout (contract: specs/023-enterprise-orchestration-runtime/
//! contracts/run-state-format.md):
//!
//! ```text
//! ~/.joey/hypercode/projects/<project-hash>/runs/<run-id>/
//! ├── graph.json            full TaskGraph (rewritten atomically per transition)
//! ├── nodes/<task-id>.json  per-task snapshot (rewritten atomically)
//! ├── evidence/<task-id>.json  immutable EvidenceRecord list per task
//! ├── patches/<task-id>.patch
//! └── decisions.jsonl       append-only, one JSON object per line
//! ```

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use joey_core::utils::atomic_replace;
use serde::{Deserialize, Serialize};

/// `atomic_replace` returns `anyhow::Result`; adapt to `std::io::Result`.
fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    atomic_replace(path, contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}

/// The pinned decision-cause vocabulary (exactly 13, nothing else valid).
pub const DECISION_CAUSES: [&str; 13] = [
    "dependency_completed",
    "worker_completed",
    "gate_passed",
    "gate_failed",
    "repair_scheduled",
    "escalated",
    "degraded",
    "override_acknowledged",
    "replanned",
    "deferred_concurrency_cap",
    "conflict_sequenced",
    "baseline_mismatch_abort",
    "run_resumed",
];

/// True iff `cause` is one of the pinned [`DECISION_CAUSES`].
pub fn is_valid_cause(cause: &str) -> bool {
    DECISION_CAUSES.contains(&cause)
}

/// FNV-1a 64-bit hash (implemented inline, no external crates).
fn fnv1a64(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Stable hash of a project root: FNV-1a 64 over the canonicalized
/// (falling back to as-is on error) absolute path's display string,
/// as lowercase hex.
pub fn project_hash(project_root: &Path) -> String {
    let abs = if project_root.is_absolute() {
        project_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(project_root))
            .unwrap_or_else(|_| project_root.to_path_buf())
    };
    let canonical = fs::canonicalize(&abs).unwrap_or(abs);
    format!("{:016x}", fnv1a64(&canonical.display().to_string()))
}

/// Pure computation of a run's root directory:
/// `joey_home()/hypercode/projects/<project-hash>/runs/<run_id>`.
pub fn run_root(project_root: &Path, run_id: &str) -> PathBuf {
    joey_core::joey_home()
        .join("hypercode")
        .join("projects")
        .join(project_hash(project_root))
        .join("runs")
        .join(run_id)
}

/// Kind of evidence attached to a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    CommandOutput,
    ReviewOutcome,
    Inspection,
    DivergenceReport,
}

/// One immutable evidence record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub id: String,
    pub task_id: String,
    pub kind: EvidenceKind,
    pub payload: serde_json::Value,
    /// RFC3339 UTC, e.g. `2026-09-02T12:00:00Z`.
    pub recorded_at: String,
}

/// One line of the append-only decision log (`decisions.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub ts: String,
    pub run_id: String,
    pub task_id: String,
    pub from: String,
    pub to: String,
    pub cause: String,
    pub evidence_ids: Vec<String>,
    pub detail: String,
}

impl DecisionEntry {
    /// Build an entry stamped with the current time. `run_id` is left
    /// empty here; [`RunHandle::append_decision`] fills it on append.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        cause: impl Into<String>,
        evidence_ids: Vec<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            ts: chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            run_id: String::new(),
            task_id: task_id.into(),
            from: from.into(),
            to: to.into(),
            cause: cause.into(),
            evidence_ids,
            detail: detail.into(),
        }
    }
}

/// Errors from [`RunHandle::resume_at`].
#[derive(thiserror::Error, Debug)]
pub enum ResumeError {
    #[error("run not found: {0}")]
    NotFound(String),
    #[error("baseline mismatch: persisted {persisted}, current {current} (FR-030)")]
    BaselineMismatch { persisted: String, current: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Handle to a run's on-disk state under `~/.joey/hypercode/projects/
/// <project-hash>/runs/<run-id>/`.
#[derive(Debug)]
pub struct RunHandle {
    root: PathBuf,
    run_id: String,
    baseline_revision: String,
    next_evidence: u64,
}

impl RunHandle {
    /// Create a fresh run directory tree and initial `graph.json`.
    pub fn create_at(root: &Path, run_id: &str, baseline_revision: &str) -> std::io::Result<Self> {
        fs::create_dir_all(root.join("nodes"))?;
        fs::create_dir_all(root.join("evidence"))?;
        fs::create_dir_all(root.join("patches"))?;

        let graph = serde_json::json!({
            "run_id": run_id,
            "baseline_revision": baseline_revision,
            "nodes": {},
        });
        atomic_write(&root.join("graph.json"), graph.to_string().as_bytes())?;

        fs::File::create(root.join("decisions.jsonl"))?;

        Ok(Self {
            root: root.to_path_buf(),
            run_id: run_id.to_string(),
            baseline_revision: baseline_revision.to_string(),
            next_evidence: 1,
        })
    }

    /// Resume a persisted run. Refuses on baseline mismatch (FR-030).
    pub fn resume_at(
        root: &Path,
        run_id: &str,
        current_baseline: &str,
    ) -> Result<RunHandle, ResumeError> {
        let graph_path = root.join("graph.json");
        if !root.is_dir() || !graph_path.is_file() {
            return Err(ResumeError::NotFound(root.display().to_string()));
        }
        let raw = fs::read_to_string(&graph_path)?;
        let graph: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("parsing {}: {e}", graph_path.display()),
            )
        })?;
        let persisted = graph
            .get("baseline_revision")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if persisted != current_baseline {
            return Err(ResumeError::BaselineMismatch {
                persisted,
                current: current_baseline.to_string(),
            });
        }

        // Rebuild next_evidence: scan evidence/*.json arrays for the max
        // numeric id suffix (ids look like "ev-7"), default 0 ⇒ next = max+1.
        let mut max: u64 = 0;
        let evidence_dir = root.join("evidence");
        if let Ok(entries) = fs::read_dir(&evidence_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(content) = fs::read_to_string(&path) else { continue };
                let Ok(serde_json::Value::Array(records)) =
                    serde_json::from_str::<serde_json::Value>(&content)
                else {
                    continue;
                };
                for record in &records {
                    if let Some(id) = record.get("id").and_then(|v| v.as_str()) {
                        if let Some(n) = id.strip_prefix("ev-").and_then(|s| s.parse::<u64>().ok())
                        {
                            max = max.max(n);
                        }
                    }
                }
            }
        }

        Ok(RunHandle {
            root: root.to_path_buf(),
            run_id: run_id.to_string(),
            baseline_revision: current_baseline.to_string(),
            next_evidence: max + 1,
        })
    }

    /// The run's root directory.
    pub fn run_dir(&self) -> &Path {
        &self.root
    }

    /// The run id.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The persisted baseline revision.
    pub fn baseline_revision(&self) -> &str {
        &self.baseline_revision
    }

    /// Atomically rewrite `graph.json`.
    pub fn write_graph(&self, graph: &serde_json::Value) -> std::io::Result<()> {
        atomic_write(&self.root.join("graph.json"), graph.to_string().as_bytes())
    }

    /// Atomically rewrite the per-task snapshot `nodes/<task-id>.json`.
    pub fn write_node(&self, task_id: &str, node: &serde_json::Value) -> std::io::Result<()> {
        atomic_write(
            &self.root.join("nodes").join(format!("{task_id}.json")),
            node.to_string().as_bytes(),
        )
    }

    /// Append one decision line to `decisions.jsonl` (run_id filled from
    /// self). Rejects causes outside [`DECISION_CAUSES`].
    pub fn append_decision(&mut self, entry: &DecisionEntry) -> std::io::Result<()> {
        if !is_valid_cause(&entry.cause) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid decision cause: {}", entry.cause),
            ));
        }
        let mut filled = entry.clone();
        filled.run_id = self.run_id.clone();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.root.join("decisions.jsonl"))?;
        writeln!(file, "{}", serde_json::to_string(&filled).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })?)?;
        file.flush()?;
        Ok(())
    }

    /// Read back all decisions, skipping blank lines.
    pub fn read_decisions(&self) -> std::io::Result<Vec<DecisionEntry>> {
        let content = fs::read_to_string(self.root.join("decisions.jsonl"))?;
        let mut out = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: DecisionEntry = serde_json::from_str(line).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("parsing decisions.jsonl line: {e}"),
                )
            })?;
            out.push(entry);
        }
        Ok(out)
    }

    /// Append an evidence record to the immutable per-task array in
    /// `evidence/<task-id>.json` (atomic rewrite; records never mutate).
    pub fn record_evidence(
        &mut self,
        task_id: &str,
        kind: EvidenceKind,
        payload: serde_json::Value,
    ) -> std::io::Result<EvidenceRecord> {
        let id = format!("ev-{}", self.next_evidence);
        self.next_evidence += 1;
        let record = EvidenceRecord {
            id: id.clone(),
            task_id: task_id.to_string(),
            kind,
            payload,
            recorded_at: chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        };

        let path = self.root.join("evidence").join(format!("{task_id}.json"));
        let mut records: Vec<EvidenceRecord> = Vec::new();
        if let Ok(existing) = fs::read_to_string(&path) {
            if let Ok(parsed) = serde_json::from_str::<Vec<EvidenceRecord>>(&existing) {
                records = parsed;
            }
        }
        records.push(record.clone());
        atomic_write(&path, serde_json::to_string(&records).unwrap().as_bytes())?;
        Ok(record)
    }

    /// Path for a task's patch artifact: `patches/<task-id>.patch`.
    pub fn patch_path(&self, task_id: &str) -> PathBuf {
        self.root.join("patches").join(format!("{task_id}.patch"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_reference_vectors() {
        assert_eq!(fnv1a64(""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64("a"), 0xaf63dc4c8601ec8c);
    }

    #[test]
    fn cause_validation() {
        assert!(is_valid_cause("run_resumed"));
        assert!(!is_valid_cause("not_a_cause"));
        assert_eq!(DECISION_CAUSES.len(), 13);
    }

    #[test]
    fn create_at_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("runs").join("r-1");
        let handle = RunHandle::create_at(&root, "r-1", "rev-abc").unwrap();
        assert!(root.join("graph.json").is_file());
        assert!(root.join("nodes").is_dir());
        assert!(root.join("evidence").is_dir());
        assert!(root.join("patches").is_dir());
        assert!(root.join("decisions.jsonl").is_file());
        assert_eq!(handle.run_id(), "r-1");
        assert_eq!(handle.baseline_revision(), "rev-abc");
    }

    #[test]
    fn write_node_and_evidence_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r-1");
        let mut handle = RunHandle::create_at(&root, "r-1", "rev-1").unwrap();

        let node = serde_json::json!({"status": "Dispatched", "attempts": 1});
        handle.write_node("task-auth", &node).unwrap();
        let read_back: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("nodes").join("task-auth.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(read_back, node);

        let rec = handle
            .record_evidence("task-auth", EvidenceKind::CommandOutput, serde_json::json!({"exit": 0}))
            .unwrap();
        assert_eq!(rec.id, "ev-1");
        let rec2 = handle
            .record_evidence("task-auth", EvidenceKind::ReviewOutcome, serde_json::json!({"ok": true}))
            .unwrap();
        assert_eq!(rec2.id, "ev-2");

        let stored: Vec<EvidenceRecord> = serde_json::from_str(
            &fs::read_to_string(root.join("evidence").join("task-auth.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].id, "ev-1");
        assert_eq!(stored[1].id, "ev-2");
        assert_eq!(stored[0].kind, EvidenceKind::CommandOutput);
        assert_eq!(stored[1].kind, EvidenceKind::ReviewOutcome);
    }

    #[test]
    fn decisions_round_trip_and_invalid_cause() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r-1");
        let mut handle = RunHandle::create_at(&root, "r-1", "rev-1").unwrap();

        let mut e1 = DecisionEntry::new("task-auth", "Dispatched", "Evaluating", "worker_completed", vec!["ev-1".into()], "worker done");
        handle.append_decision(&e1).unwrap();
        e1.cause = "bogus_cause".into();
        let err = handle.append_decision(&e1).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        let decisions = handle.read_decisions().unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].run_id, "r-1");
        assert_eq!(decisions[0].cause, "worker_completed");
        assert_eq!(decisions[0].evidence_ids, vec!["ev-1".to_string()]);
    }

    #[test]
    fn resume_happy_path_preserves_next_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r-1");
        let mut handle = RunHandle::create_at(&root, "r-1", "rev-1").unwrap();
        handle
            .record_evidence("task-a", EvidenceKind::Inspection, serde_json::json!({}))
            .unwrap();
        handle
            .record_evidence("task-b", EvidenceKind::Inspection, serde_json::json!({}))
            .unwrap();
        drop(handle);

        let mut resumed = RunHandle::resume_at(&root, "r-1", "rev-1").unwrap();
        assert_eq!(resumed.baseline_revision(), "rev-1");
        let rec = resumed
            .record_evidence("task-c", EvidenceKind::Inspection, serde_json::json!({}))
            .unwrap();
        assert_eq!(rec.id, "ev-3");
    }

    #[test]
    fn resume_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("nope");
        match RunHandle::resume_at(&root, "r-9", "rev-1") {
            Err(ResumeError::NotFound(msg)) => assert!(msg.contains("nope")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn resume_baseline_mismatch_carries_both_revisions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r-1");
        RunHandle::create_at(&root, "r-1", "rev-old").unwrap();
        match RunHandle::resume_at(&root, "r-1", "rev-new") {
            Err(ResumeError::BaselineMismatch { persisted, current }) => {
                assert_eq!(persisted, "rev-old");
                assert_eq!(current, "rev-new");
            }
            other => panic!("expected BaselineMismatch, got {other:?}"),
        }
    }

    #[test]
    fn write_graph_atomicity_smoke() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r-1");
        let handle = RunHandle::create_at(&root, "r-1", "rev-1").unwrap();
        handle.write_graph(&serde_json::json!({"nodes": {"a": 1}})).unwrap();
        handle.write_graph(&serde_json::json!({"nodes": {"b": 2}})).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.join("graph.json")).unwrap()).unwrap();
        assert_eq!(parsed, serde_json::json!({"nodes": {"b": 2}}));
    }

    #[test]
    fn run_root_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = run_root(tmp.path(), "r-1");
        let comps: Vec<_> = root.components().collect();
        let names: Vec<&str> = comps
            .iter()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();
        let hypercode = names.iter().position(|n| *n == "hypercode").unwrap();
        assert_eq!(names[hypercode + 1], "projects");
        assert_eq!(names[hypercode + 2], project_hash(tmp.path()));
        assert_eq!(names[hypercode + 3], "runs");
        assert_eq!(names[hypercode + 4], "r-1");
        assert!(root.is_absolute());
    }
}
