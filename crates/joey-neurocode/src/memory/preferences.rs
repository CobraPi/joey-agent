//! Semantic memory store (feature 027): generalized user
//! preferences/conventions in graph.db `memory_preferences` (schema v4).
//! Insert path enforces the same sanitization choke point [analyze U1]
//! (redaction + 1 KB statement cap). Supersede rank is deterministic per
//! FR-008: explicit > inferred, then recency; an inferred preference NEVER
//! supersedes an explicit one (silently refused).

use crate::memory::episodes::{
    generate_id, sanitize_text, EpisodeStoreError, MemoryItemKind, MemoryQuantization,
    MemoryVectorRecord,
};

/// Maximum length (chars) of a preference statement before persist.
pub const STATEMENT_CAP: usize = 1024;
/// Maximum length (chars) of a category slug before persist.
pub const CATEGORY_CAP: usize = 64;

/// A generalized preference/convention — the semantic memory; the applied
/// layer that shapes code output.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryPreference {
    pub id: String,
    pub category: String,
    pub statement: String,
    pub origin: PreferenceOrigin,
    pub evidence_ids: Vec<String>,
    pub status: PreferenceStatus,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub confidence: u8,
    pub created_at: String,
    pub updated_at: String,
}

/// Whether the user said it or it was distilled from evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PreferenceOrigin {
    Explicit,
    Inferred,
}

impl PreferenceOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            PreferenceOrigin::Explicit => "explicit",
            PreferenceOrigin::Inferred => "inferred",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "explicit" => Some(PreferenceOrigin::Explicit),
            "inferred" => Some(PreferenceOrigin::Inferred),
            _ => None,
        }
    }
}

/// Active vs superseded (FR-008/FR-009 transitions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PreferenceStatus {
    Active,
    Superseded,
}

impl PreferenceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PreferenceStatus::Active => "active",
            PreferenceStatus::Superseded => "superseded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(PreferenceStatus::Active),
            "superseded" => Some(PreferenceStatus::Superseded),
            _ => None,
        }
    }
}

/// Result of [`PreferenceStore::upsert`].
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertOutcome {
    pub id: String,
    pub strengthened: bool,
    pub superseded_id: Option<String>,
}

/// FR-008 supersede rank: an inferred preference NEVER supersedes an
/// explicit one. Everything else is allowed (recency breaks ties upstream).
fn rank_allows(incoming: PreferenceOrigin, target: PreferenceOrigin) -> bool {
    !(target == PreferenceOrigin::Explicit && incoming == PreferenceOrigin::Inferred)
}

/// Slugify a category: lowercase, map each whitespace run to a single `-`,
/// keep only `[a-z0-9-]`, collapse consecutive `-`, trim leading/trailing
/// `-`, cap to [`CATEGORY_CAP`] chars on a char boundary.
fn slugify_category(category: &str) -> String {
    let hyphenated: String = category
        .to_lowercase()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    let mut collapsed = String::with_capacity(hyphenated.len());
    let mut prev_hyphen = false;
    for c in hyphenated.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push(c);
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }
    let slug = collapsed.trim_matches('-');
    if slug.chars().count() <= CATEGORY_CAP {
        return slug.to_string();
    }
    match slug.char_indices().nth(CATEGORY_CAP) {
        Some((idx, _)) => slug[..idx].to_string(),
        None => slug.to_string(),
    }
}

/// SQLite-backed semantic memory store over the per-project graph.db
/// `memory_preferences` table (schema v4).
pub struct PreferenceStore {
    conn: rusqlite::Connection,
}

/// Columns of a `memory_preferences` row in declaration order.
const PREFERENCE_COLS: &str = "id, category, statement, origin, evidence_ids, status, \
                               supersedes, superseded_by, confidence, created_at, updated_at";

impl PreferenceStore {
    /// Open the store at the graph.db `path`. The memory tables already
    /// exist (schema v4 applied by GraphStore); this only opens the
    /// connection.
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    /// Open an in-memory store (test seam). Creates the memory tables with
    /// the DDL copied from the GraphStore v4 batch — keep in sync with
    /// `graph/store.rs` (and `episodes::EpisodeStore::open_in_memory`).
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

