//! Process-global terminal concurrency governor (feature 018).
//!
//! Caps the number of concurrently executing agent-initiated terminal
//! commands, queueing excess requests with per-agent FIFO queues and a
//! rotating round-robin admission cursor so no agent key starves another
//! (one request per key per admission turn).
//!
//! Design source of truth:
//! - `specs/018-please-fully-implement/plan.md` (Structure Decision)
//! - `specs/018-please-fully-implement/research.md` D1-D6
//! - `specs/018-please-fully-implement/data-model.md` (TerminalGovernor)
//!
//! This is a deliberate Joey extension with no upstream Hermes counterpart
//! (PORTING.md classification is task T022).
//!
//! Placement rationale (research.md D1): a process-global lazy singleton
//! mirroring the `process_registry()` precedent (`process_tool.rs:165`),
//! because admission must span every terminal execution in the process
//! (main agent, subagents, background tasks) and `joey-tools` is the single
//! choke point they all flow through.
//!
//! Synchronization: `std::sync::Mutex` for state (poison-recovered the same
//! way `process_tool.rs` does), `tokio::sync::oneshot` per waiter for
//! wakeup. No shared `Notify` is needed because release hands the slot
//! directly to a specific waiter through its channel.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use indexmap::IndexMap;
use once_cell::sync::Lazy;
use tokio::sync::oneshot;

/// Interrupt poll cadence while queued. Mirrors `INTERRUPT_POLL` in
/// `terminal_tool.rs` (100ms — well under the ~2s cancellation target,
/// research.md D5 / SC-003).
const INTERRUPT_POLL: Duration = Duration::from_millis(100);

/// Floor for the auto-derived concurrency limit (research.md D3 / spec Q3).
const MIN_LIMIT: usize = 4;
/// Ceiling for the auto-derived concurrency limit.
const MAX_LIMIT: usize = 16;
/// Fallback core count when `available_parallelism()` fails.
const FALLBACK_PARALLELISM: usize = 8;

/// Shared queue key used when a caller has no agent identity
/// (`ToolContext.queue_key` absent — direct `tool.execute` calls, tests,
/// gateway paths). All identity-less callers share one FIFO queue, which
/// preserves existing behavior (research.md D6).
pub const DEFAULT_QUEUE_KEY: &str = "__default__";

/// The shared default queue key as an owned `Arc<str>`.
pub fn default_queue_key() -> Arc<str> {
    Arc::from(DEFAULT_QUEUE_KEY)
}

/// Pure clamp used by the auto-sizing path (research.md D3). Exposed
/// separately from [`auto_limit`] so the 4-floor / 16-ceiling / mid
/// passthrough bounds are unit-testable on any machine.
pub fn clamp_limit(parallelism: usize) -> usize {
    parallelism.clamp(MIN_LIMIT, MAX_LIMIT)
}

/// Auto-derived default limit: `clamp(available_parallelism().unwrap_or(8),
/// 4, 16)`. The clamp precedent in `joey-orchestration/src/capacity.rs`
/// lives in a higher crate (DAG forbids reuse), so the clamp is duplicated
/// locally and cited here.
pub fn auto_limit() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(FALLBACK_PARALLELISM);
    clamp_limit(cores)
}

/// Why an [`TerminalGovernor::acquire`] attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// The cooperative interrupt flag fired while queued; the waiter was
    /// deregistered and holds no slot.
    Interrupted,
    /// The admission channel closed without a slot being delivered. Cannot
    /// happen through normal governor operation (waiters are only removed
    /// from a queue by being admitted); kept for totality.
    Dropped,
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AcquireError::Interrupted => write!(f, "interrupted while queued for a terminal slot"),
            AcquireError::Dropped => write!(f, "terminal slot admission channel closed"),
        }
    }
}

impl std::error::Error for AcquireError {}

/// A single queued waiter: its oneshot admission channel plus a unique id
/// used to deregister it on interrupt/cancel without touching other
/// waiters of the same key.
struct WaiterEntry {
    id: u64,
    tx: oneshot::Sender<SlotGuard>,
}

