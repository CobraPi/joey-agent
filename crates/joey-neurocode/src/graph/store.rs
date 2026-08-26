//! SQLite + FTS5 structural store (`graph.db`) — T006, T014.
//!
//! Implements the on-disk schema defined in
//! contracts/graph-store-schema.md (v1). Uses the workspace's existing
//! bundled rusqlite with FTS5 (research.md §2).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use super::edge::EdgeKind;
use super::node::{ArtifactKind, ArtifactStatus, CodeArtifactNode};
use super::NodeId;

/// The on-disk schema version for the NeuroCode graph store.
pub const SCHEMA_VERSION: u32 = crate::NEUROCODE_SCHEMA_VERSION;

/// The structural knowledge graph store backed by SQLite+FTS5.
pub struct GraphStore {
    conn: Connection,
}

impl GraphStore {
    /// Open (or create) a graph store at the given path, applying the schema
    /// if it doesn't exist. Honours `JOEY_HOME` via `process_joey_home()`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        // A second process touching the same graph.db (CLI index while an
        // agent session is open) must wait briefly instead of failing the
        // whole ingest with SQLITE_BUSY on the first lock contention.
        conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        Self::apply_schema(&conn)?;
        Ok(Self { conn })
    }

    /// Open an in-memory store (testing).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::apply_schema(&conn)?;
        Ok(Self { conn })
    }

    /// Apply the v1 schema if not already present (idempotent).
    fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
        // code_artifacts
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS code_artifacts (
                id              INTEGER PRIMARY KEY,
                kind            TEXT NOT NULL,
                fqcn            TEXT NOT NULL,
                enclosing_type  TEXT,
                package         TEXT NOT NULL,
                implemented_interfaces TEXT,
                annotations     TEXT,
                declared_dependencies TEXT,
                source_path     TEXT NOT NULL,
                source_span_start INTEGER,
                source_span_end   INTEGER,
                pega_metadata   TEXT,
                framework_version TEXT,
                status          TEXT NOT NULL DEFAULT 'Active',
                indexed_at      TEXT NOT NULL,
                UNIQUE(fqcn, kind, source_path)
            );
            CREATE INDEX IF NOT EXISTS idx_artifacts_enclosing ON code_artifacts(enclosing_type);
            CREATE INDEX IF NOT EXISTS idx_artifacts_package ON code_artifacts(package);
            CREATE INDEX IF NOT EXISTS idx_artifacts_status ON code_artifacts(status);
            "#,
        )?;
        // v1 → v2 additive migration: declaration signatures for methods and
        // fields (context assembly renders member rosters without file reads).
        // Existing rows keep NULL signatures until the next re-index.
        conn.execute_batch(
            "ALTER TABLE code_artifacts ADD COLUMN signature TEXT",
        )
        .or_else(|e| {
            // duplicate column name == already migrated; anything else is real.
            if e.to_string().contains("duplicate column") {
                Ok(())
            } else {
                Err(e)
            }
        })?;

        // graph_edges
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS graph_edges (
                from_id    INTEGER NOT NULL REFERENCES code_artifacts(id),
                to_id      INTEGER NOT NULL REFERENCES code_artifacts(id),
                edge_kind  TEXT NOT NULL,
                PRIMARY KEY (from_id, to_id, edge_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_edges_from ON graph_edges(from_id, edge_kind);
            CREATE INDEX IF NOT EXISTS idx_edges_to ON graph_edges(to_id, edge_kind);
            "#,
        )?;

        // FTS5 virtual table over artifacts (external-content pattern)
        conn.execute_batch(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS code_artifacts_fts USING fts5(
                fqcn,
                enclosing_type,
                package,
                annotations,
                declared_dependencies,
                content='code_artifacts',
                content_rowid='id',
                tokenize='unicode61'
            );
            "#,
        )?;
        // Sync triggers (standard external-content pattern)
        conn.execute_batch(
            r#"
            CREATE TRIGGER IF NOT EXISTS code_artifacts_ai AFTER INSERT ON code_artifacts BEGIN
                INSERT INTO code_artifacts_fts(rowid, fqcn, enclosing_type, package, annotations, declared_dependencies)
                VALUES (new.id, new.fqcn, new.enclosing_type, new.package, new.annotations, new.declared_dependencies);
            END;
            CREATE TRIGGER IF NOT EXISTS code_artifacts_ad AFTER DELETE ON code_artifacts BEGIN
                INSERT INTO code_artifacts_fts(code_artifacts_fts, rowid, fqcn, enclosing_type, package, annotations, declared_dependencies)
                VALUES ('delete', old.id, old.fqcn, old.enclosing_type, old.package, old.annotations, old.declared_dependencies);
            END;
            CREATE TRIGGER IF NOT EXISTS code_artifacts_au AFTER UPDATE ON code_artifacts BEGIN
                INSERT INTO code_artifacts_fts(code_artifacts_fts, rowid, fqcn, enclosing_type, package, annotations, declared_dependencies)
                VALUES ('delete', old.id, old.fqcn, old.enclosing_type, old.package, old.annotations, old.declared_dependencies);
                INSERT INTO code_artifacts_fts(rowid, fqcn, enclosing_type, package, annotations, declared_dependencies)
                VALUES (new.id, new.fqcn, new.enclosing_type, new.package, new.annotations, new.declared_dependencies);
            END;
            "#,
        )?;

        // patterns (LearnedPattern)
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS patterns (
                id                INTEGER PRIMARY KEY,
                prompt_signature  TEXT NOT NULL,
                generation_summary TEXT NOT NULL,
                verify_result     TEXT NOT NULL,
                artifact_ids      TEXT NOT NULL,
                tier              TEXT NOT NULL,
                created_at        TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_patterns_signature ON patterns(prompt_signature);
            "#,
        )?;

        // anti_patterns (LearnedAntiPattern)
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS anti_patterns (
                id              INTEGER PRIMARY KEY,
                error_signature TEXT NOT NULL,
                error_output    TEXT NOT NULL,
                resolution      TEXT NOT NULL,
                artifact_ids    TEXT NOT NULL,
                created_at      TEXT NOT NULL,
                hit_count       INTEGER NOT NULL DEFAULT 0,
                status          TEXT NOT NULL DEFAULT 'Active'
            );
            CREATE INDEX IF NOT EXISTS idx_anti_artifacts ON anti_patterns(artifact_ids);
            CREATE INDEX IF NOT EXISTS idx_anti_signature ON anti_patterns(error_signature);
            "#,
        )?;

        // domain_knowledge
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS domain_knowledge (
                id           INTEGER PRIMARY KEY,
                category     TEXT NOT NULL,
                source_path  TEXT NOT NULL,
                version_tag  TEXT,
                provenance   TEXT NOT NULL,
                ingested_at  TEXT NOT NULL,
                fts_indexed  INTEGER NOT NULL DEFAULT 1
            );
            "#,
        )?;

        // domain_knowledge_fts (standalone FTS5 — content is stored inline)
        conn.execute_batch(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS domain_knowledge_fts USING fts5(
                content,
                provenance,
                version_tag,
                tokenize='unicode61'
            );
            "#,
        )?;

        // schema_meta
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_meta (
                key   TEXT PRIMARY KEY,
                value TEXT
            );
            "#,
        )?;
        // Record the schema version (idempotent upsert).
        conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES('neurocode_schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;

        Ok(())
    }

    /// Upsert a code artifact node. Returns the rowid (inserted or existing).
    pub fn upsert_node(&self, node: &CodeArtifactNode) -> rusqlite::Result<NodeId> {
        let impls = serde_json::to_string(&node.implemented_interfaces).unwrap_or_default();
        let annots = serde_json::to_string(&node.annotations).unwrap_or_default();
        let deps = serde_json::to_string(&node.declared_dependencies).unwrap_or_default();
        let pega = node
            .pega_metadata
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());
        let span_start = node.source_span.map(|(s, _)| s as i64);
        let span_end = node.source_span.map(|(_, e)| e as i64);

        self.conn.execute(
            "INSERT INTO code_artifacts
                (id, kind, fqcn, enclosing_type, package, implemented_interfaces,
                 annotations, declared_dependencies, source_path,
                 source_span_start, source_span_end, pega_metadata, framework_version,
                 status, indexed_at, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(fqcn, kind, source_path) DO UPDATE SET
                enclosing_type=excluded.enclosing_type,
                implemented_interfaces=excluded.implemented_interfaces,
                annotations=excluded.annotations,
                declared_dependencies=excluded.declared_dependencies,
                source_span_start=excluded.source_span_start,
                source_span_end=excluded.source_span_end,
                pega_metadata=excluded.pega_metadata,
                framework_version=excluded.framework_version,
                status=excluded.status,
                indexed_at=excluded.indexed_at,
                signature=excluded.signature",
            params![
                if node.id == 0 { None } else { Some(node.id as i64) },
                node.kind.as_str(),
                &node.fqcn,
                &node.enclosing_type,
                &node.package,
                &impls,
                &annots,
                &deps,
                &node.source_path,
                span_start,
                span_end,
                pega.as_deref(),
                node.framework_version.as_deref(),
                node.status.as_str(),
                &node.indexed_at,
                node.signature.as_deref(),
            ],
        )?;
        // NEVER trust last_insert_rowid() here: on the ON CONFLICT DO UPDATE
        // path SQLite does NOT refresh it, so a re-index over existing nodes
        // returns the id of the last FRESH insert — silently wiring edges and
        // memberships onto the wrong node (graph corruption). The unique key
        // (fqcn, kind, source_path) pins the actual row.
        let id: i64 = self.conn.query_row(
            "SELECT id FROM code_artifacts WHERE fqcn=?1 AND kind=?2 AND source_path=?3",
            params![&node.fqcn, node.kind.as_str(), &node.source_path],
            |row| row.get(0),
        )?;
        Ok(id as NodeId)
    }

    /// Upsert a typed edge (idempotent).
    pub fn upsert_edge(&self, from: NodeId, to: NodeId, kind: EdgeKind) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO graph_edges(from_id, to_id, edge_kind) VALUES(?1, ?2, ?3)
             ON CONFLICT(from_id, to_id, edge_kind) DO NOTHING",
            params![from as i64, to as i64, kind.as_str()],
        )?;
        Ok(())
    }

    /// All DISTINCT source_path values currently marked Active (tombstone pass).
    pub fn active_source_paths(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT source_path FROM code_artifacts WHERE status='Active'")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Set status for every node with the given source_path. Returns rows changed.
    pub fn set_status_for_path(&self, source_path: &str, status: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            "UPDATE code_artifacts SET status=?1 WHERE source_path=?2 AND status != ?1",
            params![status, source_path],
        )
    }

    /// Look up a node by FQCN + kind + source_path (the unique key).
    pub fn find_node(
        &self,
        fqcn: &str,
        kind: &ArtifactKind,
        source_path: &str,
    ) -> rusqlite::Result<Option<CodeArtifactNode>> {
        self.conn
            .query_row(
                "SELECT * FROM code_artifacts WHERE fqcn=?1 AND kind=?2 AND source_path=?3",
                params![fqcn, kind.as_str(), source_path],
                row_to_node,
            )
            .optional()
    }

    /// Look up a node by its rowid.
    pub fn get_node(&self, id: NodeId) -> rusqlite::Result<Option<CodeArtifactNode>> {
        self.conn
            .query_row(
                "SELECT * FROM code_artifacts WHERE id=?1",
                params![id as i64],
                row_to_node,
            )
            .optional()
    }

    /// FTS5 search over artifact symbols. Returns ranked matches.
    pub fn query_fts(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<CodeArtifactNode>> {
        // Escape: wrap each token in double quotes for FTS5 safety.
        let fts_query: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT ca.* FROM code_artifacts_fts fts
             JOIN code_artifacts ca ON ca.id = fts.rowid
             WHERE code_artifacts_fts MATCH ?1
             ORDER BY rank
             LIMIT {}",
            limit
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![&fts_query], row_to_node)?;
        rows.collect()
    }

    /// Traverse edges from a node, optionally filtered by kind.
    /// Returns `(to_id, edge_kind)` pairs.
    pub fn traverse_from(
        &self,
        from: NodeId,
        kind_filter: Option<EdgeKind>,
    ) -> rusqlite::Result<Vec<(NodeId, EdgeKind)>> {
        let mut rows = match kind_filter {
            Some(k) => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT to_id, edge_kind FROM graph_edges WHERE from_id=?1 AND edge_kind=?2")?;
                let r = stmt.query_map(params![from as i64, k.as_str()], |row| {
                    let to: i64 = row.get(0)?;
                    let kind_str: String = row.get(1)?;
                    Ok((to as NodeId, EdgeKind::parse(&kind_str).unwrap_or(EdgeKind::Injects)))
                })?;
                r.filter_map(Result::ok).collect::<Vec<_>>()
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT to_id, edge_kind FROM graph_edges WHERE from_id=?1")?;
                let r = stmt.query_map(params![from as i64], |row| {
                    let to: i64 = row.get(0)?;
                    let kind_str: String = row.get(1)?;
                    Ok((to as NodeId, EdgeKind::parse(&kind_str).unwrap_or(EdgeKind::Injects)))
                })?;
                r.filter_map(Result::ok).collect::<Vec<_>>()
            }
        };
        rows.sort_by_key(|(_, k)| k.as_str());
        Ok(rows)
    }

    /// Traverse edges TO a node, optionally filtered by kind.
    /// Returns `(from_id, edge_kind)` pairs.
    pub fn traverse_to(
        &self,
        to: NodeId,
        kind_filter: Option<EdgeKind>,
    ) -> rusqlite::Result<Vec<(NodeId, EdgeKind)>> {
        let rows = match kind_filter {
            Some(k) => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT from_id, edge_kind FROM graph_edges WHERE to_id=?1 AND edge_kind=?2")?;
                let r = stmt.query_map(params![to as i64, k.as_str()], |row| {
                    let from: i64 = row.get(0)?;
                    let kind_str: String = row.get(1)?;
                    Ok((from as NodeId, EdgeKind::parse(&kind_str).unwrap_or(EdgeKind::Injects)))
                })?;
                r.filter_map(Result::ok).collect::<Vec<_>>()
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT from_id, edge_kind FROM graph_edges WHERE to_id=?1")?;
                let r = stmt.query_map(params![to as i64], |row| {
                    let from: i64 = row.get(0)?;
                    let kind_str: String = row.get(1)?;
                    Ok((from as NodeId, EdgeKind::parse(&kind_str).unwrap_or(EdgeKind::Injects)))
                })?;
                r.filter_map(Result::ok).collect::<Vec<_>>()
            }
        };
        Ok(rows)
    }

    /// Count active artifacts.
    pub fn artifact_count(&self) -> rusqlite::Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM code_artifacts WHERE status='Active'", [], |row| {
                row.get::<_, i64>(0).map(|n| n as usize)
            })
    }

    /// Look up the type-level nodes declared in a source file. Methods and
    /// fields are excluded — the caller wants file→type seeds. Exact path
    /// first; a trailing-component match (user wrote `src/Foo.java`, stored
    /// path is `src/main/java/src/Foo.java`) fills in when exact misses.
    pub fn nodes_by_source_path(&self, path: &str) -> rusqlite::Result<Vec<CodeArtifactNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM code_artifacts
             WHERE source_path=?1 AND kind IN ('Class','Interface','Enum','PegaRule')
               AND status='Active",
        )?;
        let exact: Vec<CodeArtifactNode> =
            stmt.query_map(params![path], row_to_node)?.collect::<Result<_, _>>()?;
        if !exact.is_empty() {
            return Ok(exact);
        }
        // Suffix match on path components, longest-path first for stability.
        let like = format!("%{}", path.trim_start_matches("./"));
        let mut stmt = self.conn.prepare(
            "SELECT * FROM code_artifacts
             WHERE source_path LIKE ?1
               AND kind IN ('Class','Interface','Enum','PegaRule')
               AND status='Active'
             ORDER BY LENGTH(source_path) ASC LIMIT 20",
        )?;
        let fuzzy: Vec<CodeArtifactNode> =
            stmt.query_map(params![like], row_to_node)?.collect::<Result<_, _>>()?;
        Ok(fuzzy)
    }

    /// The method/field member nodes whose enclosing type is `enclosing`
    /// (simple name), active only. Used to render a type's member roster in
    /// the assembled context without loading whole files.
    pub fn members_of_enclosing(
        &self,
        enclosing: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<CodeArtifactNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM code_artifacts
             WHERE enclosing_type=?1 AND status='Active'
             ORDER BY kind DESC, fqcn ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![enclosing, limit as i64], row_to_node)?;
        rows.collect()
    }

    /// The number of distinct incoming edges (fan-in) for a node — how many
    /// other artifacts depend on it. Hubs have high fan-in; expansion and
    /// formatting use this to rank and to warn before wide edits.
    /// MemberOf edges are structural (type → member), NOT dependencies —
    /// counting them made every class with ≥5 members carry a false
    /// "wide blast radius" warning.
    pub fn dependents_count(&self, id: NodeId) -> rusqlite::Result<usize> {
        self.conn.query_row(
            "SELECT COUNT(DISTINCT from_id) FROM graph_edges WHERE to_id=?1 AND edge_kind != 'MemberOf'",
            params![id as i64],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )
    }

    /// Access the raw connection (for advanced queries by other modules).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Record a learned pattern.
    pub fn record_pattern(
        &self,
        prompt_signature: &str,
        generation_summary: &str,
        verify_result: &str,
        artifact_ids: &[NodeId],
        tier: &str,
    ) -> rusqlite::Result<()> {
        let ids_json = serde_json::to_string(artifact_ids).unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO patterns(prompt_signature, generation_summary, verify_result, artifact_ids, tier, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![prompt_signature, generation_summary, verify_result, &ids_json, tier, &now],
        )?;
        Ok(())
    }

    /// Record a learned anti-pattern.
    pub fn record_anti_pattern(
        &self,
        error_signature: &str,
        error_output: &str,
        resolution: &str,
        artifact_ids: &[NodeId],
    ) -> rusqlite::Result<()> {
        let ids_json = serde_json::to_string(artifact_ids).unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO anti_patterns(error_signature, error_output, resolution, artifact_ids, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![error_signature, error_output, resolution, &ids_json, &now],
        )?;
        Ok(())
    }

    /// Count active anti-patterns.
    pub fn anti_pattern_count(&self) -> rusqlite::Result<usize> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM anti_patterns WHERE status='Active'",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )
    }

    /// Active anti-patterns attached to any of the given artifact ids
    /// (T062, FR-011). Returns `(id, error_signature, resolution)` tuples.
    ///
    /// `artifact_ids` is stored as a JSON array, so we select all Active
    /// rows and filter in Rust by JSON-parse + set intersection.
    pub fn anti_patterns_for_artifacts(
        &self,
        artifact_ids: &[NodeId],
    ) -> rusqlite::Result<Vec<(i64, String, String)>> {
        let wanted: std::collections::HashSet<NodeId> =
            artifact_ids.iter().copied().collect();
        let mut stmt = self.conn.prepare(
            "SELECT id, error_signature, resolution, artifact_ids
             FROM anti_patterns WHERE status='Active'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, sig, resolution, ids_json) = row?;
            let attached: Vec<NodeId> = serde_json::from_str(&ids_json).unwrap_or_default();
            if attached.iter().any(|a| wanted.contains(a)) {
                out.push((id, sig, resolution));
            }
        }
        Ok(out)
    }

    /// Increment the hit_count of an anti-pattern (T062, FR-011) — records
    /// that the warning was surfaced for the area it is attached to.
    pub fn bump_anti_pattern_hit(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE anti_patterns SET hit_count = hit_count + 1 WHERE id=?1",
            params![id],
        )?;
        Ok(())
    }

    /// Read an anti-pattern's hit_count (test assertions, T062).
    pub fn get_anti_pattern_hit_count(&self, id: i64) -> rusqlite::Result<u32> {
        self.conn.query_row(
            "SELECT hit_count FROM anti_patterns WHERE id=?1",
            params![id],
            |row| row.get::<_, i64>(0).map(|n| n as u32),
        )
    }

    /// Count learned patterns.
    pub fn pattern_count(&self) -> rusqlite::Result<usize> {
        self.conn.query_row("SELECT COUNT(*) FROM patterns", [], |row| {
            row.get::<_, i64>(0).map(|n| n as usize)
        })
    }

    // ── Domain knowledge ──────────────────────────────────────────────────

    /// Record a domain-knowledge source. Returns its rowid.
    pub fn upsert_domain_knowledge(
        &self,
        category: &str,
        source_path: &str,
        version_tag: Option<&str>,
        provenance: &str,
    ) -> rusqlite::Result<u64> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO domain_knowledge(category, source_path, version_tag, provenance, ingested_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![category, source_path, version_tag, provenance, &now],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// Add content to the domain-knowledge FTS index.
    pub fn index_domain_content(
        &self,
        content: &str,
        provenance: &str,
        version_tag: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO domain_knowledge_fts(content, provenance, version_tag) VALUES(?1, ?2, ?3)",
            params![content, provenance, version_tag],
        )?;
        Ok(())
    }

    /// FTS5 search over domain knowledge.
    pub fn query_domain_fts(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<DomainFtsRow>> {
        let fts_query: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT rowid, content, provenance, version_tag FROM domain_knowledge_fts
             WHERE domain_knowledge_fts MATCH ?1
             ORDER BY rank LIMIT {}",
            limit
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![&fts_query], |row| {
            Ok(DomainFtsRow {
                id: row.get(0)?,
                content: row.get(1)?,
                provenance: row.get(2)?,
                version_tag: row.get::<_, Option<String>>(3)?,
            })
        })?;
        rows.collect()
    }

    /// List all domain-knowledge sources.
    pub fn list_domain_sources(&self) -> rusqlite::Result<Vec<DomainSourceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, category, source_path, version_tag, provenance, ingested_at
             FROM domain_knowledge ORDER BY ingested_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DomainSourceRow {
                id: row.get(0)?,
                category: row.get(1)?,
                source_path: row.get(2)?,
                version_tag: row.get(3)?,
                provenance: row.get(4)?,
                ingested_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// FTS rowids for a category (T063 additive). Because NeuroCode's own
    /// ingestion path (`memory::domain::ingest_source`) registers the
    /// `domain_knowledge` row immediately before indexing the content, the
    /// FTS rowid is aligned with the registry id, so these rowids are the
    /// registry ids of that category's sources.
    ///
    /// Returns an empty vec for unknown categories.
    pub fn fts_domain_ids_by_category(&self, category: &str) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM domain_knowledge WHERE category=?1",
        )?;
        let rows = stmt.query_map(params![category], |row| row.get::<_, i64>(0))?;
        rows.collect()
    }

    /// Remove the indexed FTS content for a domain source (T064 additive).
    /// Best-effort: returns Ok(false) when there was no indexed content for
    /// `id` (never an error) — removal of the registry row must succeed even
    /// for content ingested before rowid alignment existed.
    pub fn remove_fts_domain_content(&self, id: u64) -> rusqlite::Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM domain_knowledge_fts WHERE rowid=?1",
            params![id as i64],
        )?;
        Ok(n > 0)
    }

    /// Remove a domain-knowledge source by id (T064): removes both the
    /// registry row and any indexed FTS content, so a removed source's
    /// content is no longer retrievable.
    pub fn remove_domain_knowledge(&self, id: u64) -> rusqlite::Result<bool> {
        let _ = self.remove_fts_domain_content(id);
        let n = self.conn.execute(
            "DELETE FROM domain_knowledge WHERE id=?1",
            params![id as i64],
        )?;
        Ok(n > 0)
    }
}

