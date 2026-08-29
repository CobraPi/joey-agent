//! T005 — Additive idempotent schema v2→v3 migration (rag_* tables).
//!
//! Pins the regression obligations of
//! specs/021-please-enhance-neurocode/contracts/rag-store-schema.md:
//!
//! 1. **Schema round-trip**: write chunk + vector + edge + meta rows,
//!    reopen the DB, read back identical values (incl. BLOB byte-exactness).
//! 2. **Migration-from-v2**: create a v2 DB with populated existing tables,
//!    run the v3 open path, assert existing rows intact AND new tables
//!    present AND nothing dropped/altered.
//! 3. **Idempotent-reopen**: run the migration twice; assert no error and
//!    no duplicate rows.

use std::path::PathBuf;

use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::NEUROCODE_SCHEMA_VERSION;

/// Names of every rag_* user table the v3 migration must create.
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

fn table_columns(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table)).unwrap();
    let cols = stmt
        .query_map([], |row| {
            let name: String = row.get("name")?;
            Ok(name)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    cols
}

fn row_count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// Build a class node for a v2 fixture.
fn make_node() -> CodeArtifactNode {
    CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.service.UserServiceImpl".into(),
        "com.enterprise.auth.service".into(),
        "src/main/java/com/enterprise/auth/service/UserServiceImpl.java".into(),
    )
}

/// Hand-craft a pre-v3 (v2) database: the full v2 table set with the
/// `signature` column (v1→v2) already applied, populated with existing
/// code_artifacts data, and schema_meta pinning version 2. This is exactly
/// the on-disk shape a v2 binary leaves behind.
fn create_v2_db(path: &PathBuf) {
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
        CREATE TABLE IF NOT EXISTS graph_edges (
            from_id    INTEGER NOT NULL REFERENCES code_artifacts(id),
            to_id      INTEGER NOT NULL REFERENCES code_artifacts(id),
            edge_kind  TEXT NOT NULL,
            PRIMARY KEY (from_id, to_id, edge_kind)
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS code_artifacts_fts USING fts5(
            fqcn, enclosing_type, package, annotations, declared_dependencies,
            content='code_artifacts', content_rowid='id', tokenize='unicode61'
        );
        CREATE TABLE IF NOT EXISTS patterns (
            id                INTEGER PRIMARY KEY,
            prompt_signature  TEXT NOT NULL,
            generation_summary TEXT NOT NULL,
            verify_result     TEXT NOT NULL,
            artifact_ids      TEXT NOT NULL,
            tier              TEXT NOT NULL,
            created_at        TEXT NOT NULL
        );
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
        CREATE TABLE IF NOT EXISTS domain_knowledge (
            id           INTEGER PRIMARY KEY,
            category     TEXT NOT NULL,
            source_path  TEXT NOT NULL,
            version_tag  TEXT,
            provenance   TEXT NOT NULL,
            ingested_at  TEXT NOT NULL,
            fts_indexed  INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT
        );
        INSERT INTO schema_meta(key, value) VALUES('neurocode_schema_version', '2');
        "#,
    )
    .unwrap();
    // Existing v2 data: two artifacts, one edge, one pattern, one anti-pattern.
    conn.execute(
        "INSERT INTO code_artifacts (kind, fqcn, package, source_path, indexed_at, signature)
         VALUES ('class', 'com.example.Foo', 'com.example', 'src/Foo.java',
                 '2026-08-27T10:00:00Z', 'public class Foo')",
        [],
    )
    .unwrap();
    let foo_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO code_artifacts (kind, fqcn, package, source_path, indexed_at)
         VALUES ('interface', 'com.example.IBar', 'com.example', 'src/IBar.java',
                 '2026-08-27T10:00:01Z')",
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
         VALUES ('sig', 'summary', 'pass', '[1]', 'frontier', '2026-08-27T10:00:02Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO anti_patterns (error_signature, error_output, resolution,
                                    artifact_ids, created_at)
         VALUES ('err', 'output', 'fix', '[1]', '2026-08-27T10:00:03Z')",
        [],
    )
    .unwrap();
    // Snapshot the v2 column shape for the nothing-altered assertion.
    conn.close().unwrap();
}

// ---------------------------------------------------------------------------
// Obligation 2: migration-from-v2 — v2 opens unchanged, gains empty RAG
// tables, existing rows intact, nothing dropped/altered.
// ---------------------------------------------------------------------------

