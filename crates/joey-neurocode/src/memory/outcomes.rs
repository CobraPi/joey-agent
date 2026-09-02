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

/// SQLite-backed store for verified-outcome lessons (spec 023, T027).
///
/// FR-025: [`OutcomeStore::record`] is the ONLY write path and only accepts a
/// [`VerifiedOutcome`], so every persisted lesson carries full provenance
/// (repository revision, artifact ids, policy ids, evidence ids).
/// SC-008: the type system admits no unverified writes — 100% of lessons in
/// this store are verified and provenance-complete.
///
/// FR-026: lessons whose artifacts materially changed are expired — the
/// caller recomputes artifact hashes and passes the verdict to
/// [`OutcomeStore::mark_expired_where_artifacts`] (hash computation needs the
/// graph, which this store deliberately does not hold).
///
/// R11: the single `outcome_memory` table is created additively via
/// `CREATE TABLE IF NOT EXISTS` at store open; `NEUROCODE_SCHEMA_VERSION`
/// stays 3 and the graph store's migration code is untouched.
pub struct OutcomeStore {
    conn: rusqlite::Connection,
}

/// Selected column list shared by every read; `id` first (index 0) so reads
/// can bump/expire rows by primary key, then the [`OutcomeMemory`] fields in
/// declaration order (indices 1..=10, see [`row_to_memory`]).
const LESSON_COLS: &str = "id, task_signature, repository_revision, artifact_ids, \
                           policy_ids, failure_signature, resolution, evidence_ids, \
                           confidence, hit_count, last_confirmed_at";

impl OutcomeStore {
    /// Open (or create) the store at `path`, creating the `outcome_memory`
    /// table if needed (R11: additive and idempotent across reopens).
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    /// Open an in-memory store (tests / ephemeral use).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    /// Create the additive `outcome_memory` table. R11: `NEUROCODE_SCHEMA_
    /// VERSION` stays 3 — this table lives outside the graph store's schema
    /// versioning entirely, and `CREATE TABLE IF NOT EXISTS` keeps store
    /// opens idempotent.
    fn init(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS outcome_memory (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_signature TEXT NOT NULL,
                repository_revision TEXT NOT NULL,
                artifact_ids TEXT NOT NULL,
                policy_ids TEXT NOT NULL,
                failure_signature TEXT,
                resolution TEXT,
                evidence_ids TEXT NOT NULL,
                confidence INTEGER NOT NULL,
                hit_count INTEGER NOT NULL DEFAULT 0,
                expired INTEGER NOT NULL DEFAULT 0,
                last_confirmed_at TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
        )
    }

