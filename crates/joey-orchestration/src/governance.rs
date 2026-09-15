//! Feature 030: bounded admission, priority lanes, retry budget, and busy
//! refusal for subagent delegation governance.

use std::collections::VecDeque;
use std::path::PathBuf;

/// Resolved resource-governance configuration (feature 030).
/// `Default` is governance-DISABLED (pre-feature behavior) so programmatic
/// `ManagerConfig::default()` constructions — the existing test suites and
/// test-only CLI paths — keep byte-identical pre-feature semantics
/// (FR-014/SC-007). Production enables governance via
/// `GovernanceConfig::from_config`, whose defaults mirror the
/// `delegation.*` keys declared in joey-core DEFAULT_CONFIG_YAML.
#[derive(Debug, Clone)]
pub struct GovernanceConfig {
    /// Master switch: false bypasses every governance code path.
    pub enabled: bool,
    /// Resolved waiting-queue cap (auto → 2 × resolved children, clamp ≥ 1).
    pub max_queue_depth: usize,
    /// Per-task wall-clock budget; 0 = disabled.
    pub task_timeout_secs: u64,
    /// Max retries in flight system-wide; 0 = retries refused.
    pub retry_budget: usize,
    pub backoff_base_secs: f64,
    pub backoff_max_secs: f64,
    /// Turn-boundary resume tokens on; false = timeouts always full-restart.
    pub checkpointing: bool,
    pub result_cache_enabled: bool,
    pub result_cache_max_entries: usize,
    pub result_cache_ttl_hours: u64,
    pub single_flight_enabled: bool,
    /// Hard per-task CPU ceiling (sampled); 0 = disabled.
    pub cpu_ceiling_secs: u64,
    /// CPU/memory sampling cadence (clamped ≥ 1s at use site).
    pub watchdog_interval_secs: u64,
    pub memory_tracking_enabled: bool,
    pub priority_enabled: bool,
    /// Explicitly selected degraded mode (never auto-engaged).
    pub degraded_mode_enabled: bool,
    /// Fraction of background+normal work processed under degraded mode.
    pub degraded_sample_rate: f64,
    /// Data directory for governance stores; None → ~/.joey/delegation.
    /// Programmatic override for tests — NOT a config key.
    pub data_dir: Option<PathBuf>,
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        GovernanceConfig {
            enabled: false,
            max_queue_depth: 6,
            task_timeout_secs: 600,
            retry_budget: 2,
            backoff_base_secs: 2.0,
            backoff_max_secs: 60.0,
            checkpointing: true,
            result_cache_enabled: true,
            result_cache_max_entries: 256,
            result_cache_ttl_hours: 24,
            single_flight_enabled: true,
            cpu_ceiling_secs: 300,
            watchdog_interval_secs: 1,
            memory_tracking_enabled: true,
            priority_enabled: true,
            degraded_mode_enabled: false,
            degraded_sample_rate: 0.1,
            data_dir: None,
        }
    }
}

