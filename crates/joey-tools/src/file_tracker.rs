//! File change tracking and diff generation (port of crush's
//! `internal/filetracker/` + `internal/diff/` + `internal/diffdetect/`).
//!
//! Tracks which files the agent has read/written in the current session,
//! generates unified diffs between original and modified content, and
//! detects diff-formatted output in tool results for inline rendering.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use once_cell::sync::Lazy;

/// The global file tracker for the current session.
static TRACKER: Lazy<Mutex<FileTracker>> = Lazy::new(|| Mutex::new(FileTracker::new()));

/// A per-session file tracker that records read and write operations.
pub struct FileTracker {
    /// path → first-seen content (the original, before any agent edits).
    originals: HashMap<String, String>,
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
            originals: HashMap::new(),
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
        let mut t = TRACKER.lock().unwrap();
        let key = normalize_path(path);
        t.read_times.insert(key.clone(), SystemTime::now());
        // Snapshot original content on first read if not already tracked.
        if let Some(content) = content {
            t.originals.entry(key).or_insert_with(|| content.to_string());
        }
    }

    /// Record that a file was written/modified.
    pub fn record_write(path: &str) {
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
        let t = TRACKER.lock().unwrap();
        t.originals.get(&normalize_path(path)).cloned()
    }

    /// List all files modified in this session.
    pub fn modified_files() -> Vec<String> {
        let t = TRACKER.lock().unwrap();
        t.modified_files.clone()
    }

    /// List all files read in this session.
    pub fn read_files() -> Vec<String> {
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
        let mut t = TRACKER.lock().unwrap();
        let writes = std::mem::take(&mut t.pending_writes);
        let deletes = std::mem::take(&mut t.pending_deletes);
        // Drop the lock while doing disk I/O so we don't hold it across reads.
        // (Originals map is cloned for the same reason.)
        let originals = t.originals.clone();
        drop(t);

        let mut out = Vec::with_capacity(writes.len() + deletes.len());

        for path in &writes {
            let before = originals.get(path).cloned().unwrap_or_default();
            // Try to decode the current on-disk content as UTF-8.
            let after_bytes = std::fs::read(path).ok();
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
                generate_diff(&before, &after, path)
            };
            out.push(PendingDiff {
                path: path.clone(),
                kind,
                before,
                after,
                diff,
                is_binary,
            });
        }

        for path in &deletes {
            // Prior content becomes the removal side.
            let before = originals.get(path).cloned().unwrap_or_default();
            let diff = if before.is_empty() {
                DiffResult {
                    path: path.clone(),
                    diff: String::new(),
                    added: 0,
                    removed: 0,
                }
            } else {
                generate_diff(&before, "", path)
            };
            out.push(PendingDiff {
                path: path.clone(),
                kind: PendingDiffKind::Delete,
                before,
                after: String::new(),
                diff,
                is_binary: false,
            });
        }

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

/// Generate a unified diff between two strings.
/// Uses a simple line-by-line LCS algorithm (sufficient for display).
pub fn generate_diff(before: &str, after: &str, filename: &str) -> DiffResult {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    // Compute LCS table for line-level diff.
    let lcs = lcs_table(&before_lines, &after_lines);

    // Backtrack to produce the diff.
    let mut diff_lines = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    // Unified diff header.
    diff_lines.push(format!("--- a/{}", filename));
    diff_lines.push(format!("+++ b/{}", filename));

    // Hunk generation: find changed regions with context.
    let hunks = compute_hunks(&before_lines, &after_lines, &lcs, 3);
    for hunk in &hunks {
        diff_lines.push(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start + 1,
            hunk.old_len,
            hunk.new_start + 1,
            hunk.new_len
        ));
        for entry in &hunk.lines {
            match entry {
                DiffEntry::Context(line) => {
                    diff_lines.push(format!(" {}", line));
                }
                DiffEntry::Add(line) => {
                    diff_lines.push(format!("+{}", line));
                    added += 1;
                }
                DiffEntry::Remove(line) => {
                    diff_lines.push(format!("-{}", line));
                    removed += 1;
                }
            }
        }
    }

    DiffResult {
        path: filename.to_string(),
        diff: diff_lines.join("\n"),
        added,
        removed,
    }
}

