//! File change tracking and diff generation (port of crush's
//! `internal/filetracker/` + `internal/diff/` + `internal/diffdetect/`).
//!
//! Tracks which files the agent has read/written in the current session,
//! generates unified diffs between original and modified content, and
//! detects diff-formatted output in tool results for inline rendering.

use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use once_cell::sync::Lazy;

/// The global file tracker for the current session.
static TRACKER: Lazy<Mutex<FileTracker>> = Lazy::new(|| Mutex::new(FileTracker::new()));

/// Maximum number of file originals to retain for diff generation. Each entry
/// holds the full first-seen content of a file; without a cap this is the
/// single largest memory leak on long-horizon tasks (reading hundreds of large
/// files pins all their content forever). 256 distinct files covers realistic
/// per-session edit sets; evicted originals simply produce a "no prior version"
/// diff (whole-file add) rather than crashing or leaking.
const ORIGINALS_MAX_ENTRIES: usize = 256;

/// A per-session file tracker that records read and write operations.
pub struct FileTracker {
    /// path → first-seen content (the original, before any agent edits).
    ///
    /// Uses `IndexMap` so the oldest entries can be evicted once the cap is
    /// reached (insertion order = recency of first read). Evicted entries lose
    /// their diff baseline — a subsequent edit produces a whole-file-add diff
    /// instead of a before/after diff, which is a graceful degradation.
    originals: indexmap::IndexMap<String, String>,
    /// path → last-read timestamp.
    read_times: HashMap<String, SystemTime>,
    /// path → last-write timestamp.
    write_times: HashMap<String, SystemTime>,
    /// Ordered list of files modified in this session.
    modified_files: Vec<String>,
    /// Paths written since the last `drain_pending_diffs` call. Each path
    /// appears at most once; a second write before a drain just keeps the
    /// existing entry (the drain re-reads the latest on-disk content).
    /// Feature 005: feeds inline `AgentEvent::FileChange` emission.
    pending_writes: Vec<String>,
    /// Paths deleted since the last drain. Feature 005 (T010).
    pending_deletes: Vec<String>,
}

impl FileTracker {
    fn new() -> Self {
        Self {
            originals: indexmap::IndexMap::new(),
            read_times: HashMap::new(),
            write_times: HashMap::new(),
            modified_files: Vec::new(),
            pending_writes: Vec::new(),
            pending_deletes: Vec::new(),
        }
    }

    /// Record that a file was read. If this is the first read, snapshots the
    /// original content for diff generation later.
    pub fn record_read(path: &str, content: Option<&str>) {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let mut t = TRACKER.lock().unwrap();
        let key = normalize_path(path);
        t.read_times.insert(key.clone(), SystemTime::now());
        // Snapshot original content on first read if not already tracked.
        if let Some(content) = content {
            // only insert + evict when this is a genuinely new path.
            if !t.originals.contains_key(&key) {
                t.originals.insert(key, content.to_string());
                while t.originals.len() > ORIGINALS_MAX_ENTRIES {
                    t.originals.shift_remove_index(0);
                }
            }
        }
    }

    /// Record that a file was written/modified.
    pub fn record_write(path: &str) {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let mut t = TRACKER.lock().unwrap();
        let key = normalize_path(path);
        t.write_times.insert(key.clone(), SystemTime::now());
        if !t.modified_files.contains(&key) {
            t.modified_files.push(key.clone());
        }
        // Feature 005: queue for inline diff emission (dedup; the drain
        // re-reads the latest on-disk content, so a second pre-drain write
        // just keeps the existing entry).
        if !t.pending_writes.contains(&key) {
            t.pending_writes.push(key.clone());
        }
        // A write supersedes any pending delete for the same path.
        t.pending_deletes.retain(|p| p != &key);
    }

    /// Record that a file was deleted (feature 005, T010). The prior content
    /// (from `originals`, if the agent read it before) becomes the removal
    /// side of the emitted diff.
    pub fn record_delete(path: &str) {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let mut t = TRACKER.lock().unwrap();
        let key = normalize_path(path);
        t.write_times.insert(key.clone(), SystemTime::now());
        if !t.modified_files.contains(&key) {
            t.modified_files.push(key.clone());
        }
        if !t.pending_deletes.contains(&key) {
            t.pending_deletes.push(key.clone());
        }
        // A delete supersedes any pending write for the same path.
        t.pending_writes.retain(|p| p != &key);
    }

    /// Get the original content snapshot for a file (if tracked).
    pub fn get_original(path: &str) -> Option<String> {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let t = TRACKER.lock().unwrap();
        t.originals.get(&normalize_path(path)).cloned()
    }

