//! The execution context handed to every tool, including the per-session
//! mutable state upstream keeps in module-level trackers
//! (`tools/file_tools.py` `_read_tracker` / `_patch_failure_tracker`,
//! `tools/terminal_tool.py` session cwd records) and the per-turn output
//! budget (`tools/tool_result_storage.py` layer 3).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use indexmap::IndexMap;
use joey_core::Config;

/// Caps on the per-session read-tracker containers (file_tools.py:793-795).
const READ_HISTORY_CAP: usize = 500;
const DEDUP_CAP: usize = 1000;
const READ_TIMESTAMPS_CAP: usize = 1000;

/// Maximum number of pending background-process completions queued for
/// delivery at the next turn boundary. Each entry carries a bounded output
/// tail (~1KB). The agent drains the queue every turn, so this cap is only
/// reached when many background jobs finish simultaneously during a single
/// very long turn. Without it, the queue grows unbounded if the reaper
/// out-produces the turn drain rate.
const PENDING_COMPLETIONS_MAX: usize = 64;

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
    /// Optional channel for streaming progress events from tools back to the
    /// agent turn loop. `None` by default — existing callers are unaffected.
    /// The agent sets this on a per-dispatch clone via [`with_progress_sender`]
    /// before passing the context to a tool that may emit streaming output.
    progress_sender: Option<ProgressSender>,
    /// Optional cooperative-interrupt flag shared with the agent turn loop.
    /// `None` by default — existing callers are unaffected. When set (the
    /// agent wires its Ctrl-C `AtomicBool` here via [`with_interrupt_flag`]),
    /// streaming tools (e.g. `terminal`) poll [`Self::is_interrupted`] in
    /// their read loop and stop early so long-running commands cancel promptly.
    interrupt_flag: Option<Arc<AtomicBool>>,
}

/// A background-process completion awaiting delivery to the agent's next
/// turn. Pushed by the reaper into the session-persistent queue on
/// `ToolContext`; drained by the agent at each turn boundary so the result
/// survives even when the launching turn has already ended (FR-007/FR-008).
#[derive(Debug, Clone)]
pub struct BackgroundCompletion {
    /// Process session handle (e.g. `proc-<uuid>`).
    pub session_id: String,
    /// Process exit code (same semantics as the terminal tool).
    pub exit_code: i64,
    /// Bounded tail of output captured in the ring buffer.
    pub output_tail: String,
    /// Total wall-clock duration in seconds.
    pub elapsed_secs: f64,
}

