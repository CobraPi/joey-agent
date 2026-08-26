//! Shared smart-completion engine (port of upstream `hermes_cli/commands.py`
//! `SlashCommandCompleter` internals: @-context refs, filesystem path
//! completion, fuzzy project-file search).
//!
//! Surface-neutral: the reedline completer (CLI) and the ratatui popup (TUI)
//! both consume [`CompletionItem`] lists from this module. The only shared
//! mutable state is the project-file cache (rg listing refreshed every 5s)
//! owned by [`CompletionEngine`]. The TUI refreshes it asynchronously (a
//! stale cache triggers a background refresh, never a UI stall); the CLI
//! refreshes synchronously (Tab-press latency budget).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One completion offer. `replacement` is the text substituted for the typed
/// word; `display`/`meta` feed the popup or menu row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// Text that replaces the typed word (no leading space unless intended).
    pub replacement: String,
    /// Short display name (menu left column).
    pub display: String,
    /// Right-column metadata (size label, "dir", description…).
    pub meta: String,
}

impl CompletionItem {
    fn new(replacement: impl Into<String>, display: impl Into<String>, meta: impl Into<String>) -> Self {
        Self { replacement: replacement.into(), display: display.into(), meta: meta.into() }
    }
}

/// Static @-context references (commands.py `_STATIC_REFS`).
pub const STATIC_CONTEXT_REFS: &[(&str, &str)] = &[
    ("@diff", "Git working tree diff"),
    ("@staged", "Git staged diff"),
    ("@file:", "Attach a file"),
    ("@folder:", "Attach a folder"),
    ("@git:", "Git log with diffs (e.g. @git:5)"),
    ("@url:", "Fetch web content"),
];

/// Pipe-separated subcommand extraction from an args hint, e.g.
/// `[on|off|status]` → `["on", "off", "status"]` (commands.py `_PIPE_SUBS_RE`).
pub fn pipe_subcommands(args_hint: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in args_hint.split(|c: char| c == '[' || c == ']') {
        let part = part.trim();
        if part.contains('|')
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '|' || c == '-' || c == '_' || c.is_ascii_digit())
        {
            for tok in part.split('|') {
                let tok = tok.trim();
                if !tok.is_empty() && !out.iter().any(|s: &String| s == tok) {
                    out.push(tok.to_string());
                }
            }
            return out;
        }
    }
    out
}

/// Extract the current word if it is path-like (commands.py
/// `_extract_path_word`): starts with `./`, `../`, `~/`, `/`, or contains a
/// `/` separator. URLs (containing `://`) are excluded.
pub fn extract_path_word(text_before_cursor: &str) -> Option<String> {
    let word = current_word(text_before_cursor)?;
    if word.contains("://") {
        return None; // URL — never a useful local-path completion
    }
    if word.starts_with("./")
        || word.starts_with("../")
        || word.starts_with("~/")
        || word.starts_with('/')
        || word.contains('/')
    {
        Some(word)
    } else {
        None
    }
}

/// Extract a bare `@` token (commands.py `_extract_context_word`).
pub fn extract_context_word(text_before_cursor: &str) -> Option<String> {
    let word = current_word(text_before_cursor)?;
    if word.starts_with('@') {
        Some(word)
    } else {
        None
    }
}

/// The whitespace-delimited word under the cursor (may be empty when the
/// cursor follows a space).
pub fn current_word(text: &str) -> Option<String> {
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_whitespace())
        .last()
        .map(|(i, _)| i);
    match start {
        Some(i) => Some(text[i..].to_string()),
        None if text.is_empty() => Some(String::new()),
        None => {
            // Text ends with whitespace (or is all whitespace) — empty word.
            Some(String::new())
        }
    }
}

