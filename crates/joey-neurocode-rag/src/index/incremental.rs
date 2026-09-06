//! Incremental change detection and refresh (T021–T023).
//!
//! **T021 — change detection** (data-model.md §6; research.md R3): an mtime
//! walk over the tree produces *candidates*; SHA-256 content hashing
//! *confirms* them. When mtime and content disagree, **the hash is the
//! authority**: a touched-but-identical file is NOT modified, and (with
//! [`DetectionOptions::trust_mtime`] off) an mtime-spoofed edit IS. The
//! output is [`ChangeDelta`] — the input to refresh.
//!
//! **T022 — git rename assist** (research.md R3): opportunistic rename/move
//! detection by shelling out to the git CLI (`std::process::Command` +
//! `which::which` detection), following the `crates/joey-tools/src/vcs.rs`
//! CheckpointManager pattern: `which` check at construction, explicit
//! `GIT_DIR`/`GIT_WORK_TREE` env, `current_dir(work_tree)`, config isolation
//! env, and a wall-clock timeout on every invocation. Rename pairs feed
//! [`ChangeDelta::renamed`]; when git is absent (or the pair never went
//! through the index/HEAD) the exact same content hash pairing still applies,
//! and anything unmatched degrades to remove+add — an equivalent end state
//! (data-model.md §6 `renamed` semantics).
//!
//! **T023 — incremental refresh** (FR-004/FR-005; data-model.md § Chunk
//! lifecycle): chunk-hash skip (equal `content_hash` ⇒ the chunk is NOT
//! re-embedded), purge of removed files' chunks relying on the
//! `ON DELETE CASCADE` FK to vectors (edges carry no FK by design — swept
//! explicitly), and the `neurocode.rag.refresh.max_files_per_turn` /
//! `max_bytes_per_turn` budgets with skipped work reported.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::index::chunker::normalize_source_path;

// ===========================================================================
// T021 — change detection (mtime walk + SHA-256 confirmation)
// ===========================================================================

/// Snapshot of one indexed file: repo-relative identity plus the mtime/size
/// fast-path fields and the SHA-256 content fingerprint that confirms them.
///
/// Produced by [`fingerprint_file`] / [`snapshot_tree`]; consumed by
/// [`detect_changes`] as the "previous state". The refresh worker (T024)
/// persists the authoritative copy alongside the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    /// Repository-relative, `/`-separated path (chunk-identity form).
    pub source_path: String,
    /// Last modification time at fingerprint time.
    pub mtime: SystemTime,
    /// Size in bytes at fingerprint time.
    pub size: u64,
    /// SHA-256 hex over the file's raw bytes.
    pub sha256: String,
}

/// Output of change detection (data-model.md §6 — exact field semantics):
/// input to refresh.
///
/// - `added` — new files to index.
/// - `modified` — files whose chunk hashes differ from stored.
/// - `removed` — files whose chunks+vectors are purged (FR-005).
/// - `renamed` — `(old, new)` pairs from git CLI rename detection when
///   available; otherwise detected as remove+add (equivalent end state).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeDelta {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub renamed: Vec<(PathBuf, PathBuf)>,
}

impl ChangeDelta {
    /// True when nothing changed — refresh can no-op.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.modified.is_empty()
            && self.removed.is_empty()
            && self.renamed.is_empty()
    }

    /// Total number of files needing work (renames count once).
    pub fn total_changed(&self) -> usize {
        self.added.len() + self.modified.len() + self.removed.len() + self.renamed.len()
    }
}

/// Knobs for [`detect_changes`].
#[derive(Debug, Clone)]
pub struct DetectionOptions {
    /// When `true` (default), a file whose mtime AND size equal the stored
    /// fingerprint is skipped without hashing — the mtime fast path (R3:
    /// "compare mtime vs indexed_at, confirm via content hash of changed
    /// candidates"). When `false`, every file is hashed and the hash alone
    /// decides — the deep mode that catches mtime-spoofed edits (same mtime,
    /// changed content). Hashing is ~3 orders of magnitude cheaper than
    /// re-embedding, so deep mode is always affordable.
    pub trust_mtime: bool,
}

impl Default for DetectionOptions {
    fn default() -> Self {
        Self { trust_mtime: true }
    }
}

/// Directory names never walked (VCS metadata, build output, dependencies).
const SKIPPED_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// The default indexability filter: a repo-relative path is indexable when
/// it is a supported source extension (the parse registry's set — the same
/// files the indexer can chunk) and lives under no skipped directory.
pub fn default_indexable_filter(rel: &Path) -> bool {
    let mut comps = rel.components();
    comps.all(|c| {
        !matches!(c, std::path::Component::Normal(name) if SKIPPED_DIRS.contains(&name.to_string_lossy().as_ref()))
    }) && joey_neurocode::parse::registry::is_supported_extension(
        rel.extension().and_then(|e| e.to_str()).unwrap_or(""),
    )
}

/// SHA-256 hex over raw bytes (lowercase, 64 chars).
fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn hex_lower(digest: &[u8]) -> String {
    digest.iter().fold(String::with_capacity(digest.len() * 2), |s, b| {
        s + &format!("{b:02x}")
    })
}

/// Fingerprint one file: read it, hash it, stat it. `rel` is the
/// repo-relative path recorded as [`FileFingerprint::source_path`].
pub fn fingerprint_file(root: &Path, rel: &Path) -> std::io::Result<FileFingerprint> {
    let abs = root.join(rel);
    let bytes = std::fs::read(&abs)?;
    let meta = std::fs::metadata(&abs)?;
    Ok(FileFingerprint {
        source_path: normalize_source_path(rel),
        mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        size: meta.len(),
        sha256: sha256_bytes(&bytes),
    })
}

/// Fingerprint every indexable file under `root` (the full-tree snapshot used
/// to seed "previous state" — first index, tests, and T024's persistence).
pub fn snapshot_tree(root: &Path, filter: &dyn Fn(&Path) -> bool) -> Vec<FileFingerprint> {
    let mut out = Vec::new();
    for rel in walk_relative(root, filter) {
        if let Ok(fp) = fingerprint_file(root, &rel) {
            out.push(fp);
        }
    }
    out
}

/// Deterministic (sorted) walk of repo-relative file paths passing `filter`.
fn walk_relative(root: &Path, filter: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !matches!(e.file_name().to_str(), Some(name) if SKIPPED_DIRS.contains(&name))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        if filter(rel) {
            out.push(rel.to_path_buf());
        }
    }
    out.sort();
    out
}

