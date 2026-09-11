//! Episodic memory store (feature 027, spec specs/027-please-enhance-neurocode):
//! one row per completed interactive task or orchestrated workstream (clarified
//! Q2). Rows live in the per-project graph.db `memory_episodes` table (schema
//! v4, contracts/neurocode-memory-storage.md). Insert is the store-level
//! sanitization choke point [analyze U1]: every text field is
//! secret-redacted (joey_core::redact::redact_sensitive_text) and
//! length-capped BEFORE persist, so all writers (interactive capture,
//! hypercode, distiller) inherit it.

use sha2::{Digest, Sha256};

/// Maximum length (chars) of an episode title before persist.
pub const TITLE_CAP: usize = 200;
/// Maximum length (chars) of the task/context/approach/lessons fields.
pub const FIELD_CAP: usize = 4096;

/// One concrete work event — the episodic memory (spec Key Entities). Unit:
/// one per completed interactive task, one per completed orchestrated
/// workstream/subtask, success or failure (clarified Q2).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryEpisode {
    pub id: String,
    pub kind: EpisodeKind,
    pub title: String,
    pub task: String,
    pub context: String,
    pub approach: String,
    pub outcome: EpisodeOutcome,
    pub lessons: String,
    pub source: EpisodeSource,
    pub origin_run: String,
    pub evidence_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Interactive task vs orchestrated workstream (clarified Q2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EpisodeKind {
    Task,
    Workstream,
}

impl EpisodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EpisodeKind::Task => "task",
            EpisodeKind::Workstream => "workstream",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task" => Some(EpisodeKind::Task),
            "workstream" => Some(EpisodeKind::Workstream),
            _ => None,
        }
    }
}

/// How the work ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EpisodeOutcome {
    Success,
    Failure,
    Partial,
}

impl EpisodeOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            EpisodeOutcome::Success => "success",
            EpisodeOutcome::Failure => "failure",
            EpisodeOutcome::Partial => "partial",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "success" => Some(EpisodeOutcome::Success),
            "failure" => Some(EpisodeOutcome::Failure),
            "partial" => Some(EpisodeOutcome::Partial),
            _ => None,
        }
    }
}

/// Which capture path wrote the episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EpisodeSource {
    Interactive,
    Hypercode,
}

impl EpisodeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            EpisodeSource::Interactive => "interactive",
            EpisodeSource::Hypercode => "hypercode",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "interactive" => Some(EpisodeSource::Interactive),
            "hypercode" => Some(EpisodeSource::Hypercode),
            _ => None,
        }
    }
}

/// Which memory item a [`MemoryVectorRecord`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemoryItemKind {
    Episode,
    Preference,
}

impl MemoryItemKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryItemKind::Episode => "episode",
            MemoryItemKind::Preference => "preference",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "episode" => Some(MemoryItemKind::Episode),
            "preference" => Some(MemoryItemKind::Preference),
            _ => None,
        }
    }
}

/// Quantization of the stored embedding blob (same encoding as rag_vectors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemoryQuantization {
    F32,
    Int8,
}

impl MemoryQuantization {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryQuantization::F32 => "f32",
            MemoryQuantization::Int8 => "int8",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "f32" => Some(MemoryQuantization::F32),
            "int8" => Some(MemoryQuantization::Int8),
            _ => None,
        }
    }
}

/// A stored embedding for an episode or preference (`memory_vectors` table).
pub struct MemoryVectorRecord {
    pub item_id: String,
    pub item_kind: MemoryItemKind,
    pub dim: u32,
    pub quantization: MemoryQuantization,
    pub blob: Vec<u8>,
}

/// The store-level sanitization choke point [analyze U1]: secret-redact, then
/// truncate to `cap` CHARS on a char boundary (never mid-char).
pub fn sanitize_text(text: &str, cap: usize) -> String {
    let redacted = joey_core::redact::redact_sensitive_text(text);
    if redacted.chars().count() <= cap {
        return redacted;
    }
    match redacted.char_indices().nth(cap) {
        Some((idx, _)) => redacted[..idx].to_string(),
        None => redacted,
    }
}

/// Errors from [`EpisodeStore::insert`] / [`PreferenceStore::upsert`].
#[derive(Debug)]
pub enum EpisodeStoreError {
    Sqlite(rusqlite::Error),
    /// Row skipped: the statement/task field was empty after sanitization
    /// (FR-011 rule). Reused by the preference store for empty statements —
    /// the variant name is historical; it means "empty after sanitization".
    SkippedEmptyTask,
}

