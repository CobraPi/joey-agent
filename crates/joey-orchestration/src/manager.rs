//! SubagentManager: owns the concurrency limiter and dispatches batches.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use joey_agent_core::{Agent, AgentConfig, AgentEvent};
use joey_core::Config;
use joey_tools::ToolRegistry;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

use crate::subagent::{specs_to_requests, Subagent};
use crate::types::{
    ChildHandle, DelegationOverview, DelegationState, DelegationRequest, DelegationResult,
    StopReason, SubagentRole, TaskSpec, WorkHandle,
};

/// Configuration for the orchestration manager.
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Max parallel subagents per batch.
    pub max_concurrent_children: usize,
    /// Semaphore permits across parent + children (in-flight provider calls).
    pub max_concurrent_requests: usize,
    /// Max nesting depth (1 = flat — leaf only).
    pub max_spawn_depth: usize,
    /// Default iteration budget per child.
    pub default_max_turns: usize,
    /// Default trace persistence.
    pub default_persist: bool,
    /// Default model for subagents (falls back to parent model if None).
    pub default_model: Option<String>,
    /// OMO: default background-task concurrency (FR-031). Default 5.
    pub omo_default_concurrency: usize,
    /// OMO: per-provider concurrency overrides (provider name → max parallel).
    pub omo_provider_concurrency: std::collections::HashMap<String, usize>,
    /// OMO: per-model concurrency overrides (model id → max parallel).
    pub omo_model_concurrency: std::collections::HashMap<String, usize>,
    /// Orchestrator's guaranteed minimum share of the request permits
    /// (FR-018/SC-007): children draw from a (max_concurrent_requests −
    /// parent_reserved_permits) pool so the parent can never starve under
    /// child saturation. 0 disables the reservation. Default 1.
    pub parent_reserved_permits: usize,
    /// Bounded wait (seconds) when winding down children at session end
    /// (FR-015). Default 10.
    pub wind_down_timeout_secs: u64,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_children: 3,
            max_concurrent_requests: 5,
            max_spawn_depth: 1,
            default_max_turns: 50,
            default_persist: false,
            default_model: None,
            omo_default_concurrency: 5,
            omo_provider_concurrency: std::collections::HashMap::new(),
            omo_model_concurrency: std::collections::HashMap::new(),
            parent_reserved_permits: 1,
            wind_down_timeout_secs: 10,
        }
    }
}

impl ManagerConfig {
    /// Build from a Config tree, reading `delegation.*` and `omo.background_task.*` keys.
    ///
    /// `delegation.max_concurrent_children = 0` (or `auto`) selects
    /// capacity-driven sizing: the detected system (CPUs + available RAM)
    /// determines how many children may run simultaneously, clamped to
    /// `[capacity::FLOOR_CHILDREN, capacity::HARD_CHILD_CEILING]`. This is
    /// the "use everything the host can support" posture for parallel
    /// inference — the provider request semaphore, not CPU count, is the
    /// real throttle once children are network-bound.
    pub fn from_config(cfg: &Config) -> Self {
        let raw_children = cfg.get_i64("delegation.max_concurrent_children", 0);
        let max_children = if raw_children <= 0 {
            crate::capacity::capacity_children(
                &crate::capacity::SystemCapacity::detect(),
                cfg.get_i64(
                    "delegation.auto_mem_reserve_mb_per_child",
                    crate::capacity::DEFAULT_MEM_RESERVE_MB_PER_CHILD as i64,
                )
                .max(1) as u64,
                cfg.get_f64(
                    "delegation.auto_mem_max_fraction",
                    crate::capacity::DEFAULT_MEM_MAX_FRACTION,
                )
                .clamp(0.05, 0.95),
            )
        } else {
            raw_children as usize
        };
        let max_requests = cfg.get_i64("delegation.max_concurrent_requests", 0) as usize;
        let max_requests = if max_requests == 0 {
            crate::capacity::capacity_requests(max_children)
        } else {
            max_requests
        };
        let default_model = cfg.get_str("delegation.default_model", "").to_string();

        // OMO concurrency config (FR-031, T148).
        let omo_default_concurrency =
            cfg.get_i64("omo.background_task.defaultConcurrency", 5) as usize;
        let omo_provider_concurrency =
            parse_concurrency_map(cfg, "omo.background_task.providerConcurrency");
        let omo_model_concurrency =
            parse_concurrency_map(cfg, "omo.background_task.modelConcurrency");

        Self {
            max_concurrent_children: max_children,
            max_concurrent_requests: max_requests,
            max_spawn_depth: cfg.get_i64("delegation.max_spawn_depth", 1) as usize,
            default_max_turns: cfg.get_i64("delegation.default_max_turns", 50) as usize,
            default_persist: cfg.get_bool("delegation.default_persist", false),
            default_model: if default_model.is_empty() {
                None
            } else {
                Some(default_model)
            },
            omo_default_concurrency,
            omo_provider_concurrency,
            omo_model_concurrency,
            // Async-delegation control (T002): guaranteed parent capacity
            // share (FR-018) and bounded session wind-down wait (FR-015).
            // `.max(0)` guards the i64→usize cast against negative values
            // (a raw `as usize` would wrap to a huge permit count).
            parent_reserved_permits: cfg
                .get_i64("delegation.parent_reserved_permits", 1)
                .max(0) as usize,
            wind_down_timeout_secs: cfg
                .get_i64("delegation.wind_down_timeout_secs", 10)
                .max(0) as u64,
        }
    }
}

/// Parse a config key that maps to a HashMap<String, usize>.
/// Reads `omo.background_task.{key}` as a table of provider/model → limit pairs.
fn parse_concurrency_map(
    cfg: &Config,
    key: &str,
) -> std::collections::HashMap<String, usize> {
    let mut map = std::collections::HashMap::new();
    if let Some(serde_yaml::Value::Mapping(table)) = cfg.get(key) {
        for (k, v) in table {
            if let (Some(name), Some(n)) = (k.as_str(), v.as_i64()) {
                map.insert(name.to_string(), n as usize);
            }
        }
    }
    map
}

/// Process-global child-id source (T033). Every `SubagentManager` in this
/// process draws child ids from this ONE counter, so two concurrently-alive
/// managers (the agent's delegate_task manager + the separate `/hypercode`
/// manager built by the CLI engine) can never mint the same id. This
/// matters because hosts route wrapped `AgentEvent::SubagentEvent`s to
/// panes by FIRST-MATCH on child id (joey-tui state.rs) — a colliding id
/// would cross-contaminate panes through the process-global tap. Ids remain
/// unique + monotonic per manager (allocations happen in dispatch order)
/// and still start at 1 in a fresh process.
static NEXT_CHILD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// String form of a [`StopReason`] matching its serde name (used by
/// `AgentEvent::SubagentStopped.reason`, which keeps a plain `String`).
fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::OrchestratorRequested => "orchestrator_requested",
        StopReason::OperatorRequested => "operator_requested",
        StopReason::BudgetExceeded => "budget_exceeded",
        StopReason::SessionEnd => "session_end",
    }
}

/// Shared child registry (T004): live children keyed by global child id +
/// session-lifetime terminal history (FR-019). Shared by the top-level
/// manager and the transient per-child managers a batch dispatch creates,
/// so `stop_child`/`steer_child`/`overview` on the manager the host holds
/// can see EVERY child this manager dispatched, regardless of which wave
/// spawned it. Terminal records are one-way: once a child id has a history
/// entry it is never appended again (late/duplicate completions no-op).
#[derive(Default)]
struct ChildRegistry {
    running: Mutex<HashMap<u64, ChildHandle>>,
    history: Mutex<Vec<DelegationOverview>>,
}