    /// Record a verified outcome as a new lesson (FR-025: the ONLY write
    /// path — type-enforced verified provenance). `hit_count` starts at 0;
    /// `last_confirmed_at` = `created_at` = now (RFC3339).
    pub fn record(&self, outcome: &VerifiedOutcome) -> rusqlite::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO outcome_memory (
                task_signature, repository_revision, artifact_ids, policy_ids,
                failure_signature, resolution, evidence_ids, confidence,
                hit_count, expired, last_confirmed_at, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, ?9, ?9)",
            rusqlite::params![
                outcome.task_signature,
                outcome.repository_revision,
                to_json(&outcome.artifact_ids),
                to_json(&outcome.policy_ids),
                outcome.failure_signature,
                outcome.resolution,
                to_json(&outcome.evidence_ids),
                outcome.confidence,
                now,
            ],
        )?;
        Ok(())
    }

    /// Consult lessons by exact task signature (FR-025). Returns non-expired
    /// rows whose `task_signature` matches, bumping each returned row's
    /// `hit_count` by 1 — the returned values are post-bump.
    pub fn consult_by_signature(&self, signature: &str) -> rusqlite::Result<Vec<OutcomeMemory>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {LESSON_COLS} FROM outcome_memory \
             WHERE expired = 0 AND task_signature = ?1"
        ))?;
        let rows = stmt.query_map([signature], |row| {
            Ok((row.get::<_, i64>(0)?, row_to_memory(row)?))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (id, mut lesson) = row?;
            self.conn.execute(
                "UPDATE outcome_memory SET hit_count = hit_count + 1 WHERE id = ?1",
                rusqlite::params![id],
            )?;
            lesson.hit_count += 1;
            hits.push(lesson);
        }
        Ok(hits)
    }

    /// Consult lessons by artifact overlap: non-expired rows whose
    /// `artifact_ids` intersect `artifact_ids` (selects all non-expired rows,
    /// filters in Rust). NO hit-count bump — the consultation bump applies to
    /// signature matches; artifact consults feed the artifact-hash re-check
    /// (FR-026).
    pub fn consult_by_artifacts(&self, artifact_ids: &[u64]) -> rusqlite::Result<Vec<OutcomeMemory>> {
        let mut stmt =
            self.conn
                .prepare(&format!(
                    "SELECT {LESSON_COLS} FROM outcome_memory WHERE expired = 0"
                ))?;
        let rows = stmt.query_map([], |row| row_to_memory(row))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|lesson| lesson.artifact_ids.iter().any(|id| artifact_ids.contains(id)))
            .collect())
    }

    /// FR-026: expire non-expired lessons whose `artifact_ids` intersect
    /// `artifact_ids` when the caller's artifact-hash re-check found a
    /// material change (`changed == true`); returns how many rows were
    /// expired. When `changed == false` returns 0 without writing — the
    /// caller recomputes artifact hashes and passes the verdict, because
    /// hash computation needs the graph, which this store deliberately does
    /// not hold.
    pub fn mark_expired_where_artifacts(
        &self,
        artifact_ids: &[u64],
        changed: bool,
    ) -> rusqlite::Result<usize> {
        if !changed {
            return Ok(0);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id, artifact_ids FROM outcome_memory WHERE expired = 0")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                from_json_col::<u64>(row.get(1)?),
            ))
        })?;
        let mut to_expire = Vec::new();
        for row in rows {
            let (id, ids) = row?;
            if ids.iter().any(|id| artifact_ids.contains(id)) {
                to_expire.push(id);
            }
        }
        drop(stmt);
        let mut expired = 0usize;
        for id in to_expire {
            expired += self.conn.execute(
                "UPDATE outcome_memory SET expired = 1 WHERE id = ?1",
                rusqlite::params![id],
            )?;
        }
        Ok(expired)
    }

    /// All rows including expired ones (testing / inspection).
    pub fn all_rows(&self) -> rusqlite::Result<Vec<OutcomeMemory>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {LESSON_COLS} FROM outcome_memory"))?;
        let rows = stmt.query_map([], |row| row_to_memory(row))?;
        rows.collect()
    }
}

/// Map a row selected with [`LESSON_COLS`] (id at index 0) to an
/// [`OutcomeMemory`]; JSON array columns parse via `serde_json`, yielding an
/// empty vec on a NULL/invalid column.
fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutcomeMemory> {
    Ok(OutcomeMemory {
        task_signature: row.get(1)?,
        repository_revision: row.get(2)?,
        artifact_ids: from_json_col(row.get(3)?),
        policy_ids: from_json_col(row.get(4)?),
        failure_signature: row.get(5)?,
        resolution: row.get(6)?,
        evidence_ids: from_json_col(row.get(7)?),
        confidence: row.get(8)?,
        hit_count: row.get(9)?,
        last_confirmed_at: row.get(10)?,
    })
}

/// Serialize a slice as a JSON array column.
fn to_json<T: serde::Serialize>(value: &[T]) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
}

