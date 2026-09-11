//! T046 — Legacy v2 `graph.db` migration regression test (fixture built in-test).
//!
//! Constitution VII treats the on-disk `graph.db` format as a public surface;
//! this test pins the v2→v3 additive migration against a GENUINE v2 database:
//! the fixture DDL below is transcribed from the actual v2 `apply_schema()`
//! (git 83cc9f6~1, `NEUROCODE_SCHEMA_VERSION = 2`), not re-derived from the
//! current code, so drift in either direction (v2 shape or v3 DDL) fails here.
//!
//! Assertions:
//! 1. migration succeeds and bumps `schema_meta` to 3,
//! 2. every pre-existing v2 row (artifacts, edge, pattern, anti-pattern,
//!    domain knowledge + FTS content) survives intact,
//! 3. all five `rag_*` tables are created empty, FK cascade armed,
//! 4. a second open is idempotent: no error, no data change,
//! 5. the migrated store stays fully operational (upsert / FTS / traverse).

use std::path::PathBuf;

use joey_neurocode::graph::{DependencyGraph, EdgeKind};
use joey_neurocode::NEUROCODE_SCHEMA_VERSION;

/// Every rag_* user table the v3 migration must add.
const RAG_TABLES: [&str; 5] = [
    "rag_chunks",
    "rag_vectors",
    "rag_index_meta",
    "rag_model_artifacts",
    "rag_chunk_edges",
];

fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    n > 0
}

fn row_count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn schema_version(conn: &rusqlite::Connection) -> String {
    conn.query_row(
        "SELECT value FROM schema_meta WHERE key='neurocode_schema_version'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// Build a genuine v2-era graph.db: the v2 table set exactly as the v2
/// binary's `apply_schema()` created it (unicode61 tokenizer, `signature`
/// column from the v1→v2 ALTER, no rag_* tables), populated with
/// representative rows in every table, `schema_meta` pinned to '2'.
fn create_v2_db(path: &PathBuf) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    // --- v2 DDL, transcribed from git 83cc9f6~1 apply_schema() ---
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
            signature       TEXT,
            UNIQUE(fqcn, kind, source_path)
        );
        CREATE INDEX IF NOT EXISTS idx_artifacts_enclosing ON code_artifacts(enclosing_type);
        CREATE INDEX IF NOT EXISTS idx_artifacts_package ON code_artifacts(package);
        CREATE INDEX IF NOT EXISTS idx_artifacts_status ON code_artifacts(status);
        CREATE TABLE IF NOT EXISTS graph_edges (
            from_id    INTEGER NOT NULL REFERENCES code_artifacts(id),
            to_id      INTEGER NOT NULL REFERENCES code_artifacts(id),
            edge_kind  TEXT NOT NULL,
            PRIMARY KEY (from_id, to_id, edge_kind)
        );
        CREATE INDEX IF NOT EXISTS idx_edges_from ON graph_edges(from_id, edge_kind);
        CREATE INDEX IF NOT EXISTS idx_edges_to ON graph_edges(to_id, edge_kind);
        CREATE VIRTUAL TABLE IF NOT EXISTS code_artifacts_fts USING fts5(
            fqcn, enclosing_type, package, annotations, declared_dependencies,
            content='code_artifacts', content_rowid='id', tokenize='unicode61'
        );
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
        CREATE TABLE IF NOT EXISTS domain_knowledge (
            id           INTEGER PRIMARY KEY,
            category     TEXT NOT NULL,
            source_path  TEXT NOT NULL,
            version_tag  TEXT,
            provenance   TEXT NOT NULL,
            ingested_at  TEXT NOT NULL,
            fts_indexed  INTEGER NOT NULL DEFAULT 1
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS domain_knowledge_fts USING fts5(
            content, provenance, version_tag, tokenize='unicode61'
        );
        CREATE TABLE IF NOT EXISTS schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT
        );
        INSERT INTO schema_meta(key, value) VALUES('neurocode_schema_version', '2');
        "#,
    )
    .unwrap();

    // --- representative v2 data in every table ---
    conn.execute(
        "INSERT INTO code_artifacts (kind, fqcn, enclosing_type, package,
            implemented_interfaces, annotations, declared_dependencies, source_path,
            source_span_start, source_span_end, framework_version, status,
            indexed_at, signature)
         VALUES ('class', 'com.acme.invoice.InvoiceServiceImpl', NULL, 'com.acme.invoice',
            '[\"com.acme.invoice.InvoiceService\"]', '[\"Service\"]', '[\"com.acme.rules.TaxRule\"]',
            'src/main/java/com/acme/invoice/InvoiceServiceImpl.java', 10, 220,
            'Pega-8.5', 'Active', '2026-08-01T09:00:00Z', 'public class InvoiceServiceImpl')",
        [],
    )
    .unwrap();
    let svc_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO code_artifacts (kind, fqcn, package, source_path, indexed_at)
         VALUES ('interface', 'com.acme.invoice.InvoiceService', 'com.acme.invoice',
            'src/main/java/com/acme/invoice/InvoiceService.java', '2026-08-01T09:00:01Z')",
        [],
    )
    .unwrap();
    let iface_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO graph_edges (from_id, to_id, edge_kind) VALUES (?1, ?2, 'Implements')",
        [svc_id, iface_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO patterns (prompt_signature, generation_summary, verify_result,
            artifact_ids, tier, created_at)
         VALUES ('sig-legacy', 'legacy summary', 'pass', '[1]', 'frontier',
            '2026-08-01T09:05:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO anti_patterns (error_signature, error_output, resolution,
            artifact_ids, created_at, hit_count)
         VALUES ('NPE-legacy', 'NullPointerException at legacy', 'guard the deref',
            '[1]', '2026-08-01T09:06:00Z', 3)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO domain_knowledge (category, source_path, version_tag, provenance, ingested_at)
         VALUES ('Postmortem', 'docs/postmortem/inc-42.md', 'v1', 'docs/postmortem/inc-42.md#L1',
            '2026-08-01T09:07:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO domain_knowledge_fts (rowid, content, provenance, version_tag)
         VALUES (1, 'legacy postmortem about the invoice rounding incident',
            'docs/postmortem/inc-42.md#L1', 'v1')",
        [],
    )
    .unwrap();

    // The fixture is a v2 DB: no rag_* tables, version pinned to 2.
    for table in RAG_TABLES {
        assert!(!table_exists(&conn, table), "{} existed pre-migration", table);
    }
    assert_eq!(schema_version(&conn), "2");
    conn.close().unwrap();
}