/// Normalize a path for consistent tracking.
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

// ─── LCS diff algorithm ──────────────────────────────────────────────

#[derive(Debug)]
enum DiffEntry {
    Context(String),
    Add(String),
    Remove(String),
}

#[derive(Default)]
struct Hunk {
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
    lines: Vec<DiffEntry>,
}

// Helper trait so add_entry can accept the local RawEntry type inside compute_hunks.
trait RawEntryLike {
    fn kind(&self) -> u8;
    fn line(&self) -> &str;
}

fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
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
    table
}

fn compute_hunks(
    a: &[&str],
    b: &[&str],
    lcs: &[Vec<usize>],
    context: usize,
) -> Vec<Hunk> {
    // Walk the LCS to produce the raw diff entries with indices.
    struct RawEntry {
        kind: u8, // 0=context, 1=add, 2=remove
        line: String,
        old_idx: Option<usize>,
        new_idx: Option<usize>,
    }

    impl RawEntryLike for RawEntry {
        fn kind(&self) -> u8 {
            self.kind
        }
        fn line(&self) -> &str {
            &self.line
        }
    }

    let mut raw: Vec<RawEntry> = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            raw.push(RawEntry {
                kind: 0,
                line: a[i].to_string(),
                old_idx: Some(i),
                new_idx: Some(j),
            });
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            raw.push(RawEntry {
                kind: 2,
                line: a[i].to_string(),
                old_idx: Some(i),
                new_idx: None,
            });
            i += 1;
        } else {
            raw.push(RawEntry {
                kind: 1,
                line: b[j].to_string(),
                old_idx: None,
                new_idx: Some(j),
            });
            j += 1;
        }
    }
    while i < a.len() {
        raw.push(RawEntry {
            kind: 2,
            line: a[i].to_string(),
            old_idx: Some(i),
            new_idx: None,
        });
        i += 1;
    }
    while j < b.len() {
        raw.push(RawEntry {
            kind: 1,
            line: b[j].to_string(),
            old_idx: None,
            new_idx: Some(j),
        });
        j += 1;
    }

    // Find changed regions and group into hunks with context.
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_hunk: Option<Hunk> = None;
    let mut context_count = 0usize;

    for entry in &raw {
        let is_change = entry.kind != 0;
        match &mut current_hunk {
            None => {
                if is_change {
                    // Start a new hunk.
                    let old_start = entry.old_idx.unwrap_or(0).saturating_sub(context);
                    let new_start = entry.new_idx.unwrap_or(0).saturating_sub(context);
                    let mut hunk = Hunk {
                        old_start,
                        old_len: 0,
                        new_start,
                        new_len: 0,
                        lines: Vec::new(),
                    };
                    add_entry(&mut hunk, entry);
                    current_hunk = Some(hunk);
                    context_count = 0;
                }
            }
            Some(hunk) => {
                if is_change {
                    add_entry(hunk, entry);
                    context_count = 0;
                } else {
                    context_count += 1;
                    if context_count <= context {
                        add_entry(hunk, entry);
                    } else {
                        // Close the hunk.
                        finalize_hunk(hunk);
                        hunks.push(std::mem::replace(hunk, Hunk::default()));
                        current_hunk = None;
                        context_count = 0;
                    }
                }
            }
        }
    }
    if let Some(mut hunk) = current_hunk {
        finalize_hunk(&mut hunk);
        hunks.push(hunk);
    }
    hunks
}

fn add_entry(hunk: &mut Hunk, entry: &impl RawEntryLike) {
    match entry.kind() {
        0 => hunk.lines.push(DiffEntry::Context(entry.line().to_string())),
        1 => hunk.lines.push(DiffEntry::Add(entry.line().to_string())),
        _ => hunk.lines.push(DiffEntry::Remove(entry.line().to_string())),
    }
}

fn finalize_hunk(hunk: &mut Hunk) {
    hunk.old_len = hunk.lines.iter().filter(|e| !matches!(e, DiffEntry::Add(_))).count();
    hunk.new_len = hunk
        .lines
        .iter()
        .filter(|e| !matches!(e, DiffEntry::Remove(_)))
        .count();
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