impl GovernanceConfig {
    /// Resolve from the layered config; defaults mirror contracts/config-keys.md.
    /// `resolved_children` is the already-resolved max_concurrent_children
    /// (capacity-derived when the key is auto/absent).
    pub fn from_config(cfg: &joey_core::Config, resolved_children: usize) -> Self {
        let children = resolved_children.max(1);
        // Key is int|"auto" (contracts/config-keys.md). Must accept BOTH a
        // YAML number (`max_queue_depth: 3`) and the string `"auto"` —
        // `get_str` alone can't (as_str() is None for numbers), so resolve
        // via the raw Value with number coercion; "auto"/absent/unparseable
        // → 2 × resolved children.
        let max_queue_depth = match cfg.get("delegation.max_queue_depth") {
            Some(v) => match joey_core::config::value_as_i64(v) {
                Some(n) => n.max(1) as usize,
                None => children.saturating_mul(2),
            },
            None => children.saturating_mul(2),
        }
        .max(1);
        GovernanceConfig {
            enabled: cfg.get_bool("delegation.resource_governance.enabled", true),
            max_queue_depth,
            task_timeout_secs: cfg
                .get_clamped_i64("delegation.task_timeout_secs", 600, 0, 86_400)
                .max(0) as u64,
            retry_budget: cfg
                .get_clamped_i64("delegation.retry_budget", 2, 0, 1_000)
                .max(0) as usize,
            backoff_base_secs: cfg.get_clamped_f64("delegation.backoff_base_secs", 2.0, 0.0, 3_600.0),
            backoff_max_secs: cfg.get_clamped_f64("delegation.backoff_max_secs", 60.0, 0.0, 86_400.0),
            checkpointing: cfg.get_bool("delegation.checkpointing.enabled", true),
            result_cache_enabled: cfg.get_bool("delegation.result_cache.enabled", true),
            result_cache_max_entries: cfg
                .get_clamped_i64("delegation.result_cache.max_entries", 256, 0, 100_000)
                .max(0) as usize,
            result_cache_ttl_hours: cfg
                .get_clamped_i64("delegation.result_cache.ttl_hours", 24, 0, 8_760)
                .max(0) as u64,
            single_flight_enabled: cfg.get_bool("delegation.single_flight.enabled", true),
            cpu_ceiling_secs: cfg
                .get_clamped_i64("delegation.cpu_ceiling_secs", 300, 0, 86_400)
                .max(0) as u64,
            watchdog_interval_secs: cfg
                .get_clamped_i64("delegation.watchdog_interval_secs", 1, 0, 3_600)
                .max(0) as u64,
            memory_tracking_enabled: cfg.get_bool("delegation.memory_tracking.enabled", true),
            priority_enabled: cfg.get_bool("delegation.priority.enabled", true),
            degraded_mode_enabled: cfg.get_bool("delegation.degraded_mode.enabled", false),
            degraded_sample_rate: cfg.get_clamped_f64(
                "delegation.degraded_mode.sample_rate",
                0.1,
                0.0,
                1.0,
            ),
            data_dir: None,
        }
    }
}

#[cfg(test)]
mod governance_config_tests {
    use super::*;
    use joey_core::Config;

    /// Build a Config from YAML text via the public API (mirrors the
    /// tempfile + `Config::load_from` precedent in manager.rs tests; the
    /// cfg_from/build_root helper inside joey-core's own tests touches
    /// private internals and is not reachable from this crate).
    fn cfg_from(yaml: &str) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, yaml).unwrap();
        Config::load_from(path).unwrap()
    }

    #[test]
    fn defaults_enable_all_mechanisms() {
        let g = GovernanceConfig::from_config(&cfg_from(""), 3);
        assert!(g.enabled);
        assert_eq!(g.max_queue_depth, 6);
        assert_eq!(g.task_timeout_secs, 600);
        assert_eq!(g.retry_budget, 2);
        assert_eq!(g.backoff_base_secs, 2.0);
        assert_eq!(g.backoff_max_secs, 60.0);
        assert!(g.checkpointing);
        assert!(g.result_cache_enabled);
        assert_eq!(g.result_cache_max_entries, 256);
        assert_eq!(g.result_cache_ttl_hours, 24);
        assert!(g.single_flight_enabled);
        assert_eq!(g.cpu_ceiling_secs, 300);
        assert_eq!(g.watchdog_interval_secs, 1);
        assert!(g.memory_tracking_enabled);
        assert!(g.priority_enabled);
        assert!(!g.degraded_mode_enabled);
        assert_eq!(g.degraded_sample_rate, 0.1);
        assert!(g.data_dir.is_none());
    }

    #[test]
    fn programmatic_default_is_disabled() {
        let g = GovernanceConfig::default();
        assert!(!g.enabled); // pre-feature behavior for ManagerConfig::default() users
    }

    #[test]
    fn queue_depth_auto_is_two_x_children_and_clamped() {
        assert_eq!(GovernanceConfig::from_config(&cfg_from(""), 4).max_queue_depth, 8);
        assert_eq!(GovernanceConfig::from_config(&cfg_from(""), 0).max_queue_depth, 2); // children clamps to >=1
        let g = GovernanceConfig::from_config(&cfg_from("delegation:\n  max_queue_depth: 3\n"), 8);
        assert_eq!(g.max_queue_depth, 3);
        let bad = GovernanceConfig::from_config(&cfg_from("delegation:\n  max_queue_depth: -7\n"), 2);
        assert_eq!(bad.max_queue_depth, 1);
    }

    #[test]
    fn clamps_hold() {
        let g = GovernanceConfig::from_config(
            &cfg_from(
                "delegation:\n  task_timeout_secs: -5\n  retry_budget: -1\n  degraded_mode:\n    sample_rate: 1.7\n  watchdog_interval_secs: 0\n",
            ),
            2,
        );
        assert_eq!(g.task_timeout_secs, 0);
        assert_eq!(g.retry_budget, 0);
        assert_eq!(g.degraded_sample_rate, 1.0);
        assert_eq!(g.watchdog_interval_secs, 0); // 0 clamps at use site
    }

    #[test]
    fn master_switch_off() {
        let g = GovernanceConfig::from_config(
            &cfg_from("delegation:\n  resource_governance:\n    enabled: false\n"),
            2,
        );
        assert!(!g.enabled);
    }
}