/// Detect changes between the previous fingerprints and the current tree
/// (data-model.md §6; research.md R3).
///
/// * Files on disk but not in `previous` → `added`.
/// * Files in `previous` but not on disk → `removed`.
/// * Files in both whose (mtime, size) differ from the fingerprint — or all
///   files, in deep mode — are hashed; `modified` iff the SHA-256 differs.
///   **The hash is the authority**: a touched-but-identical file (different
///   mtime, same content) is rejected as unchanged.
/// * `renamed` is left empty here — the git rename assist (T022) fills it;
///   unassisted renames surface as remove+add, an equivalent end state.
///
/// Unreadable files (permissions, races) are skipped entirely — a transient
/// stat/read failure must never purge a file's chunks.
pub fn detect_changes(
    root: &Path,
    previous: &[FileFingerprint],
    filter: &dyn Fn(&Path) -> bool,
    options: &DetectionOptions,
) -> ChangeDelta {
    let prev: HashMap<&str, &FileFingerprint> =
        previous.iter().map(|fp| (fp.source_path.as_str(), fp)).collect();

    let mut delta = ChangeDelta::default();
    let mut on_disk: HashMap<String, (SystemTime, u64)> = HashMap::new();

    for rel in walk_relative(root, filter) {
        let source_path = normalize_source_path(&rel);
        let Ok(meta) = std::fs::metadata(root.join(&rel)) else {
            continue; // raced away — neither added nor removed
        };
        on_disk.insert(
            source_path.clone(),
            (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len()),
        );

        match prev.get(source_path.as_str()) {
            None => delta.added.push(rel),
            Some(fp) => {
                let (mtime, size) = on_disk.get(&source_path).copied().unwrap();
                let mtime_candidate = mtime != fp.mtime || size != fp.size;
                if mtime_candidate || !options.trust_mtime {
                    // Confirm via content hash — the authority.
                    if let Ok(current) = fingerprint_file(root, &rel) {
                        if current.sha256 != fp.sha256 {
                            delta.modified.push(rel);
                        }
                    }
                }
            }
        }
    }

    // Removed: in previous, gone from disk (relative to the filter's world).
    let added_or_present: HashSet<&str> = on_disk.keys().map(|s| s.as_str()).collect();
    for fp in previous {
        if !added_or_present.contains(fp.source_path.as_str()) {
            delta.removed.push(PathBuf::from(fp.source_path.clone()));
        }
    }
    delta.removed.sort();

    delta.added.sort();
    delta.modified.sort();
    delta
}

// ===========================================================================
// T022 — git-CLI rename assist (CheckpointManager pattern, crates/joey-tools/src/vcs.rs)
// ===========================================================================

/// Wall-clock timeout on every git subprocess invocation — a hung git call
/// must never stall a refresh (mirrors vcs.rs `GIT_TIMEOUT`).
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Opportunistic rename/move detection via the git CLI, following the
/// `crates/joey-tools/src/vcs.rs` CheckpointManager pattern: `which::which`
/// detection at construction, `GIT_DIR`/`GIT_WORK_TREE` env +
/// `current_dir(work_tree)` on every invocation, config isolation, and a
/// bounded timeout. Construction is cheap and performs no filesystem work;
/// an unavailable or failing git simply yields no rename pairs (remove+add
/// fallback — equivalent end state).
pub struct GitRenameAssist {
    work_tree: PathBuf,
    /// True if git was found (CheckpointManager's `enabled` probe).
    enabled: bool,
}

impl GitRenameAssist {
    /// Probe `git` on `PATH` for `work_tree` (the CheckpointManager::new
    /// pattern — cheap, lazy, no mutation).
    pub fn new(work_tree: &Path) -> Self {
        let enabled = which::which("git").is_ok();
        if !enabled {
            eprintln!("[joey neurocode] git not found — rename assist falls back to remove+add");
        }
        Self { work_tree: work_tree.to_path_buf(), enabled }
    }

    /// Force-disabled assist (tests / forced degradation): every detection
    /// degrades to the remove+add fallback end state.
    pub fn new_disabled(work_tree: &Path) -> Self {
        Self { work_tree: work_tree.to_path_buf(), enabled: false }
    }

    /// Whether git was found on `PATH`.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Detect `(old, new)` rename pairs among the removed/added candidates.
    ///
    /// Returns an empty vec when git was not found — the remove+add fallback
    /// (data-model.md §6: rename pairs come "from git CLI rename detection
    /// when available; otherwise detected as remove+add" — equivalent end
    /// state). With git available, two independent sources are combined:
    ///
    /// 1. **Exact content pairing** (no diff required): a removed path's
    ///    last-known SHA-256 equal to an added path's current SHA-256 pairs
    ///    them deterministically — catches committed renames and plain
    ///    filesystem moves that never touch the git index.
    /// 2. **git similarity detection**: staged renames — `git mv`, or move +
    ///    `git add` — appear as `R` records in
    ///    `git diff --cached --find-renames --name-status -z`, catching
    ///    edited-but-similar moves the exact-hash rule cannot.
    ///
    /// Only pairs whose old side is in `removed` AND new side is in `added`
    /// are returned; git pairs take precedence over hash pairs.
    pub fn detect_renames(
        &self,
        root: &Path,
        previous: &[FileFingerprint],
        removed: &[PathBuf],
        added: &[PathBuf],
    ) -> Vec<(PathBuf, PathBuf)> {
        if !self.enabled {
            return Vec::new(); // no git ⇒ remove+add (equivalent end state)
        }
        let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut claimed_new: HashSet<String> = HashSet::new();

        // ── 1. Exact content pairing (no git required) ────────────────────
        let removed_set: HashSet<String> =
            removed.iter().map(|p| normalize_source_path(p)).collect();
        let prev_by_sha: HashMap<&str, &str> = previous
            .iter()
            .filter(|fp| removed_set.contains(&fp.source_path))
            .map(|fp| (fp.sha256.as_str(), fp.source_path.as_str()))
            .collect();
        for rel in added {
            if let Ok(fp) = fingerprint_file(root, rel) {
                if let Some(old) = prev_by_sha.get(fp.sha256.as_str()) {
                    claimed_new.insert(fp.source_path.clone());
                    pairs.push((PathBuf::from(old.to_string()), rel.clone()));
                }
            }
        }

        // ── 2. git similarity detection (staged renames vs HEAD) ─────────
        for (old, new) in self.git_staged_renames() {
            let (old, new) = (normalize_source_path(&old), normalize_source_path(&new));
            let old_in_removed = removed_set.contains(&old);
            let new_in_added = added.iter().any(|p| normalize_source_path(p) == new);
            if old_in_removed && new_in_added && !claimed_new.contains(&new) {
                // A git pair supersedes any exact-hash pair that claimed
                // this old path (git similarity is the richer signal).
                pairs.retain(|(o, _)| normalize_source_path(o) != old);
                claimed_new.insert(new.clone());
                pairs.push((PathBuf::from(old), PathBuf::from(new)));
            }
        }

        pairs.sort();
        pairs
    }

    /// Run `git diff --cached --find-renames --name-status -z` against the
    /// work tree's own repository and return the `R` (rename) records as
    /// `(old, new)` relative paths. Failure of any kind (no repo, no HEAD,
    /// timeout, exit code) yields an empty vec — the assist degrades
    /// gracefully, never errors.
    fn git_staged_renames(&self) -> Vec<(PathBuf, PathBuf)> {
        let git_dir = self.work_tree.join(".git");
        let mut cmd = std::process::Command::new("git");
        // CheckpointManager pattern: explicit GIT_DIR/GIT_WORK_TREE env,
        // current_dir(work_tree), config isolation.
        cmd.env("GIT_DIR", &git_dir);
        cmd.env("GIT_WORK_TREE", &self.work_tree);
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.current_dir(&self.work_tree);
        cmd.args([
            "diff",
            "--cached",
            "--find-renames",
            "--name-status",
            "--diff-filter=R",
            "-z",
        ]);
        let Ok(out) = run_git_capture(cmd) else {
            return Vec::new();
        };
        parse_rename_records(&out)
    }
}

