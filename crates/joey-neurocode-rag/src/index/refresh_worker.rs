//! Background refresh worker — the atomic transactional snapshot swap (T024).
//!
//! One refresh = one transaction writing chunks + vectors + edges +
//! `rag_index_meta`; COMMIT is the swap point. WAL readers keep seeing the
//! prior fully consistent snapshot until the commit lands, so no query ever
//! observes torn or mixed old/new state (FR-004, clarification Q5;
//! contracts/rag-store-schema.md § Atomicity; data-model.md § Storage
//! invariants).
//!
//! This worker ORCHESTRATES the incremental machinery from
//! [`crate::index::incremental`] (T021–T023) — it never duplicates it:
//!
//! 1. `refresh_state` flips `idle → refreshing` in its own small committed
//!    transaction BEFORE the heavy work. The flag is observable status for
//!    concurrent readers (FR-013; T025 polls it) and therefore deliberately
//!    lives OUTSIDE the data transaction — a flag readers must see
//!    mid-refresh cannot sit inside the transaction whose commit it
//!    announces.
//! 2. Previous state comes from the worker's own persisted fingerprint
//!    sidecar ([`reconstruct_previous`]) — see below.
//! 3. `detect_changes` / `detect_changes_with_rename_assist` produce the
//!    [`ChangeDelta`]; [`incremental::refresh_incremental`] owns THE single
//!    data transaction (chunks + vectors + edge sweeps + the
//!    `chunk_count`/`last_refresh_at` updates in `rag_index_meta`) — its
//!    COMMIT is the swap.
//! 4. `refresh_state` flips back `refreshing → idle` in its own transaction
//!    — on the error path too, so the flag never stays stuck.
//! 5. The fingerprint sidecar is rewritten from store truth (committed
//!    `rag_chunks` paths × current disk state), healing any drift.
//!
//! WAL note: `GraphStore::open` sets `PRAGMA journal_mode=WAL` (plus
//! `foreign_keys=ON` and `busy_timeout=5000`) on every open — see
//! `crates/joey-neurocode/src/graph/store.rs`'s open path. Verified and
//! pinned below by `graph_store_open_path_is_wal`. The worker adds no
//! pragma of its own: journal mode is a database-level property owned by
//! the open path, which already enables it.
//!
//! Callable from a background context: a plain sync `fn` — the agent-loop
//! side (T025) spawns it via `spawn_blocking`; nothing here blocks a turn
//! beyond the work it does.
//!
//! Single-writer assumption: SQLite serializes write transactions; this
//! worker assumes ONE refresh at a time per project DB (T025's spawner is
//! the single instance).

use std::path::Path;

use joey_neurocode::graph::GraphStore;
use rusqlite::OptionalExtension;

use crate::embed::profiles::EmbedProfile;
use crate::index::chunker::{ChunkEmbedder, ChunkOptions};
use crate::index::incremental::{
    self, ChangeDelta, DetectionOptions, FileFingerprint, GitRenameAssist, RefreshBudgets,
    RefreshError, RefreshOutcome,
};
use crate::vector::quantize::Quantization;

/// Knobs for [`run_refresh`] — the worker-level slice of the configuration
/// surface (budgets from `neurocode.rag.refresh.*`, detection mode, chunk
/// options, quantization, and whether the git rename assist is attempted).
#[derive(Debug, Clone)]
pub struct RefreshWorkerOptions {
    /// Change-detection mode (mtime fast path vs deep hash-everything).
    pub detection: DetectionOptions,
    /// Per-turn refresh budgets (FR-004/FR-005).
    pub budgets: RefreshBudgets,
    /// Chunking knobs (line cap, import-prefix cap).
    pub chunk_options: ChunkOptions,
    /// Vector storage encoding for newly embedded chunks.
    pub quantization: Quantization,
    /// Attempt git-CLI rename detection (T022); `false` forces the
    /// remove+add end state.
    pub rename_assist: bool,
}

impl Default for RefreshWorkerOptions {
    fn default() -> Self {
        Self {
            detection: DetectionOptions::default(),
            budgets: RefreshBudgets::default(),
            chunk_options: ChunkOptions::default(),
            quantization: Quantization::F32,
            rename_assist: true,
        }
    }
}

/// What one [`run_refresh`] did: the detected [`ChangeDelta`] plus the
/// observable counters from the committed refresh (FR-013 status
/// reporting consumes these; the pinned v3 schema carries only
/// `chunk_count`/`last_refresh_at` in-DB, so the full counters live here).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshReport {
    pub delta: ChangeDelta,
    pub outcome: RefreshOutcome,
}