    /// Upsert a preference.
    ///
    /// * Sanitizes `category` (slugified + capped) and `statement`
    ///   (redacted + capped to [`STATEMENT_CAP`]) — the same choke point as
    ///   the episode store [analyze U1]. An empty statement after
    ///   sanitization is an error ([`EpisodeStoreError::SkippedEmptyTask`];
    ///   the variant name is historical — it means "empty after
    ///   sanitization").
    /// * `strengthen_id` Some AND that row exists AND is active: append
    ///   `evidence` (deduped, capped at 32 ids), bump confidence by 10
    ///   (max 100), refresh `updated_at`, re-put `vector` if Some.
    /// * Otherwise insert a new active row (confidence 90 explicit / 50
    ///   inferred). When `supersede_id` targets an existing ACTIVE row and
    ///   the FR-008 rank allows it (inferred never supersedes explicit),
    ///   the target flips to superseded in the SAME transaction; a refused
    ///   rank is silent (the new row is still inserted, nothing is
    ///   superseded).
    pub fn upsert(
        &self,
        category: &str,
        statement: &str,
        origin: PreferenceOrigin,
        evidence: &[String],
        strengthen_id: Option<&str>,
        supersede_id: Option<&str>,
        vector: Option<MemoryVectorRecord>,
    ) -> Result<UpsertOutcome, EpisodeStoreError> {
        let category = slugify_category(category);
        let statement = sanitize_text(statement, STATEMENT_CAP);
        if statement.trim().is_empty() {
            return Err(EpisodeStoreError::SkippedEmptyTask);
        }

        // unchecked_transaction: the store shares one connection across
        // `&self` methods (no `&mut self` available for `transaction()`).
        let tx = self.conn.unchecked_transaction()?;

        // Strengthen path: existing active row.
        if let Some(sid) = strengthen_id {
            if let Some(existing) = Self::query_row(&tx, sid)? {
                if existing.status == PreferenceStatus::Active {
                    let mut merged = existing.evidence_ids.clone();
                    for ev in evidence {
                        if !merged.contains(ev) {
                            merged.push(ev.clone());
                        }
                    }
                    merged.truncate(32);
                    let confidence = existing.confidence.saturating_add(10).min(100);
                    let now = chrono::Utc::now().to_rfc3339();
                    tx.execute(
                        "UPDATE memory_preferences SET evidence_ids = ?1, confidence = ?2, \
                         updated_at = ?3 WHERE id = ?4",
                        rusqlite::params![
                            serde_json::to_string(&merged).unwrap_or_else(|_| "[]".to_string()),
                            confidence,
                            now,
                            sid,
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
                    tx.commit()?;
                    return Ok(UpsertOutcome {
                        id: sid.to_string(),
                        strengthened: true,
                        superseded_id: None,
                    });
                }
            }
        }

        // New-row path.
        let id = generate_id("pr", &format!("{}{}", category, statement));
        let now = chrono::Utc::now().to_rfc3339();
        let confidence: u8 = match origin {
            PreferenceOrigin::Explicit => 90,
            PreferenceOrigin::Inferred => 50,
        };
        let evidence_json =
            serde_json::to_string(evidence).unwrap_or_else(|_| "[]".to_string());
        tx.execute(
            "INSERT INTO memory_preferences (\
                id, category, statement, origin, evidence_ids, status, \
                supersedes, superseded_by, confidence, created_at, updated_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, 'active', NULL, NULL, ?6, ?7, ?7)",
            rusqlite::params![
                id,
                category,
                statement,
                origin.as_str(),
                evidence_json,
                confidence,
                now,
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

        // Supersede path (same transaction): FR-008 rank — an inferred
        // preference NEVER supersedes an explicit one (silently refused).
        let mut superseded_id = None;
        if let Some(tid) = supersede_id {
            let allowed = match Self::query_row(&tx, tid)? {
                Some(target)
                    if target.status == PreferenceStatus::Active
                        && rank_allows(origin, target.origin) =>
                {
                    Some(target)
                }
                _ => None,
            };
            if let Some(target) = allowed {
                tx.execute(
                    "UPDATE memory_preferences SET supersedes = ?1 WHERE id = ?2",
                    rusqlite::params![target.id, id],
                )?;
                tx.execute(
                    "UPDATE memory_preferences SET status = 'superseded', superseded_by = ?1, \
                     updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![id, now, target.id],
                )?;
                superseded_id = Some(target.id);
            }
        }
        tx.commit()?;
        Ok(UpsertOutcome {
            id,
            strengthened: false,
            superseded_id,
        })
    }

    /// Fetch one preference by id.
    pub fn get(&self, id: &str) -> rusqlite::Result<Option<MemoryPreference>> {
        Self::query_row(&self.conn, id)
    }

    /// Active preferences, optionally filtered by category, ordered by
    /// explicit-first then recency (`updated_at DESC, id DESC`), capped at
    /// `limit`.
    pub fn resolve_active(
        &self,
        category: Option<&str>,
        limit: usize,
    ) -> rusqlite::Result<Vec<MemoryPreference>> {
        let sql = match category {
            Some(_) => format!(
                "SELECT {PREFERENCE_COLS} FROM memory_preferences \
                 WHERE status = 'active' AND category = ?1 \
                 ORDER BY (origin = 'explicit') DESC, updated_at DESC, id DESC LIMIT ?2"
            ),
            None => format!(
                "SELECT {PREFERENCE_COLS} FROM memory_preferences \
                 WHERE status = 'active' \
                 ORDER BY (origin = 'explicit') DESC, updated_at DESC, id DESC LIMIT ?1"
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = if category.is_some() {
            stmt.query_map(
                rusqlite::params![category.unwrap(), limit as i64],
                row_to_preference,
            )?
        } else {
            stmt.query_map(rusqlite::params![limit as i64], row_to_preference)?
        };
        rows.collect()
    }

    /// Hard-delete one preference and its vector row (SC-004
    /// never-reappear). Returns true when a row was deleted.
    pub fn delete(&self, id: &str) -> rusqlite::Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM memory_vectors WHERE item_kind = 'preference' AND item_id = ?1",
            rusqlite::params![id],
        )?;
        let affected =
            tx.execute("DELETE FROM memory_preferences WHERE id = ?1", rusqlite::params![id])?;
        tx.commit()?;
        Ok(affected > 0)
    }

    /// Categories with >= 2 active explicit rows — competing explicit
    /// statements the user should reconcile (FR-008 surfacing), ordered by
    /// category.
    pub fn explicit_conflict_categories(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT category FROM memory_preferences \
             WHERE status = 'active' AND origin = 'explicit' \
             GROUP BY category HAVING COUNT(*) >= 2 ORDER BY category",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Number of active preferences.
    pub fn count_active(&self) -> rusqlite::Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory_preferences WHERE status = 'active'",
            [],
            |row| row.get(0),
        )?;
        Ok(n as usize)
    }

    /// Put/replace an embedding row (`item_kind = 'preference'`) — used
    /// post-hoc by the rag layer after embedding.
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
        let mut rows = stmt.query_map([item_id], |row| {
            Ok(MemoryVectorRecord {
                item_id: row.get(0)?,
                item_kind: MemoryItemKind::parse(&row.get::<_, String>(1)?)
                    .unwrap_or(MemoryItemKind::Preference),
                dim: row.get::<_, i64>(2)? as u32,
                quantization: MemoryQuantization::parse(&row.get::<_, String>(3)?)
                    .unwrap_or(MemoryQuantization::F32),
                blob: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Query one `memory_preferences` row by id (shared by upsert's
    /// strengthen/supersede lookups and the public `get`).
    fn query_row(
        conn: &rusqlite::Connection,
        id: &str,
    ) -> rusqlite::Result<Option<MemoryPreference>> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {PREFERENCE_COLS} FROM memory_preferences WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map([id], row_to_preference)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }
}

/// Map a `memory_preferences` row (in [`PREFERENCE_COLS`] order) to a
/// [`MemoryPreference`]; `evidence_ids` parses via serde_json, yielding an
/// empty vec on a NULL/invalid column.
fn row_to_preference(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryPreference> {
    let evidence_json: Option<String> = row.get(4)?;
    let supersedes: Option<String> = row.get(6)?;
    let superseded_by: Option<String> = row.get(7)?;
    Ok(MemoryPreference {
        id: row.get(0)?,
        category: row.get(1)?,
        statement: row.get(2)?,
        origin: PreferenceOrigin::parse(&row.get::<_, String>(3)?)
            .unwrap_or(PreferenceOrigin::Inferred),
        evidence_ids: evidence_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default(),
        status: PreferenceStatus::parse(&row.get::<_, String>(5)?)
            .unwrap_or(PreferenceStatus::Active),
        supersedes,
        superseded_by,
        confidence: row.get::<_, i64>(8)? as u8,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_filters() {
        assert_eq!(slugify_category("Error Handling!"), "error-handling");
        assert_eq!(slugify_category("A B"), "a-b");
        let capped = slugify_category(&"x".repeat(100));
        assert_eq!(capped.chars().count(), CATEGORY_CAP);
    }

    #[test]
    fn upsert_inserts_explicit_with_90() {
        let store = PreferenceStore::open_in_memory().unwrap();
        let out = store
            .upsert(
                "Naming",
                "I prefer snake_case",
                PreferenceOrigin::Explicit,
                &[],
                None,
                None,
                None,
            )
            .unwrap();
        assert!(!out.strengthened);
        assert!(out.superseded_id.is_none());
        let pref = store.get(&out.id).unwrap().unwrap();
        assert_eq!(pref.category, "naming");
        assert_eq!(pref.statement, "I prefer snake_case");
        assert_eq!(pref.origin, PreferenceOrigin::Explicit);
        assert_eq!(pref.status, PreferenceStatus::Active);
        assert_eq!(pref.confidence, 90);
        assert!(pref.created_at.contains('T')); // RFC 3339
        assert_eq!(store.count_active().unwrap(), 1);
    }

    #[test]
    fn upsert_empty_statement_is_error() {
        let store = PreferenceStore::open_in_memory().unwrap();
        let err = store
            .upsert("naming", "   ", PreferenceOrigin::Explicit, &[], None, None, None)
            .unwrap_err();
        assert!(matches!(err, EpisodeStoreError::SkippedEmptyTask));
    }

    #[test]
    fn strengthen_bumps_confidence_and_merges_evidence() {
        let store = PreferenceStore::open_in_memory().unwrap();
        let out = store
            .upsert("naming", "prefer snake_case", PreferenceOrigin::Inferred, &["ev-1".into()], None, None, None)
            .unwrap();
        assert_eq!(store.get(&out.id).unwrap().unwrap().confidence, 50);
        let out2 = store
            .upsert(
                "naming",
                "prefer snake_case still",
                PreferenceOrigin::Inferred,
                &["ev-2".into(), "ev-1".into()],
                Some(&out.id),
                None,
                None,
            )
            .unwrap();
        assert!(out2.strengthened);
        assert_eq!(out2.id, out.id);
        let pref = store.get(&out.id).unwrap().unwrap();
        assert_eq!(pref.confidence, 60);
        assert_eq!(pref.evidence_ids, vec!["ev-1".to_string(), "ev-2".to_string()]);
        assert_eq!(store.count_active().unwrap(), 1);
    }

    #[test]
    fn inferred_never_supersedes_explicit() {
        let store = PreferenceStore::open_in_memory().unwrap();
        let explicit = store
            .upsert("naming", "use tabs", PreferenceOrigin::Explicit, &[], None, None, None)
            .unwrap();
        let inferred = store
            .upsert(
                "naming",
                "use spaces",
                PreferenceOrigin::Inferred,
                &[],
                None,
                Some(&explicit.id),
                None,
            )
            .unwrap();
        // Rank refused: nothing superseded, both stay active.
        assert_eq!(inferred.superseded_id, None);
        let target = store.get(&explicit.id).unwrap().unwrap();
        assert_eq!(target.status, PreferenceStatus::Active);
        assert_eq!(store.count_active().unwrap(), 2);
    }

    #[test]
    fn explicit_supersedes_inferred_in_same_tx() {
        let store = PreferenceStore::open_in_memory().unwrap();
        let inferred = store
            .upsert("naming", "use spaces", PreferenceOrigin::Inferred, &[], None, None, None)
            .unwrap();
        let explicit = store
            .upsert(
                "naming",
                "use tabs",
                PreferenceOrigin::Explicit,
                &[],
                None,
                Some(&inferred.id),
                None,
            )
            .unwrap();
        assert_eq!(explicit.superseded_id.as_deref(), Some(inferred.id.as_str()));
        let target = store.get(&inferred.id).unwrap().unwrap();
        assert_eq!(target.status, PreferenceStatus::Superseded);
        assert_eq!(target.superseded_by.as_deref(), Some(explicit.id.as_str()));
        let winner = store.get(&explicit.id).unwrap().unwrap();
        assert_eq!(winner.supersedes.as_deref(), Some(inferred.id.as_str()));
        assert_eq!(store.count_active().unwrap(), 1);
    }

    #[test]
    fn resolve_active_orders_explicit_first() {
        let store = PreferenceStore::open_in_memory().unwrap();
        store
            .upsert("naming", "inferred one", PreferenceOrigin::Inferred, &[], None, None, None)
            .unwrap();
        store
            .upsert("naming", "explicit one", PreferenceOrigin::Explicit, &[], None, None, None)
            .unwrap();
        let all = store.resolve_active(None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].origin, PreferenceOrigin::Explicit);
        let scoped = store.resolve_active(Some("naming"), 10).unwrap();
        assert_eq!(scoped.len(), 2);
        assert_eq!(store.resolve_active(Some("other"), 10).unwrap().len(), 0);
    }

    #[test]
    fn conflict_categories_and_delete() {
        let store = PreferenceStore::open_in_memory().unwrap();
        store
            .upsert("naming", "one", PreferenceOrigin::Explicit, &[], None, None, None)
            .unwrap();
        assert_eq!(store.explicit_conflict_categories().unwrap().len(), 0);
        store
            .upsert("naming", "two", PreferenceOrigin::Explicit, &[], None, None, None)
            .unwrap();
        assert_eq!(
            store.explicit_conflict_categories().unwrap(),
            vec!["naming".to_string()]
        );
        let rows = store.resolve_active(None, 10).unwrap();
        assert!(store.delete(&rows[0].id).unwrap());
        assert!(!store.delete(&rows[0].id).unwrap());
    }
}