impl std::fmt::Display for EpisodeStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EpisodeStoreError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            EpisodeStoreError::SkippedEmptyTask => {
                write!(f, "skipped: field empty after sanitization")
            }
        }
    }
}

impl std::error::Error for EpisodeStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EpisodeStoreError::Sqlite(e) => Some(e),
            EpisodeStoreError::SkippedEmptyTask => None,
        }
    }
}

impl From<rusqlite::Error> for EpisodeStoreError {
    fn from(e: rusqlite::Error) -> Self {
        EpisodeStoreError::Sqlite(e)
    }
}

/// Store-level helper: generate a stable content-derived id with the given
/// prefix (`ep-` episodes, `pr-` preferences).
pub(crate) fn generate_id(prefix: &str, seed: &str) -> String {
    let digest = Sha256::digest(seed.as_bytes());
    let first8hex: String = digest.iter().take(4).map(|b| format!("{:02x}", b)).collect();
    format!("{}-{}-{}", prefix, chrono::Utc::now().timestamp_millis(), first8hex)
}

/// SQLite-backed episodic memory store over the per-project graph.db
/// `memory_episodes` table (schema v4).
pub struct EpisodeStore {
    conn: rusqlite::Connection,
}

impl EpisodeStore {
    /// Open the store at the graph.db `path`. The memory tables already exist
    /// (schema v4 applied by GraphStore); this only opens the connection.
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    /// Open an in-memory store (test seam). Creates the memory tables with
    /// the DDL copied from the GraphStore v4 batch — keep in sync with
    /// `graph/store.rs`.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS memory_episodes (
              id TEXT PRIMARY KEY,
              kind TEXT NOT NULL CHECK (kind IN ('task','workstream')),
              title TEXT NOT NULL,
              task TEXT NOT NULL,
              context TEXT NOT NULL DEFAULT '',
              approach TEXT NOT NULL DEFAULT '',
              outcome TEXT NOT NULL CHECK (outcome IN ('success','failure','partial')),
              lessons TEXT NOT NULL DEFAULT '',
              source TEXT NOT NULL CHECK (source IN ('interactive','hypercode')),
              origin_run TEXT NOT NULL DEFAULT '',
              evidence_ids TEXT NOT NULL DEFAULT '[]',
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memory_preferences (
              id TEXT PRIMARY KEY,
              category TEXT NOT NULL,
              statement TEXT NOT NULL,
              origin TEXT NOT NULL CHECK (origin IN ('explicit','inferred')),
              evidence_ids TEXT NOT NULL DEFAULT '[]',
              status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','superseded')),
              supersedes TEXT,
              superseded_by TEXT,
              confidence INTEGER NOT NULL DEFAULT 50 CHECK (confidence BETWEEN 0 AND 100),
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memory_vectors (
              item_id TEXT PRIMARY KEY,
              item_kind TEXT NOT NULL CHECK (item_kind IN ('episode','preference')),
              dim INTEGER NOT NULL,
              quantization TEXT NOT NULL CHECK (quantization IN ('f32','int8')),
              vector BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS memory_episodes_created ON memory_episodes(created_at);
            CREATE INDEX IF NOT EXISTS memory_preferences_status ON memory_preferences(status, category);
            CREATE INDEX IF NOT EXISTS memory_vectors_kind ON memory_vectors(item_kind);
            "#,
        )?;
        Ok(Self { conn })
    }

