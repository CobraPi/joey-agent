//! T008 — Additive idempotent schema v3→v4 migration (memory_* tables).
//!
//! Mirrors `rag_schema_migration.rs` (the v2→v3 test) one version up:
//!
//! 1. **Migration-from-v3**: hand-craft a v3 DB (the full current store
//!    schema MINUS the v4 memory batch, `schema_meta` pinned to '3'),
//!    populate existing tables, open via the real store open path, assert
//!    existing rows intact AND the memory tables present and empty AND
//!    version bumped to 4.
//! 2. **Idempotent-reopen**: run the migration twice; assert no error and
//!    no duplicate rows/tables.
//! 3. **Memory vector BLOB round-trip**: byte-exact put/get through
//!    `EpisodeStore`/`PreferenceStore` on the same file-backed DB.

use std::path::PathBuf;

use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::memory::episodes::{
    EpisodeStore, MemoryItemKind, MemoryQuantization, MemoryVectorRecord,
};
use joey_neurocode::memory::preferences::PreferenceStore;
use joey_neurocode::NEUROCODE_SCHEMA_VERSION;

/// Names of every memory_* user table the v4 migration must create.
const MEMORY_TABLES: [&str; 3] = [
    "memory_episodes",
    "memory_preferences",
    "memory_vectors",
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

/// Hand-craft a pre-v4 (v3) database: the full v3 table set (everything in
/// the current `graph/store.rs` apply_schema EXCEPT the v4 memory_* batch),
/// populated with existing rows, and schema_meta pinning version 3. This is
/// exactly the on-disk shape a v3 binary leaves behind.
fn create_v3_db(path: &PathBuf) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
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
        CREATE TABLE IF NOT EXISTS rag_chunks (
            chunk_id     TEXT PRIMARY KEY,
            chunk_kind   TEXT NOT NULL CHECK (chunk_kind IN ('symbol','fallback')),
            artifact_id  INTEGER REFERENCES code_artifacts(id) ON DELETE CASCADE,
            source_path  TEXT NOT NULL,
            start_line   INTEGER NOT NULL,
            end_line     INTEGER NOT NULL,
            language     TEXT,
            symbol_name  TEXT,
            symbol_kind  TEXT,
            content_hash TEXT NOT NULL,
            embed_model  TEXT,
            embed_dim    INTEGER,
            updated_at   TEXT
        );
        CREATE INDEX IF NOT EXISTS rag_chunks_source_path ON rag_chunks(source_path);
        CREATE INDEX IF NOT EXISTS rag_chunks_hash        ON rag_chunks(content_hash);
        CREATE INDEX IF NOT EXISTS rag_chunks_artifact    ON rag_chunks(artifact_id);
        CREATE TABLE IF NOT EXISTS rag_vectors (
            chunk_id     TEXT PRIMARY KEY
                         REFERENCES rag_chunks(chunk_id) ON DELETE CASCADE,
            dim          INTEGER NOT NULL,
            quantization TEXT NOT NULL CHECK (quantization IN ('f32','int8')),
            vector       BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS rag_index_meta (
            id                  INTEGER PRIMARY KEY CHECK (id = 1),
            schema_version      INTEGER NOT NULL,
            embed_profile       TEXT,
            embed_model         TEXT,
            embed_dim           INTEGER,
            pooling             TEXT,
            prefix_query        TEXT,
            prefix_document     TEXT,
            quantization_policy TEXT,
            chunk_count         INTEGER NOT NULL DEFAULT 0,
            last_refresh_at     TEXT,
            refresh_state       TEXT NOT NULL DEFAULT 'idle'
                                CHECK (refresh_state IN ('idle','refreshing')),
            created_at          TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS rag_model_artifacts (
            profile          TEXT PRIMARY KEY,
            model_sha256     TEXT NOT NULL,
            tokenizer_sha256 TEXT NOT NULL,
            model_size_bytes INTEGER NOT NULL,
            fetched_at       TEXT NOT NULL,
            mirror_url_used  TEXT
        );
        CREATE TABLE IF NOT EXISTS rag_chunk_edges (
            from_chunk_id TEXT NOT NULL,
            to_chunk_id   TEXT NOT NULL,
            edge_kind     TEXT NOT NULL,
            PRIMARY KEY (from_chunk_id, to_chunk_id, edge_kind)
        );
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
            content,
            provenance,
            version_tag,
            tokenize='unicode61'
        );
        CREATE TABLE IF NOT EXISTS schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT
        );
        INSERT INTO schema_meta(key, value) VALUES('neurocode_schema_version', '3');
        "#,
    )
    .unwrap();
    // Existing v3 data: two artifacts, one edge, one pattern, one
    // anti-pattern, one rag chunk + vector.
    conn.execute(
        "INSERT INTO code_artifacts (kind, fqcn, package, source_path, indexed_at, signature)
         VALUES ('class', 'com.example.Foo', 'com.example', 'src/Foo.java',
                 '2026-09-09T10:00:00Z', 'public class Foo')",
        [],
    )
    .unwrap();
    let foo_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO code_artifacts (kind, fqcn, package, source_path, indexed_at)
         VALUES ('interface', 'com.example.IBar', 'com.example', 'src/IBar.java',
                 '2026-09-09T10:00:01Z')",
        [],
    )
    .unwrap();
    let bar_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO graph_edges (from_id, to_id, edge_kind) VALUES (?1, ?2, 'Implements')",
        [foo_id, bar_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO patterns (prompt_signature, generation_summary, verify_result,
                               artifact_ids, tier, created_at)
         VALUES ('sig', 'summary', 'pass', '[1]', 'frontier', '2026-09-09T10:00:02Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO anti_patterns (error_signature, error_output, resolution,
                                    artifact_ids, created_at)
         VALUES ('err', 'output', 'fix', '[1]', '2026-09-09T10:00:03Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line,
            end_line, language, content_hash, updated_at)
         VALUES ('c1', 'fallback', 'a.py', 1, 9, 'python', 'h1',
                 '2026-09-09T11:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
         VALUES ('c1', 2, 'f32', x'0000803F00000040')",
        [],
    )
    .unwrap();
    conn.close().unwrap();
}

