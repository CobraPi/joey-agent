//! The execution context handed to every tool, including the per-session
//! mutable state upstream keeps in module-level trackers
//! (`tools/file_tools.py` `_read_tracker` / `_patch_failure_tracker`,
//! `tools/terminal_tool.py` session cwd records) and the per-turn output
//! budget (`tools/tool_result_storage.py` layer 3).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use indexmap::IndexMap;
use joey_core::Config;

/// Caps on the per-session read-tracker containers (file_tools.py:793-795).
const READ_HISTORY_CAP: usize = 500;
const DEDUP_CAP: usize = 1000;
const READ_TIMESTAMPS_CAP: usize = 1000;

/// Per-turn aggregate tool-output budget accumulator (layer 3 of the
/// persistence pipeline; 200_000 chars by default, `DEFAULT_TURN_BUDGET_CHARS`).
///
/// The agent loop must call [`TurnBudget::reset`] at the start of every
/// assistant turn; [`crate::registry::ToolRegistry::dispatch`] consults it
/// after each tool result and spills results to disk once the aggregate
/// exceeds the budget.
pub struct TurnBudget {
    used: AtomicUsize,
    budget: usize,
}

impl TurnBudget {
    pub fn new(budget: usize) -> Self {
        Self { used: AtomicUsize::new(0), budget }
    }

    /// Reset the accumulator — call at each turn boundary.
    pub fn reset(&self) {
        self.used.store(0, Ordering::SeqCst);
    }

    /// Record `n` chars of tool output; returns the new aggregate total.
    pub fn add(&self, n: usize) -> usize {
        self.used.fetch_add(n, Ordering::SeqCst) + n
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Whether adding `n` more chars would exceed the budget.
    pub fn would_exceed(&self, n: usize) -> bool {
        self.used() + n > self.budget
    }
}

/// Dedup key: (resolved_path, offset, limit).
pub type ReadKey = (String, usize, usize);

/// Per-session state shared by the file/terminal/memory tools.
#[derive(Default)]
pub struct SessionState {
    /// Most recent read/search call key, for consecutive-loop detection.
    pub last_key: Option<String>,
    pub consecutive: u32,
    /// Set of (path, offset, limit) reads (diagnostics only).
    pub read_history: HashSet<ReadKey>,
    /// (resolved_path, offset, limit) → mtime at read time.
    pub dedup: IndexMap<ReadKey, SystemTime>,
    /// Stub-return counters per dedup key.
    pub dedup_hits: IndexMap<ReadKey, u32>,
    /// resolved_path → mtime recorded when this session last read/wrote it.
    pub read_timestamps: IndexMap<String, SystemTime>,
    /// Consecutive patch failures per resolved path.
    pub patch_failures: IndexMap<String, u32>,
    /// The terminal session's live working directory (persists across calls).
    pub terminal_cwd: Option<PathBuf>,
    /// Per-turn memory consolidation failure counter (memory_tool.py #42405).
    pub memory_consolidation_failures: u32,
}

impl SessionState {
    /// Enforce the size caps on the tracker containers (`_cap_read_tracker_data`).
    pub fn cap(&mut self) {
        while self.read_history.len() > READ_HISTORY_CAP {
            if let Some(k) = self.read_history.iter().next().cloned() {
                self.read_history.remove(&k);
            } else {
                break;
            }
        }
        while self.dedup.len() > DEDUP_CAP {
            self.dedup.shift_remove_index(0);
        }
        while self.dedup_hits.len() > DEDUP_CAP {
            self.dedup_hits.shift_remove_index(0);
        }
        while self.read_timestamps.len() > READ_TIMESTAMPS_CAP {
            self.read_timestamps.shift_remove_index(0);
        }
    }

    /// Port of `notify_other_tool_call` — reset consecutive read/search
    /// counters when any other tool runs.
    pub fn note_other_tool(&mut self) {
        self.last_key = None;
        self.consecutive = 0;
        self.dedup_hits.clear();
    }

    /// Port of `_record_patch_failure` (with the 64-entry eviction cap).
    pub fn record_patch_failure(&mut self, resolved_path: &str) -> u32 {
        if self.patch_failures.len() >= 64 && !self.patch_failures.contains_key(resolved_path) {
            self.patch_failures.shift_remove_index(0);
        }
        let count = self.patch_failures.get(resolved_path).copied().unwrap_or(0) + 1;
        self.patch_failures.insert(resolved_path.to_string(), count);
        count
    }

    /// Port of `_reset_patch_failures`.
    pub fn reset_patch_failures(&mut self, resolved_paths: &[String]) {
        for rp in resolved_paths {
            self.patch_failures.shift_remove(rp);
        }
    }