/// A row from the domain_knowledge_fts search.
#[derive(Debug, Clone)]
pub struct DomainFtsRow {
    /// The FTS rowid — aligned with the `domain_knowledge` registry id for
    /// sources ingested via `memory::domain::ingest_source` (T063).
    pub id: i64,
    pub content: String,
    pub provenance: String,
    pub version_tag: Option<String>,
}

/// A row from the domain_knowledge table.
#[derive(Debug, Clone)]
pub struct DomainSourceRow {
    pub id: i64,
    pub category: String,
    pub source_path: String,
    pub version_tag: Option<String>,
    pub provenance: String,
    pub ingested_at: String,
}

/// Map a rusqlite Row to a CodeArtifactNode.
fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<CodeArtifactNode> {
    let id: i64 = row.get("id")?;
    let kind_str: String = row.get("kind")?;
    let fqcn: String = row.get("fqcn")?;
    let enclosing_type: Option<String> = row.get("enclosing_type")?;
    let package: String = row.get("package")?;
    let impls_json: Option<String> = row.get("implemented_interfaces")?;
    let annots_json: Option<String> = row.get("annotations")?;
    let deps_json: Option<String> = row.get("declared_dependencies")?;
    let source_path: String = row.get("source_path")?;
    let span_start: Option<i64> = row.get("source_span_start")?;
    let span_end: Option<i64> = row.get("source_span_end")?;
    let signature: Option<String> = row.get("signature")?;
    let pega_json: Option<String> = row.get("pega_metadata")?;
    let framework_version: Option<String> = row.get("framework_version")?;
    let status_str: String = row.get("status")?;
    let indexed_at: String = row.get("indexed_at")?;

    let kind = ArtifactKind::parse(&kind_str).unwrap_or(ArtifactKind::Class);
    let status = ArtifactStatus::parse(&status_str).unwrap_or_default();
    let implemented_interfaces: Vec<String> = impls_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let annotations: Vec<String> = annots_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let declared_dependencies: Vec<String> = deps_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let pega_metadata = pega_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let source_span = match (span_start, span_end) {
        (Some(s), Some(e)) => Some((s as u32, e as u32)),
        _ => None,
    };

    Ok(CodeArtifactNode {
        id: id as NodeId,
        kind,
        fqcn,
        enclosing_type,
        package,
        implemented_interfaces,
        annotations,
        declared_dependencies,
        source_path,
        source_span,
        signature,
        pega_metadata,
        framework_version,
        status,
        indexed_at,
    })
}