// ---------------------------------------------------------------------------
// Migration-from-v3 — v3 opens unchanged, gains empty memory tables,
// existing rows intact, version bumped to 4.
// ---------------------------------------------------------------------------

#[test]
fn v3_db_migrates_additively_with_data_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");
    create_v3_db(&db_path);

    // The v3 fixture must NOT contain any memory_* table before the open.
    {
        let raw = rusqlite::Connection::open(&db_path).unwrap();
        for table in MEMORY_TABLES {
            assert!(!table_exists(&raw, table), "{} existed pre-migration", table);
        }
    }

    // v4 open path: migration runs inside the normal open.
    let graph = DependencyGraph::open(&db_path).unwrap();
    let conn = graph.store().conn();

    // Existing rows intact.
    assert_eq!(row_count(conn, "code_artifacts"), 2);
    assert_eq!(row_count(conn, "graph_edges"), 1);
    assert_eq!(row_count(conn, "patterns"), 1);
    assert_eq!(row_count(conn, "anti_patterns"), 1);
    assert_eq!(row_count(conn, "rag_chunks"), 1);
    assert_eq!(row_count(conn, "rag_vectors"), 1);
    let (fqcn, signature): (String, Option<String>) = conn
        .query_row(
            "SELECT fqcn, signature FROM code_artifacts WHERE kind='class'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(fqcn, "com.example.Foo");
    assert_eq!(signature.as_deref(), Some("public class Foo"));

    // New memory_* tables present and empty (v3 had no memory data).
    for table in MEMORY_TABLES {
        assert!(table_exists(conn, table), "{} missing after migration", table);
        assert_eq!(row_count(conn, table), 0, "{} not empty", table);
    }

    // schema_meta bumped to v4.
    let version: String = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key='neurocode_schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, NEUROCODE_SCHEMA_VERSION.to_string());
    assert_eq!(NEUROCODE_SCHEMA_VERSION, 4);
}