/// Governor state guarded by the mutex. All invariant-critical mutations
/// (`active`, admissions) happen while holding the lock, so
/// `active <= limit` holds at every observable point (SC-002).
struct GovernorState {
    /// Maximum concurrently active executions.
    limit: usize,
    /// Currently admitted executions (slots handed out, not yet released).
    active: usize,
    /// Monotonic waiter-id source for deregistration.
    next_waiter_id: u64,
    /// Rotating round-robin admission cursor: an index into the
    /// insertion-ordered key list. After admitting a waiter from the key
    /// at index `i`, the cursor moves to `i + 1`, so each agent key gets
    /// at most one admission per cycle (no key starves another — research
    ///.md D2, spec Q4). Empty per-key queues are retained (not removed)
    /// so indices — and therefore cursor arithmetic — stay stable for the
    /// process lifetime.
    cursor: usize,
    /// Insertion-ordered map of agent queue key → FIFO wait queue
    /// (data-model.md). `IndexMap` preserves first-seen key order, which
    /// the cursor indexes into.
    queues: IndexMap<Arc<str>, VecDeque<WaiterEntry>>,
}

/// Process-global terminal concurrency governor (research.md D1).
///
/// Invariants:
/// - `active <= limit` at all observable times.
/// - Release is guaranteed on completion, failure, cancellation, and panic
///   via the [`SlotGuard`] drop-guard (data-model.md Execution Slot).
/// - Admission from the wait queues is round-robin over agent keys, one
///   request per key per turn.
pub struct TerminalGovernor {
    state: Mutex<GovernorState>,
}