/// Parse `--name-status -z` output restricted to renames: NUL-separated
/// tokens where each rename record is `R<score>\0<old>\0<new>\0`.
fn parse_rename_records(z_output: &str) -> Vec<(PathBuf, PathBuf)> {
    let tokens: Vec<&str> = z_output.split('\0').filter(|t| !t.is_empty()).collect();
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].starts_with('R') && i + 2 < tokens.len() {
            pairs.push((PathBuf::from(tokens[i + 1]), PathBuf::from(tokens[i + 2])));
            i += 3;
        } else {
            i += 1;
        }
    }
    pairs
}

/// Run a prepared git command with the vcs.rs wall-clock timeout pattern
/// (pipe-drain threads + poll loop + kill on timeout); stdout on exit code 0.
fn run_git_capture(mut cmd: std::process::Command) -> Result<String, String> {
    use std::io::Read;
    use std::process::Stdio;

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child: std::process::Child = cmd.spawn().map_err(|e| e.to_string())?;
    let start = std::time::Instant::now();

    // Drain both pipes on dedicated threads BEFORE polling (a git invocation
    // writing more than the OS pipe buffer blocks otherwise — vcs.rs lesson).
    let stdout_pipe = child.stdout.take().ok_or("stdout not piped")?;
    let stderr_pipe = child.stderr.take().ok_or("stderr not piped")?;
    let out_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::BufReader::new(stdout_pipe).read_to_string(&mut buf);
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::BufReader::new(stderr_pipe).read_to_string(&mut buf);
        buf
    });

    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None => {
                if start.elapsed() >= GIT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("git subprocess timed out after {GIT_TIMEOUT:?}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    if status.code() != Some(0) {
        return Err(format!(
            "git exited with code {}: {}",
            status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }
    Ok(stdout.trim_end_matches('\0').to_string())
}

/// [`detect_changes`] plus the git rename assist (T022): matched `(old, new)`
/// pairs move from `removed`/`added` into `renamed`. Unmatched pairs keep
/// the remove+add shape — the equivalent end state (data-model.md §6).
pub fn detect_changes_with_rename_assist(
    root: &Path,
    previous: &[FileFingerprint],
    filter: &dyn Fn(&Path) -> bool,
    options: &DetectionOptions,
    assist: &GitRenameAssist,
) -> ChangeDelta {
    let mut delta = detect_changes(root, previous, filter, options);
    let pairs = assist.detect_renames(root, previous, &delta.removed, &delta.added);
    if pairs.is_empty() {
        return delta;
    }
    let pair_set: HashSet<(String, String)> = pairs
        .iter()
        .map(|(o, n)| (normalize_source_path(o), normalize_source_path(n)))
        .collect();
    delta.removed.retain(|p| !pair_set.iter().any(|(o, _)| *o == normalize_source_path(p)));
    delta.added.retain(|p| !pair_set.iter().any(|(_, n)| *n == normalize_source_path(p)));
    delta.renamed = pairs;
    delta
}

// ===========================================================================
// T023 — incremental refresh (chunk-hash skip, cascade purge, budgets)
// ===========================================================================

/// Errors from the incremental refresh path.
#[derive(Debug)]
pub enum RefreshError {
    /// SQLite failure (the refresh transaction rolls back).
    Sql(rusqlite::Error),
    /// The embedder boundary failed mid-refresh.
    Embed(String),
    /// A file to (re)index could not be read/parsed.
    Io(std::io::Error),
    /// The parse layer rejected a file (language extractor error string).
    Parse(String),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::Sql(e) => write!(f, "refresh sql error: {e}"),
            RefreshError::Embed(e) => write!(f, "refresh embed failure: {e}"),
            RefreshError::Io(e) => write!(f, "refresh io failure: {e}"),
            RefreshError::Parse(e) => write!(f, "refresh parse failure: {e}"),
        }
    }
}

impl std::error::Error for RefreshError {}

impl From<rusqlite::Error> for RefreshError {
    fn from(e: rusqlite::Error) -> Self {
        RefreshError::Sql(e)
    }
}

/// What one incremental refresh actually did (T024's refresh worker and
/// FR-013 status reporting consume this).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshOutcome {
    /// Files newly indexed (from `delta.added` + rename new-sides).
    pub files_indexed: usize,
    /// Files re-indexed because at least one chunk hash changed
    /// (`delta.modified`).
    pub files_reindexed: usize,
    /// Files whose chunks+vectors were purged (`delta.removed` + rename
    /// old-sides), cascading via the FK (FR-005).
    pub files_purged: usize,
    /// Rename pairs migrated: old-path chunk ids purged, new-path chunks
    /// created (end state identical to remove+add — data-model.md § Chunk
    /// lifecycle "migrated").
    pub files_renamed: usize,
    /// Individual chunks skipped because `content_hash` was unchanged
    /// (FR-004 chunk-level skip — the SC-004 core metric).
    pub chunks_skipped: usize,
    /// Individual chunks (re-)embedded this refresh.
    pub chunks_embedded: usize,
    /// Files deferred because the per-turn budgets were exhausted
    /// (FR-004/005: report what was skipped).
    pub files_deferred: usize,
}

/// Per-turn refresh budgets (FR-004/FR-005): cap work per refresh and defer
/// the remainder to the next turn. Field names mirror the config keys
/// `neurocode.rag.refresh.max_files_per_turn` / `max_bytes_per_turn`
/// (defaults [`crate::config::DEFAULT_REFRESH_MAX_FILES_PER_TURN`] = 50 and
/// [`crate::config::DEFAULT_REFRESH_MAX_BYTES_PER_TURN`] = 50 MiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshBudgets {
    pub max_files_per_turn: usize,
    pub max_bytes_per_turn: u64,
}

impl Default for RefreshBudgets {
    fn default() -> Self {
        Self {
            max_files_per_turn: crate::config::DEFAULT_REFRESH_MAX_FILES_PER_TURN as usize,
            max_bytes_per_turn: crate::config::DEFAULT_REFRESH_MAX_BYTES_PER_TURN as u64,
        }
    }
}

impl RefreshBudgets {
    /// Build from a loaded [`crate::config::RagConfig`] (its two
    /// `refresh.*` keys, already defaulted/clamped by the config layer).
    pub fn from_config(config: &crate::config::RagConfig) -> Self {
        Self {
            max_files_per_turn: config.refresh_max_files_per_turn.max(0) as usize,
            max_bytes_per_turn: config.refresh_max_bytes_per_turn.max(0) as u64,
        }
    }
}

/// Stored per-chunk comparison state: `(chunk_id, content_hash, embed_dim)`
/// in rowid order — the chunk-hash skip's comparison set, plus the row's
/// embedded dimension so chunks written by a different embedding profile are
/// never mistaken for reusable.
struct StoredChunk {
    chunk_id: String,
    content_hash: String,
    embed_dim: Option<i64>,
}

/// Load the stored chunk state for one source path, in rowid order.
fn stored_chunks_for_path(conn: &rusqlite::Connection, source_path: &str)
    -> rusqlite::Result<Vec<StoredChunk>> {
    let mut stmt = conn.prepare(
        "SELECT chunk_id, content_hash, embed_dim FROM rag_chunks WHERE source_path = ?1 ORDER BY rowid",
    )?;
    let rows = stmt.query_map(rusqlite::params![source_path], |r| {
        Ok(StoredChunk {
            chunk_id: r.get(0)?,
            content_hash: r.get(1)?,
            embed_dim: r.get(2)?,
        })
    })?;
    rows.collect()
}