// ---------------------------------------------------------------------------
// Idempotent double-migration — open twice, no error, no duplication.
// ---------------------------------------------------------------------------

#[test]
fn migration_is_idempotent_on_double_open() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");
    create_v3_db(&db_path);

    // First open migrates v3 → v4.
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();
        for table in MEMORY_TABLES {
            assert!(table_exists(conn, table));
        }
        assert_eq!(row_count(conn, "code_artifacts"), 2);
        // Seed one memory row per table so duplication would be observable.
        conn.execute(
            "INSERT INTO memory_episodes (id, kind, title, task, outcome, source, \
                 created_at, updated_at)
             VALUES ('ep-1', 'task', 't', 'task', 'success', 'interactive', \
                     '2026-09-09T12:00:00+00:00', '2026-09-09T12:00:00+00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_preferences (id, category, statement, origin, \
                 created_at, updated_at)
             VALUES ('pr-1', 'naming', 'use tabs', 'explicit', \
                     '2026-09-09T12:00:00+00:00', '2026-09-09T12:00:00+00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_vectors (item_id, item_kind, dim, quantization, vector)
             VALUES ('ep-1', 'episode', 2, 'f32', x'0000803F00000040')",
            [],
        )
        .unwrap();
    }

    // Second open re-runs apply_schema: no error, no duplicate rows, no
    // duplicated tables (sqlite_master has exactly one of each).
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();
        for table in MEMORY_TABLES {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{} duplicated in sqlite_master", table);
        }
        assert_eq!(row_count(conn, "memory_episodes"), 1);
        assert_eq!(row_count(conn, "memory_preferences"), 1);
        assert_eq!(row_count(conn, "memory_vectors"), 1);
        assert_eq!(row_count(conn, "code_artifacts"), 2);
        assert_eq!(row_count(conn, "rag_chunks"), 1);

        // Version still pinned exactly once at v4.
        let (version, count): (String, i64) = conn
            .query_row(
                "SELECT value, (SELECT COUNT(*) FROM schema_meta
                                WHERE key='neurocode_schema_version')
                 FROM schema_meta WHERE key='neurocode_schema_version'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(version, "4");
        assert_eq!(count, 1);
    }
}

// ---------------------------------------------------------------------------
// Memory vector BLOB round-trip incl. byte-exactness, through the real
// stores on the same file-backed DB.
// ---------------------------------------------------------------------------