    /// Port of `_invalidate_dedup_for_path` — evict all offset/limit entries
    /// for a written path so subsequent reads return fresh content.
    pub fn invalidate_dedup_for_path(&mut self, resolved: &str) {
        let stale: Vec<ReadKey> = self
            .dedup
            .keys()
            .filter(|k| k.0 == resolved)
            .cloned()
            .collect();
        for k in stale {
            self.dedup.shift_remove(&k);
        }
    }

    /// Port of `reset_file_dedup` — called after context compression.
    pub fn reset_file_dedup(&mut self) {
        self.dedup.clear();
        self.dedup_hits.clear();
    }
}

/// Shared, cheaply-cloneable execution context for tools.
#[derive(Clone)]
pub struct ToolContext {
    inner: Arc<ContextInner>,
}

struct ContextInner {
    cwd: PathBuf,
    config: Config,
    session_id: String,
    /// Whether the session is interactive (gates tools like `clarify`).
    interactive: bool,
    /// Whether dangerous ops are auto-approved (`--yolo`).
    yolo: bool,
    state: Mutex<SessionState>,
    turn_budget: TurnBudget,
}

impl ToolContext {
    pub fn new(cwd: PathBuf, config: Config, session_id: impl Into<String>) -> Self {
        let turn_budget_chars = config.get_i64(
            "tool_output.turn_budget_chars",
            crate::storage::DEFAULT_TURN_BUDGET_CHARS as i64,
        ) as usize;
        Self {
            inner: Arc::new(ContextInner {
                cwd,
                config,
                session_id: session_id.into(),
                interactive: true,
                yolo: joey_core::utils::env_bool("JOEY_YOLO_MODE", false),
                state: Mutex::new(SessionState::default()),
                turn_budget: TurnBudget::new(turn_budget_chars),
            }),
        }
    }

    pub fn with_interactive(self, interactive: bool) -> Self {
        // Rebuild the inner (builder used pre-share, before any state accrues).
        let inner = &self.inner;
        Self {
            inner: Arc::new(ContextInner {
                cwd: inner.cwd.clone(),
                config: inner.config.clone(),
                session_id: inner.session_id.clone(),
                interactive,
                yolo: inner.yolo,
                state: Mutex::new(SessionState::default()),
                turn_budget: TurnBudget::new(inner.turn_budget.budget()),
            }),
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.inner.cwd
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub fn interactive(&self) -> bool {
        self.inner.interactive
    }

    pub fn yolo(&self) -> bool {
        self.inner.yolo
    }

    /// The per-session mutable tool state.
    pub fn state(&self) -> MutexGuard<'_, SessionState> {
        self.inner.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The per-turn aggregate output budget. The agent loop resets this at
    /// each turn boundary (`ctx.turn_budget().reset()`).
    pub fn turn_budget(&self) -> &TurnBudget {
        &self.inner.turn_budget
    }

    /// The session's effective working directory: the live terminal cwd when
    /// one has been recorded (upstream `_resolve_base_dir` order), else the
    /// context cwd.
    pub fn effective_cwd(&self) -> PathBuf {
        self.state()
            .terminal_cwd
            .clone()
            .unwrap_or_else(|| self.inner.cwd.clone())
    }

    /// Resolve a possibly-relative path against the session cwd, expanding `~`.
    pub fn resolve_path(&self, path: &str) -> PathBuf {
        let expanded = shellexpand::tilde(path).to_string();
        let p = PathBuf::from(expanded);
        if p.is_absolute() {
            p
        } else {
            self.effective_cwd().join(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_budget_accumulates_and_resets() {
        let tb = TurnBudget::new(100);
        assert!(!tb.would_exceed(100));
        assert_eq!(tb.add(60), 60);
        assert!(tb.would_exceed(50));
        assert!(!tb.would_exceed(40));
        tb.reset();
        assert_eq!(tb.used(), 0);
    }

    #[test]
    fn patch_failure_counts_and_reset() {
        let mut s = SessionState::default();
        assert_eq!(s.record_patch_failure("/a"), 1);
        assert_eq!(s.record_patch_failure("/a"), 2);
        s.reset_patch_failures(&["/a".to_string()]);
        assert_eq!(s.record_patch_failure("/a"), 1);
    }

    #[test]
    fn state_shared_between_clones() {
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "t");
        let ctx2 = ctx.clone();
        ctx.state().consecutive = 7;
        assert_eq!(ctx2.state().consecutive, 7);
    }
}