impl TerminalGovernor {
    /// Create a governor with an explicit limit. Returns an `Arc` because
    /// admitted slots hold a handle back to the governor for release.
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GovernorState {
                limit: limit.max(1),
                active: 0,
                next_waiter_id: 0,
                cursor: 0,
                queues: IndexMap::new(),
            }),
        })
    }

    /// Lock state, recovering from poisoning the same way the process
    /// registry does (`process_tool.rs`) — governor state is never left
    /// structurally invalid by a panic between lock and unlock.
    fn lock_state(&self) -> MutexGuard<'_, GovernorState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Set the concurrency limit. Called by the terminal tool at first
    /// admission to resolve `terminal.max_concurrent` /
    /// `TERMINAL_MAX_CONCURRENT` (tasks T014; contracts/config.md).
    /// Raising the limit immediately admits queued waiters into the new
    /// capacity; lowering it never preempts running executions — future
    /// admissions respect the smaller cap.
    pub fn set_limit(self: &Arc<Self>, limit: usize) {
        let limit = limit.max(1);
        let mut state = self.lock_state();
        let grew = limit > state.limit;
        state.limit = limit;
        if grew {
            while state.active < state.limit {
                if !Self::admit_one_locked(self, &mut state) {
                    break;
                }
            }
        }
        tracing::debug!(limit, active = state.active, "terminal governor limit set");
    }

    /// The configured limit.
    pub fn limit(&self) -> usize {
        self.lock_state().limit
    }

    /// Snapshot of `(active, queued)` for events and `/status`
    /// (data-model.md, contracts/events.md).
    pub fn stats(&self) -> (usize, usize) {
        let state = self.lock_state();
        let queued: usize = state.queues.values().map(|q| q.len()).sum();
        (state.active, queued)
    }

    /// Currently admitted (running) executions.
    pub fn active(&self) -> usize {
        self.lock_state().active
    }

    /// Total queued waiters across all agent keys.
    pub fn queued(&self) -> usize {
        self.lock_state()
            .queues
            .values()
            .map(|q| q.len())
            .sum::<usize>()
    }

    /// Queued waiter count for one agent key (test/status surface).
    pub fn queued_for(&self, key: &str) -> usize {
        self.lock_state().queues.get(key).map_or(0, |q| q.len())
    }

    /// Acquire an execution slot for `key`, yielding (queueing) when the
    /// governor is at capacity.
    ///
    /// - Fast path: capacity is free → admitted immediately under one lock
    ///   (lone-agent calls never queue, SC-004).
    /// - Contended path: the waiter is appended to its key's FIFO queue
    ///   and woken via a oneshot when a rotating round-robin admission
    ///   (starting after the last-served key) reaches it.
    /// - Interrupt-aware (research.md D5): when `interrupt` is set, the
    ///   wait races the cooperative flag on a 100ms poll cadence; on
    ///   interrupt the waiter deregisters from its queue and returns
    ///   [`AcquireError::Interrupted`] holding no slot. The future is also
    ///   cancellation-safe by construction: dropping it drops the oneshot
    ///   receiver, and a later admission attempt to that stale waiter
    ///   fails the send and moves on to the next waiter.
    /// - If the waiter is admitted the instant the interrupt fires, the
    ///   already-delivered guard is dropped (its Drop releases the slot
    ///   and admits the next waiter) — no leak either way.
    pub async fn acquire(
        self: &Arc<Self>,
        key: Arc<str>,
        interrupt: Option<Arc<AtomicBool>>,
    ) -> Result<SlotGuard, AcquireError> {
        let (mut rx, waiter_id) = {
            let mut state = self.lock_state();
            if let Some(flag) = &interrupt {
                if flag.load(Ordering::Relaxed) {
                    return Err(AcquireError::Interrupted);
                }
            }
            if state.active < state.limit {
                // Free capacity: at lock-held instants the wait queues are
                // empty whenever active < limit (release admits within the
                // same critical section), so immediate admission cannot
                // jump a queue.
                state.active += 1;
                return Ok(SlotGuard::new(self.clone()));
            }
            let (tx, rx) = oneshot::channel();
            state.next_waiter_id += 1;
            let waiter_id = state.next_waiter_id;
            state
                .queues
                .entry(key.clone())
                .or_default()
                .push_back(WaiterEntry { id: waiter_id, tx });
            (rx, waiter_id)
        };

        match interrupt {
            None => rx.await.map_err(|_| AcquireError::Dropped),
            Some(flag) => loop {
                if flag.load(Ordering::Relaxed) {
                    let mut state = self.lock_state();
                    if let Some(q) = state.queues.get_mut(&key) {
                        q.retain(|e| e.id != waiter_id);
                    }
                    return Err(AcquireError::Interrupted);
                }
                tokio::select! {
                    res = &mut rx => {
                        return res.map_err(|_| AcquireError::Dropped);
                    }
                    _ = tokio::time::sleep(INTERRUPT_POLL) => {
                        // Re-check the interrupt flag at the top of the loop.
                    }
                }
            },
        }
    }

    /// Release one slot (drop-guard path) and admit queued waiters into
    /// the freed capacity, honoring round-robin order.
    fn release_slot(self: &Arc<Self>) {
        let mut state = self.lock_state();
        state.active = state.active.saturating_sub(1);
        while state.active < state.limit {
            if !Self::admit_one_locked(self, &mut state) {
                break;
            }
        }
    }

    /// Admit exactly one queued waiter, scanning keys in insertion order
    /// starting after the cursor (`cursor..cursor+n` wrapped). The first
    /// key with a non-empty queue whose waiter accepts the slot wins; the
    /// cursor then moves just past that key — one admission per key per
    /// turn. Cancelled waiters (dropped receivers) are skipped by failed
    /// sends. Returns `false` when nothing is admissible.
    fn admit_one_locked(governor: &Arc<Self>, state: &mut GovernorState) -> bool {
        let key_count = state.queues.len();
        for offset in 0..key_count {
            let idx = (state.cursor + offset) % key_count;
            loop {
                let key = match state.queues.get_index(idx) {
                    Some((k, _)) => k.clone(),
                    None => break,
                };
                let entry = match state.queues.get_mut(&key) {
                    Some(q) => q.pop_front(),
                    None => break,
                };
                let Some(entry) = entry else { break };
                match entry.tx.send(SlotGuard::new(governor.clone())) {
                    Ok(()) => {
                        state.active += 1;
                        state.cursor = (idx + 1) % key_count;
                        return true;
                    }
                    Err(_) => {
                        // Waiter cancelled while queued: drop the stale
                        // entry and try the next waiter of this same key.
                        continue;
                    }
                }
            }
        }
        false
    }
}

impl fmt::Debug for TerminalGovernor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock_state();
        let queued: usize = state.queues.values().map(|q| q.len()).sum();
        f.debug_struct("TerminalGovernor")
            .field("limit", &state.limit)
            .field("active", &state.active)
            .field("queued", &queued)
            .field("cursor", &state.cursor)
            .finish()
    }
}

/// Shared release bookkeeping for an admitted slot. `released` makes the
/// release idempotent, which matters because the guard value itself
/// travels through the admission oneshot: if the receiving task is
/// cancelled after delivery but before constructing/polling further, the
/// channel's payload drop performs the release exactly once.
struct SlotInner {
    governor: Arc<TerminalGovernor>,
    released: AtomicBool,
}