// ---------------------------------------------------------------------------
// Feature 030: admission primitives (FR-001 bounded waiting queue with
// priority lanes; FR-002 slot release hands capacity to the next waiter).
// ---------------------------------------------------------------------------

/// One waiting delegation in the bounded admission queue (feature 030,
/// FR-001): its priority lane, arrival time for FIFO-within-lane/aging, and
/// the oneshot that hands the waiter its semaphore permit (paired with its
/// admission-sequence number for the start gate) when admitted.
pub(crate) struct QueuedWaiter {
    pub priority: crate::types::Priority,
    pub enqueued_at: std::time::Instant,
    pub tx: tokio::sync::oneshot::Sender<(
        tokio::sync::OwnedSemaphorePermit,
        u64,
    )>,
}

/// Bounded waiting queue with three priority lanes (feature 030, FR-001).
/// Lane index = `Priority::rank()` (0 = background, 1 = normal,
/// 2 = critical); admission drains higher-rank lanes first.
#[derive(Default)]
pub(crate) struct AdmissionQueue {
    pub lanes: [VecDeque<QueuedWaiter>; 3],
    pub cap: usize,
    /// Admission-order sequence handed to each waiter with its permit
    /// (start-gate ordering; see [`StartGate`]). First assigned seq is 1.
    pub next_seq: u64,
}

impl AdmissionQueue {
    pub fn new(cap: usize) -> Self {
        AdmissionQueue {
            lanes: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            cap,
            next_seq: 1,
        }
    }