/// Parse a JSON array column (`Vec<T>`); NULL or unparseable yields an empty
/// vec.
fn from_json_col<T: serde::de::DeserializeOwned>(col: Option<String>) -> Vec<T> {
    col.as_deref()
        .and_then(|s| serde_json::from_str::<Vec<T>>(s).ok())
        .unwrap_or_default()
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

    fn store_outcome(signature: &str, artifact_ids: Vec<u64>) -> VerifiedOutcome {
        VerifiedOutcome {
            task_signature: signature.to_string(),
            repository_revision: "abc123".to_string(),
            artifact_ids,
            policy_ids: vec!["repo/AGENTS.md#0".to_string()],
            failure_signature: Some("NPE at Hub".to_string()),
            resolution: Some("guard the lookup".to_string()),
            evidence_ids: vec!["ev-1".to_string()],
            confidence: 80,
        }
    }

    #[test]
    fn record_and_consult_roundtrip() {
        let store = OutcomeStore::open_in_memory().unwrap();
        store
            .record(&store_outcome("t1|obj|a.rs", vec![7, 8]))
            .unwrap();
        let hits = store.consult_by_signature("t1|obj|a.rs").unwrap();
        assert_eq!(hits.len(), 1);
        let lesson = &hits[0];
        assert_eq!(lesson.task_signature, "t1|obj|a.rs");
        assert_eq!(lesson.repository_revision, "abc123");
        assert_eq!(lesson.artifact_ids, vec![7, 8]);
        assert_eq!(lesson.policy_ids, vec!["repo/AGENTS.md#0".to_string()]);
        assert_eq!(lesson.failure_signature.as_deref(), Some("NPE at Hub"));
        assert_eq!(lesson.resolution.as_deref(), Some("guard the lookup"));
        assert_eq!(lesson.evidence_ids, vec!["ev-1".to_string()]);
        assert_eq!(lesson.confidence, 80);
        assert_eq!(lesson.hit_count, 1); // bumped from 0 by the consult
        assert!(lesson.last_confirmed_at.contains('T')); // RFC3339
    }

    #[test]
    fn consultation_bumps_hit_count() {
        let store = OutcomeStore::open_in_memory().unwrap();
        store.record(&store_outcome("sig", vec![7])).unwrap();
        let first = store.consult_by_signature("sig").unwrap();
        assert_eq!(first[0].hit_count, 1);
        let second = store.consult_by_signature("sig").unwrap();
        assert_eq!(second[0].hit_count, 2);
    }

    #[test]
    fn consult_by_artifacts_intersects() {
        let store = OutcomeStore::open_in_memory().unwrap();
        store.record(&store_outcome("sig", vec![42])).unwrap();
        let hits = store.consult_by_artifacts(&[42]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].task_signature, "sig");
        assert_eq!(store.consult_by_artifacts(&[7]).unwrap().len(), 0);
    }

    #[test]
    fn expiry_excludes_and_counts() {
        let store = OutcomeStore::open_in_memory().unwrap();
        store.record(&store_outcome("sig", vec![42])).unwrap();
        assert_eq!(store.mark_expired_where_artifacts(&[42], true).unwrap(), 1);
        assert_eq!(store.consult_by_signature("sig").unwrap().len(), 0);
        assert_eq!(store.all_rows().unwrap().len(), 1); // still present
        assert_eq!(store.mark_expired_where_artifacts(&[42], false).unwrap(), 0);
    }

    // SC-008: the type system admits no write path other than
    // `OutcomeStore::record(&VerifiedOutcome)` — there is no method accepting
    // an unverified outcome, so 100% of persisted lessons are verified with
    // provenance. The empty-result assertion below pins the default state.
    #[test]
    fn unverified_yields_no_records() {
        let store = OutcomeStore::open_in_memory().unwrap();
        assert_eq!(store.consult_by_signature("anything").unwrap().len(), 0);
        assert_eq!(store.all_rows().unwrap().len(), 0);
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outcome_memory.sqlite3");
        {
            let store = OutcomeStore::open(&path).unwrap();
            store.record(&store_outcome("sig", vec![7])).unwrap();
        }
        let reopened = OutcomeStore::open(&path).unwrap(); // CREATE TABLE IF NOT EXISTS idempotent
        let hits = reopened.consult_by_signature("sig").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].artifact_ids, vec![7]);
    }
}