#[test]
fn legacy_v2_graph_db_migrates_additively_with_data_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");
    create_v2_db(&db_path);

    // ---- first open: v2 → v3 migration runs inside the normal open path ----
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();

        // (1) version bumped to the current schema version (v4 as of feature 027)
        assert_eq!(schema_version(conn), NEUROCODE_SCHEMA_VERSION.to_string());

        // (2) pre-existing v2 rows intact, values byte-identical
        assert_eq!(row_count(conn, "code_artifacts"), 2);
        assert_eq!(row_count(conn, "graph_edges"), 1);
        assert_eq!(row_count(conn, "patterns"), 1);
        assert_eq!(row_count(conn, "anti_patterns"), 1);
        assert_eq!(row_count(conn, "domain_knowledge"), 1);
        let (fqcn, sig, span, status): (String, Option<String>, Option<i64>, String) = conn
            .query_row(
                "SELECT fqcn, signature, source_span_start, status FROM code_artifacts
                 WHERE kind='class'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(fqcn, "com.acme.invoice.InvoiceServiceImpl");
        assert_eq!(sig.as_deref(), Some("public class InvoiceServiceImpl"));
        assert_eq!(span, Some(10));
        assert_eq!(status, "Active");
        let edge: (i64, i64, String) = conn
            .query_row(
                "SELECT from_id, to_id, edge_kind FROM graph_edges",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(edge, (1, 2, "Implements".to_string()));
        let hits: i64 = conn
            .query_row("SELECT hit_count FROM anti_patterns", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 3);

        // (3) new rag_* tables exist, are empty, and the vector→chunk
        // FK cascade is armed (FR-005: no orphan vectors)
        for table in RAG_TABLES {
            assert!(table_exists(conn, table), "{} missing after migration", table);
            assert_eq!(row_count(conn, table), 0, "{} not empty", table);
        }
        let fk: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_list('rag_vectors')
                 WHERE \"table\"='rag_chunks' AND on_delete='CASCADE'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fk, 1, "rag_vectors must cascade-delete with rag_chunks");

        // (5) migrated store is operational through the public API
        assert_eq!(graph.artifact_count().unwrap(), 2);
        let mut hits_fts = graph.query_fts("InvoiceServiceImpl", 10).unwrap();
        assert_eq!(hits_fts.len(), 1);
        let node = hits_fts.swap_remove(0);
        assert_eq!(node.fqcn, "com.acme.invoice.InvoiceServiceImpl");
        let mut trav = graph
            .traverse_edges(1, Some(EdgeKind::Implements))
            .unwrap();
        assert_eq!(trav.len(), 1);
        assert_eq!(trav.remove(0), (2, EdgeKind::Implements));
    } // drop: close

    // ---- second open: idempotent — no error, no data change ----
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();
        // current schema version (v4 as of feature 027)
        assert_eq!(schema_version(conn), NEUROCODE_SCHEMA_VERSION.to_string());
        assert_eq!(row_count(conn, "code_artifacts"), 2);
        assert_eq!(row_count(conn, "graph_edges"), 1);
        assert_eq!(row_count(conn, "patterns"), 1);
        assert_eq!(row_count(conn, "anti_patterns"), 1);
        assert_eq!(row_count(conn, "domain_knowledge"), 1);
        for table in RAG_TABLES {
            assert!(table_exists(conn, table));
            assert_eq!(row_count(conn, table), 0);
        }
        // exactly one of each rag table (no duplicates from re-migration)
        let dup: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'rag_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dup, RAG_TABLES.len() as i64);
    }

    // ---- post-migration writes to new tables work (store usable for RAG) ----
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();
        conn.execute(
            "INSERT INTO rag_chunks (chunk_id, chunk_kind, artifact_id, source_path,
                start_line, end_line, language, symbol_name, symbol_kind,
                content_hash, updated_at)
             VALUES ('c1', 'symbol', 1, 'src/main/java/com/acme/invoice/InvoiceServiceImpl.java',
                10, 220, 'java', 'InvoiceServiceImpl', 'class', 'deadbeef',
                '2026-08-28T00:00:00Z')",
            [],
        )
        .unwrap();
        // cascade: deleting the chunk must purge its vector
        conn.execute(
            "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
             VALUES ('c1', 2, 'f32', x'0000803F00000040')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM rag_chunks WHERE chunk_id='c1'", [])
            .unwrap();
        assert_eq!(row_count(conn, "rag_vectors"), 0, "vector not cascade-purged");
        // and the v2 graph data is still there afterwards
        assert_eq!(graph.artifact_count().unwrap(), 2);
    }
}