/// Compact file-size label (commands.py `_file_size_label`).
fn file_size_label(path: &Path) -> String {
    let Ok(size) = std::fs::metadata(path) else {
        return String::new();
    };
    if !size.is_file() {
        return String::new();
    }
    let size = size.len();
    if size < 1024 {
        format!("{size}B")
    } else if size < 1024 * 1024 {
        format!("{:.0}K", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:.1}M", size as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1}G", size as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Expand `~` in a path word.
fn expand_tilde(word: &str) -> PathBuf {
    if let Some(rest) = word.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    } else if word == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    PathBuf::from(word)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Directory listing for path completion (commands.py `_path_completions`).
/// Up to `limit` entries matching the typed prefix (case-insensitive),
/// sorted, dirs first with a trailing `/`.
pub fn path_completions(word: &str, limit: usize) -> Vec<CompletionItem> {
    let mut out = Vec::new();
    if word.contains("://") {
        return out;
    }
    let expanded = expand_tilde(word);
    let (search_dir, prefix) = if word.ends_with('/') {
        (expanded.clone(), String::new())
    } else {
        (
            expanded.parent().unwrap_or(Path::new(".")).to_path_buf(),
            expanded
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    };
    let Ok(entries) = std::fs::read_dir(&search_dir) else {
        return out;
    };
    let prefix_lower = prefix.to_lowercase();
    let mut names: Vec<(String, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !prefix.is_empty() && !name.to_lowercase().starts_with(&prefix_lower) {
            continue;
        }
        let is_dir = entry.path().is_dir();
        names.push((name, is_dir));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    names.sort_by_key(|(_, d)| !*d);
    for (name, is_dir) in names.into_iter().take(limit) {
        let full = search_dir.join(&name);
        let mut replacement = if word.starts_with('~') {
            let rel = full
                .strip_prefix(home_dir().unwrap_or_default())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| full.to_string_lossy().into_owned());
            format!("~/{rel}")
        } else if word.starts_with('/') {
            full.to_string_lossy().into_owned()
        } else {
            let typed_dir = word
                .rsplit_once('/')
                .map(|(d, _)| format!("{d}/"))
                .unwrap_or_default();
            format!("{typed_dir}{name}")
        };
        let mut display = name.clone();
        if is_dir {
            replacement.push('/');
            display.push('/');
        }
        let meta = if is_dir { "dir".to_string() } else { file_size_label(&full) };
        out.push(CompletionItem::new(replacement, display, meta));
    }
    out
}

/// Cache TTL for the project-file listing (commands.py: 5s).
const FILE_CACHE_TTL: Duration = Duration::from_secs(5);
/// Hard cap on the rg/fd subprocess runtime.
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
/// Cap on cached entries (commands.py: 5000).
const MAX_CACHED_FILES: usize = 5000;

#[derive(Default)]
struct FileCache {
    files: Vec<String>,
    when: Option<Instant>,
    cwd: PathBuf,
}

struct EngineInner {
    file_cache: Mutex<FileCache>,
    refresh_in_flight: AtomicBool,
}

/// The completion engine: project-file cache + @-context fuzzy search.
/// Cheaply cloneable (`Arc`-backed) so the TUI can hand it to a background
/// refresh thread.
#[derive(Clone)]
pub struct CompletionEngine {
    inner: std::sync::Arc<EngineInner>,
}

impl Default for CompletionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionEngine {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(EngineInner {
                file_cache: Mutex::new(FileCache::default()),
                refresh_in_flight: AtomicBool::new(false),
            }),
        }
    }

    /// List project files under `cwd` (rg first — fast, respects .gitignore —
    /// then fd), bounded by [`LIST_TIMEOUT`] via a poll loop (std has no
    /// subprocess timeout).
    fn list_project_files_blocking(cwd: &Path) -> Vec<String> {
        for (tool, args) in [("rg", &["--files"][..]), ("fd", &["--type", "f"][..])] {
            let Ok(mut child) = std::process::Command::new(tool)
                .args(args)
                .current_dir(cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                continue;
            };
            // Drain stdout on a dedicated thread BEFORE polling for exit.
            // Without this, a listing larger than the OS pipe buffer (~64KB
            // on macOS — e.g. `rg --files` on a large repo) blocks the child
            // on a full pipe, it never exits, and the poll loop below spins
            // until the kill deadline: the completion engine then silently
            // returns nothing for every large project.
            let stdout_pipe = match child.stdout.take() {
                Some(s) => s,
                None => continue,
            };
            let reader_handle = std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                let _ = std::io::BufReader::new(stdout_pipe).read_to_string(&mut buf);
                buf
            });
            let deadline = Instant::now() + LIST_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let text = reader_handle.join().unwrap_or_default();
                        if !status.success() || text.is_empty() {
                            break;
                        }
                        let files: Vec<String> = text
                            .lines()
                            .filter(|l| !l.is_empty())
                            .take(MAX_CACHED_FILES)
                            .map(str::to_string)
                            .collect();
                        if !files.is_empty() {
                            return files;
                        }
                        break;
                    }
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            // Killing the child closes the pipe; the reader
                            // thread then sees EOF and terminates.
                            let _ = reader_handle.join();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => {
                        let _ = reader_handle.join();
                        break;
                    }
                }
            }
        }
        Vec::new()
    }

    /// Synchronous refresh (CLI path: Tab-press budget).
    pub fn refresh_blocking(&self, cwd: &Path) {
        let files = Self::list_project_files_blocking(cwd);
        let mut cache = self.inner.file_cache.lock().unwrap();
        cache.files = files;
        cache.when = Some(Instant::now());
        cache.cwd = cwd.to_path_buf();
    }

    /// Asynchronous refresh (TUI path): spawn a background thread when no
    /// refresh is already in flight. Never blocks the UI task.
    pub fn refresh_async(&self, cwd: &Path) {
        if self.inner.refresh_in_flight.swap(true, Ordering::Relaxed) {
            return; // one in flight is enough
        }
        let engine = self.clone();
        let cwd = cwd.to_path_buf();
        std::thread::spawn(move || {
            engine.refresh_blocking(&cwd);
            engine.inner.refresh_in_flight.store(false, Ordering::Relaxed);
        });
    }

    /// Cached (possibly stale) file list; refreshes synchronously when the
    /// cache is older than the TTL. CLI semantics (Tab-press latency).
    pub fn project_files_blocking(&self, cwd: &Path) -> Vec<String> {
        {
            let cache = self.inner.file_cache.lock().unwrap();
            let fresh = cache.when.map(|t| t.elapsed() < FILE_CACHE_TTL).unwrap_or(false);
            if fresh && cache.cwd == cwd && !cache.files.is_empty() {
                return cache.files.clone();
            }
        }
        self.refresh_blocking(cwd);
        self.inner.file_cache.lock().unwrap().files.clone()
    }

    /// Stale-tolerant read (TUI semantics): return the cached list without
    /// refreshing; when older than the TTL, kick a background refresh so the
    /// NEXT popup render has fresh data.
    pub fn project_files_stale_ok(&self, cwd: &Path) -> Vec<String> {
        let needs_refresh = {
            let cache = self.inner.file_cache.lock().unwrap();
            let fresh = cache.when.map(|t| t.elapsed() < FILE_CACHE_TTL).unwrap_or(false);
            !(fresh && cache.cwd == cwd && !cache.files.is_empty())
        };
        if needs_refresh {
            self.refresh_async(cwd);
        }
        self.inner.file_cache.lock().unwrap().files.clone()
    }

    /// @-context completions (commands.py `_context_completions`).
    /// `word` is the bare `@…` token under the cursor.
    pub fn context_completions(&self, word: &str, cwd: &Path, files: &[String], limit: usize) -> Vec<CompletionItem> {
        let lowered = word.to_lowercase();
        let mut out = Vec::new();

        // Static refs first (skip an exact already-typed match).
        for (candidate, meta) in STATIC_CONTEXT_REFS {
            if candidate.to_lowercase().starts_with(&lowered) && candidate.to_lowercase() != lowered {
                out.push(CompletionItem::new(*candidate, *candidate, *meta));
            }
        }

        // @file: / @folder: (and the bare @file / @folder forms) delegate to
        // filtered directory listings.
        for prefix in ["@file:", "@folder:"] {
            let bare = &prefix[..prefix.len() - 1];
            if word == bare || word.starts_with(prefix) {
                let want_dir = prefix == "@folder:";
                let path_part = if word == bare { "" } else { &word[prefix.len()..] };
                let expanded = expand_tilde(path_part);
                let (search_dir, match_prefix) = if path_part.is_empty() || path_part.ends_with('/') {
                    (PathBuf::from("."), String::new())
                } else {
                    (
                        expanded.parent().unwrap_or(Path::new(".")).to_path_buf(),
                        expanded
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    )
                };
                let Ok(entries) = std::fs::read_dir(cwd.join(&search_dir)) else {
                    return out;
                };
                let mp_lower = match_prefix.to_lowercase();
                let mut names: Vec<(String, bool)> = entries
                    .flatten()
                    .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path().is_dir()))
                    .filter(|(name, is_dir)| {
                        (match_prefix.is_empty() || name.to_lowercase().starts_with(&mp_lower))
                            && *is_dir == want_dir
                    })
                    .collect();
                names.sort();
                for (name, is_dir) in names.into_iter().take(limit) {
                    let full = search_dir.join(&name);
                    let rel = full.to_string_lossy().into_owned();
                    let suffix = if is_dir { "/" } else { "" };
                    let meta = if is_dir { "dir".to_string() } else { file_size_label(&cwd.join(&full)) };
                    out.push(CompletionItem::new(
                        format!("{prefix}{rel}{suffix}"),
                        format!("{name}{suffix}"),
                        meta,
                    ));
                }
                return out;
            }
        }

        // Bare @ or @partial — fuzzy project-wide file search.
        let query = &word[1..];
        out.extend(fuzzy_file_completions(query, cwd, files, limit));
        out
    }
}