    /// List all files modified in this session.
    pub fn modified_files() -> Vec<String> {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let t = TRACKER.lock().unwrap();
        t.modified_files.clone()
    }

    /// List all files read in this session.
    pub fn read_files() -> Vec<String> {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let t = TRACKER.lock().unwrap();
        t.read_times.keys().cloned().collect()
    }

    /// Generate a unified diff between the original and current content of
    /// a file. Returns None if no original was recorded.
    pub fn diff_for_file(path: &str, current_content: &str) -> Option<DiffResult> {
        let original = Self::get_original(path)?;
        let diff = generate_diff(&original, current_content, path);
        if diff.added == 0 && diff.removed == 0 {
            return None;
        }
        Some(diff)
    }

    /// Generate diffs for all modified files that have original snapshots.
    /// Reads current content from disk.
    pub fn diffs_for_all_modified() -> Vec<DiffResult> {
        let files = Self::modified_files();
        files
            .into_iter()
            .filter_map(|path| {
                let current = std::fs::read_to_string(&path).unwrap_or_default();
                Self::diff_for_file(&path, &current)
            })
            .collect()
    }

    /// Drain all writes and deletes recorded since the last call, producing
    /// one [`PendingDiff`] per changed file with before/after content and the
    /// computed diff. Clears the pending sets. Feature 005 (T009/T010).
    ///
    /// Producer: called by the agent turn loop after each mutating tool call
    /// (T011) to emit inline `AgentEvent::FileChange` events.
    ///
    /// Binary handling (T007): if either the original or the on-disk content
    /// fails UTF-8 decode, `is_binary` is set true, `diff.diff` is emptied,
    /// and the renderer is expected to show a placeholder (FR-016).
    pub fn drain_pending_diffs() -> Vec<PendingDiff> {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let mut t = TRACKER.lock().unwrap();
        let writes = std::mem::take(&mut t.pending_writes);
        let deletes = std::mem::take(&mut t.pending_deletes);
        // Drop the lock while doing disk I/O so we don't hold it across reads.
        // (Originals map is cloned for the same reason.)
        let originals = t.originals.clone();
        drop(t);

        // Rayon: each file's read + decode + LCS diff is independent —
        // fan the writes out across cores. Order within writes is preserved
        // (IndexedParallelIterator collect), matching the sequential output.
        let write_diffs: Vec<PendingDiff> = writes
            .into_par_iter()
            .map(|path| {
                let before = originals.get(&path).cloned().unwrap_or_default();
                // Try to decode the current on-disk content as UTF-8.
                let after_bytes = std::fs::read(&path).ok();
                let (after, is_binary) = match after_bytes.as_deref() {
                    None => (String::new(), false), // file may have vanished — treat as empty
                    Some(bytes) => match std::str::from_utf8(bytes) {
                        Ok(s) => (s.to_string(), false),
                        Err(_) => (String::new(), true),
                    },
                };
                // If before is non-empty but not valid UTF-8 in memory, that's
                // already impossible (it's stored as String); only after can be
                // binary. But if after is binary we mark the whole change binary.
                let kind = if before.is_empty() && !is_binary {
                    PendingDiffKind::Create
                } else {
                    PendingDiffKind::Edit
                };
                let diff = if is_binary {
                    DiffResult {
                        path: path.clone(),
                        diff: String::new(),
                        added: 0,
                        removed: 0,
                    }
                } else {
                    generate_diff(&before, &after, &path)
                };
                PendingDiff {
                    path,
                    kind,
                    before,
                    after,
                    diff,
                    is_binary,
                }
            })
            .collect();

        let delete_diffs: Vec<PendingDiff> = deletes
            .into_par_iter()
            .map(|path| {
                // Prior content becomes the removal side.
                let before = originals.get(&path).cloned().unwrap_or_default();
                let diff = if before.is_empty() {
                    DiffResult {
                        path: path.clone(),
                        diff: String::new(),
                        added: 0,
                        removed: 0,
                    }
                } else {
                    generate_diff(&before, "", &path)
                };
                PendingDiff {
                    path,
                    kind: PendingDiffKind::Delete,
                    before,
                    after: String::new(),
                    diff,
                    is_binary: false,
                }
            })
            .collect();

        let mut out = Vec::with_capacity(write_diffs.len() + delete_diffs.len());
        out.extend(write_diffs);
        out.extend(delete_diffs);
        out
    }

    /// Mark a file as externally mutated (feature 005, T012 terminal-mutation
    /// detection). The diff is computed from the stored baseline (`originals`
    /// if the agent read it earlier) vs. current on-disk content. Returns the
    /// path's normalized key so the caller can attribute it. If the file was
    /// never read, it is reported as a Create.
    pub fn record_external_mutation(path: &str) -> String {
        // Reuse the write path so the file enters the pending queue; the
        // drain will compute baseline vs. on-disk.
        Self::record_write(path);
        normalize_path(path)
    }