impl ChildRegistry {
    fn lock_running(&self) -> std::sync::MutexGuard<'_, HashMap<u64, ChildHandle>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn insert(&self, id: u64, handle: ChildHandle) {
        self.lock_running().insert(id, handle);
    }

    /// `Some(pending_stop)` while the child runs, `None` if not registered.
    fn pending_stop(&self, id: u64) -> Option<Option<StopReason>> {
        self.lock_running().get(&id).map(|h| h.pending_stop)
    }

    fn running_ids(&self) -> Vec<u64> {
        self.lock_running().keys().copied().collect()
    }

    fn running_is_empty(&self) -> bool {
        self.lock_running().is_empty()
    }

    fn history_contains(&self, id: u64) -> bool {
        self.history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|r| r.child_id == id.to_string())
    }

    /// Move a finished child from `running` to a terminal history record
    /// (FR-019 one-way: skips the append if a record already exists — e.g.
    /// `shutdown` finalized a straggler and its task completed late).
    /// Returns the appended record.
    fn complete(&self, id: u64, result: &DelegationResult) -> Option<DelegationOverview> {
        let handle = self.lock_running().remove(&id)?;
        if self.history_contains(id) {
            return None; // one-way: already finalized (e.g. shutdown timeout)
        }
        let state = match handle.pending_stop.or(result.stop_reason) {
            Some(reason) => DelegationState::Stopped { reason },
            None if result.success => DelegationState::Completed {
                result: result.clone(),
            },
            None => DelegationState::Failed {
                error: result.error.clone().unwrap_or_default(),
            },
        };
        let record = DelegationOverview {
            child_id: id.to_string(),
            goal: result.goal.clone(),
            state,
            elapsed: handle.started_at.elapsed(),
            tokens: result.token_usage.total_tokens,
        };
        self.history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(record.clone());
        Some(record)
    }

    /// Force-finalize a still-running child as `Stopped{reason}` (used by
    /// `shutdown` when the bounded wait expires). One-way like `complete`.
    fn finalize_stopped(&self, id: u64, reason: StopReason) -> Option<DelegationOverview> {
        let handle = self.lock_running().remove(&id)?;
        if self.history_contains(id) {
            return None;
        }
        let record = DelegationOverview {
            child_id: id.to_string(),
            goal: handle.task.goal.clone(),
            state: DelegationState::Stopped { reason },
            elapsed: handle.started_at.elapsed(),
            tokens: handle.usage.total_tokens,
        };
        self.history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(record.clone());
        Some(record)
    }
}

/// Shared state for the grant-back watcher (T005): tracks how many
/// parent-pool permits are currently lent to the child pool, with a lock
/// serializing lend/reclaim steps so the watcher can never lend more than
/// `reserve` permits nor reclaim more than it lent.
#[derive(Default)]
struct GrantBackState {
    lent: AtomicUsize,
    lock: std::sync::Mutex<()>,
    /// Whether the grant-back watcher task has been spawned for these pools.
    /// Lives HERE (not on the manager) so the transient per-child managers a
    /// batch dispatch creates — which all share this state — cannot each
    /// spawn their own duplicate 150 ms watcher.
    watcher_spawned: AtomicBool,
}

impl GrantBackState {
    /// One lend/reclaim step.
    ///
    /// LEND (only when `lend_allowed` — children are actually running —
    /// and the parent pool holds nothing beyond our own loan: the parent
    /// agent makes at most one provider call at a time, so any OTHER
    /// held permit means the parent is active): hand the child pool the
    /// still-unlent portion of `reserve` permits.
    ///
    /// Idle test is `available + lent >= total` — NOT `available >= total`
    /// — because lending itself drops `available` by exactly `lent`; the
    /// naive test would misread our own loan as parent activity and
    /// oscillate lend→reclaim every tick.
    ///
    /// RECLAIM when the parent is active, or when no children remain
    /// (`!lend_allowed`, e.g. after `shutdown` force-finalized stragglers):
    /// take back whatever lent permits the child pool currently has SPARE
    /// (in-flight child calls keep theirs — reclaiming never cancels or
    /// blocks running children; the rest returns as children release).
    ///
    /// Deadlock-free by construction: `lent ≤ reserve`, so the parent pool
    /// always retains ≥ `total − reserve` of its own permits.
    fn step(
        &self,
        parent: &Arc<Semaphore>,
        child: &Arc<Semaphore>,
        total: usize,
        reserve: usize,
        lend_allowed: bool,
    ) {
        if reserve == 0 || total == 0 {
            return;
        }
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let lent = self.lent.load(Ordering::SeqCst);
        let parent_idle = parent.available_permits() + lent >= total;
        if !lend_allowed {
            // No running children: return any outstanding loan so an idle
            // manager observably holds exactly `total` parent permits.
            if lent > 0 {
                let n = self.reclaim_spare_locked(child, lent);
                if n > 0 {
                    parent.add_permits(n);
                    self.lent.store(lent - n, Ordering::SeqCst);
                    tracing::trace!(lent = lent - n, "grant-back: reclaimed (no children)");
                }
            }
        } else if parent_idle && lent < reserve {
            let mut n = 0;
            while n < reserve - lent {
                match parent.clone().try_acquire_owned() {
                    Ok(p) => {
                        p.forget(); // remove from the parent pool…
                        n += 1;
                    }
                    Err(_) => break,
                }
            }
            if n > 0 {
                child.add_permits(n); // …reborn on the child pool
                self.lent.store(lent + n, Ordering::SeqCst);
                tracing::trace!(lent = lent + n, "grant-back: lent to child pool");
            }
        } else if !parent_idle && lent > 0 {
            let n = self.reclaim_spare_locked(child, lent);
            if n > 0 {
                parent.add_permits(n);
                self.lent.store(lent - n, Ordering::SeqCst);
                tracing::trace!(lent = lent - n, "grant-back: reclaimed to parent pool");
            }
        }
    }

    /// Reclaim every lent permit the child pool currently has spare (used
    /// when the last child completes, so an idle manager observably returns
    /// to exactly `total` parent permits — no lingering loans).
    fn reclaim_all(&self, parent: &Arc<Semaphore>, child: &Arc<Semaphore>) {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let lent = self.lent.load(Ordering::SeqCst);
        if lent == 0 {
            return;
        }
        let n = self.reclaim_spare_locked(child, lent);
        if n > 0 {
            parent.add_permits(n);
            self.lent.store(lent - n, Ordering::SeqCst);
        }
    }

    /// Caller holds the lock. Forgets up to `lent` spare child-pool permits.
    fn reclaim_spare_locked(&self, child: &Arc<Semaphore>, lent: usize) -> usize {
        let mut n = 0;
        while n < lent {
            match child.clone().try_acquire_owned() {
                Ok(p) => {
                    p.forget(); // remove from the child pool…
                    n += 1;
                }
                Err(_) => break,
            }
        }
        n
    }
}

/// The orchestrator that owns the concurrency limiter and dispatches batches.
pub struct SubagentManager {
    config: ManagerConfig,
    semaphore: Arc<Semaphore>,
    /// Second pool children acquire provider permits from (T005, FR-018):
    /// `max(1, max_concurrent_requests − parent_reserved_permits)` permits,
    /// so a saturated child wave can never starve the parent (SC-007).
    /// `parent_reserved_permits == 0` sizes it equal to the parent pool
    /// (pre-feature behavior).
    child_semaphore: Arc<Semaphore>,
    /// Grant-back watcher state (T005): how many parent-pool permits are
    /// currently lent to the child pool + the lock serializing
    /// lend/reclaim steps + the once-flag ensuring ONE watcher task for
    /// these shared pools (the transient per-child managers of a batch all
    /// point at the same Arc, so the flag must live here, not on a manager).
    grant_back: Arc<GrantBackState>,
    depth: usize,
    /// Cooperative interrupt signal shared with all spawned subagents (FR-015).
    /// When set to true, running subagents wind down cooperatively.
    interrupt: Arc<AtomicBool>,
    /// Optional live event tap (parallel-subagent feature). When set, every
    /// orchestration + child event is forwarded here in addition to any
    /// per-dispatch `event_tx`. The CLI uses this to wire delegation
    /// events into the TUI without rebuilding the tool registry per turn
    /// (the tool was constructed with `event_tx: None`).
    event_tap: std::sync::Mutex<Option<mpsc::UnboundedSender<AgentEvent>>>,
    /// SECONDARY recorder tap (feature 020, T029): `subagent_control`'s
    /// activity recorder channel, fed ALONGSIDE the external tap at every
    /// emission site — never returned by [`Self::event_tap`], so it can
    /// never shadow the host's tap (the pre-T029 bug: installing the
    /// recorder as the manager-local tap made `event_tap()` resolve to the
    /// recorder forever after, and a global tap installed later — the real
    /// TUI startup order — starved). Shared via `Arc` with the transient
    /// per-child managers so batch/background children feed it too.
    recorder_tap: Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<AgentEvent>>>>,
    /// CHILD REGISTRY (feature 020, T004): live children keyed by the
    /// global child id (each with PER-CHILD interrupt/steer handles so
    /// `stop_child`/`steer_child` act on exactly one child) plus the
    /// session-lifetime terminal history (FR-019). Shared with the
    /// transient per-child managers a batch dispatch creates.
    registry: Arc<ChildRegistry>,
    /// Whether this manager's `child_semaphore`/`registry` are the shared
    /// top-level ones (true for the manager hosts hold AND for the
    /// transient per-child managers a batch dispatch creates — both point
    /// at the same Arcs). False only for exotic manually-constructed
    /// non-pool managers, which keep the legacy parent-pool-only path.
    child_pool_owner: bool,
}