    /// Total waiters across all lanes.
    pub fn len(&self) -> usize {
        self.lanes.iter().map(|l| l.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Enqueue `w`; false (busy refusal) when the queue is at capacity.
    pub fn push(&mut self, w: QueuedWaiter) -> bool {
        if self.len() >= self.cap {
            return false;
        }
        self.lanes[w.priority.rank() as usize].push_back(w);
        true
    }

    /// Pop the next waiter: lanes are drained in REVERSE rank order
    /// (critical first), FIFO within each lane.
    pub fn pop_next(&mut self) -> Option<QueuedWaiter> {
        for lane in self.lanes.iter_mut().rev() {
            if let Some(w) = lane.pop_front() {
                return Some(w);
            }
        }
        None
    }
}

/// Owning handle for one admitted slot (feature 030, FR-002): dropping it
/// releases the permit back to the semaphore and admits the next queued
/// waiter, if capacity is now available.
pub(crate) struct SlotGuard {
    pub queue: std::sync::Arc<std::sync::Mutex<AdmissionQueue>>,
    pub slots: std::sync::Arc<tokio::sync::Semaphore>,
    pub gate: std::sync::Arc<StartGate>,
    pub permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.permit.take(); // dropping the permit releases the slot
        admit_next(self.queue.clone(), self.slots.clone(), &self.gate);
    }
}

/// Admit queued waiters while semaphore capacity remains (feature 030,
/// FR-002): pops waiters highest-priority-first and sends each a permit;
/// stops at the first failed acquisition or empty queue. A popped waiter
/// that cannot acquire a permit is READMITTED at the front of its lane —
/// dropping it would kick it out of the priority discipline entirely
/// (its oneshot sender would die and it would fall back to the legacy
/// blocking acquire, bypassing lane order — the exact inversion FR-013
/// forbids: critical must jump the queue line).
pub(crate) fn admit_next(
    queue: std::sync::Arc<std::sync::Mutex<AdmissionQueue>>,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    gate: &std::sync::Arc<StartGate>,
) {
    loop {
        // Pop WITHOUT assigning a seq — the seq is assigned only when a
        // permit is actually handed off. Assigning at pop time burned a
        // seq on every failed try_acquire + readmit cycle, and the start
        // gate (started+1 >= seq) could never be satisfied again — a
        // queued-admission deadlock whenever two or more waiters were
        // queued (feature 030 regression).
        let popped = { queue.lock().unwrap().pop_next() };
        let Some(w) = popped else { return };
        match slots.clone().try_acquire_owned() {
            Ok(permit) => {
                let seq = {
                    let mut q = queue.lock().unwrap();
                    let s = q.next_seq;
                    q.next_seq += 1;
                    s
                };
                if w.tx.send((permit, seq)).is_err() {
                    // Waiter vanished while queued: no TurnToken will ever
                    // exist for this seq, so advance the gate on its behalf.
                    gate.advance();
                }
            }
            Err(_) => {
                let mut q = queue.lock().unwrap();
                q.lanes[w.priority.rank() as usize].push_front(w);
                return;
            }
        }
    }
}

/// Start-order gate (feature 030, T022/FR-013): admitted waiters START in
/// admission order. Permit handoff order alone does not order the waiters'
/// dispatch futures — the scheduler may wake a later-admitted normal-lane
/// waiter before an earlier-admitted critical one (tokio LIFO-slot wake
/// order) — so each admitted waiter takes a cancellation-safe turn token
/// and waits until every earlier-admitted waiter has started (or been
/// cancelled, which also advances the gate). This is what makes "critical
/// jumps the queue line" observable: the critical head's first provider
/// request goes out strictly before the normals it jumped.
pub(crate) struct StartGate {
    started: std::sync::Mutex<u64>,
    notify: tokio::sync::Notify,
}

impl StartGate {
    pub fn new() -> Self {
        StartGate {
            started: std::sync::Mutex::new(0),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Take the turn token for admission sequence `seq` (call immediately
    /// after receiving the permit; the token must exist before any await
    /// so cancellation can never skip the gate bump).
    pub fn turn(&self, seq: u64) -> TurnToken<'_> {
        TurnToken {
            gate: self,
            seq,
            passed: false,
        }
    }

    /// Advance the gate by one on behalf of an admitted waiter that can
    /// never take its turn token (its dispatch was cancelled while queued
    /// and the handoff send failed). Without this, the burned seq would
    /// stall every later waiter.
    pub(crate) fn advance(&self) {
        *self.started.lock().unwrap() += 1;
        self.notify.notify_waiters();
    }
}

/// Cancellation-safe turn token (see [`StartGate`]): `wait_turn` blocks
/// until all earlier-admitted waiters started; Drop bumps the gate if the
/// waiter never passed (a cancelled dispatch must never stall later
/// waiters).
pub(crate) struct TurnToken<'a> {
    gate: &'a StartGate,
    seq: u64,
    passed: bool,
}

impl TurnToken<'_> {
    /// Wait until every earlier-admitted waiter (seq < mine) has started.
    pub async fn wait_turn(mut self) {
        loop {
            // Register interest in the NEXT notification BEFORE re-checking
            // started — a notify_waiters() firing between the check and the
            // await cannot be lost (same pattern as the flight follower).
            let mut notified = std::pin::pin!(self.gate.notify.notified());
            notified.as_mut().enable();
            if *self.gate.started.lock().unwrap() + 1 >= self.seq {
                break;
            }
            notified.await;
        }
        self.passed = true;
        self.bump();
    }

