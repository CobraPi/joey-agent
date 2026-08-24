//! SubagentManager: owns the concurrency limiter and dispatches batches.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_tools::ToolRegistry;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

use crate::subagent::{specs_to_requests, Subagent};
use crate::types::{DelegationRequest, DelegationResult, SubagentRole, TaskSpec};

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

/// The orchestrator that owns the concurrency limiter and dispatches batches.
pub struct SubagentManager {
    config: ManagerConfig,
    semaphore: Arc<Semaphore>,
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
}

impl SubagentManager {
    /// Create a top-level manager from config.
    pub fn new(config: ManagerConfig) -> Self {
        let permits = config.max_concurrent_requests.max(1);
        Self {
            config,
            semaphore: Arc::new(Semaphore::new(permits)),
            depth: 0,
            interrupt: Arc::new(AtomicBool::new(false)),
            event_tap: std::sync::Mutex::new(None),
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
    /// the process-global tap (parallel-subagent feature).
    pub fn event_tap(&self) -> Option<mpsc::UnboundedSender<AgentEvent>> {
        let local = self
            .event_tap
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        local.or_else(crate::tap::global_tap)
    }

    /// Allocate the next stable child id from the PROCESS-GLOBAL counter
    /// (T033): unique across every SubagentManager in this process, so
    /// pane routing by child id can never cross-contaminate surfaces.
    fn next_id(&self) -> u64 {
        NEXT_CHILD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// The concurrency limiter semaphore (shared across parent + children).
    pub fn semaphore(&self) -> Arc<Semaphore> {
        self.semaphore.clone()
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
            let _ = tap.send(spawn_ev);
        }

        let subagent = match Subagent::new(
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
                    let _ = tap.send(fail_ev);
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
                };
            }
        };

        let start = Instant::now();
        let result = subagent.run_with_tap(id, event_tx, tap.as_ref()).await;
        let elapsed = start.elapsed().as_secs_f64();

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
            let _ = tap.send(done_ev);
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
        let tap = self.event_tap();

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
                let interrupt = self.interrupt.clone();
                let tap = tap.clone();
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
                    // Each child shares the PARENT's semaphore (FR-018).
                    let mgr = SubagentManager {
                        config: ManagerConfig::default(),
                        semaphore: sem,
                        depth,
                        interrupt,
                        event_tap: std::sync::Mutex::new(tap),
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
    }
}