/// Drop-guard for one governor slot. Release happens exactly once on
/// drop — i.e. on completion, failure, cancellation (including interrupt
/// and task abort) and panic unwind alike (SC-003, data-model.md).
pub struct SlotGuard {
    inner: Arc<SlotInner>,
}

impl SlotGuard {
    fn new(governor: Arc<TerminalGovernor>) -> Self {
        Self {
            inner: Arc::new(SlotInner {
                governor,
                released: AtomicBool::new(false),
            }),
        }
    }

    /// Release the slot explicitly (equivalent to dropping the guard).
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if !self.inner.released.swap(true, Ordering::AcqRel) {
            self.inner.governor.release_slot();
        }
    }
}

impl fmt::Debug for SlotGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotGuard")
            .field("released", &self.inner.released.load(Ordering::Relaxed))
            .finish()
    }
}

/// Process-global governor instance. Initialized lazily on first use with
/// the auto-derived limit; the terminal tool resolves the configured limit
/// at first admission via [`TerminalGovernor::set_limit`] (task T014).
static GOVERNOR: Lazy<Arc<TerminalGovernor>> = Lazy::new(|| TerminalGovernor::new(auto_limit()));

/// Get the process-global terminal governor (mirrors the
/// `process_registry()` singleton precedent, `process_tool.rs:165`).
/// One process hosts one session's agents, so process scope == session
/// scope (FR-003, research.md D1).
pub fn terminal_governor() -> Arc<TerminalGovernor> {
    GOVERNOR.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// Poll `cond` until true, failing the test after ~5s so a broken
    /// governor fails loudly instead of deadlocking the suite.
    async fn until(cond: impl Fn() -> bool) {
        for _ in 0..1000 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("condition not met within 5s timeout");
    }

    // ------------------------------------------------------------------
    // 1. auto_limit clamp bounds (via the pure `clamp_limit` split, so the
    //    assertions hold on any machine).
    // ------------------------------------------------------------------
    #[test]
    fn clamp_limit_bounds_floor_ceiling_and_mid_passthrough() {
        assert_eq!(clamp_limit(0), MIN_LIMIT);
        assert_eq!(clamp_limit(1), MIN_LIMIT);
        assert_eq!(clamp_limit(3), MIN_LIMIT, "below-floor clamps up to 4");
        assert_eq!(clamp_limit(MIN_LIMIT), MIN_LIMIT, "at floor passes through");
        assert_eq!(clamp_limit(8), 8, "mid value passes through");
        assert_eq!(clamp_limit(12), 12, "mid value passes through");
        assert_eq!(clamp_limit(MAX_LIMIT), MAX_LIMIT, "at ceiling passes through");
        assert_eq!(clamp_limit(17), MAX_LIMIT, "above-ceiling clamps down to 16");
        assert_eq!(clamp_limit(usize::MAX), MAX_LIMIT);

        // `auto_limit()` is machine-dependent, but must land in-bounds.
        let auto = auto_limit();
        assert!((MIN_LIMIT..=MAX_LIMIT).contains(&auto));

        // Constructor never allows a zero/degenerate limit.
        assert_eq!(TerminalGovernor::new(0).limit(), 1);

        // Singleton + default-key surface sanity.
        assert_eq!(DEFAULT_QUEUE_KEY, "__default__");
        assert_eq!(default_queue_key().as_ref(), DEFAULT_QUEUE_KEY);
        let singleton = terminal_governor();
        assert!((MIN_LIMIT..=MAX_LIMIT).contains(&singleton.limit()));
    }

    // ------------------------------------------------------------------
    // 2. Round-robin fairness: a late-arriving key is admitted within one
    //    full admission cycle even when an earlier key flooded the queue.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn round_robin_late_key_admitted_within_one_cycle_under_flood() {
        let gov = TerminalGovernor::new(2);
        let key_a: Arc<str> = Arc::from("agent-a");
        let key_b: Arc<str> = Arc::from("agent-b");

        // Agent A takes both slots (fast path, no queues yet).
        let g1 = gov.acquire(key_a.clone(), None).await.unwrap();
        let g2 = gov.acquire(key_a.clone(), None).await.unwrap();
        assert_eq!(gov.stats(), (2, 0));

        // Record admission order across waiter tasks.
        let admitted: Arc<Mutex<Vec<char>>> = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();

        // Agent A floods the queue with 5 waiters...
        for _ in 0..5 {
            let gov = gov.clone();
            let key = key_a.clone();
            let admitted = admitted.clone();
            handles.push(tokio::spawn(async move {
                let guard = gov.acquire(key, None).await.unwrap();
                admitted.lock().unwrap().push('a');
                assert!(gov.active() <= gov.limit());
                guard.release();
            }));
        }
        until(|| gov.queued_for("agent-a") == 5).await;

        // ...then agent B queues exactly one waiter, last.
        {
            let gov = gov.clone();
            let admitted = admitted.clone();
            handles.push(tokio::spawn(async move {
                let guard = gov.acquire(key_b, None).await.unwrap();
                admitted.lock().unwrap().push('b');
                assert!(gov.active() <= gov.limit());
                guard.release();
            }));
        }
        until(|| gov.queued_for("agent-b") == 1).await;
        assert_eq!(gov.stats(), (2, 6));

        // Free both slots; admissions proceed round-robin from key A.
        g1.release();
        g2.release();
        for h in handles {
            h.await.unwrap();
        }

        let order = admitted.lock().unwrap().clone();
        assert_eq!(order.len(), 6);
        assert_eq!(order.iter().filter(|c| **c == 'a').count(), 5);
        assert_eq!(order.iter().filter(|c| **c == 'b').count(), 1);

        // Fairness: with N=2 keys, B must be admitted within one full
        // cycle — i.e. before A receives N further admissions. A's
        // first-cycle admission lands at index 0, so B is at index <= 1.
        let b_pos = order
            .iter()
            .position(|c| *c == 'b')
            .expect("agent B was never admitted (starved)");
        assert!(
            b_pos <= 1,
            "late key starved under flood: admission order = {:?}",
            order
        );
        assert_eq!(gov.stats(), (0, 0));
    }

    // ------------------------------------------------------------------
    // 3. Cap invariant: active never exceeds limit; excess acquires queue.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn active_never_exceeds_limit_and_excess_acquires_queue() {
        let gov = TerminalGovernor::new(3);
        let key: Arc<str> = Arc::from("cap");

        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(gov.acquire(key.clone(), None).await.unwrap());
            assert!(gov.active() <= gov.limit());
        }
        assert_eq!(gov.stats(), (3, 0));

        // Two extra waiters hold their slots until released by signal.
        let (go_tx, go_rx) = tokio::sync::watch::channel(());
        let mut handles = Vec::new();
        for _ in 0..2 {
            let gov = gov.clone();
            let key = key.clone();
            let mut go = go_rx.clone();
            handles.push(tokio::spawn(async move {
                let guard = gov.acquire(key, None).await.unwrap();
                assert!(
                    gov.active() <= gov.limit(),
                    "cap invariant broken on admission"
                );
                let _ = go.changed().await;
                guard.release();
            }));
        }
        until(|| gov.queued() == 2).await;
        assert_eq!(gov.stats(), (3, 2), "excess acquires must queue, not admit");
        assert_eq!(gov.active(), 3);

        // Releasing one slot admits exactly one waiter; cap still holds.
        held.pop().unwrap().release();
        until(|| gov.queued() == 1).await;
        assert_eq!(gov.stats(), (3, 1));
        assert!(gov.active() <= gov.limit());

        // Drain: signal waiters, release the rest, everything settles.
        go_tx.send(()).unwrap();
        for g in held {
            g.release();
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(gov.stats(), (0, 0));
    }

    // ------------------------------------------------------------------
    // 4a. Drop-guard releases on early return (`?` / early-Err path).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn slot_guard_releases_on_early_return() {
        let gov = TerminalGovernor::new(1);

        async fn fallible_work(gov: Arc<TerminalGovernor>) -> Result<(), AcquireError> {
            // `?` + early return: `_slot` must be dropped (releasing the
            // slot) without any explicit release call.
            let _slot = gov.acquire(Arc::from("early"), None).await?;
            Err(AcquireError::Dropped) // stand-in downstream failure
        }

        assert_eq!(fallible_work(gov.clone()).await, Err(AcquireError::Dropped));
        assert_eq!(gov.active(), 0, "early return must release the slot");

        // Fully released: a fresh acquire is admitted with nothing queued.
        let g = gov.acquire(Arc::from("early"), None).await.unwrap();
        assert_eq!(gov.stats(), (1, 0));
        g.release();
        assert_eq!(gov.stats(), (0, 0));
    }

    // ------------------------------------------------------------------
    // 4b. Drop-guard releases on panic unwind. The guard owns only an
    //     `Arc<SlotInner>` (governor handle + idempotence flag) — no
    //     oneshot receiver — so plain `catch_unwind` is sound here.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn slot_guard_releases_on_panic_unwind() {
        let gov = TerminalGovernor::new(1);
        let guard = gov.acquire(Arc::from("panicky"), None).await.unwrap();
        assert_eq!(gov.active(), 1);

        let result = catch_unwind(AssertUnwindSafe(move || {
            let _held = guard; // guard moves in and drops during unwind
            panic!("simulated panic while holding a terminal slot");
        }));
        assert!(result.is_err(), "closure should have panicked");
        assert_eq!(
            gov.active(),
            0,
            "drop-guard must release the slot during panic unwind"
        );

        // Slot is usable again: immediate re-admission, nothing queued.
        let g = gov.acquire(Arc::from("panicky"), None).await.unwrap();
        assert_eq!(gov.stats(), (1, 0));
        g.release();
        assert_eq!(gov.stats(), (0, 0));
    }

    // ------------------------------------------------------------------
    // 5. Interrupt-awareness.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn interrupt_while_queued_returns_interrupted_and_deregisters() {
        let gov = TerminalGovernor::new(1);
        let key: Arc<str> = Arc::from("interrupted");

        // Occupy the only slot so the waiter must queue.
        let holder = gov.acquire(key.clone(), None).await.unwrap();

        let flag = Arc::new(AtomicBool::new(false));
        let waiter = {
            let gov = gov.clone();
            let key = key.clone();
            let flag = flag.clone();
            tokio::spawn(async move {
                gov.acquire(key, Some(flag))
                    .await
                    .map(|g| g.release())
            })
        };

        until(|| gov.queued_for("interrupted") == 1).await;
        assert_eq!(gov.stats(), (1, 1));

        // Fire the interrupt; the waiter notices within one poll cadence
        // (INTERRUPT_POLL = 100ms) and deregisters.
        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            waiter.await.unwrap(),
            Err(AcquireError::Interrupted),
            "queued waiter must yield Interrupted when the flag fires"
        );
        assert_eq!(gov.queued_for("interrupted"), 0, "waiter must deregister");
        assert_eq!(gov.stats(), (1, 0));

        holder.release();
        assert_eq!(gov.stats(), (0, 0));
    }

    #[tokio::test]
    async fn pre_set_interrupt_flag_fails_fast_without_slot() {
        let gov = TerminalGovernor::new(2);
        let flag = Arc::new(AtomicBool::new(true));

        // Flag already set: rejected before any admission, even with free
        // capacity. (`SlotGuard` has no `PartialEq`, so match by variant.)
        let res = gov.acquire(Arc::from("late-flag"), Some(flag)).await;
        assert!(matches!(res, Err(AcquireError::Interrupted)));
        assert_eq!(gov.stats(), (0, 0));
    }

    // ------------------------------------------------------------------
    // 6. Empty-queue retained keys: `queued_for` on absent and drained
    //    keys stays 0 and stats don't drift.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn queued_for_absent_and_retained_keys_is_zero() {
        let gov = TerminalGovernor::new(1);

        // Never-registered key.
        assert_eq!(gov.queued_for("never-registered"), 0);

        // Drain a key that was queued then served: the empty queue entry
        // is retained internally, but must be invisible to stats.
        let key: Arc<str> = Arc::from("solo");
        let holder = gov.acquire(key.clone(), None).await.unwrap();
        let waiter = {
            let gov = gov.clone();
            let key = key.clone();
            tokio::spawn(async move {
                let g = gov.acquire(key, None).await.unwrap();
                g.release();
            })
        };
        until(|| gov.queued_for("solo") == 1).await;

        holder.release();
        waiter.await.unwrap();

        assert_eq!(gov.queued_for("solo"), 0, "retained empty key must report 0");
        assert_eq!(gov.stats(), (0, 0));
    }
}