    fn bump(&self) {
        *self.gate.started.lock().unwrap() += 1;
        self.gate.notify.notify_waiters();
    }
}

impl Drop for TurnToken<'_> {
    fn drop(&mut self) {
        if !self.passed {
            self.bump(); // cancelled before starting: advance the gate
        }
    }
}

#[cfg(test)]
mod admission_queue_tests {
    use super::*;
    use crate::types::Priority;

    fn waiter(
        priority: Priority,
    ) -> (
        QueuedWaiter,
        tokio::sync::oneshot::Receiver<(
            tokio::sync::OwnedSemaphorePermit,
            u64,
        )>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            QueuedWaiter {
                priority,
                enqueued_at: std::time::Instant::now(),
                tx,
            },
            rx,
        )
    }

    #[test]
    fn priority_order() {
        let mut q = AdmissionQueue::new(10);
        q.push(waiter(Priority::Background).0);
        q.push(waiter(Priority::Normal).0);
        q.push(waiter(Priority::Critical).0);
        assert_eq!(q.pop_next().unwrap().priority, Priority::Critical);
        assert_eq!(q.pop_next().unwrap().priority, Priority::Normal);
        assert_eq!(q.pop_next().unwrap().priority, Priority::Background);
        assert!(q.pop_next().is_none());
    }

    #[test]
    fn cap_enforced() {
        let mut q = AdmissionQueue::new(2);
        assert!(q.push(waiter(Priority::Normal).0));
        assert!(q.push(waiter(Priority::Normal).0));
        assert!(!q.push(waiter(Priority::Critical).0));
    }

    #[test]
    fn len_across_lanes() {
        let mut q = AdmissionQueue::new(10);
        q.push(waiter(Priority::Background).0);
        q.push(waiter(Priority::Normal).0);
        q.push(waiter(Priority::Critical).0);
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn first_admitted_waiter_gets_seq_one() {
        // Regression: admit_next used to increment next_seq before use,
        // handing the FIRST waiter seq=2 which the start gate can never
        // satisfy (started+1 >= 2 with started=0) — a queued-admission
        // deadlock (feature 030).
        let queue = std::sync::Arc::new(std::sync::Mutex::new(AdmissionQueue::new(4)));
        let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let (w, rx) = waiter(Priority::Normal);
        assert!(queue.lock().unwrap().push(w));
        let gate = std::sync::Arc::new(StartGate::new());
        admit_next(queue.clone(), slots.clone(), &gate);
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (_permit, seq) = rt.block_on(rx).unwrap();
        assert_eq!(seq, 1, "first admitted waiter must get admission seq 1");
    }

    #[test]
    fn readmitted_waiter_does_not_burn_admission_seqs() {
        // Regression (feature 030): admit_next used to assign the start-gate
        // seq at POP time; a waiter popped during a failed try_acquire was
        // readmitted WITHOUT its seq, burning it. The next handoff then got a
        // seq with a permanent gap and the start gate deadlocked. Seqs handed
        // to waiters must be dense: 1, 2, 3, ... with no gaps, regardless of
        // how many pop-then-readmit cycles happen under contention.
        let queue = std::sync::Arc::new(std::sync::Mutex::new(AdmissionQueue::new(4)));
        let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(0)); // all slots busy
        let gate = std::sync::Arc::new(StartGate::new());
        let (w1, rx1) = waiter(Priority::Normal);
        let (w2, rx2) = waiter(Priority::Normal);
        queue.lock().unwrap().push(w1); // push returns bool (true = enqueued)
        queue.lock().unwrap().push(w2);
        // First cascade: no permits — both pops fail try_acquire (or the loop
        // returns on the first failure after readmitting). Either way, no seq
        // may be consumed.
        admit_next(queue.clone(), slots.clone(), &gate);
        // Now two slots free at once: W1 must get seq 1 and W2 seq 2 — with
        // NO burned gap (pre-fix, W2 received seq 3 → gate deadlock).
        slots.add_permits(2);
        admit_next(queue.clone(), slots.clone(), &gate);
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (_p1, seq1) = rt.block_on(rx1).expect("W1 admitted");
        let (_p2, seq2) = rt.block_on(rx2).expect("W2 admitted");
        assert_eq!((seq1, seq2), (1, 2), "admission seqs must be dense (no burned seqs)");
    }
}

