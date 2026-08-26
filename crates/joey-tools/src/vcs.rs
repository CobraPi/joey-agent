//! Version control system: shared-store filesystem checkpointing via git.
//!
//! Checkpoint state lives in a **single shared shadow git store** at
//! `~/.joey/checkpoints/store` (honoring `JOEY_HOME`), reused across every
//! project/session/worktree instead of allocating a brand-new bare repo per
//! session. Each project (keyed by the sha256 hash of its canonicalized
//! absolute path) gets its own git ref (`refs/joey/<hash16>`), its own git
//! index file (`store/indexes/<hash16>`), and a small metadata record
//! (`store/projects/<hash16>.json`) — git's content-addressable object store
//! deduplicates blobs/trees across all projects and sessions automatically,
//! so a new worktree of an already-checkpointed project costs near-zero.
//!
//! Initialization is fully **lazy**: constructing a [`CheckpointManager`] is
//! cheap (it only resolves paths and probes `git` on `PATH`) and never
//! touches the filesystem. The shared store, this project's ref/index, and
//! the first snapshot are only created the first time [`CheckpointManager::checkpoint`]
//! is called — i.e. on the first mutating tool call or explicit `/checkpoint`
//! request — so the interactive prompt is never blocked on a full-repository
//! scan/add/commit.
//!
//! A default exclude list (build output, dependency directories, VCS
//! metadata, caches, virtualenvs, media/archives, secrets, logs) is applied
//! via the store's `info/exclude` so these are never scanned, hashed, or
//! stored. Every git subprocess invocation is isolated from the user's
//! global/system git config (`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` =
//! devnull) and bounded by a 5-second timeout so a hung git call can never
//! stall a session.
//!
//! Retention is enforced automatically (throttled, opportunistic pruning at
//! the end of `checkpoint()`): at most 50 snapshots per project, a 2GB total
//! store cap (oldest checkpoints across projects dropped first), a 90-day
//! stale-project window, and orphan pruning for projects whose working
//! directory no longer exists. Old per-session shadow-repo directories left
//! behind by the previous per-session design are discarded (not migrated)
//! opportunistically during the same pruning pass.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Wall-clock timeout applied to every git subprocess invocation (FR-005).
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Effective per-git-call wall-clock timeout. Overridable in tests via
/// `JOEY_TEST_GIT_TIMEOUT_MS` so the timeout-enforcement path can be
/// exercised without waiting the full 5s default (production default
/// unchanged).
fn git_timeout() -> Duration {
    std::env::var("JOEY_TEST_GIT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(GIT_TIMEOUT)
}

/// Max checkpoints retained per project (FR-007 default).
const MAX_SNAPSHOTS_PER_PROJECT: usize = 50;

/// Max total store size in bytes before oldest-first pruning kicks in
/// (FR-007 default: 2GB). Overridable in tests via `JOEY_TEST_STORE_CAP_BYTES`
/// so the size-cap pruning path can be exercised without allocating 2GB.
const MAX_TOTAL_STORE_SIZE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn size_cap_bytes() -> u64 {
    std::env::var("JOEY_TEST_STORE_CAP_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_TOTAL_STORE_SIZE_BYTES)
}

/// Effective per-project snapshot cap. Overridable in tests via
/// `JOEY_TEST_MAX_SNAPSHOTS_PER_PROJECT` so the cap path can be exercised
/// without creating 50+ real checkpoints.
fn max_snapshots_per_project() -> usize {
    std::env::var("JOEY_TEST_MAX_SNAPSHOTS_PER_PROJECT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_SNAPSHOTS_PER_PROJECT)
}

/// Max single tracked file size in bytes (FR-007 default: 50MB). Files
/// exceeding this are unstaged before commit by `unstage_oversized_files`.
/// Overridable in tests via `JOEY_TEST_MAX_FILE_SIZE_BYTES`.
const MAX_SINGLE_FILE_SIZE_BYTES: u64 = 50 * 1024 * 1024;

fn max_file_size_bytes() -> u64 {
    std::env::var("JOEY_TEST_MAX_FILE_SIZE_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_SINGLE_FILE_SIZE_BYTES)
}

/// Stale-project retention window in days (FR-007 default: 90 days).
const STALE_PROJECT_DAYS: u64 = 90;

/// Minimum interval between opportunistic prune passes, so pruning itself
/// never becomes a new startup-adjacent or per-turn cost.
const PRUNE_THROTTLE: Duration = Duration::from_secs(60 * 60); // 1 hour

const STORE_DIRNAME: &str = "store";
const INDEXES_DIRNAME: &str = "indexes";
const PROJECTS_DIRNAME: &str = "projects";
const REF_PREFIX: &str = "refs/joey";
const LAST_PRUNE_FILENAME: &str = ".last_prune";

/// Default exclude patterns applied to every checkpoint snapshot (FR-003),
/// ported verbatim from hermes-agent's `DEFAULT_EXCLUDES`.
const DEFAULT_EXCLUDES: &[&str] = &[
    // Dependency / build output
    "node_modules/",
    "dist/",
    "build/",
    "target/",
    "out/",
    ".next/",
    ".nuxt/",
    // Caches
    "__pycache__/",
    "*.pyc",
    "*.pyo",
    ".cache/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".ruff_cache/",
    "coverage/",
    ".coverage",
    // Virtualenvs
    ".venv/",
    "venv/",
    "env/",
    // VCS
    ".git/",
    ".hg/",
    ".svn/",
    // Worktrees convention — don't recursively snapshot siblings
    ".worktrees/",
    // Native / compiled binaries
    "*.so",
    "*.dylib",
    "*.dll",
    "*.o",
    "*.a",
    "*.jar",
    "*.class",
    "*.exe",
    "*.obj",
    // Media / large binaries
    "*.mp4",
    "*.mov",
    "*.mkv",
    "*.webm",
    "*.zip",
    "*.tar",
    "*.tar.gz",
    "*.tgz",
    "*.7z",
    "*.rar",
    "*.iso",
    // Secrets
    ".env",
    ".env.*",
    ".env.local",
    ".env.*.local",
    // OS junk
    ".DS_Store",
    "Thumbs.db",
    // Logs
    "*.log",
];

/// Per-project metadata record (`store/projects/<hash16>.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectMeta {
    workdir: String,
    created_at: u64,
    last_touch: u64,
}

/// The checkpoint manager for a single session/project.
pub struct CheckpointManager {
    /// The shared shadow store's git-dir (`~/.joey/checkpoints/store`).
    store: PathBuf,
    /// The working directory being tracked.
    work_tree: PathBuf,
    /// This project's stable 16-hex-char hash (sha256 of canonicalized abs path).
    hash16: String,
    /// True if git is available on `PATH`. Does NOT imply the store has been
    /// initialized yet — that happens lazily on first `checkpoint()`.
    enabled: bool,
    /// Sequential checkpoint counter, seeded lazily from the store on first use.
    next_checkpoint: usize,
    /// True once this manager instance has confirmed the store/project are initialized.
    store_ready: bool,
}

/// One checkpoint entry. Unchanged shape from the pre-rewrite design.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub number: usize,
    pub commit_hash: String,
    pub message: String,
    pub timestamp: String,
    pub files_changed: usize,
}

// ---------------------------------------------------------------------------
// Path / hash helpers
// ---------------------------------------------------------------------------

fn checkpoints_base() -> PathBuf {
    joey_core::joey_home().join("checkpoints")
}

fn store_path() -> PathBuf {
    checkpoints_base().join(STORE_DIRNAME)
}

fn index_path(hash16: &str) -> PathBuf {
    store_path().join(INDEXES_DIRNAME).join(hash16)
}

fn project_meta_path(hash16: &str) -> PathBuf {
    store_path()
        .join(PROJECTS_DIRNAME)
        .join(format!("{hash16}.json"))
}

fn ref_name(hash16: &str) -> String {
    format!("{REF_PREFIX}/{hash16}")
}

/// Deterministic per-project hash: sha256(canonicalized_abs_path)[:16].
fn project_hash(work_tree: &Path) -> String {
    use sha2::{Digest, Sha256};

    let canonical = work_tree
        .canonicalize()
        .unwrap_or_else(|_| work_tree.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8]) // 8 bytes -> 16 hex chars
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl CheckpointManager {
    /// Create a new checkpoint manager for the given session and working
    /// directory. Cheap: resolves the project hash and probes `git` on
    /// `PATH`. Performs NO filesystem/store mutation (FR-001) — the shared
    /// store, this project's ref/index/metadata, and the first snapshot are
    /// only created lazily on the first `checkpoint()` call.
    pub fn new(_session_id: &str, work_tree: &Path) -> Self {
        let enabled = which::which("git").is_ok();
        if !enabled {
            tracing::debug!("git not found — checkpoints disabled");
        }
        CheckpointManager {
            store: store_path(),
            work_tree: work_tree.to_path_buf(),
            hash16: project_hash(work_tree),
            enabled,
            next_checkpoint: 0,
            store_ready: false,
        }
    }

    /// Whether git was found on `PATH`. Does not imply the shared store has
    /// been initialized yet.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Create a checkpoint (commit all current file state, respecting
    /// default excludes, into this project's ref in the shared store).
    /// Lazily initializes the store/project on first call. Returns the
    /// checkpoint number, or `None` if disabled/failed (bounded by the 5s
    /// per-git-call timeout — never hangs the session).
    pub fn checkpoint(&mut self, message: &str) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        match self.checkpoint_internal(message) {
            Ok(num) => {
                tracing::debug!("Checkpoint #{} created: {}", num, message);
                // Opportunistic, throttled prune pass — never on the
                // startup path, only after a real checkpoint operation.
                if let Err(e) = self.maybe_prune() {
                    tracing::debug!("Checkpoint prune pass skipped/failed: {}", e);
                }
                Some(num)
            }
            Err(e) => {
                tracing::warn!("Checkpoint creation failed: {}", e);
                None
            }
        }
    }

    fn checkpoint_internal(&mut self, message: &str) -> Result<usize> {
        self.ensure_ready()?;

        let is_initial = !self.ref_exists()?;

        self.run_git(&["add", "--all", "--", "."])?;
        self.unstage_oversized_files()?;

        // Write the staged tree FIRST, then compare it with the current
        // ref's tree — plumbing-only, no HEAD dependency (the old
        // `diff --cached` / `status --porcelain` checks compared against
        // HEAD, which we no longer set).
        let tree = self
            .run_git_capture(&["write-tree"], &[0])?
            .trim()
            .to_string();

        if !is_initial {
            let prev_tree = self
                .run_git_capture(&["rev-parse", &format!("{}^{{tree}}", self.ref_name())], &[0])?
                .trim()
                .to_string();
            if prev_tree == tree {
                // Nothing changed — no new checkpoint needed.
                return Ok(self.next_checkpoint.max(1));
            }
        }

        let num = self.next_checkpoint + 1;
        let full_message = format!("[{}] {}", num, message);

        // Plumbing-based commit — NEVER `git commit`. `git commit` updates
        // whatever HEAD points at, and HEAD lives in the SHARED store: two
        // concurrent CheckpointManagers (two projects using one store) would
        // race the `symbolic-ref HEAD <ref>` setup and commit onto each
        // other's ref, corrupting cross-project history. write-tree /
        // commit-tree / update-ref touch only THIS project's ref.
        let commit_args: Vec<String> = if is_initial {
            vec!["commit-tree".to_string(), tree.clone(), "-m".to_string(), full_message.clone()]
        } else {
            vec![
                "commit-tree".to_string(),
                tree.clone(),
                "-p".to_string(),
                self.ref_name(),
                "-m".to_string(),
                full_message.clone(),
            ]
        };
        let arg_refs: Vec<&str> = commit_args.iter().map(|s| s.as_str()).collect();
        let commit_oid = self.run_git_capture(&arg_refs, &[0])?.trim().to_string();
        if commit_oid.len() < 40 {
            return Err(anyhow::anyhow!(
                "commit-tree returned invalid oid: {:?}",
                commit_oid
            ));
        }
        // Atomically move this project's ref to the new commit.
        self.run_git(&["update-ref", &self.ref_name(), &commit_oid])?;

        self.next_checkpoint = num;
        self.touch_project_meta()?;
        Ok(num)
    }

    /// Ensure the shared store exists, this project's ref/HEAD-mapping and
    /// index are set up, and `next_checkpoint` is seeded from any existing
    /// history. Idempotent — cheap no-op on repeat calls once `store_ready`.
    fn ensure_ready(&mut self) -> Result<()> {
        if self.store_ready {
            return Ok(());
        }
        ensure_store_initialized(&self.store)?;
        std::fs::create_dir_all(self.store.join(INDEXES_DIRNAME)).ok();
        std::fs::create_dir_all(self.store.join(PROJECTS_DIRNAME)).ok();

        // (No HEAD manipulation: commits are made with write-tree /
        // commit-tree / update-ref plumbing against THIS project's ref, so
        // the store's shared HEAD file is never touched — concurrent
        // managers for different projects cannot race each other.)

        // Seed next_checkpoint from existing history, if any.
        if self.ref_exists()? {
            let checkpoints = self.list_internal().unwrap_or_default();
            self.next_checkpoint = checkpoints.iter().map(|c| c.number).max().unwrap_or(0);
        }

        self.store_ready = true;
        Ok(())
    }

    fn ref_name(&self) -> String {
        ref_name(&self.hash16)
    }

    fn ref_exists(&self) -> Result<bool> {
        let ok = self.run_git_bool(&["rev-parse", "--verify", "--quiet", &self.ref_name()]);
        Ok(ok)
    }

    /// Enforce FR-007's max single tracked file size cap: unstage (but
    /// leave on disk) any currently-staged file whose working-tree size
    /// exceeds `MAX_SINGLE_FILE_SIZE_BYTES`, mirroring how `DEFAULT_EXCLUDES`
    /// keeps unwanted paths out of a snapshot. Best-effort — a failure to
    /// stat or unstage a given path is skipped rather than aborting the
    /// whole checkpoint.
    fn unstage_oversized_files(&self) -> Result<()> {
        let staged = self.run_git_capture(&["diff", "--cached", "--name-only"], &[0])?;
        let mut oversized: Vec<String> = Vec::new();
        for rel_path in staged.lines() {
            if rel_path.is_empty() {
                continue;
            }
            let abs_path = self.work_tree.join(rel_path);
            if let Ok(meta) = std::fs::symlink_metadata(&abs_path) {
                if meta.is_file() && meta.len() > max_file_size_bytes() {
                    oversized.push(rel_path.to_string());
                }
            }
        }
        if oversized.is_empty() {
            return Ok(());
        }
        let mut args: Vec<&str> = vec!["rm", "--cached", "--quiet", "-r", "--"];
        args.extend(oversized.iter().map(|s| s.as_str()));
        // Index-only unstage; works whether or not a HEAD commit exists yet
        // (unlike `git reset HEAD --`, which requires a HEAD). Best-effort —
        // this is a soft cap, not a hard requirement.
        let _ = self.run_git_capture(&args, &[0, 1, 128]);
        Ok(())
    }

    fn touch_project_meta(&self) -> Result<()> {
        let path = project_meta_path(&self.hash16);
        let now = now_unix();
        let created_at = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<ProjectMeta>(&s).ok())
                .map(|m| m.created_at)
                .unwrap_or(now)
        } else {
            now
        };
        let meta = ProjectMeta {
            workdir: self.work_tree.to_string_lossy().to_string(),
            created_at,
            last_touch: now,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, serde_json::to_string(&meta)?)?;
        Ok(())
    }

    /// List all checkpoints (newest first).
    pub fn list(&self) -> Result<Vec<Checkpoint>> {
        if !self.enabled || !self.ref_exists().unwrap_or(false) {
            return Ok(Vec::new());
        }
        self.list_internal()
    }

    fn list_internal(&self) -> Result<Vec<Checkpoint>> {
        let log = self.run_git_capture(
            &[
                "log",
                &self.ref_name(),
                "--pretty=format:%H|%s|%ai",
                "--name-only",
            ],
            &[0],
        )?;

        let mut checkpoints = Vec::new();
        for entry in log.split("\n\n") {
            let mut lines = entry.lines();
            let header = lines.next().unwrap_or("");
            let parts: Vec<&str> = header.splitn(3, '|').collect();
            if parts.len() < 3 {
                continue;
            }
            let commit_hash = parts[0].to_string();
            let subject = parts[1].to_string();
            let timestamp = parts[2].to_string();

            let number = subject
                .strip_prefix('[')
                .and_then(|s| s.split(']').next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);

            let files_changed = lines.filter(|l| !l.is_empty()).count();

            checkpoints.push(Checkpoint {
                number,
                commit_hash,
                message: subject
                    .strip_prefix('[')
                    .and_then(|s| s.find(']').map(|i| s[i + 1..].trim().to_string()))
                    .unwrap_or_else(|| subject.clone()),
                timestamp,
                files_changed,
            });
        }
        Ok(checkpoints)
    }

    /// Revert the working directory to the state at checkpoint `number`.
    /// Unchanged externally-observable semantics from the pre-rewrite
    /// design: checkout the target commit's tree, then remove files that
    /// were added after that checkpoint.
    pub fn revert(&self, number: usize) -> Result<()> {
        if !self.enabled {
            anyhow::bail!("Checkpoint system is not enabled");
        }
        let checkpoints = self.list()?;
        let target = checkpoints
            .iter()
            .find(|c| c.number == number)
            .with_context(|| format!("Checkpoint #{} not found", number))?;

        let hash = &target.commit_hash;
        self.run_git(&["checkout", hash, "--", "."])?;

        let files_to_remove = self.run_git_capture(
            &[
                "diff",
                "--name-only",
                "--diff-filter=A",
                &format!("{}..{}", hash, self.ref_name()),
            ],
            &[0],
        )?;
        for file in files_to_remove.lines() {
            if file.is_empty() {
                continue;
            }
            let target_path = self.work_tree.join(file);
            if target_path.exists() {
                let _ = std::fs::remove_file(&target_path);
            }
        }

        Ok(())
    }

    /// Session-end cleanup. The shared store persists across sessions, so
    /// there is no per-session directory to delete — this is a no-op.
    pub fn cleanup(&self) {
        // Intentional no-op: shared store persists across sessions.
    }

    /// The shared store's path (for debugging/tests).
    pub fn repo_path(&self) -> &Path {
        &self.store
    }

    // -----------------------------------------------------------------
    // Retention / pruning (FR-007, FR-009)
    // -----------------------------------------------------------------

    /// Run the throttled, opportunistic prune pass if enough time has
    /// passed since the last one. Never runs on the startup path — only
    /// ever triggered from the tail of a real `checkpoint()` call.
    fn maybe_prune(&self) -> Result<()> {
        let marker = checkpoints_base().join(LAST_PRUNE_FILENAME);
        let should_run = match std::fs::metadata(&marker).and_then(|m| m.modified()) {
            Ok(modified) => modified
                .elapsed()
                .map(|e| e >= PRUNE_THROTTLE)
                .unwrap_or(true),
            Err(_) => true,
        };
        if !should_run {
            return Ok(());
        }
        self.run_prune_pass()?;
        std::fs::create_dir_all(checkpoints_base()).ok();
        std::fs::write(&marker, now_unix().to_string()).ok();
        Ok(())
    }

    /// Force a prune pass regardless of throttling (used by tests).
    fn run_prune_pass(&self) -> Result<()> {
        discard_legacy_shadow_repos(&checkpoints_base(), &self.store);
        let mut any_deleted = false;
        any_deleted |= self.prune_orphaned_and_stale()?;
        any_deleted |= self.prune_per_project_cap()?;
        any_deleted |= self.prune_size_cap()?;
        if any_deleted {
            self.reflog_expire_and_gc();
        }
        Ok(())
    }

    fn all_project_hashes(&self) -> Vec<String> {
        let dir = self.store.join(PROJECTS_DIRNAME);
        let mut hashes = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(hash) = name.strip_suffix(".json") {
                        hashes.push(hash.to_string());
                    }
                }
            }
        }
        hashes
    }

    fn read_project_meta(&self, hash16: &str) -> Option<ProjectMeta> {
        let path = project_meta_path(hash16);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn remove_project(&self, hash16: &str) {
        let _ = self.run_git_with_timeout(
            &["update-ref", "-d", &ref_name(hash16)],
            None,
        );
        let _ = std::fs::remove_file(index_path(hash16));
        let _ = std::fs::remove_file(project_meta_path(hash16));
    }

    /// Remove projects whose working directory no longer exists (orphan)
    /// or whose `last_touch` is older than the stale window. Returns true
    /// if anything was deleted.
    fn prune_orphaned_and_stale(&self) -> Result<bool> {
        let stale_cutoff = now_unix().saturating_sub(STALE_PROJECT_DAYS * 24 * 60 * 60);
        let mut deleted = false;
        for hash16 in self.all_project_hashes() {
            let Some(meta) = self.read_project_meta(&hash16) else {
                continue;
            };
            let orphaned = !Path::new(&meta.workdir).exists();
            let stale = meta.last_touch < stale_cutoff;
            if orphaned || stale {
                self.remove_project(&hash16);
                deleted = true;
            }
        }
        Ok(deleted)
    }

    /// Cap each project's checkpoint history at
    /// `max_snapshots_per_project()` snapshots, keeping exactly the NEWEST
    /// that many. See [`Self::truncate_ref_history`] for the mechanics.
    fn prune_per_project_cap(&self) -> Result<bool> {
        let cap = max_snapshots_per_project();
        let mut deleted = false;
        for hash16 in self.all_project_hashes() {
            if self.truncate_ref_history(&ref_name(&hash16), cap) {
                deleted = true;
            }
        }
        Ok(deleted)
    }

    /// If total store size exceeds the cap, drop the oldest checkpoints
    /// across all projects (oldest-first) until under cap.
    fn prune_size_cap(&self) -> Result<bool> {
        let objects_dir = self.store.join("objects");
        let cap = size_cap_bytes();
        if dir_size(&objects_dir) <= cap {
            return Ok(false);
        }
        // Oldest-first across projects: repeatedly drop the single oldest
        // checkpoint of whichever project owns the globally-oldest reachable
        // commit, then gc + re-measure. (The `objects/` directory only
        // shrinks after gc, so gc must run inside the loop for progress to
        // be observable.) A project's last checkpoint is never dropped by
        // the size cap. Bounded iteration count to avoid pathological loops.
        let mut deleted = false;
        for _ in 0..500 {
            if dir_size(&objects_dir) <= cap {
                break;
            }
            // (hash16, oldest reachable commit timestamp, commit count)
            let mut victim: Option<(String, i64, usize)> = None;
            for hash16 in self.all_project_hashes() {
                let r = ref_name(&hash16);
                let log = self
                    .run_prune_git(&["log", &r, "--pretty=format:%ct"], &[])
                    .unwrap_or_default();
                let timestamps: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();
                if timestamps.len() < 2 {
                    continue; // nothing (extra) to drop for this project
                }
                let oldest: i64 = timestamps
                    .last()
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(i64::MAX);
                if victim.as_ref().map(|(_, v, _)| oldest < *v).unwrap_or(true) {
                    victim = Some((hash16.clone(), oldest, timestamps.len()));
                }
            }
            let Some((hash16, _oldest, count)) = victim else {
                break; // nothing left to trim anywhere
            };
            if !self.truncate_ref_history(&ref_name(&hash16), count - 1) {
                break; // trim failed; re-measuring would loop forever
            }
            deleted = true;
            // Realize the freed space before the next dir_size() check.
            self.reflog_expire_and_gc();
        }
        Ok(deleted)
    }

    /// Rebuild `r`'s history so that only the newest `keep` commits remain
    /// reachable. The oldest kept (boundary) commit is re-created via
    /// `git commit-tree` as a parentless root — with its original tree,
    /// message, and author/committer identity/timestamps — and every newer
    /// commit is re-created on top, so content, checkpoint numbers, and
    /// dates are preserved while the pre-boundary chain becomes
    /// unreachable and is swept by the subsequent `reflog expire` +
    /// `gc --prune=now` (see [`Self::reflog_expire_and_gc`]).
    ///
    /// A plain `update-ref` cannot do this: pointing the ref anywhere in
    /// the chain still leaves all older commits reachable through parent
    /// links (that was the retention inversion bug this replaces — the old
    /// code reset the ref to the Nth-newest commit and gc kept the OLDEST
    /// N reachable). `git replace --graft` does not help either: the
    /// replace ref itself keeps the replaced history reachable to `gc`.
    fn truncate_ref_history(&self, r: &str, keep: usize) -> bool {
        if keep == 0 {
            return false;
        }
        let log = self
            .run_prune_git(
                &[
                    "log",
                    r,
                    // hash, tree, author ident+date, committer ident+date,
                    // full raw message (%B); records NUL-separated, fields
                    // unit-separated. splitn(9) keeps any stray unit bytes
                    // inside the message (the final field) intact.
                    "--pretty=format:%H%x01%T%x01%an%x01%ae%x01%aI%x01%cn%x01%ce%x01%cI%x01%B%x00",
                ],
                &[],
            )
            .unwrap_or_default();
        let records: Vec<&str> = log
            .split('\0')
            .map(|rec| rec.trim_start_matches(['\n', '\r']))
            .filter(|rec| !rec.is_empty())
            .collect();
        if records.len() <= keep {
            return false; // already within the cap
        }
        // records[0] is the newest commit; rebuild oldest-kept-first so
        // each re-created commit can point at the previous one.
        let mut parent: Option<String> = None;
        for rec in records[..keep].iter().rev() {
            let fields: Vec<&str> = rec.splitn(9, '\u{1}').collect();
            if fields.len() < 9 || fields[0].is_empty() || fields[1].is_empty() {
                // Malformed record (exotic identity/message) — leave this
                // project's history untouched rather than risk a bad rewrite.
                return false;
            }
            let mut args: Vec<&str> = vec!["commit-tree", fields[1]]; // tree
            if let Some(p) = &parent {
                args.extend_from_slice(&["-p", p.as_str()]);
            }
            let message = fields[8].trim_end();
            args.extend_from_slice(&["-m", message]);
            let envs: &[(&str, &str)] = &[
                ("GIT_AUTHOR_NAME", fields[2]),
                ("GIT_AUTHOR_EMAIL", fields[3]),
                ("GIT_AUTHOR_DATE", fields[4]),
                ("GIT_COMMITTER_NAME", fields[5]),
                ("GIT_COMMITTER_EMAIL", fields[6]),
                ("GIT_COMMITTER_DATE", fields[7]),
            ];
            let Some(new_hash) = self.run_prune_git(&args, envs) else {
                return false;
            };
            parent = Some(new_hash.trim().to_string());
        }
        let Some(new_tip) = parent else {
            return false;
        };
        self.run_prune_git(&["update-ref", r, &new_tip], &[]).is_some()
    }

    /// Run a git command against the shared store only (no work tree, no
    /// per-project index) with extra env vars — used by retention pruning.
    /// Never errors to the caller: `None` means the command failed or
    /// timed out, and pruning degrades gracefully.
    fn run_prune_git(&self, args: &[&str], envs: &[(&str, &str)]) -> Option<String> {
        let mut cmd = Command::new("git");
        cmd.env("GIT_DIR", &self.store);
        cmd.env_remove("GIT_WORK_TREE");
        cmd.env_remove("GIT_INDEX_FILE");
        apply_isolation_env(&mut cmd);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.args(args);
        cmd.current_dir(&self.store);
        run_with_timeout(cmd, git_timeout(), &[0]).ok()
    }

    /// Make previously-unreachable objects actually leave the store:
    /// expire all reflogs (reflog entries keep pruned chains reachable)
    /// and then garbage-collect immediately.
    fn reflog_expire_and_gc(&self) {
        let _ = self.run_prune_git(
            &[
                "reflog",
                "expire",
                "--expire=now",
                "--expire-unreachable=now",
                "--all",
            ],
            &[],
        );
        let _ = self.run_prune_git(&["gc", "--prune=now", "--quiet"], &[]);
    }

    // -----------------------------------------------------------------
    // Git subprocess plumbing
    // -----------------------------------------------------------------

    fn git_env(&self, cmd: &mut Command) {
        cmd.env("GIT_DIR", &self.store);
        cmd.env("GIT_WORK_TREE", &self.work_tree);
        cmd.env("GIT_INDEX_FILE", index_path(&self.hash16));
        apply_isolation_env(cmd);
    }

    fn run_git(&self, args: &[&str]) -> Result<()> {
        self.run_git_capture(args, &[0]).map(|_| ())
    }

    fn run_git_capture(&self, args: &[&str], allowed: &[i32]) -> Result<String> {
        let mut cmd = Command::new("git");
        self.git_env(&mut cmd);
        cmd.args(args);
        cmd.current_dir(&self.work_tree);
        run_with_timeout(cmd, git_timeout(), allowed).with_context(|| {
            format!(
                "git {} (store: {})",
                args.join(" "),
                self.store.display()
            )
        })
    }

    /// Like `run_git_capture` but never errors on non-`allowed` exit codes
    /// beyond returning empty output — used by best-effort pruning helpers.
    fn run_git_with_timeout(&self, args: &[&str], allowed: Option<&[i32]>) -> Option<String> {
        let mut cmd = Command::new("git");
        self.git_env(&mut cmd);
        cmd.args(args);
        cmd.current_dir(&self.work_tree);
        run_with_timeout(cmd, git_timeout(), allowed.unwrap_or(&[0])).ok()
    }

    fn run_git_bool(&self, args: &[&str]) -> bool {
        let mut cmd = Command::new("git");
        self.git_env(&mut cmd);
        cmd.args(args);
        cmd.current_dir(&self.work_tree);
        run_with_timeout(cmd, git_timeout(), &[0]).is_ok()
    }
}