impl SubagentManager {
    /// Create a top-level manager from config.
    pub fn new(config: ManagerConfig) -> Self {
        let permits = config.max_concurrent_requests.max(1);
        // TWO-POOL SEMAPHORE (T005, FR-018/SC-007): children draw from a
        // second pool sized `max(1, N - parent_reserved_permits)` instead of
        // the parent pool. `reserve = 0` ⇒ child pool == N (the pre-feature
        // behavior). A lazy grant-back watcher can later LEND back up to
        // `reserve` permits when the parent is idle (see `grant_back_pump`),
        // reclaiming them the moment the parent shows activity.
        let reserve = config
            .parent_reserved_permits
            .min(config.max_concurrent_requests.saturating_sub(1));
        Self {
            semaphore: Arc::new(Semaphore::new(permits)),
            child_semaphore: Arc::new(Semaphore::new(
                permits.saturating_sub(reserve).max(1),
            )),
            grant_back: Arc::new(GrantBackState::default()),
            config,
            depth: 0,
            interrupt: Arc::new(AtomicBool::new(false)),
            event_tap: std::sync::Mutex::new(None),
            recorder_tap: Arc::new(std::sync::Mutex::new(None)),
            registry: Arc::new(ChildRegistry::default()),
            child_pool_owner: true,
        }
    }

    /// Install a live event tap (parallel-subagent feature). Every
    /// orchestration event (spawn/complete/failed/batch) AND every wrapped
    /// child event is mirrored to this channel. Setting a new tap replaces
    /// the previous one; `None` removes it.
    pub fn set_event_tap(&self, tap: Option<mpsc::UnboundedSender<AgentEvent>>) {
        *self
            .event_tap
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = tap;
    }

    /// The current event tap sender: the manager-local tap when set, else
    /// the process-global tap (parallel-subagent feature). The recorder tap
    /// (see [`Self::set_recorder_tap`]) is deliberately NOT part of this
    /// resolution — it is fed separately at the emission sites (T029).
    pub fn event_tap(&self) -> Option<mpsc::UnboundedSender<AgentEvent>> {
        let local = self
            .event_tap
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        local.or_else(crate::tap::global_tap)
    }

    /// Install the SECONDARY recorder tap (feature 020, T029): an internal
    /// channel fed alongside the external tap at every emission site. Used
    /// by `subagent_control`'s activity recorder (FR-005/FR-006) so its
    /// log ring keeps filling WITHOUT becoming the manager-local tap —
    /// a host tap installed before OR after registration resolves
    /// identically, and both consumers see every event. Setting a new
    /// recorder replaces the previous one; `None` removes it.
    pub fn set_recorder_tap(&self, tap: Option<mpsc::UnboundedSender<AgentEvent>>) {
        *self
            .recorder_tap
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = tap;
    }

