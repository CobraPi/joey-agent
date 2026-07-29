//! Verification evidence ledger + stop guard — port of
//! `agent/verification_evidence.py` + `agent/verification_stop.py` +
//! `agent/verify_hooks.py`.
//!
//! Tracks whether the agent has actually verified its code changes (run
//! tests, lint, typecheck, etc.) before declaring the task done. When the
//! agent produces a final text response after editing files but without
//! fresh verification evidence, a "nudge" is injected asking it to verify.
//!
//! The ledger is an in-memory store (the upstream uses SQLite, but the
//! lifecycle is per-session so memory suffices for the Rust port).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use once_cell::sync::Lazy;

// ─── Constants ────────────────────────────────────────────────────────

const MAX_OUTPUT_SUMMARY_CHARS: usize = 2000;
const MAX_VERIFY_NUDGES: usize = 3;
const MAX_VERIFY_ATTEMPTS: usize = 2;
const VERIFY_NUDGE_OUTPUT_LIMIT: usize = 1200;
const VERIFY_NUDGE_MAX_PATHS: usize = 8;

/// File extensions considered non-code (documentation, not requiring verification).
const NON_CODE_EXTS: &[&str] = &[
    ".md", ".txt", ".rst", ".csv", ".json", ".yaml", ".yml", ".toml", ".ini", ".cfg",
    ".conf", ".log", ".lock", ".gitignore", ".env", ".license", ".CHANGELOG",
];

/// Filenames considered non-code.
const NON_CODE_FILES: &[&str] = &["LICENSE", "CHANGELOG", "README", "CONTRIBUTING", "CODE_OF_CONDUCT"];

// ─── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationKind {
    Test,
    Lint,
    Typecheck,
    Build,
    Format,
    Check,
    AdHoc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationScope {
    Targeted,
    Full,
}

/// A single verification event (a command that was classified as verification).
#[derive(Debug, Clone)]
pub struct VerificationEvent {
    pub command: String,
    pub canonical_command: String,
    pub kind: VerificationKind,
    pub scope: VerificationScope,
    pub status: VerificationStatus,
    pub exit_code: i32,
    pub cwd: String,
    pub root: String,
    pub session_id: String,
    pub output_summary: String,
    pub created_at: Instant,
}

/// The status of verification for a session/workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceStatus {
    /// No project facts (no verify commands configured).
    NotApplicable,
    /// Files were edited but no verification has been run.
    Unverified,
    /// Verification passed, but files were edited after it (stale).
    Stale,
    /// Verification passed after the last edit.
    Passed,
    /// Verification failed after the last edit.
    Failed,
}

/// Query result for verification status.
#[derive(Debug, Clone)]
pub struct VerificationQuery {
    pub status: EvidenceStatus,
    pub evidence: Option<VerificationEvent>,
    pub changed_paths: Vec<String>,
    pub last_edit_at: Option<Instant>,
}

// ─── Ledger ───────────────────────────────────────────────────────────

/// Per-session state tracked by the ledger.
#[derive(Debug, Default)]
struct SessionState {
    /// The last verification event for this session+root.
    last_event: Option<VerificationEvent>,
    /// When files were last edited.
    last_edit_at: Option<Instant>,
    /// Paths changed since the last verification.
    changed_paths: Vec<String>,
}

/// The global verification evidence ledger.
pub struct VerificationLedger {
    /// (session_id, root) → state
    sessions: HashMap<(String, String), SessionState>,
}

static LEDGER: Lazy<Mutex<VerificationLedger>> =
    Lazy::new(|| Mutex::new(VerificationLedger::new()));

impl VerificationLedger {
    fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    fn get_or_create(&mut self, session_id: &str, root: &str) -> &mut SessionState {
        self.sessions
            .entry((session_id.to_string(), root.to_string()))
            .or_default()
    }
}

// ─── Public API ───────────────────────────────────────────────────────