    /// Insert an episode (sanitized: every text field redacted + capped
    /// BEFORE persist) and optionally its embedding vector, then FIFO-evict
    /// the oldest episodes beyond `max_episodes` (row + vector cascade).
    ///
    /// Returns `Ok(None)` — without writing — when `task` is empty after
    /// sanitization (FR-011 rule); `Ok(Some(id))` with the persisted id
    /// otherwise.
    pub fn insert(
        &self,
        episode: &MemoryEpisode,
        vector: Option<MemoryVectorRecord>,
        max_episodes: usize,
    ) -> Result<Option<String>, EpisodeStoreError> {
        let title = sanitize_text(&episode.title, TITLE_CAP);
        let task = sanitize_text(&episode.task, FIELD_CAP);
        let context = sanitize_text(&episode.context, FIELD_CAP);
        let approach = sanitize_text(&episode.approach, FIELD_CAP);
        let lessons = sanitize_text(&episode.lessons, FIELD_CAP);
        if task.trim().is_empty() {
            return Ok(None);
        }
        let id = if episode.id.is_empty() {
            generate_id("ep", &format!("{}{}", title, task))
        } else {
            episode.id.clone()
        };
        let now = chrono::Utc::now().to_rfc3339();
        let created_at = if episode.created_at.is_empty() {
            now.clone()
        } else {
            episode.created_at.clone()
        };
        let updated_at = if episode.updated_at.is_empty() {
            now.clone()
        } else {
            episode.updated_at.clone()
        };
        let evidence_json =
            serde_json::to_string(&episode.evidence_ids).unwrap_or_else(|_| "[]".to_string());

        // unchecked_transaction: the store shares one connection across
        // `&self` methods (no `&mut self` available for `transaction()`).
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO memory_episodes (
                id, kind, title, task, context, approach, outcome, lessons,
                source, origin_run, evidence_ids, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                id,
                episode.kind.as_str(),
                title,
                task,
                context,
                approach,
                episode.outcome.as_str(),
                lessons,
                episode.source.as_str(),
                sanitize_text(&episode.origin_run, FIELD_CAP),
                evidence_json,
                created_at,
                updated_at,
            ],
        )?;
        if let Some(rec) = vector {
            tx.execute(
                "INSERT OR REPLACE INTO memory_vectors \
                 (item_id, item_kind, dim, quantization, vector) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    rec.item_id,
                    rec.item_kind.as_str(),
                    rec.dim,
                    rec.quantization.as_str(),
                    rec.blob,
                ],
            )?;
        }
        // FIFO eviction: delete the oldest episodes beyond `max_episodes`
        // (vector rows first — the vector table has no FK to the owner).
        tx.execute(
            "DELETE FROM memory_vectors WHERE item_kind = 'episode' AND item_id IN (\
             SELECT id FROM memory_episodes ORDER BY created_at DESC, id DESC \
             LIMIT -1 OFFSET ?1)",
            rusqlite::params![max_episodes as i64],
        )?;
        tx.execute(
            "DELETE FROM memory_episodes WHERE id IN (\
             SELECT id FROM memory_episodes ORDER BY created_at DESC, id DESC \
             LIMIT -1 OFFSET ?1)",
            rusqlite::params![max_episodes as i64],
        )?;
        tx.commit()?;
        Ok(Some(id))
    }

    /// Fetch one episode by id.
    pub fn get(&self, id: &str) -> rusqlite::Result<Option<MemoryEpisode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, title, task, context, approach, outcome, lessons, \
             source, origin_run, evidence_ids, created_at, updated_at \
             FROM memory_episodes WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_episode)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Most recent episodes (`ORDER BY created_at DESC, id DESC LIMIT ?`).
    pub fn list_recent(&self, limit: usize) -> rusqlite::Result<Vec<MemoryEpisode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, title, task, context, approach, outcome, lessons, \
             source, origin_run, evidence_ids, created_at, updated_at \
             FROM memory_episodes ORDER BY created_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], row_to_episode)?;
        rows.collect()
    }

    /// Total number of persisted episodes.
    pub fn count(&self) -> rusqlite::Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memory_episodes", [], |row| {
                row.get(0)
            })?;
        Ok(n as usize)
    }

    /// Hard-delete one episode and its vector row (SC-004 never-reappear).
    /// Returns true when a row was deleted.
    pub fn delete(&self, id: &str) -> rusqlite::Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM memory_vectors WHERE item_kind = 'episode' AND item_id = ?1",
            rusqlite::params![id],
        )?;
        let affected = tx.execute("DELETE FROM memory_episodes WHERE id = ?1", rusqlite::params![id])?;
        tx.commit()?;
        Ok(affected > 0)
    }

    /// Put/replace an embedding row — used post-hoc by the rag layer after
    /// embedding.
    pub fn put_vector(&self, rec: &MemoryVectorRecord) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO memory_vectors \
             (item_id, item_kind, dim, quantization, vector) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                rec.item_id,
                rec.item_kind.as_str(),
                rec.dim,
                rec.quantization.as_str(),
                rec.blob,
            ],
        )?;
        Ok(())
    }

    /// Fetch the embedding row for `item_id`, if any.
    pub fn get_vector(&self, item_id: &str) -> rusqlite::Result<Option<MemoryVectorRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT item_id, item_kind, dim, quantization, vector \
             FROM memory_vectors WHERE item_id = ?1",
        )?;
        let mut rows = stmt.query_map([item_id], row_to_vector)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }
}