/// A v1→v2 straggler (no `signature` column yet) also lands on v3 via the
/// same open path — the ALTER in apply_schema tolerates the column arriving
/// from either era. This mirrors the v1→v2 migration code (store.rs:76-89).
#[test]
fn legacy_v1_style_db_also_reaches_v3() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE code_artifacts (
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
            CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO schema_meta(key, value) VALUES('neurocode_schema_version', '1');
            INSERT INTO code_artifacts (kind, fqcn, package, source_path, indexed_at)
             VALUES ('class', 'com.old.Legacy', 'com.old', 'src/Old.java',
                '2026-01-01T00:00:00Z');
            "#,
        )
        .unwrap();
        conn.close().unwrap();
    }

    let graph = DependencyGraph::open(&db_path).unwrap();
    let conn = graph.store().conn();
    // current schema version (v4 as of feature 027)
    assert_eq!(schema_version(conn), NEUROCODE_SCHEMA_VERSION.to_string());
    assert_eq!(row_count(conn, "code_artifacts"), 1);
    // v1→v2 ALTER added the signature column (NULL until re-index)
    let sig: Option<String> = conn
        .query_row(
            "SELECT signature FROM code_artifacts WHERE fqcn='com.old.Legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sig, None);
    for table in RAG_TABLES {
        assert!(table_exists(conn, table));
        assert_eq!(row_count(conn, table), 0);
    }
}