/// The per-file work computed by change detection + chunk-hash comparison —
/// the unit the budget gate admits or defers.
#[derive(Debug, Default)]
struct FileWork {
    /// Repo-relative path (`/`-separated).
    source_path: String,
    /// Built chunk records (parse + chunk) for the current file content.
    records: Vec<crate::index::chunker::ChunkRecord>,
    /// Indices into `records` whose `content_hash` already matches a stored
    /// row — SKIPPED: not re-embedded, not rewritten.
    unchanged: Vec<usize>,
    /// Indices needing (re-)embedding (new or changed hash).
    changed: Vec<usize>,
}

/// Parse + chunk one file and split its chunks into unchanged (hash-skip)
/// vs changed. `None` when the file cannot be read or has no supported
/// language extractor (left untouched rather than purged — a transient or
/// out-of-scope file must never lose its chunks).
fn compute_file_work(
    root: &Path,
    rel: &Path,
    store: &joey_neurocode::graph::GraphStore,
    options: &crate::index::chunker::ChunkOptions,
    profile: &crate::embed::profiles::EmbedProfile,
) -> Option<FileWork> {
    let abs = root.join(rel);
    let source = std::fs::read_to_string(&abs).ok()?;
    let source_path = crate::index::chunker::normalize_source_path(rel);
    let mut extraction = match joey_neurocode::parse::registry::parse_any(&abs, &source) {
        None => return None, // unsupported extension — out of RAG scope
        Some(Err(e)) => {
            eprintln!("[joey neurocode] refresh parse skip {source_path}: {e}");
            return None; // extractor error — keep previous chunks, skip file
        }
        Some(Ok(ex)) => ex,
    };
    extraction.populate_fallback_chunks(&source);
    let records = crate::index::chunker::build_chunk_records(
        &extraction, &source, &source_path, store, options,
    );

    // A brand-new file has no stored rows, so every chunk lands in
    // `changed` naturally; a tracked file skips exactly the chunks whose
    // recomputed identity AND hash match a stored row.
    let stored = stored_chunks_for_path(store.conn(), &source_path).unwrap_or_default();
    // Skip set keyed by CHUNK IDENTITY, not content hash: a symbol that
    // moved to new lines in a modified file rebuilds with a NEW chunk_id
    // (line-range identity — chunker.rs `make_chunk_id`) even when its
    // body, hence content_hash, is byte-identical. Keying the skip by
    // content hash alone marked such a moved chunk unchanged (never
    // written) while the stale purge below deleted the old chunk_id row —
    // the chunk VANISHED from the index. Skip only when the stored row
    // with the SAME chunk_id carries the same content_hash, and only when
    // that row was embedded with the CURRENT profile's dimension (a chunk
    // carried over from another embedding profile — backend switch
    // without a full rebuild — must re-embed, never skip, or dense search
    // keeps a stale dimension-mismatched vector).
    let stored_by_id: HashMap<&str, (&str, Option<i64>)> = stored
        .iter()
        .map(|c| (c.chunk_id.as_str(), (c.content_hash.as_str(), c.embed_dim)))
        .collect();
    let profile_dim = profile.dim as i64;

    let mut work =
        FileWork { source_path, records, unchanged: Vec::new(), changed: Vec::new() };
    for (i, rec) in work.records.iter().enumerate() {
        let reusable = stored_by_id
            .get(rec.chunk_id.as_str())
            .is_some_and(|(hash, dim)| {
                *hash == rec.content_hash.as_str() && *dim == Some(profile_dim)
            });
        if reusable {
            work.unchanged.push(i);
        } else {
            work.changed.push(i);
        }
    }
    Some(work)
}