// ---------------------------------------------------------------------------
// Feature 030: system-wide in-flight retry budget (FR-005, research.md R3b).
// ---------------------------------------------------------------------------

/// System-wide in-flight retry budget (FR-005, research.md R3b).
/// `try_acquire` returns None when the budget is spent → caller fails fast
/// with `retry budget exhausted` semantics (no new load).
pub(crate) struct RetryBudget {
    budget: usize,
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl RetryBudget {
    pub fn new(budget: usize) -> Self {
        RetryBudget {
            budget,
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
    /// Returns a guard accounting one in-flight retry, or None when full.
    /// T013: the guard is OWNED (Arc-shared counter) so the manager's
    /// retry loop can hold it across a backoff sleep and the next attempt.
    pub fn try_acquire(&self) -> Option<RetryGuard> {
        let now = self.in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if now > self.budget {
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return None;
        }
        Some(RetryGuard {
            in_flight: self.in_flight.clone(),
        })
    }
}
pub(crate) struct RetryGuard {
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for RetryGuard {
    fn drop(&mut self) {
        self.in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod retry_budget_tests {
    use super::*;

    #[test]
    fn budget_of_two_allows_two_concurrent() {
        let b = RetryBudget::new(2);
        let _g1 = b.try_acquire().expect("first acquire within budget 2");
        let _g2 = b.try_acquire().expect("second acquire within budget 2");
    }

    #[test]
    fn budget_of_zero_refuses_all() {
        let b = RetryBudget::new(0);
        assert!(b.try_acquire().is_none(), "budget 0 must refuse every acquire");
    }

    #[test]
    fn guard_release_restores_capacity() {
        let b = RetryBudget::new(1);
        {
            let _g = b.try_acquire().expect("acquire on fresh budget 1");
            assert!(
                b.try_acquire().is_none(),
                "capacity is spent while the guard is alive"
            );
        } // guard dropped → capacity restored
        assert!(
            b.try_acquire().is_some(),
            "release must restore capacity (acquire again)"
        );
    }
}

/// One in-flight governed execution shared by identical signatures (FR-008).
/// The leader owns the `Flight` while its dispatch runs; followers await
/// `notify` and read the outcome from `result` once published.
pub(crate) struct Flight {
    pub result: std::sync::Mutex<Option<crate::types::DelegationResult>>,
    pub notify: tokio::sync::Notify,
}

impl Flight {
    pub(crate) fn new() -> Self {
        Flight {
            result: std::sync::Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        }
    }
}

/// Feature 030 (R8): sustained-overload tracker for busy refusals —
/// records refusal timestamps and flags when refusals reach a sustained
/// rate (3+ within 60s), driving the ` [overload]` suffix appended to
/// busy refusals (contracts/busy-and-outcomes.md).
pub(crate) struct OverloadTracker {
    refusals: std::sync::Mutex<Vec<std::time::Instant>>,
}

impl OverloadTracker {
    pub fn new() -> Self {
        OverloadTracker {
            refusals: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Record a refusal at `now`, retaining only the last 60s.
    pub fn record_refusal(&self, now: std::time::Instant) {
        const WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
        let mut v = self.refusals.lock().unwrap();
        v.push(now);
        v.retain(|t| now.duration_since(*t) < WINDOW);
    }

    /// True when refusals within the last 60s number 3 or more.
    pub fn is_sustained(&self, now: std::time::Instant) -> bool {
        const WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
        self.refusals
            .lock()
            .unwrap()
            .iter()
            .filter(|t| now.duration_since(**t) < WINDOW)
            .count()
            >= 3
    }
}

#[cfg(test)]
mod overload_tracker_tests {
    use super::*;

    #[test]
    fn three_refusals_within_60s_is_sustained() {
        let t = OverloadTracker::new();
        let now = std::time::Instant::now();
        t.record_refusal(now);
        t.record_refusal(now);
        t.record_refusal(now);
        assert!(t.is_sustained(now), "3 refusals within 60s must be sustained");
    }

    #[test]
    fn two_refusals_is_not_sustained() {
        let t = OverloadTracker::new();
        let now = std::time::Instant::now();
        t.record_refusal(now);
        t.record_refusal(now);
        assert!(
            !t.is_sustained(now),
            "2 refusals within 60s must not be sustained"
        );
    }

    #[test]
    fn refusals_older_than_60s_are_pruned() {
        let t = OverloadTracker::new();
        let now = std::time::Instant::now();
        // Two stale refusals (61s and 120s old), then two fresh ones.
        let stale1 = now - std::time::Duration::from_secs(61);
        let stale2 = now - std::time::Duration::from_secs(120);
        t.record_refusal(stale1);
        t.record_refusal(stale2);
        t.record_refusal(now);
        t.record_refusal(now);
        assert!(
            !t.is_sustained(now),
            "stale refusals must be pruned: only 2 remain within the window"
        );
        // The prune happened during record_refusal(now) — the stored vec
        // retains only the fresh entries.
        assert_eq!(t.refusals.lock().unwrap().len(), 2);
    }
}

/// LEADER-side RAII for a registered [`Flight`] (T018 exit safety).
/// `complete` publishes the outcome, wakes every follower, and
/// deregisters. `Drop` is the panic/cancellation fallback: if no outcome
/// was published, it synthesizes a failed result so followers never hang,
/// then deregisters. Deregistration is pointer-identity-checked so a
/// LATER leader that re-registered the same signature is never removed.
pub(crate) struct FlightGuard {
    pub flights:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Flight>>>>,
    pub sig: String,
    pub flight: std::sync::Arc<Flight>,
}

impl FlightGuard {
    /// Normal completion: publish the outcome, wake all followers,
    /// deregister this flight.
    pub(crate) fn complete(&self, result: &crate::types::DelegationResult) {
        *self.flight.result.lock().unwrap() = Some(result.clone());
        self.flight.notify.notify_waiters();
        self.deregister();
    }

    /// Remove this flight from the map — but only if the map still holds
    /// OUR flight (a new leader may have re-registered the same signature
    /// after we handed the outcome over).
    fn deregister(&self) {
        let mut flights = self.flights.lock().unwrap();
        if let Some(current) = flights.get(&self.sig) {
            if std::sync::Arc::ptr_eq(current, &self.flight) {
                flights.remove(&self.sig);
            }
        }
    }
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        // Panic/cancellation fallback: if no outcome was published,
        // synthesize one so followers never await forever.
        if self.flight.result.lock().unwrap().is_none() {
            let failed = crate::types::DelegationResult {
                goal: String::new(),
                summary: String::new(),
                success: false,
                error: Some("[panic] leader vanished".to_string()),
                token_usage: Default::default(),
                wall_clock: std::time::Duration::ZERO,
                model: String::new(),
                iterations: 0,
                persisted_session_id: None,
                stop_reason: None,
            };
            *self.flight.result.lock().unwrap() = Some(failed);
            self.flight.notify.notify_waiters();
        }
        self.deregister();
    }
}