#[test]
fn v2_db_migrates_additively_with_data_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");
    create_v2_db(&db_path);

    // The v2 fixture must NOT contain any rag_* table before the open.
    {
        let raw = rusqlite::Connection::open(&db_path).unwrap();
        for table in RAG_TABLES {
            assert!(!table_exists(&raw, table), "{} existed pre-migration", table);
        }
    }

    // v3 open path: migration runs inside the normal open.
    let graph = DependencyGraph::open(&db_path).unwrap();
    let conn = graph.store().conn();

    // Existing rows intact.
    assert_eq!(row_count(conn, "code_artifacts"), 2);
    assert_eq!(row_count(conn, "graph_edges"), 1);
    assert_eq!(row_count(conn, "patterns"), 1);
    assert_eq!(row_count(conn, "anti_patterns"), 1);
    let (fqcn, signature): (String, Option<String>) = conn
        .query_row(
            "SELECT fqcn, signature FROM code_artifacts WHERE kind='class'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(fqcn, "com.example.Foo");
    assert_eq!(signature.as_deref(), Some("public class Foo"));

    // Existing table shape unaltered (v2 columns, nothing added/renamed).
    let mut v2_cols = table_columns(conn, "code_artifacts");
    v2_cols.sort();
    let mut expected: Vec<&str> = [
        "id", "kind", "fqcn", "enclosing_type", "package",
        "implemented_interfaces", "annotations", "declared_dependencies",
        "source_path", "source_span_start", "source_span_end", "pega_metadata",
        "framework_version", "status", "indexed_at", "signature",
    ]
    .to_vec();
    expected.sort();
    assert_eq!(v2_cols, expected);

    // New rag_* tables present and empty (v2 had no RAG data).
    for table in RAG_TABLES {
        assert!(table_exists(conn, table), "{} missing after migration", table);
        assert_eq!(row_count(conn, table), 0, "{} not empty", table);
    }

    // schema_meta bumped to v3.
    let version: String = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key='neurocode_schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, NEUROCODE_SCHEMA_VERSION.to_string());
    assert_eq!(NEUROCODE_SCHEMA_VERSION, 3);
}

// ---------------------------------------------------------------------------
// Obligation 1: schema round-trip incl. BLOB byte-exactness.
// ---------------------------------------------------------------------------