/// Apply full git-config isolation env vars (FR-004): no inherited global/
/// system git config (gpgsign, credential helpers, hooks) can slow down or
/// block a checkpoint operation.
fn apply_isolation_env(cmd: &mut Command) {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
}

/// Initialize the shared shadow store if not already present. Idempotent.
fn ensure_store_initialized(store: &Path) -> Result<()> {
    if store.join("HEAD").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(store)?;

    let mut cmd = Command::new("git");
    apply_isolation_env(&mut cmd);
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env_remove("GIT_INDEX_FILE");
    cmd.args(["init", "--bare", "--quiet", &store.to_string_lossy()]);
    run_with_timeout(cmd, git_timeout(), &[0]).context("git init --bare (shared store)")?;

    // Per-store config. Written directly as a config block appended to the
    // store's own `config` file — semantically identical to running
    // `git config <k> <v>` for each pair below (Apple Git rejects
    // multi-pair `git config k v k v` invocations with "no action
    // specified", and five separate subprocesses add ~0.25s of spawn
    // overhead per first checkpoint). The store is freshly created by the
    // `git init --bare` above (this function returns early if `HEAD`
    // already exists), so the file either does not exist or is the bare
    // init template — appending a dedicated block mirrors the previous
    // create/overwrite-per-key semantics exactly. Env-var isolation
    // (above) keeps global/system config from interfering either way.
    const STORE_CONFIG: &str = "[user]
\temail = joey@local
\tname = Joey Checkpoint
[commit]
\tgpgsign = false
[tag]
\tgpgSign = false
[gc]
\tauto = 0
";
    let config_path = store.join("config");
    let mut existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    existing.push_str(STORE_CONFIG);
    std::fs::write(&config_path, existing)
        .context("writing shared-store git config")?;

    let info_dir = store.join("info");
    std::fs::create_dir_all(&info_dir)?;
    std::fs::write(
        info_dir.join("exclude"),
        DEFAULT_EXCLUDES.join("\n") + "\n",
    )?;

    tracing::debug!("Initialised checkpoint store at {}", store.display());
    Ok(())
}

