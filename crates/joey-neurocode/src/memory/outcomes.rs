//! Verified-outcome memory store (spec 023, FR-025/FR-026).
//!
//! FR-025: a [`VerifiedOutcome`] is the ONLY legal input to outcome memory —
//! written when a task Completes through a passed verification gate (or
//! reaches a terminal failure). An [`OutcomeMemory`] is the persisted lesson
//! with full provenance; consultations bump `hit_count` and artifact-hash
//! re-checks expire or down-rank lessons whose artifacts materially changed.
//!
//! This module currently defines the TYPES plus a minimal in-memory buffer;
//! the SQLite-backed store lands in T027 and replaces the buffer.

/// The only legal input to outcome memory (FR-025): recorded when a task
/// Completes through a passed verification gate (or fails terminally).
/// Provenance-complete so a later store can re-verify artifact hashes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VerifiedOutcome {
    /// Stable v1 task signature (matches `analysis::task_signature`).
    pub task_signature: String,
    /// Repository revision the outcome was verified against.
    pub repository_revision: String,
    /// Graph NodeIds the task touched.
    pub artifact_ids: Vec<u64>,
    /// Policy ids that were in effect (combined bindings).
    pub policy_ids: Vec<String>,
    /// Stable failure signature, when the outcome records a failure.
    pub failure_signature: Option<String>,
    /// How the issue was resolved (the lesson).
    pub resolution: Option<String>,
    /// Evidence record ids backing the outcome.
    pub evidence_ids: Vec<String>,
    /// 0..=100 confidence in the lesson.
    pub confidence: u8,
}

/// A persisted lesson with provenance (FR-025/FR-026). Consulted by task
/// signature; each consultation bumps `hit_count`, and an artifact-hash
/// re-check expires or down-ranks the lesson when its artifacts materially
/// changed (confidence scaled toward 0 — the SQLite store in T027 enforces
/// this; the in-memory buffer below does not).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutcomeMemory {
    pub task_signature: String,
    pub repository_revision: String,
    pub artifact_ids: Vec<u64>,
    pub policy_ids: Vec<String>,
    pub failure_signature: Option<String>,
    pub resolution: Option<String>,
    pub evidence_ids: Vec<String>,
    pub confidence: u8,
    /// Times this lesson has been consulted (bumped on consultation).
    pub hit_count: u32,
    /// RFC3339 timestamp of the last confirming consultation.
    pub last_confirmed_at: String,
}

/// Minimal in-memory outcome buffer (REPLACED by the SQLite store in T027 —
/// kept deliberately tiny). Records verified outcomes as lessons and serves
/// consultations by exact task-signature match.
#[derive(Default)]
pub struct OutcomeMemoryBuffer {
    lessons: Vec<OutcomeMemory>,
}

impl OutcomeMemoryBuffer {
    /// Record a verified outcome as a new lesson (hit_count starts at 0,
    /// last_confirmed_at = now, RFC3339).
    pub fn record(&mut self, outcome: &VerifiedOutcome) -> OutcomeMemory {
        let lesson = OutcomeMemory {
            task_signature: outcome.task_signature.clone(),
            repository_revision: outcome.repository_revision.clone(),
            artifact_ids: outcome.artifact_ids.clone(),
            policy_ids: outcome.policy_ids.clone(),
            failure_signature: outcome.failure_signature.clone(),
            resolution: outcome.resolution.clone(),
            evidence_ids: outcome.evidence_ids.clone(),
            confidence: outcome.confidence,
            hit_count: 0,
            last_confirmed_at: chrono::Utc::now().to_rfc3339(),
        };
        self.lessons.push(lesson.clone());
        lesson
    }

    /// All lessons matching `signature` exactly (cloned). The T027 store
    /// additionally bumps hit_count and re-checks artifact hashes here.
    pub fn consult_by_signature(&self, signature: &str) -> Vec<OutcomeMemory> {
        self.lessons
            .iter()
            .filter(|lesson| lesson.task_signature == signature)
            .cloned()
            .collect()
    }

    /// Number of buffered lessons.
    pub fn len(&self) -> usize {
        self.lessons.len()
    }

    /// True when no lessons are buffered.
    pub fn is_empty(&self) -> bool {
        self.lessons.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(signature: &str) -> VerifiedOutcome {
        VerifiedOutcome {
            task_signature: signature.to_string(),
            repository_revision: "abc123".to_string(),
            artifact_ids: vec![7, 8],
            policy_ids: vec!["repo/AGENTS.md#0".to_string()],
            failure_signature: Some("NPE at Hub".to_string()),
            resolution: Some("guard the lookup".to_string()),
            evidence_ids: vec!["ev-1".to_string()],
            confidence: 80,
        }
    }

    #[test]
    fn record_maps_provenance_and_stamps_now() {
        let mut buffer = OutcomeMemoryBuffer::default();
        let lesson = buffer.record(&outcome("t1|obj|a.rs"));
        assert_eq!(lesson.task_signature, "t1|obj|a.rs");
        assert_eq!(lesson.repository_revision, "abc123");
        assert_eq!(lesson.artifact_ids, vec![7, 8]);
        assert_eq!(lesson.policy_ids, vec!["repo/AGENTS.md#0".to_string()]);
        assert_eq!(lesson.failure_signature.as_deref(), Some("NPE at Hub"));
        assert_eq!(lesson.resolution.as_deref(), Some("guard the lookup"));
        assert_eq!(lesson.evidence_ids, vec!["ev-1".to_string()]);
        assert_eq!(lesson.confidence, 80);
        assert_eq!(lesson.hit_count, 0);
        assert!(!lesson.last_confirmed_at.is_empty());
        assert!(lesson.last_confirmed_at.contains('T')); // RFC3339
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn consult_returns_all_matching_clones() {
        let mut buffer = OutcomeMemoryBuffer::default();
        buffer.record(&outcome("sig"));
        buffer.record(&outcome("sig"));
        buffer.record(&outcome("other"));
        let hits = buffer.consult_by_signature("sig");
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|l| l.task_signature == "sig"));
        assert_eq!(buffer.consult_by_signature("missing").len(), 0);
        assert_eq!(buffer.len(), 3);
        assert!(!buffer.is_empty());
    }

    #[test]
    fn serde_round_trips_verified_outcome_and_memory() {
        let memory = buffer_lesson();
        let json = serde_json::to_string(&memory).unwrap();
        assert!(json.contains("\"task_signature\""));
        assert!(json.contains("\"hit_count\""));
        assert!(json.contains("\"last_confirmed_at\""));
        let back: OutcomeMemory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, memory);

        let verified = outcome("t|o|p");
        let json = serde_json::to_string(&verified).unwrap();
        let back: VerifiedOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, verified);
    }

    fn buffer_lesson() -> OutcomeMemory {
        OutcomeMemory {
            task_signature: "t1|obj|a.rs".to_string(),
            repository_revision: "abc123".to_string(),
            artifact_ids: vec![7],
            policy_ids: vec!["p1".to_string()],
            failure_signature: None,
            resolution: Some("r".to_string()),
            evidence_ids: vec!["e".to_string()],
            confidence: 50,
            hit_count: 2,
            last_confirmed_at: "2026-09-02T00:00:00+00:00".to_string(),
        }
    }
}