/// Resolve the per-project graph.db path: hash of repo-root path →
/// `~/.joey/neurocode/projects/<hash>/graph.db` (T014).
///
/// Honours `JOEY_HOME` via `process_joey_home()`.
pub fn project_graph_db_path(project_root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let hash = hasher.finalize();
    let hash_hex: String = hash.iter().take(16).map(|b| format!("{:02x}", b)).collect();

    let home = joey_core::constants::process_joey_home();
    home.join("neurocode").join("projects").join(&hash_hex).join("graph.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_creates_tables() {
        let store = GraphStore::open_in_memory().unwrap();
        assert_eq!(store.artifact_count().unwrap(), 0);
        assert_eq!(store.pattern_count().unwrap(), 0);
        assert_eq!(store.anti_pattern_count().unwrap(), 0);
    }

    #[test]
    fn upsert_and_find_node() {
        let store = GraphStore::open_in_memory().unwrap();
        let mut node = CodeArtifactNode::new(
            ArtifactKind::Class,
            "com.example.UserService".into(),
            "com.example".into(),
            "src/UserService.java".into(),
        );
        node.implemented_interfaces = vec!["IUserService".into()];
        node.annotations = vec!["Service".into()];
        node.declared_dependencies = vec!["UserRepository".into()];

        let id = store.upsert_node(&node).unwrap();
        assert!(id > 0);

        let found = store
            .find_node("com.example.UserService", &ArtifactKind::Class, "src/UserService.java")
            .unwrap()
            .expect("node should exist");
        assert_eq!(found.fqcn, "com.example.UserService");
        assert_eq!(found.implemented_interfaces, vec!["IUserService"]);
        assert_eq!(found.annotations, vec!["Service"]);
        assert_eq!(found.declared_dependencies, vec!["UserRepository"]);
    }

    #[test]
    fn upsert_is_idempotent() {
        let store = GraphStore::open_in_memory().unwrap();
        let node = CodeArtifactNode::new(
            ArtifactKind::Interface,
            "com.example.IFoo".into(),
            "com.example".into(),
            "src/IFoo.java".into(),
        );
        let id1 = store.upsert_node(&node).unwrap();
        let id2 = store.upsert_node(&node).unwrap();
        assert_eq!(id1, id2, "re-upsert of same node should return same id");
        assert_eq!(store.artifact_count().unwrap(), 1);
    }

    #[test]
    fn upsert_and_traverse_edge() {
        let store = GraphStore::open_in_memory().unwrap();
        let a = CodeArtifactNode::new(
            ArtifactKind::Class,
            "com.example.A".into(),
            "com.example".into(),
            "src/A.java".into(),
        );
        let b = CodeArtifactNode::new(
            ArtifactKind::Interface,
            "com.example.B".into(),
            "com.example".into(),
            "src/B.java".into(),
        );
        let id_a = store.upsert_node(&a).unwrap();
        let id_b = store.upsert_node(&b).unwrap();
        store.upsert_edge(id_a, id_b, EdgeKind::Implements).unwrap();

        let edges = store.traverse_from(id_a, Some(EdgeKind::Implements)).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, id_b);
        assert_eq!(edges[0].1, EdgeKind::Implements);
    }

    #[test]
    fn fts_search_finds_node() {
        let store = GraphStore::open_in_memory().unwrap();
        let node = CodeArtifactNode::new(
            ArtifactKind::Class,
            "com.example.UserRepository".into(),
            "com.example".into(),
            "src/UserRepository.java".into(),
        );
        store.upsert_node(&node).unwrap();

        let results = store.query_fts("UserRepository", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].fqcn, "com.example.UserRepository");
    }

    #[test]
    fn record_pattern_and_count() {
        let store = GraphStore::open_in_memory().unwrap();
        store
            .record_pattern("sig", "summary", "passed", &[1, 2], "economical")
            .unwrap();
        assert_eq!(store.pattern_count().unwrap(), 1);
    }

    #[test]
    fn record_anti_pattern_and_count() {
        let store = GraphStore::open_in_memory().unwrap();
        store
            .record_anti_pattern("BeanCreation", "error", "fix", &[1])
            .unwrap();
        assert_eq!(store.anti_pattern_count().unwrap(), 1);
    }

    #[test]
    fn domain_knowledge_lifecycle() {
        let store = GraphStore::open_in_memory().unwrap();
        let id = store
            .upsert_domain_knowledge("FrameworkDocs", "docs/spring.md", Some("3.2"), "Spring Docs")
            .unwrap();
        store
            .index_domain_content("Spring Boot configuration", "Spring Docs", Some("3.2"))
            .unwrap();
        let sources = store.list_domain_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, id as i64);

        let results = store.query_domain_fts("Spring", 10).unwrap();
        assert!(!results.is_empty());

        assert!(store.remove_domain_knowledge(id).unwrap());
        assert!(store.list_domain_sources().unwrap().is_empty());
        // T063/T064: the indexed FTS content is removed along with the row,
        // so the source's content is no longer retrievable.
        assert!(store.query_domain_fts("Spring", 10).unwrap().is_empty());
    }

    #[test]
    fn domain_fts_ids_by_category_and_removal() {
        // T063/T064: registry-row removal hides the source's indexed content.
        let store = GraphStore::open_in_memory().unwrap();
        let id = store
            .upsert_domain_knowledge("FrameworkDocs", "docs/a.md", Some("3.2"), "Docs A")
            .unwrap();
        store
            .index_domain_content("alpha content", "Docs A", Some("3.2"))
            .unwrap();
        assert!(store
            .fts_domain_ids_by_category("FrameworkDocs")
            .unwrap()
            .contains(&(id as i64)));
        assert!(store.fts_domain_ids_by_category("Postmortem").unwrap().is_empty());

        assert!(!store.query_domain_fts("alpha", 10).unwrap().is_empty());
        assert!(store.remove_fts_domain_content(id).unwrap());
        assert!(store.query_domain_fts("alpha", 10).unwrap().is_empty());
        // Registry row still present until remove_domain_knowledge is called.
        assert_eq!(store.list_domain_sources().unwrap().len(), 1);
    }

    #[test]
    fn schema_version_recorded() {
        let store = GraphStore::open_in_memory().unwrap();
        let version: String = store
            .conn()
            .query_row(
                "SELECT value FROM schema_meta WHERE key='neurocode_schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }
}