/// Record a terminal result. If the command is classified as a verification
/// command, it's recorded as evidence. Returns the event if classified.
pub fn record_terminal_result(
    command: &str,
    cwd: &str,
    session_id: &str,
    exit_code: i32,
    output: &str,
) -> Option<VerificationEvent> {
    let root = find_project_root(cwd);
    let event = classify_verification_command(command, cwd, session_id, exit_code, output, &root)?;

    // SAFETY: LEDGER is an internal Mutex; poisoning only occurs on a
    // prior panic-while-locked, which is a bug.
    let mut ledger = LEDGER.lock().unwrap();
    let state = ledger.get_or_create(session_id, &root);
    state.last_event = Some(event.clone());
    // Clear changed paths on successful verification.
    if event.status == VerificationStatus::Passed {
        state.changed_paths.clear();
        state.last_edit_at = None;
    }
    Some(event)
}

/// Mark that files were edited in the workspace.
pub fn mark_workspace_edited(session_id: &str, cwd: &str, paths: &[String]) {
    let root = find_project_root(cwd);
    // SAFETY: LEDGER is an internal Mutex; poisoning only occurs on a
    // prior panic-while-locked, which is a bug.
    let mut ledger = LEDGER.lock().unwrap();
    let state = ledger.get_or_create(session_id, &root);

    let now = Instant::now();
    if state.last_edit_at.is_none() {
        state.last_edit_at = Some(now);
    }
    for p in paths {
        if !state.changed_paths.contains(p) {
            state.changed_paths.push(p.clone());
        }
    }
    // Cap changed paths to 200.
    if state.changed_paths.len() > 200 {
        let start = state.changed_paths.len() - 200;
        state.changed_paths = state.changed_paths[start..].to_vec();
    }
}

/// Query the verification status for a session.
pub fn verification_status(session_id: &str, cwd: &str) -> VerificationQuery {
    let root = find_project_root(cwd);
    // SAFETY: LEDGER is an internal Mutex; poisoning only occurs on a
    // prior panic-while-locked, which is a bug.
    let ledger = LEDGER.lock().unwrap();
    let key = (session_id.to_string(), root.clone());
    match ledger.sessions.get(&key) {
        None => VerificationQuery {
            status: EvidenceStatus::NotApplicable,
            evidence: None,
            changed_paths: vec![],
            last_edit_at: None,
        },
        Some(state) => {
            let status = if state.last_event.is_none() && state.changed_paths.is_empty() {
                EvidenceStatus::NotApplicable
            } else if state.last_event.is_none() {
                EvidenceStatus::Unverified
            } else if state.changed_paths.is_empty()
                && state.last_edit_at.is_none()
            {
                // SAFETY: `last_event.is_none()` was excluded by the
                // preceding elif branches; guaranteed Some here.
                match state.last_event.as_ref().unwrap().status {
                    VerificationStatus::Passed => EvidenceStatus::Passed,
                    VerificationStatus::Failed => EvidenceStatus::Failed,
                }
            } else {
                // We have edits and/or a verification event.
                // Stale = edits happened after the verification.
                // SAFETY: `last_event.is_none()` was excluded by the
                // preceding elif branches; guaranteed Some here.
                match state.last_event.as_ref().unwrap().status {
                    VerificationStatus::Passed => {
                        // If there are pending changed_paths, it's stale.
                        if !state.changed_paths.is_empty() {
                            EvidenceStatus::Stale
                        } else {
                            EvidenceStatus::Passed
                        }
                    }
                    VerificationStatus::Failed => EvidenceStatus::Failed,
                }
            };
            VerificationQuery {
                status,
                evidence: state.last_event.clone(),
                changed_paths: state.changed_paths.clone(),
                last_edit_at: state.last_edit_at,
            }
        }
    }
}

/// Clear state for a session (on session end/reset).
pub fn clear_session(session_id: &str) {
    // SAFETY: LEDGER is an internal Mutex; poisoning only occurs on a
    // prior panic-while-locked, which is a bug.
    let mut ledger = LEDGER.lock().unwrap();
    ledger.sessions.retain(|(sid, _), _| sid != session_id);
}

/// Clear all state (for tests).
pub fn clear_all() {
    // SAFETY: LEDGER is an internal Mutex; poisoning only occurs on a
    // prior panic-while-locked, which is a bug.
    let mut ledger = LEDGER.lock().unwrap();
    ledger.sessions.clear();
}