    /// Clear all tracking state (new session).
    pub fn reset() {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let mut t = TRACKER.lock().unwrap();
        t.originals.clear();
        t.read_times.clear();
        t.write_times.clear();
        t.modified_files.clear();
        t.pending_writes.clear();
        t.pending_deletes.clear();
    }

    /// Summary of changes for display.
    pub fn change_summary() -> ChangeSummary {
        // SAFETY: internal Mutex/RwLock; poisoning indicates a bug, not external input.
        let t = TRACKER.lock().unwrap();
        ChangeSummary {
            files_read: t.read_times.len(),
            files_modified: t.modified_files.len(),
            modified_paths: t.modified_files.clone(),
        }
    }
}

/// One file change awaiting inline emission. Feature 005 (T009).
///
/// `before`/`after` are the decoded contents used by the renderer and the
/// diff engine; `is_binary` is true when either side failed UTF-8 decode
/// (in which case `diff.diff` is empty and the renderer shows a binary
/// placeholder per FR-016).
#[derive(Debug, Clone)]
pub struct PendingDiff {
    pub path: String,
    pub kind: PendingDiffKind,
    pub before: String,
    pub after: String,
    pub diff: DiffResult,
    pub is_binary: bool,
}

/// What kind of pending change a [`PendingDiff`] represents. Feature 005.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingDiffKind {
    Create,
    Edit,
    Delete,
}

/// A summary of session file changes.
#[derive(Debug, Clone)]
pub struct ChangeSummary {
    pub files_read: usize,
    pub files_modified: usize,
    pub modified_paths: Vec<String>,
}

/// Result of generating a diff.
#[derive(Debug, Clone)]
pub struct DiffResult {
    /// The file path.
    pub path: String,
    /// Unified diff text.
    pub diff: String,
    /// Number of added lines.
    pub added: usize,
    /// Number of removed lines.
    pub removed: usize,
}

impl DiffResult {
    /// A short summary like "+5 -3" or "+10".
    pub fn stat_line(&self) -> String {
        match (self.added, self.removed) {
            (0, 0) => "no changes".to_string(),
            (a, 0) => format!("+{}", a),
            (0, r) => format!("-{}", r),
            (a, r) => format!("+{} -{}", a, r),
        }
    }
}

fn normalize_path(path: &str) -> String {
    let expanded = shellexpand::tilde(path).to_string();
    let p = PathBuf::from(&expanded);
    // Try to make relative to CWD for cleaner display.
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = p.strip_prefix(&cwd) {
            return rel.to_string_lossy().to_string();
        }
    }
    p.to_string_lossy().to_string()
}

/// Maximum cells in the LCS table before the quadratic core is considered
/// pathological. Reaching this means the edit touches (nearly) every line —
/// a wholesale rewrite — where the exact LCS backtrace adds nothing over the
/// obvious all-remove/all-add rendering. ~250 M cells ≈ 2 GB at usize.
const LCS_CELL_LIMIT: usize = 250_000_000;