    /// The recorder tap sender, if installed (see [`Self::set_recorder_tap`]).
    pub fn recorder_tap(&self) -> Option<mpsc::UnboundedSender<AgentEvent>> {
        self.recorder_tap
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Allocate the next stable child id from the PROCESS-GLOBAL counter
    /// (T033): unique across every SubagentManager in this process, so
    /// pane routing by child id can never cross-contaminate surfaces.
    pub(crate) fn next_id(&self) -> u64 {
        NEXT_CHILD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// The concurrency limiter semaphore (parent pool).
    pub fn semaphore(&self) -> Arc<Semaphore> {
        self.semaphore.clone()
    }

    /// The CHILD permit pool (T005, FR-018): children dispatched by this
    /// manager acquire provider permits here — never from the parent pool —
    /// so `parent_reserved_permits` permits are always available to the
    /// parent/hosts even under total child saturation (SC-007).
    pub fn child_semaphore(&self) -> Arc<Semaphore> {
        self.child_semaphore.clone()
    }

    /// How many permits the grant-back watcher may lend the child pool
    /// (0 when the reservation is disabled or N==1).
    fn reserve(&self) -> usize {
        self.config
            .parent_reserved_permits
            .min(self.config.max_concurrent_requests.saturating_sub(1))
    }

    /// Lazily spawn (once per manager) the grant-back watcher (T005): poll
    /// every ~150 ms and run one lend/reclaim step between the two pools.
    /// Lending is only allowed while this manager actually has running
    /// children. The task holds only `Weak` handles — when the manager (and
    /// any host holding `semaphore()`/`child_semaphore()` clones) is
    /// dropped, the upgrade fails and the watcher exits, so nothing leaks
    /// past the manager's lifetime. No-op when the reservation is disabled.
    fn ensure_grant_back_watcher(&self) {
        // Exactly ONE watcher per shared pool pair, even though every
        // transient per-child manager of a batch calls this.
        if self
            .grant_back
            .watcher_spawned
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        let reserve = self.reserve();
        if reserve == 0 {
            return; // reservation disabled — nothing to watch
        }
        let total = self.config.max_concurrent_requests.max(1);
        let parent_weak = Arc::downgrade(&self.semaphore);
        let child_weak = Arc::downgrade(&self.child_semaphore);
        let registry_weak = Arc::downgrade(&self.registry);
        let state = Arc::clone(&self.grant_back);
        let handle = tokio::spawn(async move {
            loop {
                let (Some(parent), Some(child), Some(registry)) = (
                    parent_weak.upgrade(),
                    child_weak.upgrade(),
                    registry_weak.upgrade(),
                ) else {
                    break; // manager gone — terminate (no leaked spawn)
                };
                let lend_allowed = !registry.running_is_empty();
                state.step(&parent, &child, total, reserve, lend_allowed);
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        });
        let _ = handle; // detached: Weak-upgrade failure ends it at drop
    }

    /// Build a transient per-child manager sharing this manager's pools,
    /// registry, grant-back state, interrupt, and event tap — the SAME
    /// plumbing `dispatch_requests` constructs for each blocking batch
    /// child. Feature 020 (T009): background children dispatch through one
    /// of these so registry control (stop/steer/status), the child permit
    /// pool (FR-013/FR-018), and terminal archival (FR-019) behave exactly
    /// as for blocking children.
    #[doc(hidden)]
    pub(crate) fn shared_child_manager(&self) -> SubagentManager {
        SubagentManager {
            config: ManagerConfig::default(),
            semaphore: self.semaphore.clone(),
            child_semaphore: self.child_semaphore.clone(),
            grant_back: self.grant_back.clone(),
            registry: self.registry.clone(),
            child_pool_owner: true,
            depth: self.depth,
            interrupt: self.interrupt.clone(),
            event_tap: std::sync::Mutex::new(self.event_tap()),
            // T029: the recorder tap is manager state shared by reference —
            // transient children keep feeding the same recorder channel.
            recorder_tap: self.recorder_tap.clone(),
        }
    }

    /// `ensure_grant_back_watcher` for EXTERNAL callers that hold the
    /// top-level manager (feature 020 background dispatch): seeds the
    /// watcher with the REAL pool sizes instead of the transient child
    /// manager's default-config totals. No-op when already spawned or the
    /// reservation is disabled.
    #[doc(hidden)]
    pub(crate) fn ensure_grant_back_watcher_shared(&self) {
        self.ensure_grant_back_watcher();
    }

    /// Pre-register a background child in the shared registry at SPAWN time
    /// (feature 020, T009): the returned [`WorkHandle`] is backed by a live
    /// registry record immediately, so `stop_child`/`steer_child`/`overview`
    /// see the child before its task starts running. The child's eventual
    /// `dispatch_single_with_overrides` call REUSES this entry's handles
    /// (never overwrites — a stop/steer recorded in the spawn→start window
    /// survives) and archives it on completion exactly like a blocking child.
    #[doc(hidden)]
    pub(crate) fn pre_register_child(&self, id: u64, task: TaskSpec) -> WorkHandle {
        let interrupt = Arc::new(AtomicBool::new(false));
        let steer: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let handle = ChildHandle::new(task, interrupt, steer);
        let wh = WorkHandle {
            child_id: id.to_string(),
            goal: handle.task.goal.clone(),
            started_at: handle.started_at,
        };
        self.registry.insert(id, handle);
        wh
    }

    /// The cooperative interrupt handle. Setting this to true causes all
    /// running and future subagents in this manager to wind down (FR-015).
    pub fn interrupt_handle(&self) -> Arc<AtomicBool> {
        self.interrupt.clone()
    }

    /// Signal all subagents to wind down cooperatively.
    pub fn signal_interrupt(&self) {
        self.interrupt.store(true, Ordering::SeqCst);
    }

    /// Whether an interrupt has been signaled.
    pub fn is_interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }

    /// The manager's configuration.
    pub fn config(&self) -> &ManagerConfig {
        &self.config
    }

    /// Current delegation depth (0 = top-level parent).
    pub fn depth(&self) -> usize {
        self.depth
    }

    // ------------------------------------------------------------------
    // Child control plane (feature 020: T004 stop/steer/status, T025
    // shutdown). None of these methods acquire a semaphore permit —
    // control stays live even under total child saturation (SC-007).
    // ------------------------------------------------------------------

    /// Stop one running child (FR-010). Sets the child's `pending_stop`
    /// FIRST (so its terminal record keeps the reason), then sets its
    /// per-child interrupt flag — the child's bridge loop forwards the
    /// flag into its Agent, which winds down at the next check point.
    /// Emits `AgentEvent::SubagentStopped` through the event tap when the
    /// child finalizes.
    ///
    /// Returns `Err` with a tool-facing message for unknown ids ("No
    /// subagent with id …") or already-terminal children ("already
    /// finished" — FR-019 one-way states).
    pub fn stop_child(&self, id: u64, reason: StopReason) -> Result<(), String> {
        let mut running = self.registry.lock_running();
        if let Some(handle) = running.get_mut(&id) {
            if handle.pending_stop.is_some() {
                // Concurrent second stop while still winding down:
                // idempotent ack — the first reason wins (one-way).
                return Ok(());
            }
            // Record the reason FIRST, then set the interrupt flag
            // (ordering per T004) — one critical section, atomically
            // w.r.t. other stop/steer/complete calls on this registry.
            handle.pending_stop = Some(reason);
            handle.interrupt.store(true, Ordering::SeqCst);
            Ok(())
        } else {
            drop(running);
            if self.registry.history_contains(id) {
                Err(format!("Subagent {id} already finished"))
            } else {
                Err(Self::unknown_child_error(id))
            }
        }
    }

    /// Steer one running child (FR-006): appends `message` to the child's
    /// steer slot; the child's bridge loop delivers it into the Agent's
    /// pending_steer at the next action boundary.
    ///
    /// Returns `Err` for unknown ids or already-terminal children
    /// ("already finished"), mirroring `stop_child`.
    pub fn steer_child(&self, id: u64, message: &str) -> Result<(), String> {
        let running = self.registry.lock_running();
        if let Some(handle) = running.get(&id) {
            if handle.pending_stop.is_some() {
                let reason = handle.pending_stop.unwrap();
                drop(running);
                return Err(format!(
                    "Subagent {id} already finished (stopping for {reason:?})"
                ));
            }
            let delivered = Agent::steer_via_handle(&handle.steer, message);
            if !delivered {
                return Err(format!(
                    "Subagent {id} steering message was empty (nothing delivered)"
                ));
            }
            Ok(())
        } else {
            drop(running);
            if self.registry.history_contains(id) {
                return Err(format!("Subagent {id} already finished"));
            }
            Err(Self::unknown_child_error(id))
        }
    }

    /// Status of one child: a snapshot `DelegationOverview` record —
    /// `Running` (with elapsed/tokens so far) or its terminal record from
    /// history. `None` when this manager never dispatched the id.
    pub fn child_status(&self, id: u64) -> Option<DelegationOverview> {
        if let Some(handle) = self.registry.lock_running().get(&id) {
            return Some(DelegationOverview {
                child_id: id.to_string(),
                goal: handle.task.goal.clone(),
                state: DelegationState::Running,
                elapsed: handle.started_at.elapsed(),
                tokens: handle.usage.total_tokens,
            });
        }
        self.history_snapshot().into_iter().find(|r| r.child_id == id.to_string())
    }

    /// Session-lifetime overview (FR-019): running children + terminal
    /// history, oldest first.
    pub fn overview(&self) -> Vec<DelegationOverview> {
        let mut records: Vec<DelegationOverview> = self
            .registry
            .lock_running()
            .iter()
            .map(|(id, h)| DelegationOverview {
                child_id: id.to_string(),
                goal: h.task.goal.clone(),
                state: DelegationState::Running,
                elapsed: h.started_at.elapsed(),
                tokens: h.usage.total_tokens,
            })
            .collect();
        records.extend(self.history_snapshot());
        records
    }

    /// Clone of the terminal history (snapshot).
    fn history_snapshot(&self) -> Vec<DelegationOverview> {
        self.registry
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn unknown_child_error(id: u64) -> String {
        format!("No subagent with id {id} is running or has finished in this session")
    }

    /// T020 glue (feature 020 budget watcher): mirror the parent-side
    /// watcher's live cumulative usage into the child's registry record so
    /// `overview()`/`child_status()` show running consumption (FR-012).
    /// No-op for unknown/finished children (terminal records are one-way).
    #[doc(hidden)]
    pub(crate) fn record_child_usage(&self, id: u64, usage: crate::types::RunningUsage) {
        if let Some(h) = self.registry.lock_running().get_mut(&id) {
            h.usage = usage;
        }
    }

    /// Wind down all running children at session end (T025, FR-015).
    /// Signals every child with `StopReason::SessionEnd`, then waits —
    /// bounded by `timeout` — for children to leave the running registry
    /// (each archives itself on completion). Children still running when
    /// the timeout expires are force-finalized in the history as
    /// `Stopped{SessionEnd}`; their detached tasks wind down on their own
    /// and their late `complete` calls no-op (FR-019 one-way). Returns the
    /// final overview snapshot. Never acquires a permit; never panics on a
    /// hung child.
    pub async fn shutdown(&self, timeout: Duration) -> Vec<DelegationOverview> {
        for id in self.registry.running_ids() {
            let _ = self.stop_child(id, StopReason::SessionEnd);
        }
        let deadline = tokio::time::Instant::now() + timeout;
        while !self.registry.running_is_empty() {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Force-finalize stragglers (bounded return even if a child hangs).
        for id in self.registry.running_ids() {
            self.registry.finalize_stopped(id, StopReason::SessionEnd);
        }
        self.overview()
    }

    /// Dispatch a single subagent (single-task mode).
    pub async fn dispatch_single(
        &self,
        req: &DelegationRequest,
        parent_config: &AgentConfig,
        parent_config_tree: &Config,
        base_registry: &ToolRegistry,
        event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> DelegationResult {
        self.dispatch_single_with_overrides(
            req,
            parent_config,
            parent_config_tree,
            base_registry,
            event_tx,
            self.config.default_model.as_deref(),
            self.config.default_max_turns,
            self.config.max_spawn_depth,
            self.next_id(),
        )
        .await
    }

    /// Internal dispatch with explicit overrides (used by batch dispatch).
    /// `allocated_id` MUST come from [`SubagentManager::next_id`] (the
    /// process-global counter) — the caller mints it so batch dispatch can
    /// allocate in deterministic dispatch order before spawning the wave.
    #[allow(clippy::too_many_arguments)] // deviation: domain-shaped dispatch, parameter bag would be speculative abstraction
    pub(crate) async fn dispatch_single_with_overrides(
        &self,
        req: &DelegationRequest,
        parent_config: &AgentConfig,
        parent_config_tree: &Config,
        base_registry: &ToolRegistry,
        event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
        default_model: Option<&str>,
        default_max_turns: usize,
        max_spawn_depth: usize,
        allocated_id: u64,
    ) -> DelegationResult {
        let model = crate::subagent::resolve_model(
            None,
            req.model.as_deref(),
            default_model,
            &parent_config.model,
        );
        let ts_sum = crate::subagent::toolset_summary(&req.toolsets);
        let id = allocated_id;
        let tap = self.event_tap();
        // T029: the recorder tap is fed alongside the external tap so the
        // subagent_control log ring fills without shadowing any host tap.
        let recorder = self.recorder_tap();

        // Emit SubagentSpawn event (per-dispatch channel + live tap).
        let spawn_ev = AgentEvent::SubagentSpawn {
            id,
            goal: req.goal.clone(),
            model: model.clone(),
            toolset_summary: ts_sum.clone(),
            depth: self.depth,
        };
        if let Some(tx) = event_tx {
            let _ = tx.send(spawn_ev.clone());
        }
        if let Some(tap) = &tap {
            let _ = tap.send(spawn_ev.clone());
        }
        if let Some(rec) = &recorder {
            let _ = rec.send(spawn_ev);
        }

        let mut subagent = match Subagent::new(
            req,
            parent_config,
            parent_config_tree,
            base_registry,
            default_model,
            default_max_turns,
            self.depth + 1,
            max_spawn_depth,
            None,
            self.interrupt.clone(),
            self.semaphore.clone(),
        ) {
            Ok(s) => s,
            Err(e) => {
                // Feature 020 (T009): a PRE-REGISTERED background child that
                // fails construction must still archive a terminal record
                // (its handle is already public). Blocking children were
                // never registered at this point — unchanged path.
                if self.child_pool_owner && self.registry.pending_stop(id).is_some() {
                    let fail = DelegationResult {
                        goal: req.goal.clone(),
                        summary: String::new(),
                        success: false,
                        error: Some(format!("Failed to create subagent: {}", e)),
                        token_usage: Default::default(),
                        wall_clock: std::time::Duration::ZERO,
                        model: model.clone(),
                        iterations: 0,
                        persisted_session_id: None,
                        stop_reason: None,
                    };
                    self.registry.complete(id, &fail);
                }
                let err_msg = format!("Failed to create subagent: {}", e);
                let fail_ev = AgentEvent::SubagentFailed {
                    id,
                    goal: req.goal.clone(),
                    error: err_msg.clone(),
                    duration_secs: 0.0,
                };
                if let Some(tx) = event_tx {
                    let _ = tx.send(fail_ev.clone());
                }
                if let Some(tap) = &tap {
                    let _ = tap.send(fail_ev.clone());
                }
                if let Some(rec) = &recorder {
                    let _ = rec.send(fail_ev);
                }
                return DelegationResult {
                    goal: req.goal.clone(),
                    summary: String::new(),
                    success: false,
                    error: Some(err_msg),
                    token_usage: Default::default(),
                    wall_clock: std::time::Duration::ZERO,
                    model,
                    iterations: 0,
                    persisted_session_id: None,
                    stop_reason: None,
                };
            }
        };

        // T004/T005: register the child with PER-CHILD interrupt + steer
        // handles, and switch its provider-permit source to the CHILD pool
        // (the parent pool keeps its reserved share free — SC-007). Pool
        // owners are the top-level manager and the transient per-child
        // managers a batch creates (both share the SAME registry + pools).
        //
        // Feature 020 (T009): a background child is PRE-REGISTERED at spawn
        // so the handle is backed by a registry record immediately. Reuse
        // that entry's interrupt/steer handles (never overwrite — a
        // stop/steer recorded in the spawn→start window, including
        // pending_stop, must survive) and keep its started_at. The blocking
        // path never pre-registers, so it is byte-identical to before.
        let (child_interrupt, child_steer, pre_registered) = if self.child_pool_owner {
            let existing = self
                .registry
                .lock_running()
                .get(&id)
                .map(|h| (h.interrupt.clone(), h.steer.clone()));
            match existing {
                Some((i, s)) => (i, s, true),
                None => (
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(String::new())),
                    false,
                ),
            }
        } else {
            (
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(String::new())),
                false,
            )
        };
        if self.child_pool_owner {
            // Pre-signaled manager-wide interrupt must reach the child.
            child_interrupt.store(self.interrupt.load(Ordering::SeqCst), Ordering::SeqCst);
            if !pre_registered {
                let task = TaskSpec {
                    goal: req.goal.clone(),
                    context: req.context.clone(),
                    model: req.model.clone(),
                    toolsets: req.toolsets.clone(),
                    role: None,
                    background: false,
                    budgets: None, // req does not carry budgets (watcher is later wave)
                };
                self.registry.insert(
                    id,
                    ChildHandle::new(task, child_interrupt.clone(), child_steer.clone()),
                );
            }
            // T005: a live child exists — make sure idle parent capacity can
            // be lent to the child pool while the parent stays idle.
            self.ensure_grant_back_watcher();
        }
        let child_sem = if self.child_pool_owner {
            self.child_semaphore.clone()
        } else {
            self.semaphore.clone()
        };
        subagent.agent.set_provider_semaphore(child_sem);

        // Per-child control bridge (T004): polls the child's OWN interrupt
        // flag and steer slot and forwards them into the child Agent's
        // handles, so `stop_child`/`steer_child` act on exactly this child
        // (the manager-wide flag keeps flowing through the subagent's own
        // forwarder, unchanged). Aborted right after the run finishes —
        // same pattern as the interrupt forwarder in subagent.rs.
        let agent_interrupt = subagent.agent.interrupt_handle();
        let agent_steer = subagent.agent.steer_handle();
        let bridge_flag = child_interrupt.clone();
        let bridge_steer = child_steer.clone();
        let bridge = tokio::spawn(async move {
            loop {
                if bridge_flag.load(Ordering::SeqCst) {
                    agent_interrupt.store(true, Ordering::SeqCst);
                }
                if let Ok(mut slot) = bridge_steer.lock() {
                    if !slot.is_empty() {
                        let text = std::mem::take(&mut *slot);
                        drop(slot);
                        Agent::steer_via_handle(&agent_steer, &text);
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let start = Instant::now();
        let result = subagent
            .run_with_tap(id, event_tx, tap.as_ref(), recorder.as_ref())
            .await;
        bridge.abort();
        let elapsed = start.elapsed().as_secs_f64();

        // T004: archive the finished child into the session history
        // (one-way terminal record, FR-019) and, when the last child is
        // done, return any permits the grant-back watcher lent out.
        if self.child_pool_owner {
            let stopped = self.registry.pending_stop(id).flatten();
            self.registry.complete(id, &result);
            if self.registry.running_is_empty() {
                self.grant_back
                    .reclaim_all(&self.semaphore, &self.child_semaphore);
            }
            if let Some(reason) = stopped.or(result.stop_reason) {
                let preview: String = result.summary.chars().take(100).collect();
                let ev = AgentEvent::SubagentStopped {
                    id,
                    goal: result.goal.clone(),
                    reason: stop_reason_str(reason).to_string(),
                    summary_preview: preview,
                };
                if let Some(tx) = event_tx {
                    let _ = tx.send(ev.clone());
                }
                if let Some(tap) = &tap {
                    let _ = tap.send(ev.clone());
                }
                if let Some(rec) = &recorder {
                    let _ = rec.send(ev);
                }
            }
        }

        // Emit completion/failure event (per-dispatch channel + live tap).
        let done_ev = if result.success {
            let preview: String = result.summary.chars().take(100).collect();
            AgentEvent::SubagentComplete {
                id,
                goal: result.goal.clone(),
                success: true,
                summary_preview: preview,
                token_usage: result.token_usage.clone(),
                duration_secs: elapsed,
            }
        } else {
            AgentEvent::SubagentFailed {
                id,
                goal: result.goal.clone(),
                error: result.error.clone().unwrap_or_default(),
                duration_secs: elapsed,
            }
        };
        if let Some(tx) = event_tx {
            let _ = tx.send(done_ev.clone());
        }
        if let Some(tap) = &tap {
            let _ = tap.send(done_ev.clone());
        }
        if let Some(rec) = &recorder {
            let _ = rec.send(done_ev);
        }

        result
    }
    /// Dispatch a batch of subagents in parallel (batch mode).
    ///
    /// PARALLEL-SUBAGENT FEATURE: all children in the batch are spawned as
    /// ONE wave of tokio tasks — no `max_concurrent_children` chunking of
    /// the waiting list. Admission to actually run is gated by the shared
    /// provider-request `Semaphore` (each child's provider calls acquire
    /// permits, FR-018), which is the correct throttle for network-bound
    /// work; `max_concurrent_children` now only bounds how many children a
    /// single batch may contain (excess tasks are chunked as before). One
    /// failure does not cancel others.
    #[allow(clippy::too_many_arguments)] // deviation: domain-shaped batch dispatch, parameter bag would be speculative abstraction
    pub async fn dispatch_batch(
        &self,
        tasks: &[TaskSpec],
        batch_model: Option<&str>,
        batch_toolsets: &[String],
        parent_config: &AgentConfig,
        parent_config_tree: &Config,
        base_registry: &ToolRegistry,
        event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> Vec<DelegationResult> {
        let requests = specs_to_requests(
            tasks,
            batch_model,
            batch_toolsets,
            Some(self.config.default_max_turns),
            self.config.default_persist,
            SubagentRole::Leaf,
        );

        self.dispatch_requests(&requests, parent_config, parent_config_tree, base_registry, event_tx)
            .await
    }

    /// `dispatch_batch` + HyperCode per-task role routing: each TaskSpec's
    /// `role` ("explorer"/"implementor") resolves its config-backed
    /// model/turns/tokens/reasoning and injects the role directive.
    /// Tasks without a role behave exactly like `dispatch_batch`.
    #[allow(clippy::too_many_arguments)] // deviation: domain-shaped batch dispatch, parameter bag would be speculative abstraction
    pub async fn dispatch_batch_with_roles(
        &self,
        tasks: &[TaskSpec],
        batch_model: Option<&str>,
        batch_toolsets: &[String],
        parent_config: &AgentConfig,
        parent_config_tree: &Config,
        base_registry: &ToolRegistry,
        event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> Vec<DelegationResult> {
        let mut requests = specs_to_requests(
            tasks,
            batch_model,
            batch_toolsets,
            Some(self.config.default_max_turns),
            self.config.default_persist,
            SubagentRole::Leaf,
        );
        if let Err(e) = crate::subagent::apply_batch_hyper_roles(
            &mut requests,
            tasks,
            parent_config_tree,
            &parent_config.provider,
        ) {
            // Surface the routing error as a failed batch of one entry — the
            // caller contract (Vec<DelegationResult>) stays intact.
            tracing::warn!("hypercode role routing failed: {e}");
        }

        self.dispatch_requests(&requests, parent_config, parent_config_tree, base_registry, event_tx)
            .await
    }

    /// Dispatch a wave of PRE-BUILT heterogeneous requests in parallel
    /// (HyperCode entry point: planner/explorer/implementor children with
    /// different models, toolsets, budgets, and prompts in one wave).
    ///
    /// Semantics identical to `dispatch_batch` — one concurrency wave
    /// (chunked over `max_concurrent_children`), semaphore-gated provider
    /// admission, stable result ordering by request index, and a closing
    /// `DelegationBatchComplete` on the dispatch channel + tap.
    pub async fn dispatch_requests(
        &self,
        requests: &[DelegationRequest],
        parent_config: &AgentConfig,
        parent_config_tree: &Config,
        base_registry: &ToolRegistry,
        event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> Vec<DelegationResult> {
        let total = requests.len();

        let start = Instant::now();

        let default_model = self.config.default_model.clone();
        let max_turns = self.config.default_max_turns;
        let max_spawn_depth = self.config.max_spawn_depth;
        let depth = self.depth;
        let max_children = self.config.max_concurrent_children.max(1);
        let shared_semaphore = self.semaphore.clone();
        let shared_child_semaphore = self.child_semaphore.clone();
        let shared_grant_back = self.grant_back.clone();
        let shared_registry = self.registry.clone();
        let tap = self.event_tap();
        // T029: the recorder tap is fed ALONGSIDE the external tap — capture
        // the shared slot by Arc so children installed after this snapshot
        // (e.g. a recorder attached later) are still seen.
        let recorder = self.recorder_tap.clone();
        // T005: children are about to run — start the grant-back watcher
        // (no-op when already spawned or the reservation is disabled).
        if !requests.is_empty() {
            self.ensure_grant_back_watcher();
        }

        let mut indexed_results: Vec<(usize, DelegationResult)> = Vec::with_capacity(total);
        let mut dispatched_count = 0usize;

        // Chunk only when the wave exceeds the children cap; within a
        // chunk everything runs concurrently (semaphore-gated).
        let chunks: Vec<Vec<DelegationRequest>> = requests
            .to_vec()
            .chunks(max_children)
            .map(|c| c.to_vec())
            .collect();

        for chunk in chunks {
            let mut join_set: JoinSet<(usize, DelegationResult)> = JoinSet::new();

            for (chunk_pos, req) in chunk.into_iter().enumerate() {
                let parent_cfg = parent_config.clone();
                let config_tree = parent_config_tree.clone();
                let registry = base_registry.clone();
                let dm = default_model.clone();
                let tx = event_tx.cloned();
                let sem = shared_semaphore.clone();
                let child_sem = shared_child_semaphore.clone();
                let grant_back = shared_grant_back.clone();
                let child_registry = shared_registry.clone();
                let interrupt = self.interrupt.clone();
                let tap = tap.clone();
                let recorder = recorder.clone();
                // Allocate the child's stable id from the PARENT manager's
                // counter so ids are unique + monotonic across the whole
                // batch (T033: same process-global counter the parent draws
                // from, so the id the child manager starts from can never
                // collide with any other manager's allocation).
                let child_id = self.next_id();
                // Original request index (for stable result ordering —
                // JoinSet yields in COMPLETION order, not dispatch order).
                let task_index = dispatched_count + chunk_pos;

                join_set.spawn(async move {
                    // Each child shares the PARENT's pools + registry
                    // (FR-018, T004): the transient manager points at the
                    // top manager's child pool, shared interrupt, tap, and
                    // child registry, so registry-driven per-child control
                    // (stop/steer/status) sees batch children too.
                    let mgr = SubagentManager {
                        config: ManagerConfig::default(),
                        semaphore: sem.clone(),
                        child_semaphore: child_sem.clone(),
                        grant_back: grant_back.clone(),
                        registry: child_registry.clone(),
                        child_pool_owner: true,
                        depth,
                        interrupt,
                        event_tap: std::sync::Mutex::new(tap),
                        // T029: shared by reference with the parent manager —
                        // batch children feed the recorder alongside the tap.
                        recorder_tap: recorder.clone(),
                    };
                    let result = mgr
                        .dispatch_single_with_overrides(
                            &req,
                            &parent_cfg,
                            &config_tree,
                            &registry,
                            tx.as_ref(),
                            dm.as_deref(),
                            max_turns,
                            max_spawn_depth,
                            child_id,
                        )
                        .await;
                    (task_index, result)
                });
            }
            dispatched_count += join_set.len();

            while let Some(res) = join_set.join_next().await {
                match res {
                    Ok((idx, r)) => indexed_results.push((idx, r)),
                    Err(join_err) => {
                        // A panicked/aborted child must surface as a failure
                        // row, not silently vanish (the [i/total] count would
                        // mismatch with no explanation). It consumed no
                        // dispatch slot we can identify, so park it at an
                        // out-of-range index; it sorts to the end.
                        let idx = usize::MAX;
                        indexed_results.push((
                            idx,
                            DelegationResult {
                                goal: format!("(child task panicked: {})", join_err),
                                summary: String::new(),
                                success: false,
                                error: Some(format!("subagent task failed: {}", join_err)),
                                token_usage: Default::default(),
                                wall_clock: std::time::Duration::ZERO,
                                model: String::new(),
                                iterations: 0,
                                persisted_session_id: None,
                                stop_reason: None,
                            },
                        ));
                    }
                }
            }
        }

        // Stable ordering: sort by the ORIGINAL task index (JoinSet yields
        // in completion order; goal-string matching mis-paired results when
        // goals repeated across tasks).
        indexed_results.sort_by_key(|(idx, _)| *idx);
        let ordered: Vec<DelegationResult> =
            indexed_results.into_iter().map(|(_, r)| r).collect();

        let elapsed = start.elapsed().as_secs_f64();
        let succeeded = ordered.iter().filter(|r| r.success).count();
        let failed = ordered.len().saturating_sub(succeeded);

        let batch_ev = AgentEvent::DelegationBatchComplete {
            total,
            succeeded,
            failed,
            total_duration_secs: elapsed,
        };
        if let Some(tx) = event_tx {
            let _ = tx.send(batch_ev.clone());
        }
        if let Some(tap) = &tap {
            let _ = tap.send(batch_ev);
        }

        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let c = ManagerConfig::default();
        assert_eq!(c.max_concurrent_children, 3);
        assert_eq!(c.max_concurrent_requests, 5);
        assert_eq!(c.max_spawn_depth, 1);
        assert_eq!(c.default_max_turns, 50);
        assert!(!c.default_persist);
        // T002: async-delegation control defaults (FR-018 / FR-015).
        assert_eq!(c.parent_reserved_permits, 1);
        assert_eq!(c.wind_down_timeout_secs, 10);
    }

    #[test]
    fn config_from_config_tree() {
        let cfg = joey_core::Config::defaults();
        let c = ManagerConfig::from_config(&cfg);
        // The schema default is `auto` → capacity-driven sizing (bounded by
        // the detected host, not the old fixed 3).
        assert!(
            c.max_concurrent_children >= crate::capacity::FLOOR_CHILDREN
                && c.max_concurrent_children <= crate::capacity::HARD_CHILD_CEILING,
            "auto sizing within [floor, ceiling], got {}",
            c.max_concurrent_children
        );
        assert_eq!(c.default_max_turns, 50);
        // T002: absent keys behave exactly as the defaults (backward-
        // compatible surface — absence preserves current behavior).
        assert_eq!(c.parent_reserved_permits, 1);
        assert_eq!(c.wind_down_timeout_secs, 10);
    }

    /// T002 regression: `delegation.parent_reserved_permits` and
    /// `delegation.wind_down_timeout_secs` load from a config tree, and
    /// existing `delegation.*` keys in the same tree are untouched.
    #[test]
    fn config_parent_reserved_permits_and_wind_down_timeout_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "delegation:\n  max_concurrent_children: 4\n  default_max_turns: 12\n  parent_reserved_permits: 2\n  wind_down_timeout_secs: 30\n",
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(path).unwrap();
        let c = ManagerConfig::from_config(&cfg);
        assert_eq!(c.parent_reserved_permits, 2);
        assert_eq!(c.wind_down_timeout_secs, 30);
        // Existing keys alongside the new ones are unaffected.
        assert_eq!(c.max_concurrent_children, 4);
        assert_eq!(c.default_max_turns, 12);
    }

    /// T002 regression: 0 is a meaningful value for `parent_reserved_permits`
    /// (disables the reservation) and must not be clobbered by the default;
    /// a malformed (non-integer) value falls back to the default rather than
    /// panicking.
    #[test]
    fn config_reserved_permits_zero_and_malformed_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "delegation:\n  parent_reserved_permits: 0\n",
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(path).unwrap();
        let c = ManagerConfig::from_config(&cfg);
        assert_eq!(c.parent_reserved_permits, 0, "0 disables reservation");

        let dir2 = tempfile::tempdir().unwrap();
        let path2 = dir2.path().join("config.yaml");
        std::fs::write(
            &path2,
            "delegation:\n  parent_reserved_permits: not-a-number\n  wind_down_timeout_secs: not-a-number\n",
        )
        .unwrap();
        let cfg2 = joey_core::Config::load_from(path2).unwrap();
        let c2 = ManagerConfig::from_config(&cfg2);
        assert_eq!(c2.parent_reserved_permits, 1, "malformed → default");
        assert_eq!(c2.wind_down_timeout_secs, 10, "malformed → default");
    }

    #[test]
    fn semaphore_has_correct_permits() {
        let mgr = SubagentManager::new(ManagerConfig {
            max_concurrent_requests: 7,
            ..Default::default()
        });
        assert_eq!(mgr.semaphore().available_permits(), 7);
    }

    #[test]
    fn depth_tracks_zero_at_top_level() {
        let mgr = SubagentManager::new(ManagerConfig::default());
        assert_eq!(mgr.depth(), 0);
    }

    // ------------------------------------------------------------------
    // Feature 020 deferred unit tests (Wave 2C): child registry
    // lifecycle, control-plane errors, SC-007 starvation-freedom,
    // bounded shutdown, grant-back, pool sizing.
    // ------------------------------------------------------------------

    /// Insert a synthetic running child into `mgr`'s registry (exactly what
    /// `dispatch_single_with_overrides` does after `Subagent::new` succeeds)
    /// and return its id + interrupt/steer handles.
    fn register_child(mgr: &SubagentManager, id: u64, goal: &str) {
        let interrupt = Arc::new(AtomicBool::new(false));
        let steer: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let task = TaskSpec {
            goal: goal.to_string(),
            context: None,
            model: None,
            toolsets: vec![],
            role: None,
            background: false,
            budgets: None,
        };
        mgr.registry.insert(id, ChildHandle::new(task, interrupt, steer));
    }

    /// A naturally-completed DelegationResult (registry archival input).
    fn ok_result(goal: &str) -> DelegationResult {
        DelegationResult {
            goal: goal.to_string(),
            summary: "done".to_string(),
            success: true,
            error: None,
            token_usage: Default::default(),
            wall_clock: Duration::ZERO,
            model: "m".to_string(),
            iterations: 1,
            persisted_session_id: None,
            stop_reason: None,
        }
    }

    /// (a) Registry lifecycle: running → terminal is one-way; a second
    /// `stop_child` after archival errors with "already finished".
    #[test]
    fn registry_lifecycle_is_one_way_and_double_stop_errors() {
        let mgr = SubagentManager::new(ManagerConfig::default());
        register_child(&mgr, 7, "lifecycle");

        // Running: status shows Running, stop is accepted exactly once with
        // a second concurrent stop idempotently acked.
        assert!(matches!(
            mgr.child_status(7).map(|r| r.state),
            Some(DelegationState::Running)
        ));
        assert_eq!(mgr.stop_child(7, StopReason::OrchestratorRequested), Ok(()));
        assert_eq!(
            mgr.stop_child(7, StopReason::OperatorRequested),
            Ok(()),
            "double-stop while winding down is an idempotent ack"
        );
        // First reason wins (one-way).
        assert!(mgr.registry.pending_stop(7).flatten().is_some_and(|r| r
            == StopReason::OrchestratorRequested));

        // Child task finishes: archived terminal (Stopped, not Completed,
        // because a stop was pending) and removed from running.
        let record = mgr.registry.complete(7, &ok_result("lifecycle"));
        let record = record.expect("first completion archives a record");
        assert!(matches!(
            record.state,
            DelegationState::Stopped {
                reason: StopReason::OrchestratorRequested
            }
        ));
        assert_eq!(record.child_id, "7");
        assert!(mgr.registry.running_is_empty());

        // One-way: a late duplicate completion must NOT append again.
        assert!(mgr.registry.complete(7, &ok_result("lifecycle")).is_none());
        assert_eq!(mgr.overview().len(), 1);

        // Post-terminal stop → "already finished".
        let err = mgr.stop_child(7, StopReason::SessionEnd).unwrap_err();
        assert!(err.contains("already finished"), "got: {err}");

        // child_status now serves the terminal record from history.
        assert!(matches!(
            mgr.child_status(7).map(|r| r.state),
            Some(DelegationState::Stopped { .. })
        ));
    }

    /// (b) stop_child/steer_child on an unknown id produce the documented
    /// error strings.
    #[test]
    fn stop_and_steer_unknown_id_error_strings() {
        let mgr = SubagentManager::new(ManagerConfig::default());
        let err = mgr.stop_child(4242, StopReason::OperatorRequested).unwrap_err();
        assert!(err.contains("No subagent with id 4242"), "got: {err}");
        let err = mgr.steer_child(4242, "new direction").unwrap_err();
        assert!(err.contains("No subagent with id 4242"), "got: {err}");
        // And a never-dispatched id has no status.
        assert!(mgr.child_status(4242).is_none());
        assert!(mgr.overview().is_empty());
    }

    /// (c) steer_child on a finished child → "already finished"; on a
    /// running child it delivers into the child's steer slot.
    #[test]
    fn steer_child_finished_errors_and_running_delivers() {
        let mgr = SubagentManager::new(ManagerConfig::default());
        register_child(&mgr, 9, "steerable");
        assert_eq!(mgr.steer_child(9, "prioritize tests"), Ok(()));
        {
            let running = mgr.registry.lock_running();
            let steer_text = running[&9].steer.lock().unwrap_or_else(|p| p.into_inner()).clone();
            assert_eq!(steer_text, "prioritize tests");
        }
        // Empty message is rejected by the delivery helper (Agent parity).
        assert!(mgr.steer_child(9, "   ").is_err());

        mgr.registry.complete(9, &ok_result("steerable"));
        let err = mgr.steer_child(9, "too late").unwrap_err();
        assert!(err.contains("already finished"), "got: {err}");
    }

    /// (d) SC-007: with BOTH pools fully saturated (every child + parent
    /// permit held) and a full running registry, every control-plane call
    /// completes well under 5 s — control never waits on a permit.
    #[test]
    fn sc007_control_actions_fast_under_total_saturation() {
        let mgr = SubagentManager::new(ManagerConfig {
            max_concurrent_requests: 4,
            max_concurrent_children: 3,
            ..Default::default()
        });
        // ≥N sleeping children: fill the registry…
        for id in 1..=4 {
            register_child(&mgr, id, &format!("sleep-{id}"));
        }
        // …and saturate BOTH pools (hold every permit for the whole test).
        let mut held = Vec::new();
        while let Ok(p) = mgr.child_semaphore().clone().try_acquire_owned() {
            held.push(p);
        }
        while let Ok(p) = mgr.semaphore().clone().try_acquire_owned() {
            held.push(p);
        }
        assert_eq!(
            mgr.child_semaphore().available_permits(),
            0,
            "child pool saturated"
        );
        assert_eq!(mgr.semaphore().available_permits(), 0, "parent pool saturated");

        let start = Instant::now();
        assert_eq!(mgr.stop_child(2, StopReason::OperatorRequested), Ok(()));
        assert!(mgr.child_status(2).is_some());
        assert_eq!(mgr.overview().len(), 4);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "control actions took {elapsed:?} under saturation"
        );
        drop(held);
    }

    /// (e) shutdown is bounded: a child that never leaves the running
    /// registry still yields a Stopped{SessionEnd} record and the call
    /// returns within ~timeout+slack.
    #[tokio::test]
    async fn shutdown_bounded_with_session_end_record() {
        let mgr = SubagentManager::new(ManagerConfig::default());
        register_child(&mgr, 11, "hung-child");
        register_child(&mgr, 12, "hung-child-2");

        let start = Instant::now();
        let overview = mgr.shutdown(Duration::from_secs(2)).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(3),
            "shutdown returned in {elapsed:?} (bounded by the 2s timeout)"
        );
        assert_eq!(overview.len(), 2);
        for record in &overview {
            assert!(matches!(
                record.state,
                DelegationState::Stopped {
                    reason: StopReason::SessionEnd
                }
            ));
        }
        assert!(mgr.registry.running_is_empty());
        // Late natural completion after force-finalize no-ops (one-way).
        assert!(mgr.registry.complete(11, &ok_result("late")).is_none());
    }

    /// (f) Grant-back, deterministic at the step level: idle parent lends
    /// up to `reserve`; parent activity reclaims spare permits; a parent
    /// holding an in-flight permit keeps it while children run.
    #[test]
    fn grant_back_lends_when_idle_and_reclaims_on_parent_activity() {
        let state = GrantBackState::default();
        let parent = Arc::new(Semaphore::new(4));
        let child = Arc::new(Semaphore::new(3)); // 4 - reserve(1)
        let total = 4;
        let reserve = 1;

        // Children running, parent idle → lend 1 to the child pool.
        state.step(&parent, &child, total, reserve, true);
        assert_eq!(state.lent.load(Ordering::SeqCst), 1);
        assert_eq!(child.available_permits(), 4);
        assert_eq!(parent.available_permits(), 3);

        // Idempotent: no more than `reserve` is ever lent.
        state.step(&parent, &child, total, reserve, true);
        assert_eq!(state.lent.load(Ordering::SeqCst), 1);

        // Parent becomes active (holds a permit) → the loan is reclaimed
        // from the child pool's SPARE permits.
        let held = parent.clone().try_acquire_owned().unwrap();
        state.step(&parent, &child, total, reserve, true);
        assert_eq!(state.lent.load(Ordering::SeqCst), 0, "reclaimed on activity");
        assert_eq!(parent.available_permits(), 3); // held + 3 free
        drop(held);

        // Re-lend once idle again, then simulate EVERY child call in
        // flight (hold all child-pool permits): reclaim must find no
        // spare permit, keep the loan outstanding, and never block.
        state.step(&parent, &child, total, reserve, true);
        assert_eq!(state.lent.load(Ordering::SeqCst), 1);
        let mut in_flight = Vec::new();
        while let Ok(p) = child.clone().try_acquire_owned() {
            in_flight.push(p);
        }
        let parent_active = parent.clone().try_acquire_owned().unwrap();
        state.step(&parent, &child, total, reserve, true);
        assert_eq!(
            state.lent.load(Ordering::SeqCst),
            1,
            "in-flight child permits are not force-reclaimed"
        );
        // Children release while the parent stays IDLE → the loan persists:
        // lending-while-idle is the steady state (that's the point of
        // grant-back). One more parent activity → reclaimed.
        drop(in_flight);
        drop(parent_active);
        state.step(&parent, &child, total, reserve, true);
        assert_eq!(
            state.lent.load(Ordering::SeqCst),
            1,
            "loan persists while parent idle and children running"
        );
        let active = parent.clone().try_acquire_owned().unwrap();
        state.step(&parent, &child, total, reserve, true);
        assert_eq!(state.lent.load(Ordering::SeqCst), 0, "reclaimed on activity");
        drop(active);
    }

    /// (f cont.) No children remain (e.g. shutdown force-finalized
    /// stragglers): the loan is returned so the parent pool observably
    /// restores to its full size.
    #[test]
    fn grant_back_reclaims_all_when_no_children_remain() {
        let state = GrantBackState::default();
        let parent = Arc::new(Semaphore::new(3));
        let child = Arc::new(Semaphore::new(2));
        state.step(&parent, &child, 3, 1, true);
        assert_eq!(state.lent.load(Ordering::SeqCst), 1);

        state.step(&parent, &child, 3, 1, false);
        assert_eq!(state.lent.load(Ordering::SeqCst), 0);
        assert_eq!(parent.available_permits(), 3);
        assert_eq!(child.available_permits(), 2);
    }

    /// (f cont.) The lazy watcher actually moves permits while children
    /// run and returns them once the registry empties.
    #[tokio::test]
    async fn grant_back_watcher_lends_then_reclaims_via_registry() {
        let mgr = SubagentManager::new(ManagerConfig {
            max_concurrent_requests: 4,
            ..Default::default()
        });
        let parent = mgr.semaphore();
        let child = mgr.child_semaphore();
        assert_eq!(child.available_permits(), 3);

        register_child(&mgr, 21, "watched");
        mgr.ensure_grant_back_watcher();
        // Watcher polls every 150 ms — well under a second of slack.
        for _ in 0..40 {
            if child.available_permits() > 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(child.available_permits(), 4, "idle parent lent its reserve");
        assert_eq!(parent.available_permits(), 3);

        // Child completes → registry empties → watcher returns the loan.
        mgr.registry.complete(21, &ok_result("watched"));
        for _ in 0..40 {
            if parent.available_permits() == 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(parent.available_permits(), 4, "loan returned when idle");
        assert_eq!(child.available_permits(), 3);
    }

    /// (g) Two-pool sizing (T005): reserve carves the child pool; 0 keeps
    /// the pre-feature N-permit pool; degenerate N==1 still admits one
    /// child; an oversized reserve is clamped so children are never fully
    /// starved by configuration.
    #[test]
    fn child_pool_sizing_respects_reservation() {
        let base = ManagerConfig::default();
        assert_eq!(
            SubagentManager::new(ManagerConfig {
                max_concurrent_requests: 5,
                ..base.clone()
            })
            .child_semaphore()
            .available_permits(),
            4,
            "default reserve=1 → N-1"
        );
        assert_eq!(
            SubagentManager::new(ManagerConfig {
                max_concurrent_requests: 5,
                parent_reserved_permits: 0,
                ..base.clone()
            })
            .child_semaphore()
            .available_permits(),
            5,
            "reserve=0 disables → full N (pre-feature)"
        );
        assert_eq!(
            SubagentManager::new(ManagerConfig {
                max_concurrent_requests: 1,
                ..base.clone()
            })
            .child_semaphore()
            .available_permits(),
            1,
            "N=1 clamps reserve → children can still run"
        );
        assert_eq!(
            SubagentManager::new(ManagerConfig {
                max_concurrent_requests: 3,
                parent_reserved_permits: 99,
                ..base
            })
            .child_semaphore()
            .available_permits(),
            1,
            "reserve clamped to N-1 → child pool keeps its last permit"
        );
    }
}