/// Fuzzy project-wide file search (commands.py `_fuzzy_file_completions`).
/// `files` is the caller-supplied project listing (cached upstream).
pub fn fuzzy_file_completions(query: &str, cwd: &Path, files: &[String], limit: usize) -> Vec<CompletionItem> {
    let mut out = Vec::new();
    if query.is_empty() {
        for fp in files.iter().take(limit) {
            let is_dir = fp.ends_with('/');
            let filename = fp.rsplit('/').next().unwrap_or(fp);
            let kind = if is_dir { "folder" } else { "file" };
            let meta = if is_dir { "dir".to_string() } else { file_size_label(&cwd.join(fp)) };
            out.push(CompletionItem::new(
                format!("@{kind}:{fp}"),
                filename.to_string(),
                if meta.is_empty() { fp.clone() } else { format!("{fp}  {meta}") },
            ));
        }
        return out;
    }
    let mut scored: Vec<(u32, &String)> = files
        .iter()
        .filter_map(|fp| {
            let s = score_path(fp, query);
            (s > 0).then_some((s, fp))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    for (_, fp) in scored.into_iter().take(limit) {
        let is_dir = fp.ends_with('/');
        let filename = fp.rsplit('/').next().unwrap_or(fp);
        let kind = if is_dir { "folder" } else { "file" };
        let meta = if is_dir { "dir".to_string() } else { file_size_label(&cwd.join(fp)) };
        out.push(CompletionItem::new(
            format!("@{kind}:{fp}"),
            filename.to_string(),
            if meta.is_empty() { fp.clone() } else { format!("{fp}  {meta}") },
        ));
    }
    out
}

/// Fuzzy path score (commands.py `_score_path`). Higher = better.
pub fn score_path(filepath: &str, query: &str) -> u32 {
    if query.is_empty() {
        return 1;
    }
    let filename = filepath.rsplit('/').next().unwrap_or(filepath);
    let lower_file = filename.to_lowercase();
    let lower_path = filepath.to_lowercase();
    let lower_q = query.to_lowercase();
    if lower_file == lower_q {
        return 100;
    }
    if lower_file.starts_with(&lower_q) {
        return 80;
    }
    if lower_file.contains(&lower_q) {
        return 60;
    }
    if lower_path.contains(&lower_q) {
        return 40;
    }
    let file_chars: Vec<char> = lower_file.chars().collect();
    let q: Vec<char> = lower_q.chars().collect();
    let mut qi = 0usize;
    for &c in &file_chars {
        if qi < q.len() && c == q[qi] {
            qi += 1;
        }
    }
    if qi == q.len() {
        let mut boundary_hits = 0usize;
        let mut qi = 0usize;
        let mut prev = '_';
        for &c in &file_chars {
            if qi < q.len() && c == q[qi] {
                if prev == '_' || prev == '-' || prev == '/' || prev == '.' {
                    boundary_hits += 1;
                }
                qi += 1;
            }
            prev = c;
        }
        if boundary_hits * 2 >= q.len() {
            return 35;
        }
        return 25;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_subcommands_extraction() {
        assert_eq!(pipe_subcommands("[on|off|status]"), vec!["on", "off", "status"]);
        assert_eq!(
            pipe_subcommands("[status|pool|enable|disable|help]"),
            vec!["status", "pool", "enable", "disable", "help"]
        );
        assert_eq!(pipe_subcommands("[model] [--global]"), Vec::<String>::new());
        assert_eq!(pipe_subcommands(""), Vec::<String>::new());
        // Options group is not a pipe run — skipped.
        assert_eq!(pipe_subcommands("[here [N] | focus topic | --preview|--dry-run]"), Vec::<String>::new());
    }

    #[test]
    fn extract_path_word_cases() {
        assert_eq!(extract_path_word("look at ./src/main.py").as_deref(), Some("./src/main.py"));
        assert_eq!(extract_path_word("edit ~/docs/").as_deref(), Some("~/docs/"));
        assert_eq!(extract_path_word("read /etc/hosts").as_deref(), Some("/etc/hosts"));
        assert_eq!(extract_path_word("check ../config.yaml").as_deref(), Some("../config.yaml"));
        assert_eq!(extract_path_word("open src/utils/helpers.py").as_deref(), Some("src/utils/helpers.py"));
        assert_eq!(extract_path_word("hello world"), None);
        assert_eq!(extract_path_word("see https://example.com/x"), None);
    }

    #[test]
    fn extract_context_word_cases() {
        assert_eq!(extract_context_word("check @").as_deref(), Some("@"));
        assert_eq!(extract_context_word("check @compl").as_deref(), Some("@compl"));
        assert_eq!(extract_context_word("no token here"), None);
    }

    #[test]
    fn current_word_basics() {
        assert_eq!(current_word("hello wor"), Some("wor".to_string()));
        assert_eq!(current_word("hello "), Some("".to_string()));
        assert_eq!(current_word(""), Some("".to_string()));
        assert_eq!(current_word("word"), Some("word".to_string()));
    }

    #[test]
    fn static_refs_offered_for_bare_at() {
        let e = CompletionEngine::new();
        let items = e.context_completions("@", Path::new("."), &[], 30);
        let values: Vec<&str> = items.iter().map(|i| i.replacement.as_str()).collect();
        assert!(values.contains(&"@diff"));
        assert!(values.contains(&"@url:"));
    }

    #[test]
    fn static_refs_filtered_by_prefix() {
        let e = CompletionEngine::new();
        let items = e.context_completions("@fi", Path::new("."), &[], 30);
        assert!(items.iter().any(|i| i.replacement == "@file:"));
        assert!(!items.iter().any(|i| i.replacement == "@diff"));
    }

    #[test]
    fn exact_static_ref_not_reoffered() {
        let e = CompletionEngine::new();
        let items = e.context_completions("@diff", Path::new("."), &[], 30);
        assert!(!items.iter().any(|i| i.replacement == "@diff"));
    }

    #[test]
    fn fuzzy_ranking_over_supplied_files() {
        let files = vec![
            "src/main.rs".to_string(),
            "src/domain.rs".to_string(),
            "src/file_operations.rs".to_string(),
        ];
        let items = fuzzy_file_completions("main", Path::new("."), &files, 10);
        assert!(!items.is_empty());
        assert_eq!(items[0].replacement, "@file:src/main.rs");
        // Boundary initials.
        let items = fuzzy_file_completions("fo", Path::new("."), &files, 10);
        assert!(items.iter().any(|i| i.replacement == "@file:src/file_operations.rs"));
    }

    #[test]
    fn file_size_label_compact() {
        let dir = std::env::temp_dir().join("joey_completion_test_size");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("f.txt");
        std::fs::write(&f, "123456789").unwrap(); // 9 bytes
        assert_eq!(file_size_label(&f), "9B");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn score_path_ranking() {
        assert_eq!(score_path("src/file_operations.rs", "file_operations.rs"), 100);
        assert_eq!(score_path("src/main.rs", "main"), 80);
        assert_eq!(score_path("src/domain.rs", "main"), 60);
        assert_eq!(score_path("src/main.rs", "src/main"), 40);
        assert_eq!(score_path("file_operations.rs", "fo"), 35);
        assert!(score_path("zebra.rs", "xyz") == 0);
    }

    #[test]
    fn path_completions_lists_dirs_first() {
        let dir = std::env::temp_dir().join("joey_completion_test_paths");
        let _ = std::fs::create_dir_all(dir.join("subdir"));
        std::fs::write(dir.join("alpha.txt"), "x").unwrap();
        std::fs::write(dir.join("beta.txt"), "x").unwrap();
        let word = format!("{}/", dir.to_string_lossy());
        let items = path_completions(&word, 30);
        let displays: Vec<&str> = items.iter().map(|i| i.display.as_str()).collect();
        assert!(displays.contains(&"subdir/"), "dirs listed: {displays:?}");
        assert!(displays.contains(&"alpha.txt"));
        assert_eq!(items[0].display, "subdir/");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_completions_prefix_filter() {
        let dir = std::env::temp_dir().join("joey_completion_test_prefix");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("alpha.txt"), "x").unwrap();
        std::fs::write(dir.join("beta.txt"), "x").unwrap();
        let word = format!("{}/al", dir.to_string_lossy());
        let items = path_completions(&word, 30);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].display, "alpha.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_files_blocking_survives_pipe_buffer_overflow() {
        // Regression: a listing larger than the OS pipe buffer (~64KB) used
        // to deadlock rg on a full pipe — the poll loop never saw exit and
        // the engine returned nothing for large repos. Generate ~250KB of
        // path output and require the listing to come back populated well
        // inside the timeout budget.
        let dir = std::env::temp_dir().join("joey_completion_test_pipe_overflow");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let filler = "x".repeat(40);
        for i in 0..4000 {
            std::fs::write(dir.join(format!("f{i:06}_{filler}.txt")), "x").unwrap();
        }
        let engine = CompletionEngine::new();
        let start = std::time::Instant::now();
        let files = engine.project_files_blocking(&dir);
        let elapsed = start.elapsed();
        assert!(
            files.iter().any(|f| f.contains("f000000_")),
            "listing must be populated (pipe-buffer deadlock regression), got {} files",
            files.len()
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "listing must not sit through the kill timeout, took {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_files_blocking_lists_tmp_dir() {
        let dir = std::env::temp_dir().join("joey_completion_test_rg");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("unique_name_zq.rs"), "x").unwrap();
        let e = CompletionEngine::new();
        let files = e.project_files_blocking(&dir);
        assert!(files.iter().any(|f| f.contains("unique_name_zq.rs")), "files: {files:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