#[test]
fn memory_vector_blob_round_trip_byte_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");

    // Create the on-disk schema via the real open path.
    {
        let _graph = DependencyGraph::open(&db_path).unwrap();
    }

    // f32 vector: dim=8 → 32 bytes little-endian IEEE-754.
    let f32_values: [f32; 8] = [0.25, -1.5, 3.25e-7, 1024.0, 0.5, -0.125, 7.5, -1024.0];
    let f32_bytes: Vec<u8> = f32_values.iter().flat_map(|f| f.to_le_bytes()).collect();
    // int8 vector: dim=3 → 4-byte LE f32 scale prefix + 3 codes.
    let mut int8_bytes: Vec<u8> = 0.03125f32.to_le_bytes().to_vec();
    int8_bytes.extend_from_slice(&[7u8, 0x80, 127u8]);
    assert_eq!(f32_bytes.len(), 32);
    assert_eq!(int8_bytes.len(), 7);

    // EpisodeStore put/get round-trip.
    {
        let store = EpisodeStore::open(&db_path).unwrap();
        store
            .put_vector(&MemoryVectorRecord {
                item_id: "ep-vec-f32".to_string(),
                item_kind: MemoryItemKind::Episode,
                dim: 8,
                quantization: MemoryQuantization::F32,
                blob: f32_bytes.clone(),
            })
            .unwrap();
        store
            .put_vector(&MemoryVectorRecord {
                item_id: "ep-vec-int8".to_string(),
                item_kind: MemoryItemKind::Episode,
                dim: 3,
                quantization: MemoryQuantization::Int8,
                blob: int8_bytes.clone(),
            })
            .unwrap();

        let f32_back = store.get_vector("ep-vec-f32").unwrap().unwrap();
        assert_eq!(f32_back.item_id, "ep-vec-f32");
        assert_eq!(f32_back.item_kind, MemoryItemKind::Episode);
        assert_eq!(f32_back.dim, 8);
        assert_eq!(f32_back.quantization, MemoryQuantization::F32);
        assert_eq!(f32_back.blob.len(), 32);
        assert_eq!(f32_back.blob, f32_bytes);
        // Decode and compare every word.
        let words: Vec<f32> = f32_back
            .blob
            .chunks_exact(4)
            .map(|w| f32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        assert_eq!(words, f32_values.to_vec());

        let int8_back = store.get_vector("ep-vec-int8").unwrap().unwrap();
        assert_eq!(int8_back.item_id, "ep-vec-int8");
        assert_eq!(int8_back.item_kind, MemoryItemKind::Episode);
        assert_eq!(int8_back.dim, 3);
        assert_eq!(int8_back.quantization, MemoryQuantization::Int8);
        assert_eq!(int8_back.blob.len(), 7);
        assert_eq!(int8_back.blob, int8_bytes);
        let scale =
            f32::from_le_bytes([int8_back.blob[0], int8_back.blob[1], int8_back.blob[2], int8_back.blob[3]]);
        assert_eq!(scale, 0.03125f32);
        assert_eq!(&int8_back.blob[4..], &[7u8, 128u8, 127u8]);
    }

    // PreferenceStore put/get round-trip on the same DB.
    {
        let pref_bytes: Vec<u8> = [1.0f32, 2.0, -3.5, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let store = PreferenceStore::open(&db_path).unwrap();
        store
            .put_vector(&MemoryVectorRecord {
                item_id: "pr-vec-1".to_string(),
                item_kind: MemoryItemKind::Preference,
                dim: 4,
                quantization: MemoryQuantization::F32,
                blob: pref_bytes.clone(),
            })
            .unwrap();
        let back = store.get_vector("pr-vec-1").unwrap().unwrap();
        assert_eq!(back.item_id, "pr-vec-1");
        assert_eq!(back.item_kind, MemoryItemKind::Preference);
        assert_eq!(back.dim, 4);
        assert_eq!(back.quantization, MemoryQuantization::F32);
        assert_eq!(back.blob, pref_bytes);
        assert_eq!(back.blob.len(), 16);
    }
}

// ---------------------------------------------------------------------------
// T025 — fresh DB lands directly on v4 with all memory tables.
// ---------------------------------------------------------------------------

#[test]
fn v4_fresh_db_has_memory_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");

    // Fresh open on a brand-new path: no v3 history, straight to v4.
    let graph = DependencyGraph::open(&db_path).unwrap();
    let conn = graph.store().conn();

    for table in MEMORY_TABLES {
        assert!(table_exists(conn, table), "{} missing on fresh DB", table);
        assert_eq!(row_count(conn, table), 0, "{} not empty", table);
    }

    // Version pinned exactly once at '4'.
    let (version, count): (String, i64) = conn
        .query_row(
            "SELECT value, (SELECT COUNT(*) FROM schema_meta
                            WHERE key='neurocode_schema_version')
             FROM schema_meta WHERE key='neurocode_schema_version'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(version, "4");
    assert_eq!(count, 1);
}