// ─── Classification ───────────────────────────────────────────────────

/// Classify a command as a verification command. Returns None if it's not
/// a recognized verification command.
fn classify_verification_command(
    command: &str,
    _cwd: &str,
    _session_id: &str,
    exit_code: i32,
    output: &str,
    root: &str,
) -> Option<VerificationEvent> {
    let canonical = canonicalize_command(command);
    let kind = classify_kind(&canonical)?;
    let scope = classify_scope(command, &canonical);
    let status = if exit_code == 0 {
        VerificationStatus::Passed
    } else {
        VerificationStatus::Failed
    };
    let output_summary = summarize_output(output);

    Some(VerificationEvent {
        command: command.to_string(),
        canonical_command: canonical.clone(),
        kind,
        scope,
        status,
        exit_code,
        cwd: _cwd.to_string(),
        root: root.to_string(),
        session_id: _session_id.to_string(),
        output_summary,
        created_at: Instant::now(),
    })
}

/// Canonicalize a command for matching (strip env vars, wrappers).
fn canonicalize_command(command: &str) -> String {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut idx = 0;
    // Skip env var assignments (FOO=bar) and wrappers.
    while idx < parts.len() {
        let p = parts[idx];
        if p.contains('=') && !p.starts_with('-') {
            idx += 1;
            continue;
        }
        match p {
            "env" | "command" | "time" | "noglob" => {
                idx += 1;
                continue;
            }
            _ => break,
        }
    }
    if idx >= parts.len() {
        return String::new();
    }
    let base = parts[idx];
    // Normalize common equivalents.
    match base {
        "npm" => {
            // npm run X → npm X
            if idx + 1 < parts.len() && parts[idx + 1] == "run" {
                let mut result = vec!["npm"];
                result.extend_from_slice(&parts[idx + 2..]);
                result.join(" ")
            } else {
                parts[idx..].join(" ")
            }
        }
        "python" | "python3" => {
            // python -m pytest → pytest
            if idx + 2 < parts.len() && parts[idx + 1] == "-m" {
                parts[idx + 2..].join(" ")
            } else {
                parts[idx..].join(" ")
            }
        }
        "uv" | "uvx" | "pipenv" | "poetry" => {
            // uv run pytest → pytest, poetry run pytest → pytest
            if idx + 1 < parts.len() && parts[idx + 1] == "run" {
                parts[idx + 2..].join(" ")
            } else {
                parts[idx..].join(" ")
            }
        }
        _ => parts[idx..].join(" "),
    }
}