/// Run one full atomic refresh of `store` from the tree at `root`.
///
/// The clean background entrypoint (T025 spawns this via `spawn_blocking`):
/// previous state is derived from the store itself, the data swap is the
/// single transaction inside [`incremental::refresh_incremental`], and
/// `rag_index_meta.refresh_state` transitions `idle → refreshing → idle`
/// around it — with `refreshing` committed BEFORE the heavy work so
/// concurrent status readers observe it mid-refresh.
///
/// The indexability filter is [`incremental::default_indexable_filter`]
/// (supported source extensions, VCS/build/dependency dirs skipped).
///
/// On error the data transaction has rolled back (readers never saw it)
/// and the flag is still reset to `idle` before the error propagates.
pub fn run_refresh(
    store: &GraphStore,
    root: &Path,
    embedder: &mut dyn ChunkEmbedder,
    profile: &EmbedProfile,
    options: &RefreshWorkerOptions,
) -> Result<RefreshReport, RefreshError> {
    // 1. Flag up FIRST so 'refreshing' covers the entire operation
    //    (detection walk included), not just the write transaction.
    begin_refreshing(store, profile, options.quantization)?;

    // 2. Previous state from store truth — self-contained refresh.
    let previous = reconstruct_previous(store, root);

    // 3. Detect + refresh (refresh_incremental owns the single data
    //    transaction; its COMMIT is the swap point).
    let result = detect_and_refresh(store, root, &previous, embedder, profile, options);

    // 4. Flag down whatever happened — the flag must never stay stuck.
    if let Err(finish_err) = finish_refreshing(store) {
        if result.is_ok() {
            return Err(finish_err);
        }
        // Both failed: the refresh error is the root cause; the flag may be
        // stuck 'refreshing' until the next refresh — surfaced loudly.
        eprintln!("[joey neurocode] refresh failed to reset refresh_state: {finish_err}");
    }

    // 5. Persist the next refresh's "previous state" — only after a
    //    successful commit, so the sidecar never describes rolled-back
    //    data. Best-effort: a lost sidecar heals via reconstruct's
    //    disk-fingerprint fallback next refresh.
    if result.is_ok() {
        persist_fingerprints(store, root);
    }

    result
}

/// Steps 3's body, isolated so the flag-down in [`run_refresh`] runs on
/// both the success and error paths.
fn detect_and_refresh(
    store: &GraphStore,
    root: &Path,
    previous: &[FileFingerprint],
    embedder: &mut dyn ChunkEmbedder,
    profile: &EmbedProfile,
    options: &RefreshWorkerOptions,
) -> Result<RefreshReport, RefreshError> {
    let delta = if options.rename_assist {
        incremental::detect_changes_with_rename_assist(
            root,
            previous,
            &incremental::default_indexable_filter,
            &options.detection,
            &GitRenameAssist::new(root),
        )
    } else {
        incremental::detect_changes(
            root,
            previous,
            &incremental::default_indexable_filter,
            &options.detection,
        )
    };
    let outcome = incremental::refresh_incremental(
        store,
        root,
        &delta,
        embedder,
        profile,
        options.quantization,
        &options.chunk_options,
        &options.budgets,
    )?;
    Ok(RefreshReport { delta, outcome })
}