/// Type alias for the progress channel sender. Tools that produce streaming
/// output (e.g. `terminal`) push `String` progress deltas through this channel;
/// the agent loop forwards them as `AgentEvent::ToolProgress` events.
pub type ProgressSender = tokio::sync::mpsc::UnboundedSender<String>;

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
    /// Session-persistent queue of background-process completions awaiting
    /// delivery at the next turn boundary. Shared across all `ToolContext`
    /// clones via `Arc<ContextInner>` so the reaper (spawned in a prior turn)
    /// can push here even after the launching turn's event channel is gone.
    /// Drained by the agent at the start of each `run_turn`.
    pending_completions: Arc<Mutex<Vec<BackgroundCompletion>>>,
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
                pending_completions: Arc::new(Mutex::new(Vec::new())),
            }),
            progress_sender: None,
            interrupt_flag: None,
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
                pending_completions: inner.pending_completions.clone(),
            }),
            progress_sender: None,
            interrupt_flag: None,
        }
    }

    /// Set the streaming-progress channel sender. Called by the agent turn loop
    /// on a per-dispatch clone before passing the context to a tool that may
    /// emit streaming output. The sender lets the tool push progress deltas
    /// via [`Self::emit_progress`].
    ///
    /// **Backward compatibility**: existing callers that never call this method
    /// get `None`, and `emit_progress` silently becomes a no-op.
    pub fn with_progress_sender(mut self, sender: Option<ProgressSender>) -> Self {
        self.progress_sender = sender;
        self
    }

    /// Returns the progress sender, if one was set via [`with_progress_sender`].
    pub fn progress_sender(&self) -> Option<&ProgressSender> {
        self.progress_sender.as_ref()
    }

    /// Convenience: push a progress delta string to the channel, if a sender
    /// is set. Silently does nothing if no sender is configured (backward-
    /// compatible no-op for callers that never set one).
    pub fn emit_progress(&self, msg: impl Into<String>) {
        if let Some(tx) = &self.progress_sender {
            let _ = tx.send(msg.into());
        }
    }

    /// Set the cooperative-interrupt flag. The agent turn loop shares its
    /// Ctrl-C `AtomicBool` here so streaming tools can poll
    /// [`Self::is_interrupted`] and cancel promptly. Existing callers that
    /// never call this get `None`, and `is_interrupted` reports `false`.
    ///
    /// **Backward compatibility**: additive — never calling this is a no-op.
    pub fn with_interrupt_flag(mut self, flag: Option<Arc<AtomicBool>>) -> Self {
        self.interrupt_flag = flag;
        self
    }

    /// Returns the shared interrupt flag, if one was set.
    pub fn interrupt_flag(&self) -> Option<&Arc<AtomicBool>> {
        self.interrupt_flag.as_ref()
    }

    /// Whether a cooperative interrupt has been requested. Returns `false`
    /// when no flag is wired (the backward-compatible default), so streaming
    /// tools can call this unconditionally without special-casing `None`.
    pub fn is_interrupted(&self) -> bool {
        self.interrupt_flag
            .as_ref()
            .map_or(false, |f| f.load(Ordering::SeqCst))
    }

    /// Push a background-process completion into the session-persistent queue.
    /// The reaper calls this when a background job finishes. The agent drains
    /// the queue at the next turn boundary, emitting a visual notice and
    /// injecting the result into the conversation (non-interrupting). This
    /// survives the launching turn's event channel, unlike `emit_progress`.
    ///
    /// The queue is bounded ([`PENDING_COMPLETIONS_MAX`]): if a very long
    /// turn produces more completions than the cap (many background jobs
    /// finishing simultaneously), the oldest are dropped to prevent
    /// unbounded memory growth. The agent drains the queue every turn, so
    /// the cap is only reached in pathological scenarios.
    pub fn push_background_completion(&self, completion: BackgroundCompletion) {
        let mut queue = self
            .inner
            .pending_completions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        queue.push(completion);
        // Bound the queue: drop the oldest entries when over the cap.
        while queue.len() > PENDING_COMPLETIONS_MAX {
            queue.remove(0);
        }
    }

    /// Drain all pending background-process completions from the queue.
    /// Called by the agent at the start of each turn. Returns the completions
    /// in insertion order (oldest first). An empty `Vec` means no pending work.
    pub fn drain_pending_completions(&self) -> Vec<BackgroundCompletion> {
        let mut queue = self
            .inner
            .pending_completions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *queue)
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

    // ── Regression: progress_sender backward compatibility (T004) ──────────

    #[test]
    fn progress_sender_defaults_to_none() {
        // ToolContext::new must not set a progress sender — existing callers
        // that never call with_progress_sender must see None.
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        assert!(ctx.progress_sender().is_none());
    }

    #[test]
    fn with_interactive_clears_progress_sender() {
        // with_interactive rebuilds the context and must not inherit a stale sender.
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let ctx = ctx.with_progress_sender(Some(tx));
        assert!(ctx.progress_sender().is_some());
        let ctx = ctx.with_interactive(false);
        assert!(ctx.progress_sender().is_none());
    }

    #[test]
    fn with_progress_sender_attaches_and_emits() {
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let ctx = ctx.with_progress_sender(Some(tx));
        assert!(ctx.progress_sender().is_some());
        ctx.emit_progress("hello");
        assert_eq!(rx.try_recv().unwrap(), "hello");
    }

    #[test]
    fn emit_progress_noop_without_sender() {
        // When no sender is set, emit_progress must silently do nothing.
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        // This must not panic.
        ctx.emit_progress("should be a no-op");
        assert!(ctx.progress_sender().is_none());
    }

    #[test]
    fn context_is_clone_send_sync() {
        // ToolContext must remain Clone + Send + Sync for the agent's
        // multi-threaded dispatch (Constitution Principle VII).
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        let _clone = ctx.clone();
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ToolContext>();
    }

    // ── Regression: interrupt_flag backward compatibility (T012) ───────────

    #[test]
    fn interrupt_flag_defaults_to_none_and_not_interrupted() {
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        assert!(ctx.interrupt_flag().is_none());
        assert!(!ctx.is_interrupted(), "no flag => not interrupted");
    }

    #[test]
    fn with_interrupt_flag_reports_state() {
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        let flag = Arc::new(AtomicBool::new(false));
        let ctx = ctx.with_interrupt_flag(Some(flag.clone()));
        assert!(!ctx.is_interrupted());
        flag.store(true, Ordering::SeqCst);
        assert!(ctx.is_interrupted(), "flag flip is observed live");
    }

    #[test]
    fn with_interactive_clears_interrupt_flag() {
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        let flag = Arc::new(AtomicBool::new(true));
        let ctx = ctx.with_interrupt_flag(Some(flag));
        assert!(ctx.is_interrupted());
        let ctx = ctx.with_interactive(false);
        assert!(
            !ctx.is_interrupted(),
            "with_interactive must not inherit a stale interrupt flag"
        );
        assert!(ctx.interrupt_flag().is_none());
    }

    // ── Regression: pending_completions survives turns (T026) ─────────────

    #[test]
    fn pending_completions_defaults_empty() {
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        assert!(ctx.drain_pending_completions().is_empty());
    }

    #[test]
    fn pending_completions_push_and_drain() {
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        ctx.push_background_completion(BackgroundCompletion {
            session_id: "proc-a".into(),
            exit_code: 0,
            output_tail: "done".into(),
            elapsed_secs: 1.5,
        });
        ctx.push_background_completion(BackgroundCompletion {
            session_id: "proc-b".into(),
            exit_code: 1,
            output_tail: "fail".into(),
            elapsed_secs: 2.0,
        });
        let drained = ctx.drain_pending_completions();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].session_id, "proc-a", "insertion order preserved");
        assert_eq!(drained[1].exit_code, 1);
        // Drain clears the queue.
        assert!(ctx.drain_pending_completions().is_empty());
    }

    #[test]
    fn pending_completions_survives_with_interactive() {
        // The queue is session-scoped: with_interactive must NOT clear it
        // (it shares the Arc<Mutex<Vec>> from the original inner).
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        ctx.push_background_completion(BackgroundCompletion {
            session_id: "proc-x".into(),
            exit_code: 0,
            output_tail: "ok".into(),
            elapsed_secs: 3.0,
        });
        let ctx = ctx.with_interactive(false);
        let drained = ctx.drain_pending_completions();
        assert_eq!(drained.len(), 1, "queue must survive with_interactive");
        assert_eq!(drained[0].session_id, "proc-x");
    }

    #[test]
    fn pending_completions_shared_between_clones() {
        // The reaper holds a clone of the ToolContext; pushes must be visible
        // to the agent's original context (shared Arc<Mutex>).
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        let reaper_ctx = ctx.clone();
        reaper_ctx.push_background_completion(BackgroundCompletion {
            session_id: "proc-y".into(),
            exit_code: 0,
            output_tail: "from reaper".into(),
            elapsed_secs: 0.5,
        });
        // The agent's original context sees the push.
        let drained = ctx.drain_pending_completions();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].session_id, "proc-y");
    }

    #[test]
    fn pending_completions_queue_is_bounded() {
        // Pushing well beyond the cap must not let the queue grow unbounded.
        let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "s");
        for i in 0..(PENDING_COMPLETIONS_MAX + 50) {
            ctx.push_background_completion(BackgroundCompletion {
                session_id: format!("proc-{}", i),
                exit_code: 0,
                output_tail: "x".into(),
                elapsed_secs: 1.0,
            });
        }
        let drained = ctx.drain_pending_completions();
        assert!(
            drained.len() <= PENDING_COMPLETIONS_MAX,
            "queue must not exceed cap: got {} (cap {})",
            drained.len(),
            PENDING_COMPLETIONS_MAX
        );
        // The newest entries survive (oldest dropped).
        let last = drained.last().unwrap();
        assert_eq!(last.session_id, format!("proc-{}", PENDING_COMPLETIONS_MAX + 49));
    }
}