/// Remove non-`store` directories directly under `~/.joey/checkpoints/`
/// left behind by the previous per-session shadow-repo design (FR-009).
/// Discarded outright (no migration, no archiving) per the feature's
/// clarification — old per-session data is documented as ephemeral.
fn discard_legacy_shadow_repos(base: &Path, store: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == *store {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name == LAST_PRUNE_FILENAME {
                continue;
            }
        }
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Recursively compute the total size in bytes of everything under `path`.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// Run a command with a wall-clock timeout, killing the child if it
/// exceeds `timeout`. Returns stdout on success (exit code in `allowed`);
/// errors (including timeout) otherwise. This is the sole enforcement
/// point for FR-005 — no git subprocess call in this module bypasses it.
fn run_with_timeout(mut cmd: Command, timeout: Duration, allowed: &[i32]) -> Result<String> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child: Child = cmd.spawn().context("spawning git subprocess")?;
    let start = Instant::now();

    // Drain both pipes on dedicated threads BEFORE polling for exit. Without
    // this, a git invocation writing more than the OS pipe buffer (~64KB on
    // macOS — e.g. `git status`/`git diff` on a large work tree) blocks on a
    // full pipe, never exits, and the poll loop below spins until the kill
    // timeout: a multi-second UI freeze on every such call.
    let stdout_pipe = child
        .stdout
        .take()
        .context("git subprocess stdout was not piped")?;
    let stderr_pipe = child
        .stderr
        .take()
        .context("git subprocess stderr was not piped")?;
    let out_handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let mut r = std::io::BufReader::new(stdout_pipe);
        let _ = r.read_to_string(&mut buf);
        buf
    });
    let err_handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let mut r = std::io::BufReader::new(stderr_pipe);
        let _ = r.read_to_string(&mut buf);
        buf
    });

    let status = loop {
        match child.try_wait().context("polling git subprocess")? {
            Some(status) => break status,
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("git subprocess timed out after {:?}", timeout);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };

    let output_stdout = out_handle.join().unwrap_or_default();
    let output_stderr = err_handle.join().unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    if !allowed.contains(&code) {
        anyhow::bail!(
            "git exited with code {}: {}",
            code,
            output_stderr.trim()
        );
    }
    Ok(output_stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        which::which("git").is_ok()
    }

    /// Set up an isolated JOEY_HOME + work tree for a test, returning
    /// guards that must be kept alive for the test's duration.
    fn test_setup() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        joey_core::constants::HomeOverrideGuard,
    ) {
        let home_dir = tempfile::tempdir().unwrap();
        let guard =
            joey_core::constants::HomeOverrideGuard::new(home_dir.path().to_path_buf());
        let work_dir = tempfile::tempdir().unwrap();
        (home_dir, work_dir, guard)
    }

    #[test]
    fn checkpoint_lifecycle() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            eprintln!("git not available — skipping checkpoint test");
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();

        std::fs::write(work_tree.join("a.txt"), "initial").unwrap();

        let mut mgr = CheckpointManager::new("test-session-1", work_tree);
        assert!(mgr.is_enabled(), "checkpoint system should be enabled");

        // First checkpoint call lazily initializes the store and creates
        // checkpoint #1 (was previously created implicitly by `new()`).
        let cp1 = mgr.checkpoint("Session start (initial state)");
        assert_eq!(cp1, Some(1));

        std::fs::write(work_tree.join("b.txt"), "second file").unwrap();
        let cp2 = mgr.checkpoint("Added b.txt");
        assert_eq!(cp2, Some(2));

        let list = mgr.list().unwrap();
        assert!(list.len() >= 2, "should have at least 2 checkpoints");

        std::fs::write(work_tree.join("c.txt"), "third file").unwrap();
        std::fs::write(work_tree.join("a.txt"), "modified").unwrap();
        let cp3 = mgr.checkpoint("Added c.txt, modified a.txt");
        assert_eq!(cp3, Some(3));

        mgr.revert(2).unwrap();
        assert!(
            !work_tree.join("c.txt").exists(),
            "c.txt should be removed after revert"
        );
        assert_eq!(
            std::fs::read_to_string(work_tree.join("a.txt")).unwrap(),
            "initial",
            "a.txt should be reverted to initial state"
        );
        assert!(work_tree.join("b.txt").exists(), "b.txt should still exist");

        mgr.cleanup();
        // Shared store persists across sessions — cleanup is a no-op now.
        assert!(mgr.repo_path().exists());
    }

    #[test]
    fn checkpoint_noop_on_no_changes() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();
        std::fs::write(work_tree.join("x.txt"), "content").unwrap();

        let mut mgr = CheckpointManager::new("test-noop", work_tree);
        assert!(mgr.is_enabled());

        let cp1 = mgr.checkpoint("initial");
        assert_eq!(cp1, Some(1));

        // No changes since the last checkpoint — returns the same number.
        let cp2 = mgr.checkpoint("nothing changed");
        assert_eq!(cp2, Some(1));

        mgr.cleanup();
    }

    #[test]
    fn lazy_init_no_store_before_checkpoint() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();
        std::fs::write(work_tree.join("a.txt"), "x").unwrap();

        let mut mgr = CheckpointManager::new("lazy-test", work_tree);
        assert!(mgr.is_enabled());
        assert!(
            !mgr.repo_path().join("HEAD").exists(),
            "store must not exist before first checkpoint()"
        );

        mgr.checkpoint("first").unwrap();
        assert!(
            mgr.repo_path().join("HEAD").exists(),
            "store must exist after first checkpoint()"
        );
    }

    #[test]
    fn graceful_degradation_when_git_missing() {
        let _lock = crate::test_env_lock();
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();

        // Simulate git being unavailable via an empty PATH.
        let orig_path = std::env::var_os("PATH");
        std::env::set_var("PATH", "");

        let mut mgr = CheckpointManager::new("no-git", work_tree);
        assert!(!mgr.is_enabled(), "should be disabled without git on PATH");
        assert_eq!(mgr.checkpoint("noop"), None);
        assert!(
            !mgr.repo_path().join("HEAD").exists(),
            "no store should be created when disabled"
        );

        if let Some(p) = orig_path {
            std::env::set_var("PATH", p);
        }
    }

    #[test]
    fn dedup_across_manager_instances() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();
        std::fs::write(work_tree.join("a.txt"), "content").unwrap();

        let mut mgr1 = CheckpointManager::new("s1", work_tree);
        mgr1.checkpoint("first").unwrap();
        let size_after_first = dir_size(&mgr1.repo_path().join("objects"));

        // Second manager instance for the same work tree — same hash16,
        // same ref/index, no new project entry.
        let mut mgr2 = CheckpointManager::new("s2", work_tree);
        assert_eq!(mgr1.hash16, mgr2.hash16);
        let cp = mgr2.checkpoint("no changes").unwrap();
        assert_eq!(cp, 1, "unchanged content should not create a new checkpoint");

        let size_after_second = dir_size(&mgr2.repo_path().join("objects"));
        assert_eq!(
            size_after_first, size_after_second,
            "re-checkpointing unchanged content should add ~0 bytes"
        );
    }

    #[test]
    fn excludes_applied_to_snapshot() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();
        std::fs::create_dir_all(work_tree.join("node_modules")).unwrap();
        std::fs::write(work_tree.join("node_modules/pkg.js"), "x").unwrap();
        std::fs::write(work_tree.join(".env"), "SECRET=1").unwrap();
        std::fs::write(work_tree.join("keep.txt"), "kept").unwrap();

        let mut mgr = CheckpointManager::new("excl-test", work_tree);
        let cp = mgr.checkpoint("with excludes").unwrap();
        let checkpoints = mgr.list().unwrap();
        let target = checkpoints.iter().find(|c| c.number == cp).unwrap();

        let stat = mgr
            .run_git_capture(&["show", "--stat", "--pretty=format:", &target.commit_hash], &[0])
            .unwrap();
        assert!(!stat.contains("node_modules"), "node_modules must be excluded");
        assert!(!stat.contains(".env"), ".env must be excluded");
    }

    #[test]
    fn oversized_file_excluded_from_snapshot() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();

        // Force a tiny test-only cap so we don't need a real 50MB fixture.
        std::env::set_var("JOEY_TEST_MAX_FILE_SIZE_BYTES", "100");
        std::fs::write(work_tree.join("small.txt"), "x".repeat(10)).unwrap();
        std::fs::write(work_tree.join("huge.bin"), "x".repeat(500)).unwrap();

        let mut mgr = CheckpointManager::new("filesize-test", work_tree);
        let cp = mgr.checkpoint("with oversized file").unwrap();
        let checkpoints = mgr.list().unwrap();
        let target = checkpoints.iter().find(|c| c.number == cp).unwrap();

        let stat = mgr
            .run_git_capture(&["show", "--stat", "--pretty=format:", &target.commit_hash], &[0])
            .unwrap();

        std::env::remove_var("JOEY_TEST_MAX_FILE_SIZE_BYTES");

        assert!(
            !stat.contains("huge.bin"),
            "file exceeding the size cap must be excluded from the snapshot, stat: {}",
            stat
        );
        assert!(
            stat.contains("small.txt"),
            "file within the size cap must still be tracked, stat: {}",
            stat
        );
        // Oversized file must remain untouched on disk (unstaged, not deleted).
        assert!(work_tree.join("huge.bin").exists());
    }

    #[test]
    fn prune_orphaned_project() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path().join("proj");
        std::fs::create_dir_all(&work_tree).unwrap();
        std::fs::write(work_tree.join("a.txt"), "x").unwrap();

        let mut mgr = CheckpointManager::new("orphan-test", &work_tree);
        mgr.checkpoint("first").unwrap();
        let hash16 = mgr.hash16.clone();
        assert!(project_meta_path(&hash16).exists());

        // Delete the working directory to simulate orphan.
        std::fs::remove_dir_all(&work_tree).unwrap();

        mgr.run_prune_pass().unwrap();
        assert!(
            !project_meta_path(&hash16).exists(),
            "orphaned project metadata should be removed"
        );
    }

    #[test]
    fn prune_stale_project() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();
        std::fs::write(work_tree.join("a.txt"), "x").unwrap();

        let mut mgr = CheckpointManager::new("stale-test", work_tree);
        mgr.checkpoint("first").unwrap();
        let hash16 = mgr.hash16.clone();

        // Backdate last_touch beyond the 90-day stale window.
        let meta = ProjectMeta {
            workdir: work_tree.to_string_lossy().to_string(),
            created_at: 0,
            last_touch: 0,
        };
        std::fs::write(project_meta_path(&hash16), serde_json::to_string(&meta).unwrap())
            .unwrap();

        mgr.run_prune_pass().unwrap();
        assert!(
            !project_meta_path(&hash16).exists(),
            "stale project metadata should be removed"
        );
    }

    #[test]
    fn prune_size_cap_drops_oldest_first() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();

        // Force a tiny test-only cap so a handful of commits triggers it.
        std::env::set_var("JOEY_TEST_STORE_CAP_BYTES", "2000");

        let mut mgr = CheckpointManager::new("size-cap-test", work_tree);
        for i in 0..8 {
            std::fs::write(work_tree.join("f.txt"), "x".repeat(500)).unwrap();
            std::fs::write(work_tree.join(format!("g{i}.txt")), format!("v{i}").repeat(50))
                .unwrap();
            mgr.checkpoint(&format!("cp {i}")).unwrap();
        }

        let before = mgr.list_internal().unwrap().len();
        mgr.run_prune_pass().unwrap();
        let after = mgr.list_internal().unwrap();

        std::env::remove_var("JOEY_TEST_STORE_CAP_BYTES");

        assert!(
            after.len() < before,
            "size-cap pruning should drop at least the oldest checkpoint(s)"
        );
        let mut remaining_numbers: Vec<usize> =
            after.iter().map(|c| c.number).collect();
        remaining_numbers.sort_unstable();
        assert!(
            remaining_numbers.first().copied().unwrap_or(0) >= 1,
            "oldest checkpoints should be dropped first, remaining: {:?}",
            remaining_numbers
        );
        // Retention must keep the NEWEST `after.len()` checkpoints: the
        // surviving numbers are contiguous and end at the newest number
        // (regression test for the retention inversion bug).
        assert_eq!(
            remaining_numbers.last().copied(),
            Some(before),
            "the newest checkpoint must survive size-cap pruning, remaining: {:?}",
            remaining_numbers
        );
        assert!(
            remaining_numbers.windows(2).all(|w| w[1] == w[0] + 1),
            "surviving checkpoints must be the contiguous newest tail, got: {:?}",
            remaining_numbers
        );
        // The pruned (oldest) commits must actually be gone from the store.
        assert_pruned_commits_gone(&mgr, &after);
    }

    #[test]
    fn prune_per_project_snapshot_cap() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();

        // Force a small test-only cap (default is 50) — 6 checkpoints,
        // cap 3: exactly checkpoints #4, #5, #6 must survive.
        std::env::set_var("JOEY_TEST_MAX_SNAPSHOTS_PER_PROJECT", "3");

        let mut mgr = CheckpointManager::new("cap-test", work_tree);
        let mut oldest_hash = String::new();
        for i in 0..6 {
            std::fs::write(work_tree.join("f.txt"), format!("v{i}")).unwrap();
            mgr.checkpoint(&format!("cp {i}")).unwrap();
            if i == 0 {
                oldest_hash = mgr
                    .list_internal()
                    .unwrap()
                    .iter()
                    .map(|c| c.commit_hash.clone())
                    .max()
                    .unwrap_or_default();
            }
        }
        assert!(!oldest_hash.is_empty(), "must capture the first commit");

        mgr.run_prune_pass().unwrap();
        let remaining = mgr.list_internal().unwrap();

        std::env::remove_var("JOEY_TEST_MAX_SNAPSHOTS_PER_PROJECT");

        assert_eq!(
            remaining.len(),
            3,
            "expected exactly 3 checkpoints after capping, found {}: {:?}",
            remaining.len(),
            remaining.iter().map(|c| c.number).collect::<Vec<_>>()
        );
        // Newest-3 retention (regression test for the retention inversion
        // bug where the OLDEST N survived instead).
        let numbers: Vec<usize> = remaining.iter().map(|c| c.number).collect();
        assert_eq!(numbers, vec![6, 5, 4], "newest-first order expected");
        // Messages/dates survive the rebuild.
        assert_eq!(remaining[0].message, "cp 5");
        // The original first commit must be gone from the object store.
        assert_pruned_commits_gone(&mgr, &remaining);

        // Checkpointing must continue cleanly on the rebuilt chain.
        std::fs::write(work_tree.join("f.txt"), "v-after").unwrap();
        let cp7 = mgr.checkpoint("after prune");
        assert_eq!(cp7, Some(7), "checkpoint numbering continues after prune");
        let list = mgr.list_internal().unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].number, 7);
    }

    /// Regression-test helper: every commit hash from the pre-prune history
    /// that is NOT in `remaining` must be absent from the object store
    /// (`git cat-file -e` fails) — i.e. pruning really deleted them.
    fn assert_pruned_commits_gone(mgr: &CheckpointManager, remaining: &[Checkpoint]) {
        let kept: Vec<&str> = remaining.iter().map(|c| c.commit_hash.as_str()).collect();
        let log = mgr
            .run_prune_git(
                &["log", "--all", "--pretty=format:%H"],
                &[],
            )
            .unwrap_or_default();
        for hash in log.lines().filter(|l| !l.is_empty()) {
            assert!(
                kept.contains(&hash),
                "commit {} survived pruning but is not in the retained set {:?}",
                hash,
                kept
            );
        }
        // Belt-and-suspenders: any dangling (unreachable) leftover objects
        // would also be a bug — gc --prune=now must have swept them.
        let fsck = mgr
            .run_prune_git(&["fsck", "--no-progress", "--unreachable"], &[])
            .unwrap_or_default();
        assert!(
            fsck.trim().is_empty(),
            "no unreachable objects should remain after gc, fsck said: {}",
            fsck
        );
    }

    #[test]
    fn git_timeout_enforced() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        // Prepend a fake, hanging `git` script to PATH.
        let fake_bin_dir = tempfile::tempdir().unwrap();
        let fake_git = fake_bin_dir.path().join("git");
        // Sleep long enough to trip even the shrunk test timeout (>= 2x).
        std::fs::write(&fake_git, "#!/bin/sh\nsleep 5\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_git).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_git, perms).unwrap();
        }

        let orig_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", fake_bin_dir.path().display(), orig_path);
        std::env::set_var("PATH", &new_path);
        // Shrink the per-call timeout so the test trips it in milliseconds
        // instead of waiting out the 5s production default. The fake git
        // sleeps 5s — far beyond the 400ms override (kept >= 2x it).
        std::env::set_var("JOEY_TEST_GIT_TIMEOUT_MS", "400");

        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();
        std::fs::write(work_tree.join("a.txt"), "x").unwrap();

        let mut mgr = CheckpointManager::new("timeout-test", work_tree);
        let start = Instant::now();
        let result = mgr.checkpoint("should time out");
        let elapsed = start.elapsed();

        std::env::set_var("PATH", &orig_path);
        std::env::remove_var("JOEY_TEST_GIT_TIMEOUT_MS");

        assert!(result.is_none(), "hung git call should fail gracefully");
        assert!(
            elapsed < Duration::from_secs(4),
            "should not hang beyond the 400ms test timeout + overhead, took {:?}",
            elapsed
        );
    }

    #[test]
    fn legacy_shadow_repos_discarded() {
        let _lock = crate::test_env_lock();
        if !git_available() {
            return;
        }
        let (_home, dir, _guard) = test_setup();
        let work_tree = dir.path();
        std::fs::write(work_tree.join("a.txt"), "x").unwrap();

        // Create a stale pre-v2-style per-session directory.
        let legacy_dir = checkpoints_base().join("old-session-123");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let mut mgr = CheckpointManager::new("legacy-test", work_tree);
        mgr.checkpoint("first").unwrap();
        mgr.run_prune_pass().unwrap();

        assert!(
            !legacy_dir.exists(),
            "legacy per-session shadow repo should be discarded"
        );
    }
}