/// One incremental refresh of `store` from the tree at `root`, driven by
/// `delta` (T021/T022) and bounded by `budgets` (FR-004/FR-005).
///
/// Per data-model.md § Chunk lifecycle:
///
/// - **unchanged** — stored `content_hash` == recomputed hash ⇒ row
///   untouched, NOT re-embedded (the embedder never sees it).
/// - **re-embedded** — hash differs ⇒ new vector written in-transaction.
/// - **new / migrated** — added files and rename new-sides get fresh chunks.
/// - **purged** — removed files and rename old-sides: chunk rows deleted,
///   cascading to vectors via the `ON DELETE CASCADE` FK; derived
///   `rag_chunk_edges` carry no FK BY DESIGN (T005) and are swept explicitly
///   so no orphans remain.
///
/// All DB writes happen in ONE transaction — COMMIT is the swap point
/// (FR-004; the atomic-swap coordination itself is T024's worker).
///
/// Files beyond the budgets are DEFERRED (reported in
/// [`RefreshOutcome::files_deferred`]), never dropped: the next refresh
/// re-detects them.
pub fn refresh_incremental(
    store: &joey_neurocode::graph::GraphStore,
    root: &Path,
    delta: &ChangeDelta,
    embedder: &mut dyn crate::index::chunker::ChunkEmbedder,
    profile: &crate::embed::profiles::EmbedProfile,
    quantization: crate::vector::quantize::Quantization,
    chunk_options: &crate::index::chunker::ChunkOptions,
    budgets: &RefreshBudgets,
) -> Result<RefreshOutcome, RefreshError> {
    let mut outcome = RefreshOutcome::default();
    let mut files_used = 0usize;
    let mut bytes_used = 0u64;

    // Purges are cheap (no read/parse/embed) and never starve: they run
    // before the budget gate so a wave of deletions can't be starved by a
    // large modify batch, and FR-005 (removed entries purged) holds even
    // when the file budget is exhausted.
    let mut purge_paths: Vec<String> =
        delta.removed.iter().map(|p| crate::index::chunker::normalize_source_path(p)).collect();
    purge_paths.extend(
        delta.renamed.iter().map(|(old, _)| crate::index::chunker::normalize_source_path(old)),
    );
    outcome.files_purged = purge_paths.len();
    outcome.files_renamed = delta.renamed.len();

    // Work queue: renamed-new-sides and adds are new files; modified files
    // get chunk-hash comparison. Order: renames (migration semantics first),
    // then adds, then modifies — stable, deterministic.
    let mut queue: Vec<(PathBuf, bool)> = Vec::new(); // (rel, is_new)
    for (_, new) in &delta.renamed {
        queue.push((new.clone(), true));
    }
    for p in &delta.added {
        queue.push((p.clone(), true));
    }
    for p in &delta.modified {
        queue.push((p.clone(), false));
    }

    // ONE transaction over everything this refresh writes.
    let conn = store.conn();
    let tx = conn.unchecked_transaction()?;

    // ── Purge (inside the transaction) ───────────────────────────────────
    for path in &purge_paths {
        tx.execute("DELETE FROM rag_chunks WHERE source_path = ?1", rusqlite::params![path])?;
        // rag_chunk_edges has no FK by design (T005) — sweep explicitly.
        tx.execute(
            "DELETE FROM rag_chunk_edges WHERE from_chunk_id NOT IN (SELECT chunk_id FROM rag_chunks)
             OR to_chunk_id NOT IN (SELECT chunk_id FROM rag_chunks)",
            [],
        )?;
    }

    // ── Index/re-index under the budgets ────────────────────────────────
    let mut deferred: Vec<PathBuf> = Vec::new();
    for (rel, _is_new) in queue {
        let Ok(meta) = std::fs::metadata(root.join(&rel)) else {
            continue; // raced away — next refresh re-detects
        };
        let file_bytes = meta.len().max(1);
        let over_files = files_used >= budgets.max_files_per_turn;
        let over_bytes = bytes_used + file_bytes > budgets.max_bytes_per_turn;
        if over_files || over_bytes {
            deferred.push(rel);
            continue;
        }

        let Some(work) = compute_file_work(root, &rel, store, chunk_options, profile) else {
            // Unparseable/unreadable: not admitted against the budget, not
            // purged — the file keeps its previous chunks.
            continue;
        };

        // Embed ONLY the changed chunks (chunk-hash skip, FR-004/SC-004),
        // in bounded EMBED_BATCH_SIZE batches — the same bound index_file
        // applies (chunker.rs; the backend contract's batch-64 default,
        // research.md R2). One unbounded call embedded an entire changed
        // file's chunks in a single request, defeating that bound.
        let mut texts: Vec<String> = Vec::with_capacity(work.changed.len());
        for &i in &work.changed {
            texts.push(profile.document_input(&work.records[i].embed_text));
        }
        let vectors = if texts.is_empty() {
            Vec::new()
        } else {
            let mut embedded: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
            for batch in texts.chunks(crate::index::chunker::EMBED_BATCH_SIZE) {
                let part = embedder
                    .embed_texts(batch)
                    .map_err(RefreshError::Embed)?;
                if part.len() != batch.len() {
                    return Err(RefreshError::Embed(format!(
                        "embedder returned {} vectors for {} texts",
                        part.len(),
                        batch.len()
                    )));
                }
                embedded.extend(part);
            }
            if embedded.len() != texts.len() {
                return Err(RefreshError::Embed(format!(
                    "embedder returned {} vectors for {} texts",
                    embedded.len(),
                    texts.len()
                )));
            }
            embedded
        };

        // Delete the path's stale chunks that no longer exist in the rebuilt
        // record set (shifted/moved chunks get new ids — data-model.md §1),
        // then upsert the current records. Unchanged chunks are neither
        // re-embedded nor rewritten (their rows/vectors stay untouched).
        let current_ids: std::collections::HashSet<&str> =
            work.records.iter().map(|r| r.chunk_id.as_str()).collect();
        let stale: Vec<String> = stored_chunks_for_path(&tx, &work.source_path)?
            .into_iter()
            .filter(|c| !current_ids.contains(c.chunk_id.as_str()))
            .map(|c| c.chunk_id)
            .collect();
        for id in &stale {
            tx.execute("DELETE FROM rag_chunks WHERE chunk_id = ?1", rusqlite::params![id])?;
        }
        if !stale.is_empty() {
            // Derived edges referencing purged chunks must not survive.
            tx.execute(
                "DELETE FROM rag_chunk_edges WHERE from_chunk_id NOT IN (SELECT chunk_id FROM rag_chunks)
                 OR to_chunk_id NOT IN (SELECT chunk_id FROM rag_chunks)",
                [],
            )?;
        }

        let now = chrono::Utc::now().to_rfc3339();
        {
            let mut chunk_stmt = tx.prepare(
                r#"
                INSERT INTO rag_chunks
                    (chunk_id, chunk_kind, artifact_id, source_path, start_line, end_line,
                     language, symbol_name, symbol_kind, content_hash, embed_model, embed_dim,
                     updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                ON CONFLICT(chunk_id) DO UPDATE SET
                    artifact_id   = excluded.artifact_id,
                    start_line    = excluded.start_line,
                    end_line      = excluded.end_line,
                    language      = excluded.language,
                    symbol_name   = excluded.symbol_name,
                    symbol_kind   = excluded.symbol_kind,
                    content_hash  = excluded.content_hash,
                    embed_model   = excluded.embed_model,
                    embed_dim     = excluded.embed_dim,
                    updated_at    = excluded.updated_at
                "#,
            )?;
            let mut vec_stmt = tx.prepare(
                r#"
                INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(chunk_id) DO UPDATE SET
                    dim          = excluded.dim,
                    quantization = excluded.quantization,
                    vector       = excluded.vector
                "#,
            )?;
            for (i, rec) in work.records.iter().enumerate() {
                if work.unchanged.contains(&i) {
                    continue; // hash equal ⇒ untouched (no re-embed, no rewrite)
                }
                let (kind_str, artifact_id, symbol_name, symbol_kind) = match &rec.kind {
                    crate::index::chunker::ChunkKind::Symbol {
                        artifact_id, symbol_name, symbol_kind,
                    } => ("symbol", *artifact_id, Some(symbol_name.as_str()), Some(symbol_kind.as_str())),
                    crate::index::chunker::ChunkKind::Fallback => ("fallback", None, None, None),
                };
                let pos = work.changed.iter().position(|&c| c == i).expect("changed index");
                let v = &vectors[pos];
                if v.len() != profile.dim as usize {
                    return Err(RefreshError::Embed(format!(
                        "vector dim mismatch for chunk {}: expected {}, got {}",
                        rec.chunk_id, profile.dim, v.len()
                    )));
                }
                chunk_stmt.execute(rusqlite::params![
                    rec.chunk_id,
                    kind_str,
                    artifact_id,
                    rec.source_path,
                    rec.start_line as i64,
                    rec.end_line as i64,
                    rec.language,
                    symbol_name,
                    symbol_kind,
                    rec.content_hash,
                    profile.name,
                    profile.dim as i64,
                    now,
                ])?;
                let blob = crate::vector::quantize::encode(v, quantization);
                vec_stmt.execute(rusqlite::params![
                    rec.chunk_id,
                    v.len() as i64,
                    quantization.as_str(),
                    blob
                ])?;
            }
        }

        // Outcome accounting.
        if delta.added.iter().any(|p| crate::index::chunker::normalize_source_path(p) == work.source_path)
            || delta.renamed.iter().any(|(_, n)| crate::index::chunker::normalize_source_path(n) == work.source_path)
        {
            outcome.files_indexed += 1;
        } else {
            outcome.files_reindexed += 1;
        }
        outcome.chunks_skipped += work.unchanged.len();
        outcome.chunks_embedded += work.changed.len();
        files_used += 1;
        bytes_used += file_bytes;
    }

    // ── Deferred-work accounting (FR-004/005: report what was skipped) ──
    outcome.files_deferred = deferred.len();

    // Keep the denormalized status counter honest (FR-013 groundwork).
    tx.execute(
        "UPDATE rag_index_meta
         SET chunk_count = (SELECT COUNT(*) FROM rag_chunks), last_refresh_at = ?1
         WHERE id = 1",
        rusqlite::params![chrono::Utc::now().to_rfc3339()],
    )?;

    tx.commit()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn set_mtime(rel_path: &Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(rel_path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn default_filter_skips_unsupported_and_hidden_dirs() {
        assert!(default_indexable_filter(Path::new("src/app.py")));
        assert!(default_indexable_filter(Path::new("Main.hs")));
        assert!(!default_indexable_filter(Path::new("README.md")));
        assert!(!default_indexable_filter(Path::new(".git/config")));
        assert!(!default_indexable_filter(Path::new("target/debug/main.rs")));
        assert!(!default_indexable_filter(Path::new("node_modules/x.js")));
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parse_rename_records_z_format() {
        let z = "R100\0src/old.py\0src/new.py\0R087\0a.rs\0b.rs\0";
        let pairs = parse_rename_records(z);
        assert_eq!(
            pairs,
            vec![
                (PathBuf::from("src/old.py"), PathBuf::from("src/new.py")),
                (PathBuf::from("a.rs"), PathBuf::from("b.rs")),
            ]
        );
        assert!(parse_rename_records("").is_empty());
    }

    #[test]
    fn detect_added_modified_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.py", "x = 1\n");
        write(root, "b.py", "y = 2\n");
        let previous = snapshot_tree(root, &default_indexable_filter);
        assert_eq!(previous.len(), 2);

        write(root, "c.py", "z = 3\n"); // added
        write(root, "b.py", "y = 22\n"); // modified
        std::fs::remove_file(root.join("a.py")).unwrap(); // removed

        let delta = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions::default());
        assert_eq!(delta.added, vec![PathBuf::from("c.py")]);
        assert_eq!(delta.modified, vec![PathBuf::from("b.py")]);
        assert_eq!(delta.removed, vec![PathBuf::from("a.py")]);
        assert!(delta.renamed.is_empty());
        assert_eq!(delta.total_changed(), 3);
    }

    #[test]
    fn no_changes_is_empty_delta() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "a.py", "x = 1\n");
        let previous = snapshot_tree(root, &default_indexable_filter);
        let delta = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions::default());
        assert!(delta.is_empty());
    }

    /// mtime false positive REJECTED via hash: content identical, mtime
    /// different ⇒ NOT modified (the hash is the authority).
    #[test]
    fn same_content_different_mtime_not_modified() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "m.py", "x = 1\n");
        let previous = snapshot_tree(root, &default_indexable_filter);
        // Touch only: same bytes, new mtime.
        set_mtime(&root.join("m.py"), SystemTime::now() + std::time::Duration::from_secs(120));
        let delta = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions::default());
        assert!(delta.modified.is_empty(), "hash must reject the mtime false positive: {delta:?}");
        assert!(delta.is_empty());
    }

    /// mtime spoof (same mtime, changed content): the hash — the authority —
    /// flags the edit even though mtime is restored (the size fast-path
    /// trigger escalates to hash confirmation).
    #[test]
    fn same_mtime_changed_content_caught_by_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "m.py", "x = 1\n");
        let previous = snapshot_tree(root, &default_indexable_filter);
        let old_mtime = previous[0].mtime;
        write(root, "m.py", "x = 111\n"); // content changes…
        set_mtime(&root.join("m.py"), old_mtime); // …mtime restored (spoofed)
        let delta = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions::default());
        assert_eq!(delta.modified, vec![PathBuf::from("m.py")], "hash is the authority");
    }

    /// The pathological spoof — mtime AND size both restored — escapes the
    /// mtime/size fast path; deep mode (`trust_mtime: false`) hashes
    /// everything and the hash still decides.
    #[test]
    fn equal_size_mtime_spoof_caught_in_deep_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "m.py", "x = 1\n");
        let previous = snapshot_tree(root, &default_indexable_filter);
        let old_mtime = previous[0].mtime;
        write(root, "m.py", "y = 1\n"); // content changes, SIZE stays equal…
        set_mtime(&root.join("m.py"), old_mtime); // …mtime restored (spoofed)
        // Fast path: trusted mtime+size hides the edit (documented R3 trade-off).
        let fast = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions::default());
        assert!(fast.modified.is_empty());
        // Deep mode: hash is the authority.
        let deep = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions { trust_mtime: false });
        assert_eq!(deep.modified, vec![PathBuf::from("m.py")]);
    }

    // ── T022: git rename assist ──────────────────────────────────────────

    /// Run git in `dir` (isolation env, bounded by the same timeout idea —
    /// plain `output()` is fine for the test-side SETUP calls).
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "--quiet"]);
        git(root, &["add", "--all"]);
        git(root, &["commit", "--quiet", "-m", "init"]);
    }

    /// A staged `git mv` rename is detected as a rename pair, not remove+add
    /// (git similarity detection source 2).
    #[test]
    fn staged_git_mv_rename_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "old.py", "x = 1\n");
        init_repo(root);
        let previous = snapshot_tree(root, &default_indexable_filter);

        git(root, &["mv", "old.py", "new.py"]); // staged rename
        // Pre-assist sanity: raw detection sees remove+add…
        let raw = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions::default());
        assert_eq!(raw.removed, vec![PathBuf::from("old.py")]);
        assert_eq!(raw.added, vec![PathBuf::from("new.py")]);
        // …the assist pairs it into `renamed` instead.
        let delta = detect_changes_with_rename_assist(
            root,
            &previous,
            &default_indexable_filter,
            &DetectionOptions::default(),
            &GitRenameAssist::new(root),
        );
        assert_eq!(delta.renamed, vec![(PathBuf::from("old.py"), PathBuf::from("new.py"))]);
        assert!(delta.removed.is_empty(), "paired old side leaves removed: {delta:?}");
        assert!(delta.added.is_empty(), "paired new side leaves added: {delta:?}");
    }

    /// A rename with NO git assistance at all — plain filesystem move in a
    /// non-repo — still pairs via exact content hash, but only when git is
    /// available per data-model.md §6 (`renamed` requires git); with git
    /// forced off the end state is remove+add.
    #[test]
    fn no_git_fallback_is_remove_plus_add() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "old.py", "x = 1\n");
        let previous = snapshot_tree(root, &default_indexable_filter);
        std::fs::rename(root.join("old.py"), root.join("new.py")).unwrap();

        // Disabled assist (simulated git absence): remove+add end state.
        let delta = detect_changes_with_rename_assist(
            root,
            &previous,
            &default_indexable_filter,
            &DetectionOptions::default(),
            &GitRenameAssist::new_disabled(root),
        );
        assert_eq!(delta.removed, vec![PathBuf::from("old.py")]);
        assert_eq!(delta.added, vec![PathBuf::from("new.py")]);
        assert!(delta.renamed.is_empty(), "no git ⇒ no rename pairs (§6)");
    }

    /// With git present, a committed rename (no staged state left) is paired
    /// by exact content hash — the hash-based source catches what the staged
    /// diff cannot.
    #[test]
    fn committed_rename_paired_by_exact_hash_when_git_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "old.py", "x = 1\n");
        init_repo(root);
        let previous = snapshot_tree(root, &default_indexable_filter);

        // Filesystem move, then stage AND commit — nothing left in the index
        // diff, only the exact-hash source can pair it.
        std::fs::rename(root.join("old.py"), root.join("new.py")).unwrap();
        git(root, &["add", "--all"]);
        git(root, &["commit", "--quiet", "-m", "rename"]);

        let delta = detect_changes_with_rename_assist(
            root,
            &previous,
            &default_indexable_filter,
            &DetectionOptions::default(),
            &GitRenameAssist::new(root),
        );
        assert_eq!(
            delta.renamed,
            vec![(PathBuf::from("old.py"), PathBuf::from("new.py"))],
            "exact-hash pairing (git present)"
        );
        assert!(delta.removed.is_empty());
        assert!(delta.added.is_empty());
    }

    /// Disabled assist in a REAL git repo: even with a perfectly staged
    /// rename on disk, no git ⇒ remove+add (the fallback end state must not
    /// depend on repository state).
    #[test]
    fn disabled_assist_in_real_repo_still_remove_plus_add() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "old.py", "x = 1\n");
        init_repo(root);
        let previous = snapshot_tree(root, &default_indexable_filter);
        git(root, &["mv", "old.py", "new.py"]);
        let delta = detect_changes_with_rename_assist(
            root,
            &previous,
            &default_indexable_filter,
            &DetectionOptions::default(),
            &GitRenameAssist::new_disabled(root),
        );
        assert!(delta.renamed.is_empty());
        assert_eq!(delta.removed, vec![PathBuf::from("old.py")]);
        assert_eq!(delta.added, vec![PathBuf::from("new.py")]);
    }

    // ── T023: incremental refresh ────────────────────────────────────────

    use joey_neurocode::graph::GraphStore;
    use crate::embed::profiles::default_profile;
    use crate::index::chunker::ChunkOptions;
    use crate::vector::quantize::Quantization;

    /// Counting embedder wrapper: records every text handed in.
    struct CountingEmbedder {
        dim: usize,
        texts_seen: Vec<String>,
    }

    impl CountingEmbedder {
        fn new(dim: usize) -> Self {
            Self { dim, texts_seen: Vec::new() }
        }
    }

    impl crate::index::chunker::ChunkEmbedder for CountingEmbedder {
        fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            self.texts_seen.extend_from_slice(texts);
            Ok(texts
                .iter()
                .map(|t| {
                    let v: Vec<f32> = (0..self.dim)
                        .map(|i| ((t.len() as f32 * 0.001 + i as f32) % 7.0) / 7.0)
                        .collect();
                    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                    v.into_iter().map(|x| x / norm).collect()
                })
                .collect())
        }
    }

    fn temp_store() -> (tempfile::TempDir, GraphStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
        (tmp, store)
    }

    fn delta_added(paths: &[&str]) -> ChangeDelta {
        ChangeDelta {
            added: paths.iter().map(PathBuf::from).collect(),
            ..ChangeDelta::default()
        }
    }

    /// THE chunk-hash-skip pin: editing one region of a file re-embeds ONLY
    /// the chunks whose hash changed; untouched chunks are neither
    /// re-embedded nor rewritten (FR-004/SC-004).
    #[test]
    fn unchanged_chunks_are_not_re_embedded() {
        let (tmp, store) = temp_store();
        let root = tmp.path();
        let src = "def handler():\n    return 42\n\nx = 1\n";
        write(root, "m.py", src);

        let profile = default_profile();
        // First index: everything embedded.
        let mut embedder = CountingEmbedder::new(profile.dim as usize);
        let first = refresh_incremental(
            &store, root, &delta_added(&["m.py"]), &mut embedder, profile,
            Quantization::F32, &ChunkOptions::default(), &RefreshBudgets::default(),
        )
        .unwrap();
        assert!(first.chunks_skipped == 0, "first index embeds all: {first:?}");
        let first_texts = embedder.texts_seen.len();
        assert!(first_texts >= 2, "symbol + fallback chunks: {first_texts}");
        assert_eq!(first.chunks_embedded, first_texts);

        // Capture the symbol chunk's row BEFORE the edit (its hash must not
        // change, so its row — updated_at included — must stay untouched).
        let sym_row_before: (String, String) = store
            .conn()
            .query_row(
                "SELECT content_hash, updated_at FROM rag_chunks WHERE symbol_name = 'handler'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        // Edit ONLY the fallback region (line count preserved): the symbol
        // chunk's text — hence hash, hence id — is identical.
        write(root, "m.py", "def handler():\n    return 42\n\nx = 111\n");
        let mut embedder2 = CountingEmbedder::new(profile.dim as usize);
        let second = refresh_incremental(
            &store,
            root,
            &ChangeDelta { modified: vec![PathBuf::from("m.py")], ..ChangeDelta::default() },
            &mut embedder2,
            profile,
            Quantization::F32,
            &ChunkOptions::default(),
            &RefreshBudgets::default(),
        )
        .unwrap();

        // The counting wrapper PROVES only the changed chunk was embedded.
        assert_eq!(embedder2.texts_seen.len(), 1, "only the fallback chunk re-embedded");
        assert_eq!(second.chunks_embedded, 1);
        assert_eq!(second.chunks_skipped, 1, "symbol chunk skipped by hash");
        assert_eq!(second.files_reindexed, 1);

        // The skipped chunk's row was not rewritten (updated_at preserved).
        let sym_row_after: (String, String) = store
            .conn()
            .query_row(
                "SELECT content_hash, updated_at FROM rag_chunks WHERE symbol_name = 'handler'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(sym_row_before, sym_row_after, "untouched chunk row must stay byte-identical");

        // And no duplicates: chunk count unchanged (same ids upserted).
        assert_eq!(
            crate::vector::store::chunk_count(store.conn()).unwrap(),
            first_texts as u64
        );
    }

    /// FR-005 pin: purging a removed file leaves NO orphan vectors (FK
    /// cascade) and NO orphan edges (explicit sweep — `rag_chunk_edges`
    /// has no FK by design, T005).
    #[test]
    fn purge_leaves_no_orphan_vectors_or_edges() {
        let (tmp, store) = temp_store();
        let root = tmp.path();
        write(root, "a.py", "def a():\n    return 1\n\nz = 9\n");
        write(root, "b.py", "def b():\n    return 2\n\ny = 8\n");

        let profile = default_profile();
        let mut embedder = CountingEmbedder::new(profile.dim as usize);
        refresh_incremental(
            &store, root, &delta_added(&["a.py", "b.py"]), &mut embedder, profile,
            Quantization::F32, &ChunkOptions::default(), &RefreshBudgets::default(),
        )
        .unwrap();
        let total = crate::vector::store::chunk_count(store.conn()).unwrap();
        assert!(total >= 4);

        // Synthesize derived edges (T028 derives them for real): one
        // a.py→b.py edge and one dangling edge — both referencing chunks
        // that exist right now.
        let a_chunk: String = store
            .conn()
            .query_row(
                "SELECT chunk_id FROM rag_chunks WHERE source_path = 'a.py' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let b_chunk: String = store
            .conn()
            .query_row(
                "SELECT chunk_id FROM rag_chunks WHERE source_path = 'b.py' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO rag_chunk_edges (from_chunk_id, to_chunk_id, edge_kind) VALUES (?1, ?2, 'ReferencesRule')",
                rusqlite::params![a_chunk, b_chunk],
            )
            .unwrap();

        // Remove a.py from disk and purge it via a refresh.
        std::fs::remove_file(root.join("a.py")).unwrap();
        let mut embedder2 = CountingEmbedder::new(profile.dim as usize);
        let outcome = refresh_incremental(
            &store,
            root,
            &ChangeDelta { removed: vec![PathBuf::from("a.py")], ..ChangeDelta::default() },
            &mut embedder2,
            profile,
            Quantization::F32,
            &ChunkOptions::default(),
            &RefreshBudgets::default(),
        )
        .unwrap();
        assert_eq!(outcome.files_purged, 1);
        assert_eq!(embedder2.texts_seen.len(), 0, "purge-only refresh embeds nothing");

        // No orphan chunks/vectors/edges anywhere.
        let orphans: i64 = store
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM rag_vectors WHERE chunk_id NOT IN
                         (SELECT chunk_id FROM rag_chunks))
                     + (SELECT COUNT(*) FROM rag_chunk_edges WHERE from_chunk_id NOT IN
                         (SELECT chunk_id FROM rag_chunks))
                     + (SELECT COUNT(*) FROM rag_chunk_edges WHERE to_chunk_id NOT IN
                         (SELECT chunk_id FROM rag_chunks))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "no orphan vectors or edges may survive the purge");
        // b.py untouched.
        let b_rows: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM rag_chunks WHERE source_path = 'b.py'", [], |r| r.get(0))
            .unwrap();
        assert!(b_rows >= 2);
        let a_rows: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM rag_chunks WHERE source_path = 'a.py'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(a_rows, 0);
    }

    /// Rename migration: old-path chunk ids purged (cascading), new-path
    /// chunks created — end state identical to remove+add (data-model.md §
    /// Chunk lifecycle "migrated").
    #[test]
    fn rename_purges_old_path_and_indexes_new() {
        let (tmp, store) = temp_store();
        let root = tmp.path();
        write(root, "old.py", "def f():\n    return 1\n\nq = 2\n");

        let profile = default_profile();
        let mut embedder = CountingEmbedder::new(profile.dim as usize);
        refresh_incremental(
            &store, root, &delta_added(&["old.py"]), &mut embedder, profile,
            Quantization::F32, &ChunkOptions::default(), &RefreshBudgets::default(),
        )
        .unwrap();

        std::fs::rename(root.join("old.py"), root.join("new.py")).unwrap();
        let mut embedder2 = CountingEmbedder::new(profile.dim as usize);
        let outcome = refresh_incremental(
            &store,
            root,
            &ChangeDelta {
                renamed: vec![(PathBuf::from("old.py"), PathBuf::from("new.py"))],
                ..ChangeDelta::default()
            },
            &mut embedder2,
            profile,
            Quantization::F32,
            &ChunkOptions::default(),
            &RefreshBudgets::default(),
        )
        .unwrap();
        assert_eq!(outcome.files_renamed, 1);
        assert_eq!(outcome.files_indexed, 1, "rename new-side indexes");
        assert_eq!(outcome.chunks_skipped, 0, "new path ⇒ fresh chunks");

        let count = |path: &str| -> i64 {
            store
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM rag_chunks WHERE source_path = ?1",
                    rusqlite::params![path],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count("old.py"), 0);
        assert!(count("new.py") >= 2);
        // No orphans after the migration either.
        let orphan_vectors: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM rag_vectors WHERE chunk_id NOT IN (SELECT chunk_id FROM rag_chunks)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan_vectors, 0);
    }

    /// FR-004/005 budgets: `max_files_per_turn` caps processed files and the
    /// overflow is REPORTED as deferred (never silently dropped).
    #[test]
    fn file_budget_caps_and_reports_deferred() {
        let (tmp, store) = temp_store();
        let root = tmp.path();
        for name in ["f1.py", "f2.py", "f3.py"] {
            write(root, name, "v = 1\n");
        }
        let profile = default_profile();
        let mut embedder = CountingEmbedder::new(profile.dim as usize);
        let budgets = RefreshBudgets { max_files_per_turn: 2, ..RefreshBudgets::default() };
        let outcome = refresh_incremental(
            &store, root, &delta_added(&["f1.py", "f2.py", "f3.py"]), &mut embedder,
            profile, Quantization::F32, &ChunkOptions::default(), &budgets,
        )
        .unwrap();
        assert_eq!(outcome.files_indexed, 2);
        assert_eq!(outcome.files_deferred, 1, "skipped work is reported");
        // Only the two admitted files have rows.
        let distinct: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(DISTINCT source_path) FROM rag_chunks",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(distinct, 2);
    }

    /// Byte budget: a file larger than `max_bytes_per_turn` is deferred
    /// whole (never partially indexed).
    #[test]
    fn byte_budget_defers_oversized_file() {
        let (tmp, store) = temp_store();
        let root = tmp.path();
        write(root, "small.py", "s = 1\n");
        write(root, "big.py", &format!("b = '{}'\n", "x".repeat(400)));
        let profile = default_profile();
        let mut embedder = CountingEmbedder::new(profile.dim as usize);
        let budgets = RefreshBudgets {
            max_files_per_turn: 50,
            max_bytes_per_turn: 128, // smaller than big.py
        };
        let outcome = refresh_incremental(
            &store, root, &delta_added(&["small.py", "big.py"]), &mut embedder,
            profile, Quantization::F32, &ChunkOptions::default(), &budgets,
        )
        .unwrap();
        assert_eq!(outcome.files_indexed, 1);
        assert_eq!(outcome.files_deferred, 1);
        let big_rows: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM rag_chunks WHERE source_path = 'big.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(big_rows, 0, "oversized file deferred whole");
    }

    #[test]
    fn stale_profile_chunk_is_re_embedded_not_skipped() {
        let (tmp, store) = temp_store();
        let root = tmp.path();
        write(root, "m.py", "def a():\n    return 1\n");
        let profile = crate::embed::profiles::default_profile();
        let mut first = CountingEmbedder::new(profile.dim as usize);
        let added = delta_added(&["m.py"]);
        refresh_incremental(
            &store, root, &added, &mut first, profile, Quantization::F32,
            &ChunkOptions::default(), &RefreshBudgets::default(),
        ).unwrap();

        // Simulate a chunk left over from a DIFFERENT embedding profile
        // (e.g. a backend switch whose full rebuild never ran): same
        // content hash, wrong embed_dim.
        let bumped: i64 = store
            .conn()
            .query_row("UPDATE rag_chunks SET embed_dim = ?1 RETURNING embed_dim", [1536], |r| r.get(0))
            .unwrap();
        assert_eq!(bumped, 1536);

        let mut second = CountingEmbedder::new(profile.dim as usize);
        let modified = ChangeDelta {
            modified: vec![PathBuf::from("m.py")],
            ..ChangeDelta::default()
        };
        let report = refresh_incremental(
            &store, root, &modified, &mut second, profile, Quantization::F32,
            &ChunkOptions::default(), &RefreshBudgets::default(),
        ).unwrap();
        assert!(
            !second.texts_seen.is_empty(),
            "stale-dim chunk must be re-embedded, not skipped"
        );
        assert_eq!(report.chunks_skipped, 0);
        let dim: i64 = store
            .conn()
            .query_row("SELECT embed_dim FROM rag_chunks LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(dim, profile.dim as i64);
    }

    #[test]
    fn matching_profile_chunk_is_still_skipped() {
        let (tmp, store) = temp_store();
        let root = tmp.path();
        write(root, "m.py", "def a():\n    return 1\n");
        let profile = crate::embed::profiles::default_profile();
        let mut first = CountingEmbedder::new(profile.dim as usize);
        let added = delta_added(&["m.py"]);
        refresh_incremental(
            &store, root, &added, &mut first, profile, Quantization::F32,
            &ChunkOptions::default(), &RefreshBudgets::default(),
        ).unwrap();

        let mut second = CountingEmbedder::new(profile.dim as usize);
        let modified = ChangeDelta {
            modified: vec![PathBuf::from("m.py")],
            ..ChangeDelta::default()
        };
        let report = refresh_incremental(
            &store, root, &modified, &mut second, profile, Quantization::F32,
            &ChunkOptions::default(), &RefreshBudgets::default(),
        ).unwrap();
        assert_eq!(report.chunks_skipped, 1);
        assert!(second.texts_seen.is_empty());
    }
}