/// Classify the kind of verification from a canonical command.
fn classify_kind(canonical: &str) -> Option<VerificationKind> {
    let lower = canonical.to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let base = parts[0];
    match base {
        "pytest" | "cargo" | "go" | "jest" | "vitest" | "mocha" | "ruby" | "rspec"
        | "phpunit" | "dotnet" | "xcodebuild" | "mvn" | "gradle" => {
            // For multi-tool commands, check subcommand.
            if base == "cargo" {
                if parts.get(1) == Some(&"test") {
                    Some(VerificationKind::Test)
                } else if parts.get(1) == Some(&"check") || parts.get(1) == Some(&"clippy") {
                    Some(VerificationKind::Typecheck)
                } else if parts.get(1) == Some(&"build") {
                    Some(VerificationKind::Build)
                } else if parts.get(1) == Some(&"fmt") {
                    Some(VerificationKind::Format)
                } else {
                    None
                }
            } else if base == "go" {
                match parts.get(1) {
                    Some(&"test") => Some(VerificationKind::Test),
                    Some(&"build") => Some(VerificationKind::Build),
                    Some(&"vet") => Some(VerificationKind::Lint),
                    Some(&"fmt") => Some(VerificationKind::Format),
                    _ => None,
                }
            } else {
                Some(VerificationKind::Test)
            }
        }
        "eslint" | "pylint" | "flake8" | "ruff" | "golangci-lint" | "tsc" | "mypy"
        | "pyright" | "typecheck" => {
            if base == "tsc" || base == "mypy" || base == "pyright" || base == "typecheck" {
                Some(VerificationKind::Typecheck)
            } else {
                Some(VerificationKind::Lint)
            }
        }
        "prettier" | "black" | "rustfmt" | "gofmt" | "shfmt" | "stylua" => {
            Some(VerificationKind::Format)
        }
        "make" => {
            // make test, make check, make lint...
            match parts.get(1).copied() {
                Some("test") | Some("check") => Some(VerificationKind::Test),
                Some("lint") => Some(VerificationKind::Lint),
                Some("build") => Some(VerificationKind::Build),
                _ => None,
            }
        }
        "shellcheck" | "checkbashisms" => Some(VerificationKind::Lint),
        "swift" => {
            if parts.get(1) == Some(&"build") {
                Some(VerificationKind::Build)
            } else {
                None
            }
        }
        "zig" => {
            if parts.get(1) == Some(&"build") || parts.get(1) == Some(&"test") {
                Some(VerificationKind::Build)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Classify the scope of verification.
fn classify_scope(original_command: &str, _canonical: &str) -> VerificationScope {
    // If the command targets specific files/paths, it's targeted.
    let parts: Vec<&str> = original_command.split_whitespace().collect();
    for p in &parts {
        if p.starts_with("./")
            || p.starts_with("/")
            || (p.contains('.') && !p.starts_with('-') && !p.contains('='))
            || p.starts_with("test_")
            || p.starts_with("Test")
        {
            return VerificationScope::Targeted;
        }
    }
    // cargo test --lib, go test ./...
    if parts.iter().any(|p| p.starts_with("--")) {
        return VerificationScope::Targeted;
    }
    VerificationScope::Full
}

/// Summarize output: head 1/3 + omitted marker + tail.
fn summarize_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_SUMMARY_CHARS {
        return output.to_string();
    }
    let head_size = MAX_OUTPUT_SUMMARY_CHARS / 3;
    let tail_size = MAX_OUTPUT_SUMMARY_CHARS / 3;
    let head = &output[..head_size.min(output.len())];
    let tail_start = output.len().saturating_sub(tail_size);
    let tail = &output[tail_start..];
    format!(
        "{}\n... [output truncated, {} chars omitted] ...\n{}",
        head,
        output.len() - head_size - tail_size,
        tail
    )
}

/// Find the project root by looking for common markers.
fn find_project_root(cwd: &str) -> String {
    let cwd_path = PathBuf::from(cwd);
    let mut current = cwd_path.as_path();
    loop {
        for marker in &["Cargo.toml", "package.json", "go.mod", "pyproject.toml", ".git", "Makefile"] {
            if current.join(marker).exists() {
                return current.to_string_lossy().to_string();
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    cwd_path.to_string_lossy().to_string()
}

// ─── Stop Guard (verification_stop.py) ────────────────────────────────

/// Check if a path is a code file (not documentation).
fn is_code_path(path: &str) -> bool {
    let filename = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if NON_CODE_FILES.iter().any(|f| filename.starts_with(f)) {
        return false;
    }
    for ext in NON_CODE_EXTS {
        if path.ends_with(ext) {
            return false;
        }
    }
    true
}

/// Build a verification nudge if the agent needs to verify changes before
/// declaring the task done.
///
/// Returns None if:
/// - No code files were changed
/// - The attempt budget is exhausted
/// - All workspaces have passing verification
pub fn build_verify_on_stop_nudge(
    session_id: &str,
    cwd: &str,
    changed_paths: &[String],
    attempts: usize,
) -> Option<String> {
    // Filter to code paths only.
    let code_paths: Vec<&String> = changed_paths
        .iter()
        .filter(|p| is_code_path(p))
        .collect();
    if code_paths.is_empty() {
        return None;
    }
    if attempts >= MAX_VERIFY_ATTEMPTS {
        return None;
    }

    // Check verification status.
    let status = verification_status(session_id, cwd);
    match status.status {
        EvidenceStatus::NotApplicable => return None,
        EvidenceStatus::Passed => return None,
        EvidenceStatus::Unverified | EvidenceStatus::Stale | EvidenceStatus::Failed => {}
    }

    // Build the nudge.
    let paths_display: Vec<String> = code_paths
        .iter()
        .take(VERIFY_NUDGE_MAX_PATHS)
        .map(|p| p.to_string())
        .collect();
    let more = if code_paths.len() > VERIFY_NUDGE_MAX_PATHS {
        format!("\n  ... and {} more", code_paths.len() - VERIFY_NUDGE_MAX_PATHS)
    } else {
        String::new()
    };

    let status_detail = if let Some(ev) = &status.evidence {
        let summary = if ev.output_summary.len() > VERIFY_NUDGE_OUTPUT_LIMIT {
            ev.output_summary[..VERIFY_NUDGE_OUTPUT_LIMIT].to_string()
        } else {
            ev.output_summary.clone()
        };
        format!(
            "\n\nLast verification attempt: `{}` (exit code: {})\nOutput:\n{}",
            ev.command, ev.exit_code, summary
        )
    } else {
        String::new()
    };

    Some(format!(
        "You edited code files but haven't verified your changes yet. \
Before declaring the task complete, run the project's verification commands \
(tests, linter, type checker, or build) to confirm your changes work.\n\n\
Changed files:\n  {}{}\n\
Verification status: {}{}\
\n\nPlease run the appropriate verification command(s) and report the results. \
Do not declare the task done until verification passes.",
        paths_display.join("\n  "),
        more,
        match status.status {
            EvidenceStatus::Unverified => "no verification has been run yet",
            EvidenceStatus::Stale => "files changed since last verification",
            EvidenceStatus::Failed => "last verification FAILED",
            _ => "unknown",
        },
        status_detail,
    ))
}

/// Check if verification-on-stop is enabled.
/// Default: ON for CLI/TUI, can be disabled via config.
pub fn verify_on_stop_enabled(config_value: Option<bool>) -> bool {
    config_value.unwrap_or(true)
}

/// Get the max verify nudges from config.
pub fn max_verify_nudges(config_value: Option<usize>) -> usize {
    config_value.unwrap_or(MAX_VERIFY_NUDGES)
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests that share the global LEDGER.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
            std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn test_classify_cargo_test() {
        let ev = classify_verification_command(
            "cargo test",
            "/tmp/project",
            "sess1",
            0,
            "running tests...\nall passed",
            "/tmp/project",
        )
        .unwrap();
        assert_eq!(ev.kind, VerificationKind::Test);
        assert_eq!(ev.status, VerificationStatus::Passed);
        assert_eq!(ev.scope, VerificationScope::Full);
    }

    #[test]
    fn test_classify_cargo_test_failed() {
        let ev = classify_verification_command(
            "cargo test",
            "/tmp/project",
            "sess1",
            1,
            "test failed",
            "/tmp/project",
        )
        .unwrap();
        assert_eq!(ev.status, VerificationStatus::Failed);
    }

    #[test]
    fn test_classify_pytest() {
        let ev = classify_verification_command(
            "python -m pytest",
            "/tmp/project",
            "sess1",
            0,
            "",
            "/tmp/project",
        )
        .unwrap();
        assert_eq!(ev.kind, VerificationKind::Test);
        assert_eq!(ev.canonical_command, "pytest");
    }

    #[test]
    fn test_classify_non_verification() {
        assert!(classify_verification_command(
            "ls -la",
            "/tmp",
            "s1",
            0,
            "",
            "/tmp"
        )
        .is_none());
        assert!(classify_verification_command(
            "echo hello",
            "/tmp",
            "s1",
            0,
            "",
            "/tmp"
        )
        .is_none());
    }

    #[test]
    fn test_classify_env_prefix() {
        let ev = classify_verification_command(
            "FOO=bar cargo test",
            "/tmp",
            "s1",
            0,
            "",
            "/tmp",
        )
        .unwrap();
        assert_eq!(ev.canonical_command, "cargo test");
    }

    #[test]
    fn test_targeted_scope() {
        let ev = classify_verification_command(
            "cargo test --lib",
            "/tmp",
            "s1",
            0,
            "",
            "/tmp",
        )
        .unwrap();
        assert_eq!(ev.scope, VerificationScope::Targeted);
    }

    #[test]
    fn test_record_and_query() {
        let _g = lock();
        clear_all();
        record_terminal_result("cargo test", "/tmp/proj", "s1", 0, "all passed");
        let q = verification_status("s1", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Passed);
    }

    #[test]
    fn test_mark_edited_makes_stale() {
        let _g = lock();
        clear_all();
        record_terminal_result("cargo test", "/tmp/proj", "s1", 0, "ok");
        mark_workspace_edited("s1", "/tmp/proj", &["src/main.rs".to_string()]);
        let q = verification_status("s1", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Stale);
    }

    #[test]
    fn test_unverified_status() {
        let _g = lock();
        clear_all();
        mark_workspace_edited("s1", "/tmp/proj", &["src/main.rs".to_string()]);
        let q = verification_status("s1", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Unverified);
    }

    #[test]
    fn test_passing_clears_changed_paths() {
        let _g = lock();
        clear_all();
        mark_workspace_edited("s1", "/tmp/proj", &["src/main.rs".to_string()]);
        record_terminal_result("cargo test", "/tmp/proj", "s1", 0, "ok");
        let q = verification_status("s1", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Passed);
        assert!(q.changed_paths.is_empty());
    }

    #[test]
    fn test_nudge_unverified() {
        let _g = lock();
        clear_all();
        mark_workspace_edited("s1", "/tmp/proj", &["src/main.rs".to_string()]);
        let nudge = build_verify_on_stop_nudge("s1", "/tmp/proj", &["src/main.rs".to_string()], 0);
        assert!(nudge.is_some());
        let nudge = nudge.unwrap();
        assert!(nudge.contains("verification"));
        assert!(nudge.contains("src/main.rs"));
    }

    #[test]
    fn test_nudge_passed_no_nudge() {
        let _g = lock();
        clear_all();
        mark_workspace_edited("s1", "/tmp/proj", &["src/main.rs".to_string()]);
        record_terminal_result("cargo test", "/tmp/proj", "s1", 0, "ok");
        let nudge = build_verify_on_stop_nudge("s1", "/tmp/proj", &["src/main.rs".to_string()], 0);
        assert!(nudge.is_none());
    }

    #[test]
    fn test_nudge_non_code_no_nudge() {
        clear_all();
        let nudge = build_verify_on_stop_nudge("s1", "/tmp/proj", &["README.md".to_string()], 0);
        assert!(nudge.is_none());
    }

    #[test]
    fn test_nudge_budget_exhausted() {
        let _g = lock();
        clear_all();
        mark_workspace_edited("s1", "/tmp/proj", &["src/main.rs".to_string()]);
        let nudge = build_verify_on_stop_nudge("s1", "/tmp/proj", &["src/main.rs".to_string()], MAX_VERIFY_ATTEMPTS);
        assert!(nudge.is_none());
    }

    #[test]
    fn test_is_code_path() {
        assert!(is_code_path("src/main.rs"));
        assert!(is_code_path("lib/utils.py"));
        assert!(!is_code_path("README.md"));
        assert!(!is_code_path("CHANGELOG.md"));
        assert!(!is_code_path("LICENSE"));
    }

    #[test]
    fn test_output_summary_short() {
        let summary = summarize_output("short output");
        assert_eq!(summary, "short output");
    }

    #[test]
    fn test_output_summary_long() {
        let long = "x".repeat(5000);
        let summary = summarize_output(&long);
        assert!(summary.contains("[output truncated"));
        assert!(summary.len() < 5000);
    }

    // ── FR-006/SC-005 regression tests (hardened sites) ──────────────────

    /// SAFETY site: `LEDGER.lock().unwrap()` in `record_terminal_result`.
    /// Mutex lock must not panic on normal concurrent-ish use.
    #[test]
    fn record_terminal_result_ledger_lock_does_not_panic() {
        let _g = lock();
        clear_all();
        // Multiple records to the same session exercise the lock path.
        let ev = record_terminal_result("cargo test", "/tmp/proj", "fr006-a", 0, "ok");
        assert!(ev.is_some());
        let ev2 = record_terminal_result("cargo test", "/tmp/proj", "fr006-a", 1, "fail");
        assert!(ev2.is_some());
        clear_all();
    }

    /// SAFETY site: `LEDGER.lock().unwrap()` in `mark_workspace_edited`
    /// plus the `changed_paths[start..]` slice after the 200-item cap.
    #[test]
    fn mark_workspace_edited_lock_and_cap_slice_does_not_panic() {
        let _g = lock();
        clear_all();
        // Edge: exactly 200 paths — boundary of the cap slice.
        let paths: Vec<String> = (0..200).map(|i| format!("src/file_{i}.rs")).collect();
        mark_workspace_edited("fr006-b", "/tmp/proj", &paths);
        let q = verification_status("fr006-b", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Unverified);
        assert_eq!(q.changed_paths.len(), 200);

        // Edge: 201 paths — exercises `len() - 200` slice start.
        let paths2: Vec<String> = (0..201).map(|i| format!("src/extra_{i}.rs")).collect();
        mark_workspace_edited("fr006-c", "/tmp/proj", &paths2);
        let q2 = verification_status("fr006-c", "/tmp/proj");
        assert_eq!(q2.changed_paths.len(), 200);

        // Edge: empty paths vec.
        mark_workspace_edited("fr006-d", "/tmp/proj", &[]);
        clear_all();
    }

    /// SAFETY site: `LEDGER.lock().unwrap()` + `last_event.as_ref().unwrap()`
    /// in `verification_status` — exercises all four EvidenceStatus branches
    /// that depend on the unwrap'd Option.
    #[test]
    fn verification_status_last_event_unwrap_does_not_panic() {
        let _g = lock();
        clear_all();

        // Branch 1: no session → NotApplicable (no unwrap reached).
        let q = verification_status("fr006-e", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::NotApplicable);

        // Branch 2: edits but no verification event → Unverified (no unwrap
        // on last_event; the is_none() elif catches it).
        mark_workspace_edited("fr006-e", "/tmp/proj", &["src/a.rs".to_string()]);
        let q = verification_status("fr006-e", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Unverified);

        // Branch 3: passed verification, no pending edits → the
        // `changed_paths.is_empty() && last_edit_at.is_none()` elif that
        // does `last_event.as_ref().unwrap()`.
        record_terminal_result("cargo test", "/tmp/proj", "fr006-e", 0, "ok");
        let q = verification_status("fr006-e", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Passed);

        // Branch 4: edits after verification → Stale, also hits
        // `last_event.as_ref().unwrap()` in the else arm.
        mark_workspace_edited("fr006-e", "/tmp/proj", &["src/b.rs".to_string()]);
        let q = verification_status("fr006-e", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Stale);

        // Branch 5: failed verification → the unwrap in the else arm.
        record_terminal_result("cargo test", "/tmp/proj", "fr006-f", 1, "boom");
        let q = verification_status("fr006-f", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::Failed);

        clear_all();
    }

    /// SAFETY site: `LEDGER.lock().unwrap()` in `clear_session`.
    #[test]
    fn clear_session_ledger_lock_does_not_panic() {
        let _g = lock();
        clear_all();
        mark_workspace_edited("fr006-g", "/tmp/proj", &["src/x.rs".to_string()]);
        clear_session("fr006-g");
        // Clearing a non-existent session is also safe.
        clear_session("fr006-nonexistent");
        let q = verification_status("fr006-g", "/tmp/proj");
        assert_eq!(q.status, EvidenceStatus::NotApplicable);
        clear_all();
    }

    /// SAFETY site: `LEDGER.lock().unwrap()` in `clear_all`.
    #[test]
    fn clear_all_ledger_lock_does_not_panic() {
        let _g = lock();
        mark_workspace_edited("fr006-h", "/tmp/proj", &["src/y.rs".to_string()]);
        clear_all();
        clear_all(); // double clear is safe
    }
}