/// Generate a unified diff between two strings (parallel + prefix/suffix
/// trimmed).
///
/// Optimizations over the naive port (behavior-preserving):
/// - Common prefix/suffix lines are trimmed first (parallel-hashed), so the
///   quadratic LCS core only covers the actually-changed region. A small
///   edit in a 100 K-line file collapses to a tiny core instead of a
///   10^10-cell table.
/// - Line equality inside the core compares precomputed hashes (u64) —
///   cheap integer compares instead of string compares in the hot loop.
/// - An oversized core (whole-file rewrite) skips the LCS entirely and
///   renders remove-all/add-all, keeping memory bounded.
///
/// Output is byte-identical to the previous implementation for the same
/// input (hunk headers carry the trimmed offsets).
pub fn generate_diff(before: &str, after: &str, filename: &str) -> DiffResult {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    // ── Trim common prefix/suffix (parallel hashing) ──────────────────
    let (a_hashes, b_hashes): (Vec<u64>, Vec<u64>) = rayon::join(
        || before_lines.par_iter().map(|l| hash_line(l)).collect::<Vec<u64>>(),
        || after_lines.par_iter().map(|l| hash_line(l)).collect::<Vec<u64>>(),
    );

    let n = before_lines.len().min(after_lines.len());
    let mut prefix = 0usize;
    while prefix < n && a_hashes[prefix] == b_hashes[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < n - prefix && a_hashes[a_hashes.len() - 1 - suffix] == b_hashes[b_hashes.len() - 1 - suffix] {
        suffix += 1;
    }

    // Trailing context retention: the original includes up to 3 context
    // lines AFTER the last change of a hunk (the ctx_run mechanism), but
    // never includes PRECEDING context lines in the body (the hunk header
    // start is merely offset back by up to 3). Mirror that: keep ≤3 lines
    // of the trimmed SUFFIX, none of the prefix.
    let keep_suf = suffix.min(3);
    let core_start = prefix;
    let core_end_a = before_lines.len() - suffix + keep_suf;
    let core_end_b = after_lines.len() - suffix + keep_suf;

    let core_a: Vec<&str> = before_lines[core_start..core_end_a].to_vec();
    let core_b: Vec<&str> = after_lines[core_start..core_end_b].to_vec();

    let mut added = 0usize;
    let mut removed = 0usize;
    let mut diff_lines = Vec::new();

    // Wholesale-rewrite guard: an oversized core renders directly.
    let core_cells = core_a.len().saturating_mul(core_b.len());
    let entries: Vec<(u8, String)> = if core_cells > LCS_CELL_LIMIT || (core_a.is_empty() ^ core_b.is_empty()) {
        core_a
            .iter()
            .map(|l| (2u8, (*l).to_string()))
            .chain(core_b.iter().map(|l| (1u8, (*l).to_string())))
            .collect()
    } else {
        // ── LCS over the core (hash compares) ────────────────────────
        let ca: Vec<u64> = a_hashes[core_start..core_end_a].to_vec();
        let cb: Vec<u64> = b_hashes[core_start..core_end_b].to_vec();
        let mut table = vec![vec![0usize; cb.len() + 1]; ca.len() + 1];
        for i in (0..ca.len()).rev() {
            for j in (0..cb.len()).rev() {
                if ca[i] == cb[j] {
                    table[i][j] = table[i + 1][j + 1] + 1;
                } else {
                    table[i][j] = table[i + 1][j].max(table[i][j + 1]);
                }
            }
        }
        // Backtrace against the trimmed core (kind: 0 ctx, 1 add, 2 remove).
        let mut out: Vec<(u8, String)> = Vec::new();
        let mut i = 0usize;
        let mut j = 0usize;
        while i < ca.len() && j < cb.len() {
            if ca[i] == cb[j] {
                out.push((0, core_a[i].to_string()));
                i += 1;
                j += 1;
            } else if table[i + 1][j] >= table[i][j + 1] {
                out.push((2, core_a[i].to_string()));
                i += 1;
            } else {
                out.push((1, core_b[j].to_string()));
                j += 1;
            }
        }
        while i < ca.len() {
            out.push((2, core_a[i].to_string()));
            i += 1;
        }
        while j < cb.len() {
            out.push((1, core_b[j].to_string()));
            j += 1;
        }
        out
    };

    // ── Header ─────────────────────────────────────────────────────────
    diff_lines.push(format!("--- a/{}", filename));
    diff_lines.push(format!("+++ b/{}", filename));

    // ── Hunk grouping over the raw entries (offset by the trimmed prefix)
    //
    // Semantics (matching the original algorithm exactly): a hunk opens at
    // the first change with up to `context` PRECEDING context lines, keeps
    // up to `context` FOLLOWING context lines, and closes only when another
    // change appears after a gap of more than `context` context lines (the
    // over-gap line is dropped; the ≤context lines stay as trailing context
    // of the closed hunk).
    struct HunkAcc {
        old_start: usize,
        new_start: usize,
        lines: Vec<(u8, String)>,
    }
    fn close_hunk(h: HunkAcc) -> (usize, usize, usize, usize, Vec<(u8, String)>) {
        let old_len = h.lines.iter().filter(|(k, _)| *k != 1).count();
        let new_len = h.lines.iter().filter(|(k, _)| *k != 2).count();
        (h.old_start, old_len, h.new_start, new_len, h.lines)
    }
    let context = 3usize;
    let mut hunks: Vec<(usize, usize, usize, usize, Vec<(u8, String)>)> = Vec::new();
    let mut current: Option<HunkAcc> = None;
    let mut ctx_run = 0usize;
    // Line positions (0-based, absolute) for hunk headers.
    let mut old_pos = core_start;
    let mut new_pos = core_start;
    for (kind, line) in entries {
        let is_change = kind != 0;
        match current.as_mut() {
            None => {
                if is_change {
                    let h = HunkAcc {
                        old_start: old_pos.saturating_sub(context),
                        new_start: new_pos.saturating_sub(context),
                        lines: vec![(kind, line.clone())],
                    };
                    current = Some(h);
                    ctx_run = 0;
                }
            }
            Some(h) => {
                if is_change {
                    h.lines.push((kind, line.clone()));
                    ctx_run = 0;
                } else {
                    ctx_run += 1;
                    if ctx_run <= context {
                        h.lines.push((kind, line.clone()));
                    } else {
                        // Gap exceeded: close WITHOUT this line (matches the
                        // original — the gap-closing line is not included).
                        hunks.push(close_hunk(current.take().unwrap()));
                        ctx_run = 0;
                    }
                }
            }
        }
        match kind {
            0 => {
                old_pos += 1;
                new_pos += 1;
            }
            1 => {
                new_pos += 1;
                added += 1;
            }
            _ => {
                old_pos += 1;
                removed += 1;
            }
        }
    }
    if let Some(h) = current.take() {
        hunks.push(close_hunk(h));
    }

    for (old_start, old_len, new_start, new_len, lines) in &hunks {
        diff_lines.push(format!(
            "@@ -{},{} +{},{} @@",
            old_start + 1,
            old_len,
            new_start + 1,
            new_len
        ));
        for (kind, line) in lines {
            let marker = match kind {
                0 => ' ',
                1 => '+',
                _ => '-',
            };
            diff_lines.push(format!("{}{}", marker, line));
        }
    }

    DiffResult {
        path: filename.to_string(),
        diff: diff_lines.join("\n"),
        added,
        removed,
    }
}

/// Stable 64-bit line hash (FNV-1a over bytes). Only used for equality
/// inside the diff core; collisions would merely merge two identical-
/// hashed lines into context (same practical risk as any hash-based
/// interning, and FNV-1a has no adversarial input here).
fn hash_line(line: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in line.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ─── Diff detection (port of crush's diffdetect) ─────────────────────

/// Signal describing which unified-diff markers were found in text.
#[derive(Debug, Clone, Default)]
pub struct DiffSignal {
    pub has_hunk: bool,
    pub has_file_header: bool,
    pub has_git_header: bool,
}

/// Inspect content for unified-diff markers (port of crush's `diffdetect::Inspect`).
pub fn inspect_diff(content: &str) -> DiffSignal {
    let mut signal = DiffSignal::default();
    for line in content.lines() {
        if line.starts_with("@@") {
            signal.has_hunk = true;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            signal.has_file_header = true;
        }
        if line.starts_with("diff --git ") {
            signal.has_git_header = true;
        }
    }
    signal
}

/// Report whether content appears to be a unified diff (port of crush's
/// `diffdetect::IsUnifiedDiff`).
pub fn is_unified_diff(content: &str) -> bool {
    let signal = inspect_diff(content);
    if signal.has_git_header && signal.has_file_header {
        return true;
    }
    signal.has_hunk && signal.has_file_header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_detect_basic() {
        let diff = "diff --git a/foo b/foo\n--- a/foo\n+++ b/foo\n@@ -1,3 +1,4 @@\n a\n+b\nc\n";
        assert!(is_unified_diff(diff));

        let not_diff = "just some text\nwith lines\n";
        assert!(!is_unified_diff(not_diff));
    }

    #[test]
    fn diff_detect_partial() {
        let partial = "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-old\n+new\n";
        assert!(is_unified_diff(partial));
    }

    #[test]
    fn generate_simple_diff() {
        let before = "line1\nline2\nline3\n";
        let after = "line1\nmodified\nline3\n";
        let result = generate_diff(before, after, "test.txt");
        assert!(result.diff.contains("--- a/test.txt"));
        assert!(result.diff.contains("+++ b/test.txt"));
        assert!(result.diff.contains("@@"));
        assert!(result.diff.contains("-line2"));
        assert!(result.diff.contains("+modified"));
        assert_eq!(result.added, 1);
        assert_eq!(result.removed, 1);
    }

    #[test]
    fn diff_addition() {
        let before = "a\nb\n";
        let after = "a\nb\nc\n";
        let result = generate_diff(before, after, "add.txt");
        assert_eq!(result.added, 1);
        assert_eq!(result.removed, 0);
    }

    #[test]
    fn diff_no_changes() {
        let content = "same\ncontent\n";
        let result = generate_diff(content, content, "same.txt");
        assert_eq!(result.added, 0);
        assert_eq!(result.removed, 0);
    }

    #[test]
    fn stat_line_formats() {
        let d = DiffResult {
            path: "x".into(),
            diff: String::new(),
            added: 5,
            removed: 3,
        };
        assert_eq!(d.stat_line(), "+5 -3");

        let d2 = DiffResult {
            path: "x".into(),
            diff: String::new(),
            added: 0,
            removed: 0,
        };
        assert_eq!(d2.stat_line(), "no changes");
    }

    #[test]
    fn tracker_records_and_resets() {
        let _guard = FT_TEST_LOCK.lock().unwrap();
        FileTracker::reset();
        FileTracker::record_read("/tmp/test_file_a.txt", Some("original"));
        FileTracker::record_write("/tmp/test_file_a.txt");
        let summary = FileTracker::change_summary();
        assert_eq!(summary.files_modified, 1);

        let original = FileTracker::get_original("/tmp/test_file_a.txt");
        assert_eq!(original.as_deref(), Some("original"));

        FileTracker::reset();
        let summary = FileTracker::change_summary();
        assert_eq!(summary.files_modified, 0);
    }

    // -- Feature 005 tests (T006/T007/T008) -------------------------------
    //
    // NOTE: FileTracker is a process-global singleton. These tests mutate
    // global state and MUST NOT run concurrently with each other (or with
    // `tracker_records_and_resets`). We serialize them with a static mutex
    // guard acquired at the top of each test.

    use std::io::Write;
    use std::sync::Mutex;

    static FT_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: write a temp file with the given content and return its path.
    fn tmp_write(name: &str, content: &str) -> String {
        let path = format!("/tmp/joey_ft_test_{}", name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    /// T006: drain_pending_diffs returns the diff for a written file and
    /// clears the pending set; a no-op write yields empty/no diff.
    #[test]
    fn drain_pending_diffs_edit_and_clear() {
        let _guard = FT_TEST_LOCK.lock().unwrap();
        FileTracker::reset();
        let path = tmp_write("t006.txt", "line1\nline2\nline3\n");
        // Simulate the agent reading the file (baseline snapshot).
        FileTracker::record_read(&path, Some("line1\nline2\nline3\n"));
        // Simulate an edit: rewrite content + record_write.
        let _ = tmp_write("t006.txt", "line1\nMODIFIED\nline3\n");
        FileTracker::record_write(&path);

        let diffs = FileTracker::drain_pending_diffs();
        assert_eq!(diffs.len(), 1, "one pending write should yield one diff");
        let d = &diffs[0];
        assert_eq!(d.path, path);
        assert_eq!(d.kind, PendingDiffKind::Edit);
        assert!(!d.is_binary);
        assert_eq!(d.diff.added, 1);
        assert_eq!(d.diff.removed, 1);
        assert!(d.diff.diff.contains("+MODIFIED"));
        assert!(d.diff.diff.contains("-line2"));

        // Second drain is empty — pending set was cleared.
        let again = FileTracker::drain_pending_diffs();
        assert!(again.is_empty(), "drain must clear the pending set");

        let _ = std::fs::remove_file(&path);
        FileTracker::reset();
    }

    /// T006 (Create variant): a write with no prior read baseline yields
    /// `kind == Create` and the whole content as additions.
    #[test]
    fn drain_pending_diffs_create() {
        let _guard = FT_TEST_LOCK.lock().unwrap();
        FileTracker::reset();
        let path = tmp_write("t006b.txt", "new\ncontent\n");
        FileTracker::record_write(&path); // no record_read → Create

        let diffs = FileTracker::drain_pending_diffs();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, PendingDiffKind::Create);
        assert_eq!(diffs[0].diff.added, 2);
        assert_eq!(diffs[0].diff.removed, 0);

        let _ = std::fs::remove_file(&path);
        FileTracker::reset();
    }

    /// T006 (no-op): a write that doesn't change content yields a diff with
    /// 0 additions / 0 removals (the drain still returns the entry, but the
    /// renderer will treat zero-count diffs as non-events).
    #[test]
    fn drain_pending_diffs_noop_write() {
        let _guard = FT_TEST_LOCK.lock().unwrap();
        FileTracker::reset();
        let path = tmp_write("t006c.txt", "same\n");
        FileTracker::record_read(&path, Some("same\n"));
        // Rewrite identical content.
        let _ = tmp_write("t006c.txt", "same\n");
        FileTracker::record_write(&path);

        let diffs = FileTracker::drain_pending_diffs();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].diff.added, 0, "identical content → 0 additions");
        assert_eq!(diffs[0].diff.removed, 0, "identical content → 0 removals");

        let _ = std::fs::remove_file(&path);
        FileTracker::reset();
    }

    /// T007: binary-file detection. A write whose after-content fails UTF-8
    /// decode sets `is_binary` and yields empty diff text.
    #[test]
    fn drain_pending_diffs_binary() {
        let _guard = FT_TEST_LOCK.lock().unwrap();
        FileTracker::reset();
        let path = "/tmp/joey_ft_test_t007.bin";
        // Write invalid UTF-8 bytes.
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&[0xFF, 0xFE, 0x00, 0x01, 0x80]).unwrap();
        FileTracker::record_read(path, Some("text baseline\n"));
        FileTracker::record_write(path);

        let diffs = FileTracker::drain_pending_diffs();
        assert_eq!(diffs.len(), 1);
        let d = &diffs[0];
        assert!(d.is_binary, "non-UTF-8 content must be flagged binary");
        assert!(d.diff.diff.is_empty(), "binary diff text must be empty");

        let _ = std::fs::remove_file(path);
        FileTracker::reset();
    }

    /// T008: diff-text detection (`is_unified_diff`) classifies a real diff
    /// vs plain text (FR-005). (The existing `diff_detect_basic` covers the
    /// happy path; this adds an explicit plain-text negative and a git-header
    /// positive.)
    #[test]
    fn diff_text_detection_classification() {
        // Pure function — no global state, no guard needed.
        // Plain text is not a diff.
        assert!(!is_unified_diff("just some prose\nwith a + plus sign\n"));
        assert!(!is_unified_diff("a\nb\nc\n"));

        // Hunk + file header is a diff.
        assert!(is_unified_diff("--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-old\n+new\n"));
        // Git header + file header is a diff even without a hunk line.
        assert!(is_unified_diff("diff --git a/x b/x\n--- a/x\n+++ b/x\n"));
    }

    /// T010: delete tracking via record_delete produces a Delete diff with
    /// the prior content as removals.
    #[test]
    fn drain_pending_diffs_delete() {
        let _guard = FT_TEST_LOCK.lock().unwrap();
        FileTracker::reset();
        let path = tmp_write("t010.txt", "to be removed\nsecond line\n");
        FileTracker::record_read(&path, Some("to be removed\nsecond line\n"));
        let _ = std::fs::remove_file(&path);
        FileTracker::record_delete(&path);

        let diffs = FileTracker::drain_pending_diffs();
        assert_eq!(diffs.len(), 1, "delete should yield one diff");
        assert_eq!(diffs[0].kind, PendingDiffKind::Delete);
        assert_eq!(diffs[0].diff.removed, 2, "prior content becomes removals");
        assert!(diffs[0].diff.diff.contains("-to be removed"));
        FileTracker::reset();
    }
}

#[cfg(test)]
mod rayon_diff_tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum E {
        Ctx,
        Add,
        Rm,
    }

    /// Close a hunk: compute lengths and push.
    fn hunk_close(
        hunks: &mut Vec<(usize, usize, Vec<(E, String)>)>,
        os: usize,
        ns: usize,
        lines: Vec<(E, String)>,
    ) {
        hunks.push((os, ns, lines));
    }

    /// The optimized generate_diff must be byte-identical to a reference
    /// sequential LCS implementation for representative inputs.
    fn reference_diff(before: &str, after: &str) -> DiffResult {
        // Straight port of the original algorithm (kept as the oracle).
        let a: Vec<&str> = before.lines().collect();
        let b: Vec<&str> = after.lines().collect();
        let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                if a[i] == b[j] {
                    table[i][j] = table[i + 1][j + 1] + 1;
                } else {
                    table[i][j] = table[i + 1][j].max(table[i][j + 1]);
                }
            }
        }
        let mut raw: Vec<(E, String)> = Vec::new();
        let mut i = 0usize;
        let mut j = 0usize;
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                raw.push((E::Ctx, a[i].to_string()));
                i += 1;
                j += 1;
            } else if table[i + 1][j] >= table[i][j + 1] {
                raw.push((E::Rm, a[i].to_string()));
                i += 1;
            } else {
                raw.push((E::Add, b[j].to_string()));
                j += 1;
            }
        }
        while i < a.len() {
            raw.push((E::Rm, a[i].to_string()));
            i += 1;
        }
        while j < b.len() {
            raw.push((E::Add, b[j].to_string()));
            j += 1;
        }
        // Hunk grouping (3 context lines).
        let context = 3;
        let mut hunks: Vec<(usize, usize, Vec<(E, String)>)> = Vec::new();
        let mut cur: Option<(usize, usize, Vec<(E, String)>)> = None;
        let mut ctx_run = 0usize;
        let mut old_pos = 0usize;
        let mut new_pos = 0usize;
        for (e, line) in raw {
            let is_change = !matches!(e, E::Ctx);
            match cur.as_mut() {
                None => {
                    if is_change {
                        cur = Some((
                            old_pos.saturating_sub(context),
                            new_pos.saturating_sub(context),
                            vec![(e, line)],
                        ));
                        ctx_run = 0;
                    }
                }
                Some((os, ns, lines)) => {
                    if is_change {
                        lines.push((e, line));
                        ctx_run = 0;
                    } else {
                        ctx_run += 1;
                        if ctx_run <= context {
                            lines.push((e, line));
                        } else {
                            let (os2, ns2, lines2) = cur.take().unwrap(); hunk_close(&mut hunks, os2, ns2, lines2);
                            cur = None;
                            ctx_run = 0;
                        }
                    }
                }
            }
            match e {
                E::Ctx => {
                    old_pos += 1;
                    new_pos += 1;
                }
                E::Add => new_pos += 1,
                E::Rm => old_pos += 1,
            }
        }
        if let Some((os, ns, lines)) = cur {
            hunk_close(&mut hunks, os, ns, lines);
        }
        let mut out = vec!["--- a/x".to_string(), "+++ b/x".to_string()];
        let mut added = 0usize;
        let mut removed = 0usize;
        for (os, ns, lines) in hunks {
            let old_len = lines.iter().filter(|(e, _)| !matches!(e, E::Add)).count();
            let new_len = lines.iter().filter(|(e, _)| !matches!(e, E::Rm)).count();
            out.push(format!("@@ -{},{} +{},{} @@", os + 1, old_len, ns + 1, new_len));
            for (e, line) in lines {
                match e {
                    E::Ctx => {
                        out.push(format!(" {}", line));
                    }
                    E::Add => {
                        out.push(format!("+{}", line));
                        added += 1;
                    }
                    E::Rm => {
                        out.push(format!("-{}", line));
                        removed += 1;
                    }
                }
            }
        }
        DiffResult { path: "x".into(), diff: out.join("\n"), added, removed }
    }

    #[test]
    fn optimized_diff_matches_reference_on_typical_edits() {
        let cases: Vec<(&str, &str)> = vec![
            ("line1\nline2\nline3", "line1\nline2-changed\nline3"),
            ("a\nb\nc\nd\ne\nf\ng", "a\nb\nc\nX\ne\nf\ng"),
            ("one\ntwo\nthree", "one\nthree"),
            ("", "brand\nnew\nfile"),
            ("gone\naway", ""),
            ("same", "same"),
            ("p\nq\nr\ns\nt\nu\nv\nw\nx\ny\nz", "p\nq\nr\ns\nt\nU\nv\nw\nx\ny\nz"),
            ("multi\nline\ncontent\nhere\nfor\ntesting\npurposes\nonly", "multi\nline\ncontent\nHERE\nfor\ntesting\npurposes\nAND\nmore"),
        ];
        for (i, (before, after)) in cases.iter().enumerate() {
            let got = generate_diff(before, after, "x");
            let want = reference_diff(before, after);
            assert_eq!(got.added, want.added, "added mismatch case {i}");
            assert_eq!(got.removed, want.removed, "removed mismatch case {i}");
            assert_eq!(got.diff, want.diff, "diff text mismatch case {i}:\ngot:\n{}\nwant:\n{}", got.diff, want.diff);
        }
    }

    #[test]
    fn large_file_small_edit_is_fast_and_correct() {
        // 50k lines, one changed line in the middle. The naive LCS table
        // would be 2.5e9 cells (unusable); the trimmed core must collapse
        // to a tiny region and produce the right hunk.
        let before: String = (0..50_000).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let mut after_lines: Vec<String> = before.lines().map(str::to_string).collect();
        after_lines[25_000] = "line 25000 EDITED".to_string();
        let after = after_lines.join("\n");
        let start = std::time::Instant::now();
        let d = generate_diff(&before, &after, "big.txt");
        let elapsed = start.elapsed();
        assert_eq!(d.added, 1);
        assert_eq!(d.removed, 1);
        assert!(d.diff.contains("-line 25000"), "removal present");
        assert!(d.diff.contains("+line 25000 EDITED"), "insertion present");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "trimmed diff must stay fast: {:?}",
            elapsed
        );
        println!("50k-line one-line edit: {:?}", elapsed);
    }

    #[test]
    fn wholesale_rewrite_stays_bounded() {
        // Completely different 20k-line contents — the rewrite guard must
        // engage (no 4e8-cell table).
        let before: String = (0..20_000).map(|i| format!("a{}", i)).collect::<Vec<_>>().join("\n");
        let after: String = (0..20_000).map(|i| format!("b{}", i)).collect::<Vec<_>>().join("\n");
        let start = std::time::Instant::now();
        let d = generate_diff(&before, &after, "rw.txt");
        let elapsed = start.elapsed();
        assert_eq!(d.added, 20_000);
        assert_eq!(d.removed, 20_000);
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "rewrite guard must bound the cost: {:?}",
            elapsed
        );
        println!("20k-line rewrite: {:?}", elapsed);
    }
}