#[test]
fn rag_schema_round_trip_blob_byte_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");

    let artifact_id = {
        let graph = DependencyGraph::open(&db_path).unwrap();
        graph.upsert_node(&make_node()).unwrap()
    };

    // f32 vector: dim=4 → 16 bytes little-endian IEEE-754.
    let f32_bytes: Vec<u8> = [0.25f32, -1.5, 3.25e-7, 1024.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    // int8 vector: dim=3 → 4-byte LE f32 scale prefix + 3 codes.
    let mut int8_bytes: Vec<u8> = 0.03125f32.to_le_bytes().to_vec();
    int8_bytes.extend_from_slice(&[7u8, 0x80, 127u8]);
    assert_eq!(f32_bytes.len(), 16);
    assert_eq!(int8_bytes.len(), 7);

    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();
        conn.execute(
            "INSERT INTO rag_chunks (chunk_id, chunk_kind, artifact_id, source_path,
                start_line, end_line, language, symbol_name, symbol_kind,
                content_hash, embed_model, embed_dim, updated_at)
             VALUES ('c-symbol', 'symbol', ?1, 'src/Foo.java', 10, 42, 'java',
                     'UserServiceImpl', 'class',
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'nomic-embed-text-v1.5', 4, '2026-08-27T11:00:00Z')",
            [artifact_id as i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_chunks (chunk_id, chunk_kind, artifact_id, source_path,
                start_line, end_line, language, symbol_name, symbol_kind,
                content_hash, embed_model, embed_dim, updated_at)
             VALUES ('c-fallback', 'fallback', NULL, 'scripts/run.py', 1, 9, 'python',
                     NULL, NULL,
                     'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     'nomic-embed-text-v1.5', 3, '2026-08-27T11:00:01Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
             VALUES ('c-symbol', 4, 'f32', ?1)",
            [&f32_bytes],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
             VALUES ('c-fallback', 3, 'int8', ?1)",
            [&int8_bytes],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_chunk_edges (from_chunk_id, to_chunk_id, edge_kind)
             VALUES ('c-symbol', 'c-fallback', 'ReferencesRule')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_index_meta (id, schema_version, embed_profile, embed_model,
                embed_dim, pooling, prefix_query, prefix_document, quantization_policy,
                chunk_count, last_refresh_at, refresh_state, created_at)
             VALUES (1, 3, 'nomic-embed-text-v1.5', 'nomic-embed-text-v1.5', 768,
                     'mean', 'search_query: ', 'search_document: ', 'auto',
                     2, '2026-08-27T11:05:00Z', 'idle', '2026-08-27T11:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_model_artifacts (profile, model_sha256, tokenizer_sha256,
                model_size_bytes, fetched_at, mirror_url_used)
             VALUES ('nomic-embed-text-v1.5',
                     'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                     'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                     1, '2026-08-27T12:00:00Z', NULL)",
            [],
        )
        .unwrap();
    } // drop: close the connection

    // Reopen and read back identical values.
    let graph = DependencyGraph::open(&db_path).unwrap();
    let conn = graph.store().conn();

    let (kind, aid, path, start, end, lang, sym, symkind, hash, updated): (
        String, Option<i64>, String, i64, i64, Option<String>, Option<String>,
        Option<String>, String, Option<String>,
    ) = conn
        .query_row(
            "SELECT chunk_kind, artifact_id, source_path, start_line, end_line,
                    language, symbol_name, symbol_kind, content_hash, updated_at
             FROM rag_chunks WHERE chunk_id='c-symbol'",
            [],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(kind, "symbol");
    assert_eq!(aid, Some(artifact_id as i64));
    assert_eq!(path, "src/Foo.java");
    assert_eq!((start, end), (10, 42));
    assert_eq!(lang.as_deref(), Some("java"));
    assert_eq!(sym.as_deref(), Some("UserServiceImpl"));
    assert_eq!(symkind.as_deref(), Some("class"));
    assert_eq!(hash.len(), 64);
    assert_eq!(updated.as_deref(), Some("2026-08-27T11:00:00Z"));

    // Fallback chunk: symbol fields NULL, artifact_id NULL.
    let (kind2, aid2, sym2): (String, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT chunk_kind, artifact_id, symbol_name FROM rag_chunks
             WHERE chunk_id='c-fallback'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(kind2, "fallback");
    assert_eq!(aid2, None);
    assert_eq!(sym2, None);

    // BLOB byte-exactness: read back and compare every byte.
    let (f32_back,): (Vec<u8>,) = conn
        .query_row(
            "SELECT vector FROM rag_vectors WHERE chunk_id='c-symbol'",
            [],
            |row| Ok((row.get(0)?,)),
        )
        .unwrap();
    assert_eq!(f32_back.len(), 16);
    assert_eq!(f32_back, f32_bytes);
    let words: Vec<f32> = f32_back
        .chunks_exact(4)
        .map(|w| f32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect();
    assert_eq!(words, vec![0.25f32, -1.5, 3.25e-7, 1024.0]);

    let (int8_back,): (Vec<u8>,) = conn
        .query_row(
            "SELECT vector FROM rag_vectors WHERE chunk_id='c-fallback'",
            [],
            |row| Ok((row.get(0)?,)),
        )
        .unwrap();
    assert_eq!(int8_back.len(), 7);
    assert_eq!(int8_back, int8_bytes);
    let scale = f32::from_le_bytes([int8_back[0], int8_back[1], int8_back[2], int8_back[3]]);
    assert_eq!(scale, 0.03125f32);
    assert_eq!(&int8_back[4..], &[7u8, 128u8, 127u8]);

    // Edge + meta + artifacts rows round-trip.
    let (from, to, ek): (String, String, String) = conn
        .query_row(
            "SELECT from_chunk_id, to_chunk_id, edge_kind FROM rag_chunk_edges",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((from.as_str(), to.as_str(), ek.as_str()), ("c-symbol", "c-fallback", "ReferencesRule"));

    let (sv, profile, pooling, pq, pd, chunk_count, state): (
        i64, Option<String>, Option<String>, Option<String>, Option<String>, i64, String,
    ) = conn
        .query_row(
            "SELECT schema_version, embed_profile, pooling, prefix_query,
                    prefix_document, chunk_count, refresh_state
             FROM rag_index_meta WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                    row.get(4)?, row.get(5)?, row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(sv, 3);
    assert_eq!(profile.as_deref(), Some("nomic-embed-text-v1.5"));
    assert_eq!(pooling.as_deref(), Some("mean"));
    assert_eq!(pq.as_deref(), Some("search_query: "));
    assert_eq!(pd.as_deref(), Some("search_document: "));
    assert_eq!(chunk_count, 2);
    assert_eq!(state, "idle");

    let (ma_profile, size, mirror): (String, i64, Option<String>) = conn
        .query_row(
            "SELECT profile, model_size_bytes, mirror_url_used FROM rag_model_artifacts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(ma_profile, "nomic-embed-text-v1.5");
    assert_eq!(size, 1);
    assert_eq!(mirror, None);
}

// ---------------------------------------------------------------------------
// Obligation 3: idempotent double-migration — open twice, no error, no
// duplication.
// ---------------------------------------------------------------------------

#[test]
fn migration_is_idempotent_on_double_open() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");
    create_v2_db(&db_path);

    // First open migrates v2 → v3.
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();
        for table in RAG_TABLES {
            assert!(table_exists(conn, table));
        }
        assert_eq!(row_count(conn, "code_artifacts"), 2);
        // Seed one rag row per table so duplication would be observable.
        conn.execute(
            "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line,
                end_line, language, content_hash, updated_at)
             VALUES ('c1', 'fallback', 'a.py', 1, 2, 'python', 'h1',
                     '2026-08-27T13:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
             VALUES ('c1', 2, 'f32', x'0000803F00000040')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_index_meta (id, schema_version, created_at)
             VALUES (1, 3, '2026-08-27T13:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_model_artifacts (profile, model_sha256, tokenizer_sha256,
                model_size_bytes, fetched_at)
             VALUES ('p', 'a', 'b', 1, '2026-08-27T13:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rag_chunk_edges (from_chunk_id, to_chunk_id, edge_kind)
             VALUES ('c1', 'c1', 'MemberOf')",
            [],
        )
        .unwrap();
    }

    // Second open re-runs apply_schema: no error, no duplicate rows, no
    // duplicated tables (sqlite_master has exactly one of each).
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let conn = graph.store().conn();
        for table in RAG_TABLES {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{} duplicated in sqlite_master", table);
        }
        assert_eq!(row_count(conn, "rag_chunks"), 1);
        assert_eq!(row_count(conn, "rag_vectors"), 1);
        assert_eq!(row_count(conn, "rag_index_meta"), 1);
        assert_eq!(row_count(conn, "rag_model_artifacts"), 1);
        assert_eq!(row_count(conn, "rag_chunk_edges"), 1);
        assert_eq!(row_count(conn, "code_artifacts"), 2);
        assert_eq!(row_count(conn, "graph_edges"), 1);

        // Version still pinned exactly once at v3.
        let (version, count): (String, i64) = conn
            .query_row(
                "SELECT value, (SELECT COUNT(*) FROM schema_meta
                                WHERE key='neurocode_schema_version')
                 FROM schema_meta WHERE key='neurocode_schema_version'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(version, "3");
        assert_eq!(count, 1);
    }
}

// ---------------------------------------------------------------------------
// Extra pin: the cascade the contract mandates — deleting a chunk purges its
// vector; deleting an artifact purges its chunks (which cascades on).
// ---------------------------------------------------------------------------

#[test]
fn rag_vector_cascade_on_chunk_delete() {
    // File-backed: the on-disk open path sets PRAGMA foreign_keys=ON
    // (open_in_memory does not), and the cascade guarantee is about the
    // real graph.db.
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");
    let graph = DependencyGraph::open(&db_path).unwrap();
    let conn = graph.store().conn();
    conn.execute(
        "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line,
            end_line, language, content_hash, updated_at)
         VALUES ('c1', 'fallback', 'a.py', 1, 2, 'python', 'h1',
                 '2026-08-27T13:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
         VALUES ('c1', 2, 'f32', x'0000803F00000040')",
        [],
    )
    .unwrap();
    assert_eq!(row_count(conn, "rag_vectors"), 1);
    conn.execute("DELETE FROM rag_chunks WHERE chunk_id='c1'", []).unwrap();
    assert_eq!(
        row_count(conn, "rag_vectors"),
        0,
        "chunk purge must cascade to rag_vectors (FR-005)"
    );
}