/// Map a `memory_episodes` row to a [`MemoryEpisode`]; `evidence_ids` parses
/// via serde_json, yielding an empty vec on a NULL/invalid column.
fn row_to_episode(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryEpisode> {
    let evidence_json: Option<String> = row.get(10)?;
    Ok(MemoryEpisode {
        id: row.get(0)?,
        kind: EpisodeKind::parse(&row.get::<_, String>(1)?).unwrap_or(EpisodeKind::Task),
        title: row.get(2)?,
        task: row.get(3)?,
        context: row.get(4)?,
        approach: row.get(5)?,
        outcome: EpisodeOutcome::parse(&row.get::<_, String>(6)?).unwrap_or(EpisodeOutcome::Success),
        lessons: row.get(7)?,
        source: EpisodeSource::parse(&row.get::<_, String>(8)?).unwrap_or(EpisodeSource::Interactive),
        origin_run: row.get(9)?,
        evidence_ids: evidence_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default(),
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// Map a `memory_vectors` row to a [`MemoryVectorRecord`].
fn row_to_vector(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryVectorRecord> {
    Ok(MemoryVectorRecord {
        item_id: row.get(0)?,
        item_kind: MemoryItemKind::parse(&row.get::<_, String>(1)?)
            .unwrap_or(MemoryItemKind::Episode),
        dim: row.get::<_, i64>(2)? as u32,
        quantization: MemoryQuantization::parse(&row.get::<_, String>(3)?)
            .unwrap_or(MemoryQuantization::F32),
        blob: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(task: &str) -> MemoryEpisode {
        MemoryEpisode {
            id: String::new(),
            kind: EpisodeKind::Task,
            title: "title".to_string(),
            task: task.to_string(),
            context: String::new(),
            approach: String::new(),
            outcome: EpisodeOutcome::Success,
            lessons: String::new(),
            source: EpisodeSource::Interactive,
            origin_run: String::new(),
            evidence_ids: vec!["ev-1".to_string()],
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn sanitize_redacts_and_caps_on_char_boundary() {
        let out = sanitize_text(&"x".repeat(10), 5);
        assert_eq!(out, "xxxxx");
        // Multi-byte chars are never split.
        let multibyte = "é".repeat(10);
        let out = sanitize_text(&multibyte, 3);
        assert_eq!(out, "ééé");
    }

    #[test]
    fn insert_generates_id_and_persists() {
        let store = EpisodeStore::open_in_memory().unwrap();
        let id = store.insert(&episode("do the thing"), None, 100).unwrap();
        assert!(id.is_some());
        let fetched = store.get(id.as_deref().unwrap()).unwrap().unwrap();
        assert_eq!(fetched.task, "do the thing");
        assert_eq!(fetched.evidence_ids, vec!["ev-1".to_string()]);
        assert!(fetched.id.starts_with("ep-"));
        assert!(fetched.created_at.contains('T')); // RFC 3339
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn insert_skips_empty_task() {
        let store = EpisodeStore::open_in_memory().unwrap();
        let out = store.insert(&episode("   "), None, 100).unwrap();
        assert!(out.is_none());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn fifo_eviction_oldest_beyond_cap() {
        let store = EpisodeStore::open_in_memory().unwrap();
        for i in 0..5 {
            let mut ep = episode(&format!("task {i}"));
            // Deterministic, increasing created_at keeps FIFO order stable.
            ep.created_at = format!("2026-09-09T00:00:{:02}+00:00", i);
            ep.updated_at = ep.created_at.clone();
            store.insert(&ep, None, 3).unwrap();
        }
        assert_eq!(store.count().unwrap(), 3);
        let recent: Vec<String> = store
            .list_recent(10)
            .unwrap()
            .into_iter()
            .map(|e| e.task)
            .collect();
        assert_eq!(recent, vec!["task 4", "task 3", "task 2"]);
    }

    #[test]
    fn delete_removes_row_and_vector() {
        let store = EpisodeStore::open_in_memory().unwrap();
        let id = store.insert(&episode("t"), None, 100).unwrap().unwrap();
        store
            .put_vector(&MemoryVectorRecord {
                item_id: id.clone(),
                item_kind: MemoryItemKind::Episode,
                dim: 4,
                quantization: MemoryQuantization::F32,
                blob: vec![1, 2, 3, 4],
            })
            .unwrap();
        assert!(store.get_vector(&id).unwrap().is_some());
        assert!(store.delete(&id).unwrap());
        assert!(store.get(&id).unwrap().is_none());
        assert!(store.get_vector(&id).unwrap().is_none());
        assert!(!store.delete(&id).unwrap());
    }
}