/// Flip `refresh_state` to `refreshing`, ensuring the `rag_index_meta`
/// singleton exists first (cold DB: no `write_index` has run yet, and
/// `refresh_incremental`'s `UPDATE … WHERE id = 1` is a no-op without a
/// row). Autocommit — one small committed transaction, immediately visible
/// to WAL status readers.
///
/// The INSERT seeds the profile-identity columns (mirroring
/// [`crate::vector::store::write_index`]'s upsert shape); the
/// `ON CONFLICT` arm only touches the flag — profile identity belongs to
/// the data transaction, not the status flag.
fn begin_refreshing(
    store: &GraphStore,
    profile: &EmbedProfile,
    quantization: Quantization,
) -> Result<(), RefreshError> {
    store.conn().execute(
        r#"
        INSERT INTO rag_index_meta
            (id, schema_version, embed_profile, embed_model, embed_dim, pooling,
             prefix_query, prefix_document, quantization_policy, chunk_count,
             last_refresh_at, refresh_state, created_at)
        VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                (SELECT COUNT(*) FROM rag_chunks), NULL, 'refreshing', ?9)
        ON CONFLICT(id) DO UPDATE SET refresh_state = 'refreshing'
        "#,
        rusqlite::params![
            joey_neurocode::NEUROCODE_SCHEMA_VERSION as i64,
            profile.name,
            profile.name,
            profile.dim as i64,
            profile.pooling.as_str(),
            profile.prefix_query,
            profile.prefix_document,
            quantization.as_str(),
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Flip `refresh_state` back to `idle` and re-sync the denormalized
/// `chunk_count` counter. `last_refresh_at` is deliberately NOT touched
/// here — it means "last committed refresh" and is stamped inside the data
/// transaction by `refresh_incremental` (only successful swaps bump it).
fn finish_refreshing(store: &GraphStore) -> Result<(), RefreshError> {
    store.conn().execute(
        "UPDATE rag_index_meta
         SET refresh_state = 'idle',
             chunk_count   = (SELECT COUNT(*) FROM rag_chunks)
         WHERE id = 1",
        [],
    )?;
    Ok(())
}

/// Reconstruct the "previous state" fingerprints for change detection.
///
/// **Authoritative source: the worker's fingerprint sidecar**
/// (`graph.db`'s sibling `refresh_fingerprints.json` — see
/// [`fingerprint_sidecar_path`]). incremental.rs's `FileFingerprint` docs
/// assign this persistence to the worker ("The refresh worker (T024)
/// persists the authoritative copy alongside the index"), and the pinned
/// v3 schema deliberately has no fingerprint table — so this JSON file,
/// written after each successful refresh commit, IS that persisted copy.
///
/// **Healing ladder** (sidecar lost/stale/corrupt — never an error):
///
/// 1. Sidecar present and loadable: exactly those fingerprints.
/// 2. No loadable sidecar (first refresh, deleted, corrupt JSON): fall
///    back to fingerprinting the files backing committed `rag_chunks`
///    rows. This is re-detection, not full correctness: files modified
///    while the sidecar was absent fall back to chunk-hash skipping
///    (equal stored chunk hashes ⇒ skipped — refresh_incremental's own
///    correctness net), and gone files still purge via the sentinel rule
///    below. The next successful refresh rewrites the sidecar.
/// 3. Files with chunks that no longer exist on disk get a **sentinel**
///    fingerprint (a mtime/size/hash combination no real file produces)
///    so change detection classifies them as `removed` and the refresh
///    purges them (FR-005). Dropping them would leave stale rows forever.
///
/// Budget-deferred files ([`RefreshOutcome::files_deferred`]) have no
/// committed rows yet, so they re-detect next refresh exactly as FR-004
/// requires.
pub fn reconstruct_previous(store: &GraphStore, root: &Path) -> Vec<FileFingerprint> {
    // ── Authoritative: the sidecar ──────────────────────────────────────
    if let Some(sidecar) = load_fingerprint_sidecar(store) {
        return sidecar;
    }

    // ── Healing fallback: store truth × current disk ────────────────────
    let indexed_paths: Vec<String> = {
        let conn = store.conn();
        let Ok(mut stmt) = conn.prepare("SELECT DISTINCT source_path FROM rag_chunks") else {
            return Vec::new();
        };
        let rows = stmt.query_map([], |r| r.get::<_, String>(0));
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    };

    indexed_paths
        .into_iter()
        .map(|source_path| match incremental::fingerprint_file(root, Path::new(&source_path)) {
            Ok(fp) => fp,
            // Gone from disk (or unreadable): keep the path in `previous`
            // with values no real file can produce. If the file is really
            // gone, detect_changes marks it `removed` ⇒ purge. If it is
            // merely unreadable-but-present, the hash confirmation fails
            // soft and the file keeps its chunks (detect_changes skips it).
            Err(_) => FileFingerprint {
                source_path,
                mtime: std::time::SystemTime::UNIX_EPOCH,
                size: 0,
                sha256: String::new(),
            },
        })
        .collect()
}

// ===========================================================================
// Fingerprint sidecar (`refresh_fingerprints.json`, sibling of graph.db)
// ===========================================================================

/// One persisted fingerprint entry (serde mirror of
/// [`incremental::FileFingerprint`]; `mtime` serialized as RFC 3339).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FingerprintEntry {
    source_path: String,
    mtime: String,
    size: u64,
    sha256: String,
}

/// The sidecar lives beside `graph.db` (same directory) — derived from the
/// store's own main-database path via `PRAGMA database_list`, so callers
/// that open the store via `project_graph_db_path` or a literal path both
/// land on the same sidecar. Format: JSON object keyed by `source_path`.
fn fingerprint_sidecar_path(store: &GraphStore) -> Option<std::path::PathBuf> {
    let row: Option<(i64, String, String)> = store
        .conn()
        .query_row("PRAGMA database_list", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .ok()
        .flatten();
    // Main DB is seq 0 and has a file path ("" only for in-memory/temp).
    let file = row.filter(|(seq, _, _)| *seq == 0).map(|(_, _, p)| p)?;
    if file.is_empty() {
        return None;
    }
    Some(
        std::path::Path::new(&file)
            .parent()
            .map(|p| p.join("refresh_fingerprints.json"))
            .unwrap_or_else(|| std::path::PathBuf::from("refresh_fingerprints.json")),
    )
}

fn load_fingerprint_sidecar(store: &GraphStore) -> Option<Vec<FileFingerprint>> {
    let path = fingerprint_sidecar_path(store)?;
    let raw = std::fs::read_to_string(path).ok()?;
    let map: std::collections::BTreeMap<String, FingerprintEntry> =
        serde_json::from_str(&raw).ok()?;
    let mut out = Vec::with_capacity(map.len());
    for (_, e) in map {
        let mtime = chrono::DateTime::parse_from_rfc3339(&e.mtime)
            .map(|t| t.into())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        out.push(FileFingerprint { source_path: e.source_path, mtime, size: e.size, sha256: e.sha256 });
    }
    Some(out)
}

/// Persist the post-refresh fingerprint state (store truth × current
/// disk). Best-effort by design: failure costs only the healing ladder's
/// re-detection next refresh, never correctness — so errors are logged,
/// not propagated.
///
/// **Index truth, not disk truth**: the sidecar lists exactly the paths
/// with committed `rag_chunks` rows, fingerprinted against the CURRENT
/// disk state for those paths only. A disk-wide `snapshot_tree` here
/// would fingerprint budget-deferred files (present on disk, never
/// actually indexed because `max_files_per_turn` < backlog) as-if-indexed
/// — and `detect_changes` trusts this sidecar as previous state, so they
/// would be PERMANENTLY skipped. Files on disk but absent from
/// `rag_chunks` stay OUT of the sidecar and re-detect next refresh,
/// exactly as FR-004 requires.
fn persist_fingerprints(store: &GraphStore, root: &Path) {
    let Some(sidecar_path) = fingerprint_sidecar_path(store) else { return };

    let indexed_paths: Vec<String> = {
        let conn = store.conn();
        let Ok(mut stmt) = conn.prepare("SELECT DISTINCT source_path FROM rag_chunks") else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            return;
        };
        rows.flatten().collect()
    };

    let map: std::collections::BTreeMap<String, FingerprintEntry> = indexed_paths
        .into_iter()
        .map(|source_path| {
            match incremental::fingerprint_file(root, Path::new(&source_path)) {
                Ok(fp) => FingerprintEntry {
                    source_path: fp.source_path,
                    mtime: chrono::DateTime::<chrono::Utc>::from(fp.mtime).to_rfc3339(),
                    size: fp.size,
                    sha256: fp.sha256,
                },
                // Committed rows whose file vanished mid-refresh (between
                // the data commit and this sidecar write) keep the same
                // sentinel reconstruct_previous uses for gone files: the
                // next refresh classifies them `removed` and purges,
                // instead of silently leaking committed chunks forever.
                Err(_) => FingerprintEntry {
                    mtime: chrono::DateTime::<chrono::Utc>::from(
                        std::time::SystemTime::UNIX_EPOCH,
                    )
                    .to_rfc3339(),
                    size: 0,
                    sha256: String::new(),
                    source_path,
                },
            }
        })
        .map(|e| (e.source_path.clone(), e))
        .collect();
    let json = serde_json::to_string_pretty(&map).unwrap_or_default();
    if let Err(e) = std::fs::write(&sidecar_path, json) {
        eprintln!("[joey neurocode] refresh fingerprint sidecar write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;

    use crate::embed::profiles::default_profile;
    use crate::vector::store::{
        chunk_count, load_index_meta, vector_count, RagIndexMeta,
    };

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// Deterministic unit vector keyed on the text (same shape as the
    /// counting embedder in incremental.rs's tests).
    fn unit_vector(dim: usize, t: &str) -> Vec<f32> {
        let v: Vec<f32> = (0..dim)
            .map(|i| ((t.len() as f32 * 0.001 + i as f32) % 7.0) / 7.0)
            .collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    /// Fast deterministic embedder (counts calls).
    struct FastEmbedder {
        dim: usize,
        calls: usize,
        texts: usize,
    }

    impl FastEmbedder {
        fn new(dim: usize) -> Self {
            Self { dim, calls: 0, texts: 0 }
        }
    }

    impl ChunkEmbedder for FastEmbedder {
        fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            self.calls += 1;
            self.texts += texts.len();
            Ok(texts.iter().map(|t| unit_vector(self.dim, t)).collect())
        }
    }

    /// Slow embedder: sleeps per call to stretch the mid-refresh window so
    /// concurrent readers can poll while the write transaction is open.
    struct SlowEmbedder {
        dim: usize,
        delay_ms: u64,
    }

    impl ChunkEmbedder for SlowEmbedder {
        fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
            Ok(texts.iter().map(|t| unit_vector(self.dim, t)).collect())
        }
    }

    /// Gated embedder: signals before returning from `embed_texts` only
    /// after release — the writer thread parks INSIDE the refresh
    /// transaction, giving the test a guaranteed mid-refresh observation
    /// point.
    struct GatedEmbedder {
        dim: usize,
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl ChunkEmbedder for GatedEmbedder {
        fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            self.started.send(()).expect("started channel");
            self.release.recv().expect("release channel");
            Ok(texts.iter().map(|t| unit_vector(self.dim, t)).collect())
        }
    }

    fn multi_chunk_source(name: &str, n: usize) -> String {
        format!("def {name}():\n    return {n}\n\nz{name} = {n}\n")
    }

    /// One consistency snapshot read inside a SINGLE read transaction (all
    /// values from one WAL snapshot — separate statements could straddle
    /// the writer's commit).
    #[allow(clippy::type_complexity)]
    fn consistency_snapshot(
        store: &GraphStore,
    ) -> (i64, i64, i64, i64, String, Vec<String>) {
        let conn = store.conn();
        let tx = conn.unchecked_transaction().unwrap();
        let chunks: i64 =
            tx.query_row("SELECT COUNT(*) FROM rag_chunks", [], |r| r.get(0)).unwrap();
        let vectors: i64 =
            tx.query_row("SELECT COUNT(*) FROM rag_vectors", [], |r| r.get(0)).unwrap();
        let missing_vectors: i64 = tx.query_row(
            "SELECT COUNT(*) FROM rag_chunks c
             LEFT JOIN rag_vectors v ON v.chunk_id = c.chunk_id
             WHERE v.chunk_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
        let orphan_vectors: i64 = tx.query_row(
            "SELECT COUNT(*) FROM rag_vectors v
             LEFT JOIN rag_chunks c ON c.chunk_id = v.chunk_id
             WHERE c.chunk_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
        let state: String = tx
            .query_row(
                "SELECT refresh_state FROM rag_index_meta WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut paths: Vec<String> = tx
            .prepare("SELECT DISTINCT source_path FROM rag_chunks")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        paths.sort();
        drop(tx); // read-only; rollback of an empty tx is a no-op
        (chunks, vectors, missing_vectors, orphan_vectors, state, paths)
    }

    fn meta_state(store: &GraphStore) -> RagIndexMeta {
        load_index_meta(store.conn()).unwrap().expect("rag_index_meta singleton")
    }

    // ── WAL finding pin ─────────────────────────────────────────────────

    /// The store's OPEN path enables WAL (graph/store.rs sets
    /// `PRAGMA journal_mode=WAL`) — the prerequisite for the concurrent
    /// snapshot reads above. Pinned here so the atomic-swap contract's
    /// "WAL readers" premise cannot silently regress. The worker adds no
    /// pragma of its own: journal mode belongs to the open path.
    #[test]
    fn graph_store_open_path_is_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
        let mode: String = store
            .conn()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    // ── refresh_state observability ─────────────────────────────────────

    /// FR-004/T025 pin: while a refresh's write transaction is open (the
    /// gated embedder parks inside it), a concurrent reader on its own
    /// connection observes `refresh_state = 'refreshing'`; after the
    /// refresh joins, the state is `idle` and `last_refresh_at` is set.
    #[test]
    fn refresh_state_transitions_idle_refreshing_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let db = root.join("graph.db");
        write(&root, "a.py", &multi_chunk_source("a", 1));

        let profile = default_profile();
        let dim = profile.dim as usize;

        // Reader pre-opened (WAL: independent snapshot per read tx).
        let reader = GraphStore::open(&db).unwrap();
        assert!(
            load_index_meta(reader.conn()).unwrap().is_none(),
            "cold DB has no meta row before the first refresh"
        );

        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let root2: PathBuf = root.to_path_buf();
        let db2 = db.clone();
        let writer = thread::spawn(move || {
            let store = GraphStore::open(&db2).unwrap();
            let mut embedder =
                GatedEmbedder { dim, started: started_tx, release: release_rx };
            run_refresh(&store, &root2, &mut embedder, &profile, &RefreshWorkerOptions::default())
                .unwrap()
        });

        // Park inside the refresh transaction, then observe from outside.
        started_rx.recv().unwrap();
        let mid = meta_state(&reader);
        assert_eq!(mid.refresh_state, "refreshing", "flag must be observable mid-refresh");
        assert_eq!(mid.embed_dim, default_profile().dim, "cold row seeded with profile identity");

        release_tx.send(()).unwrap();
        let report = writer.join().unwrap();
        assert_eq!(report.outcome.files_indexed, 1);

        let after = meta_state(&reader);
        assert_eq!(after.refresh_state, "idle");
        assert!(after.last_refresh_at.is_some(), "last_refresh_at recorded");
        assert!(after.chunk_count >= 2);
        assert_eq!(after.chunk_count, chunk_count(reader.conn()).unwrap());
    }

    // ── THE concurrent-reader no-torn-state test ────────────────────────

    /// FR-004 / clarification Q5 / contracts/rag-store-schema.md § Atomicity:
    /// while a refresh writes MULTIPLE files (adds + modify + purge) with a
    /// deliberately slowed embedder, a pool of concurrent readers polling
    /// chunk/vector counts, per-file sets, and `refresh_state` must ALWAYS
    /// see either the full pre-refresh or the full post-refresh consistent
    /// state — never a partial file set, never a chunk without its vector,
    /// never an orphan vector. `refreshing` must be observed at least once.
    #[test]
    fn concurrent_readers_never_observe_torn_state() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let db = root.join("graph.db");

        // Pre-state A: three multi-chunk files.
        for (name, n) in [("a", 1), ("b", 2), ("c", 3)] {
            write(root, &format!("{name}.py"), &multi_chunk_source(name, n));
        }
        let main_store = GraphStore::open(&db).unwrap();
        let profile = default_profile();
        let mut fast = FastEmbedder::new(profile.dim as usize);
        let cold = run_refresh(&main_store, root, &mut fast, &profile, &RefreshWorkerOptions::default()).unwrap();
        assert_eq!(cold.outcome.files_indexed, 3);
        assert!(cold.outcome.chunks_embedded >= 6, "multi-chunk pre-state: {:?}", cold.outcome);

        let pre = consistency_snapshot(&main_store);
        let pre_paths: Vec<String> = vec!["a.py".into(), "b.py".into(), "c.py".into()];
        assert_eq!(pre.5, pre_paths, "pre-state file set");
        assert_eq!((pre.2, pre.3), (0, 0), "pre-state has no missing/orphan vectors");
        let pre = (pre.0, pre.1, Arc::new(pre.5));

        // Mutate to state B: remove one, add three, modify one (both regions).
        std::fs::remove_file(root.join("a.py")).unwrap();
        for (name, n) in [("d", 4), ("e", 5), ("f", 6)] {
            write(root, &format!("{name}.py"), &multi_chunk_source(name, n));
        }
        write(root, "b.py", &multi_chunk_source("b", 22));

        // Pre-open the reader pool (each thread its own connection).
        let done = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for _ in 0..3 {
            let store = GraphStore::open(&db).unwrap();
            let done = Arc::clone(&done);
            let pre = pre.clone();
            readers.push(thread::spawn(move || {
                let mut violations: Vec<String> = Vec::new();
                let mut saw_refreshing = false;
                while !done.load(Ordering::Relaxed) {
                    let (chunks, vectors, missing, orphans, state, paths) =
                        consistency_snapshot(&store);
                    if missing != 0 {
                        violations.push(format!("chunk without vector mid-refresh: {missing}"));
                    }
                    if orphans != 0 {
                        violations.push(format!("orphan vector mid-refresh: {orphans}"));
                    }
                    if state == "refreshing" {
                        saw_refreshing = true;
                    } else if state != "idle" {
                        violations.push(format!("unexpected refresh_state: {state}"));
                    }
                    if !((chunks == pre.0 && vectors == pre.1 && paths == *pre.2)
                        || (paths == ["b.py", "c.py", "d.py", "e.py", "f.py"]
                            && chunks == vectors))
                    {
                        violations.push(format!(
                            "torn state: chunks={chunks} vectors={vectors} paths={paths:?}"
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                (violations, saw_refreshing)
            }));
        }

        // Writer: its own connection; slowed embedder stretches the window.
        let root2: PathBuf = root.to_path_buf();
        let db2 = db.clone();
        let dim = profile.dim as usize;
        let writer = thread::spawn(move || {
            let store = GraphStore::open(&db2).unwrap();
            let mut slow = SlowEmbedder { dim, delay_ms: 25 };
            let report = run_refresh(&store, &root2, &mut slow, &profile, &RefreshWorkerOptions::default()).unwrap();
            done.store(true, Ordering::Relaxed);
            report
        });

        let report = writer.join().unwrap();
        assert_eq!(report.outcome.files_purged, 1, "a.py purged: {:?}", report.outcome);
        assert_eq!(report.outcome.files_indexed, 3, "d/e/f added: {:?}", report.outcome);
        assert_eq!(report.outcome.files_reindexed, 1, "b.py modified: {:?}", report.outcome);
        assert_eq!(report.delta.removed, vec![PathBuf::from("a.py")]);

        let post = consistency_snapshot(&main_store);
        let mut saw_refreshing = false;
        for r in readers {
            let (violations, refreshing) = r.join().unwrap();
            saw_refreshing |= refreshing;
            assert!(violations.is_empty(), "torn state observed: {violations:?}");
        }
        assert!(saw_refreshing, "readers must observe the 'refreshing' window");

        // Post-state sanity: the swap actually happened, consistently.
        assert_eq!(post.5, ["b.py", "c.py", "d.py", "e.py", "f.py"]);
        assert_eq!(post.0, post.1, "every chunk has its vector");
        assert_ne!((post.0, post.1), (pre.0, pre.1), "refresh changed the index");
        assert_eq!(post.0 as u64, chunk_count(main_store.conn()).unwrap());
        assert_eq!(post.1 as u64, vector_count(main_store.conn()).unwrap());
        assert_eq!(meta_state(&main_store).refresh_state, "idle");
    }

    // ── end-to-end orchestration ────────────────────────────────────────

    /// Cold index → mutate (add/modify/remove) → second refresh: the
    /// worker's previous-state reconstruction (store truth) drives correct
    /// deltas and outcomes across calls, and the meta singleton ends each
    /// refresh `idle` with honest counters.
    #[test]
    fn run_refresh_reports_delta_outcome_and_purges_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let store = GraphStore::open(&root.join("graph.db")).unwrap();

        for (name, n) in [("a", 1), ("b", 2), ("c", 3)] {
            write(root, &format!("{name}.py"), &multi_chunk_source(name, n));
        }
        let profile = default_profile();
        let mut embedder = FastEmbedder::new(profile.dim as usize);
        let first = run_refresh(&store, root, &mut embedder, &profile, &RefreshWorkerOptions::default()).unwrap();
        assert_eq!(first.outcome.files_indexed, 3);
        assert!(first.outcome.chunks_embedded >= 6);
        assert_eq!(first.outcome.chunks_skipped, 0);
        let meta = meta_state(&store);
        assert_eq!(meta.refresh_state, "idle");
        assert!(meta.last_refresh_at.is_some());
        assert_eq!(meta.chunk_count, chunk_count(store.conn()).unwrap());

        // Mutate: remove c.py, add d.py (distinct content — no rename pair),
        // modify a.py (both regions).
        std::fs::remove_file(root.join("c.py")).unwrap();
        write(root, "d.py", &multi_chunk_source("d", 4));
        write(root, "a.py", &multi_chunk_source("a", 11));

        let mut embedder2 = FastEmbedder::new(profile.dim as usize);
        let second = run_refresh(&store, root, &mut embedder2, &profile, &RefreshWorkerOptions::default()).unwrap();
        assert_eq!(second.delta.removed, vec![PathBuf::from("c.py")]);
        assert_eq!(second.delta.added, vec![PathBuf::from("d.py")]);
        assert_eq!(second.delta.modified, vec![PathBuf::from("a.py")]);
        assert_eq!(second.outcome.files_purged, 1);
        assert_eq!(second.outcome.files_indexed, 1);
        assert_eq!(second.outcome.files_reindexed, 1);

        let c_rows: i64 = store.conn().query_row(
            "SELECT COUNT(*) FROM rag_chunks WHERE source_path = 'c.py'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(c_rows, 0, "removed file fully purged");
        let d_rows: i64 = store.conn().query_row(
            "SELECT COUNT(*) FROM rag_chunks WHERE source_path = 'd.py'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(d_rows >= 2, "added file indexed with chunks");

        // No orphans anywhere; meta honest.
        let orphans: i64 = store.conn().query_row(
            "SELECT (SELECT COUNT(*) FROM rag_vectors WHERE chunk_id NOT IN
                     (SELECT chunk_id FROM rag_chunks))
                 + (SELECT COUNT(*) FROM rag_chunk_edges WHERE from_chunk_id NOT IN
                     (SELECT chunk_id FROM rag_chunks))",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(orphans, 0);
        let meta2 = meta_state(&store);
        assert_eq!(meta2.refresh_state, "idle");
        assert_eq!(meta2.chunk_count, chunk_count(store.conn()).unwrap());
    }

    /// The reconstruct rule for gone files: a path with chunks but no file
    /// on disk stays in `previous` as a purge candidate (sentinel
    /// fingerprint), so the next refresh actually purges it (FR-005).
    #[test]
    fn reconstructed_previous_keeps_gone_files_as_purge_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let store = GraphStore::open(&root.join("graph.db")).unwrap();
        write(root, "solo.py", &multi_chunk_source("solo", 1));

        let profile = default_profile();
        let mut embedder = FastEmbedder::new(profile.dim as usize);
        run_refresh(&store, root, &mut embedder, &profile, &RefreshWorkerOptions::default()).unwrap();

        std::fs::remove_file(root.join("solo.py")).unwrap();
        let prev = reconstruct_previous(&store, root);
        assert_eq!(prev.len(), 1, "gone file remains a purge candidate");
        assert_eq!(prev[0].source_path, "solo.py");

        let mut embedder2 = FastEmbedder::new(profile.dim as usize);
        let report = run_refresh(&store, root, &mut embedder2, &profile, &RefreshWorkerOptions::default()).unwrap();
        assert_eq!(report.outcome.files_purged, 1);
        assert_eq!(chunk_count(store.conn()).unwrap(), 0);
        assert_eq!(vector_count(store.conn()).unwrap(), 0, "cascade removed the vectors");
    }

    /// Error path: a failing embedder rolls the data transaction back (no
    /// partial rows) AND still resets `refresh_state` to `idle`.
    struct FailingEmbedder {
        dim: usize,
        fail_after: usize,
        seen: usize,
    }

    impl ChunkEmbedder for FailingEmbedder {
        fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            self.seen += texts.len();
            if self.seen > self.fail_after {
                return Err("simulated embedder outage".to_string());
            }
            Ok(texts.iter().map(|t| unit_vector(self.dim, t)).collect())
        }
    }

    #[test]
    fn failed_refresh_rolls_back_and_resets_state() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let store = GraphStore::open(&root.join("graph.db")).unwrap();

        for (name, n) in [("a", 1), ("b", 2)] {
            write(root, &format!("{name}.py"), &multi_chunk_source(name, n));
        }
        let profile = default_profile();
        let mut embedder = FastEmbedder::new(profile.dim as usize);
        run_refresh(&store, root, &mut embedder, &profile, &RefreshWorkerOptions::default()).unwrap();
        let pre = consistency_snapshot(&store);

        // Mutate so the next refresh embeds, and kill the embedder early.
        write(root, "c.py", &multi_chunk_source("c", 3));
        write(root, "d.py", &multi_chunk_source("d", 4));
        let mut failing = FailingEmbedder { dim: profile.dim as usize, fail_after: 0, seen: 0 };
        let err = run_refresh(&store, root, &mut failing, &profile, &RefreshWorkerOptions::default())
            .unwrap_err();
        assert!(matches!(err, RefreshError::Embed(_)), "{err:?}");

        // Flag reset, and the data snapshot is EXACTLY the pre state.
        assert_eq!(meta_state(&store).refresh_state, "idle");
        let post = consistency_snapshot(&store);
        assert_eq!(post.0, pre.0, "no partial chunks landed");
        assert_eq!(post.1, pre.1, "no partial vectors landed");
        assert_eq!(post.5, pre.5, "no partial file sets landed");
        assert_eq!((post.2, post.3), (0, 0));
    }
}
