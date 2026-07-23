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
        }
    }
}

impl ManagerConfig {
    /// Build from a Config tree, reading `delegation.*` keys.
    pub fn from_config(cfg: &Config) -> Self {
        let max_children = cfg.get_i64("delegation.max_concurrent_children", 3) as usize;
        let max_requests = cfg.get_i64(
            "delegation.max_concurrent_requests",
            (max_children + 2) as i64,
        ) as usize;
        let default_model = cfg.get_str("delegation.default_model", "").to_string();
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
        }
    }
}

/// The orchestrator that owns the concurrency limiter and dispatches batches.
pub struct SubagentManager {
    config: ManagerConfig,
    semaphore: Arc<Semaphore>,
    depth: usize,
    /// Cooperative interrupt signal shared with all spawned subagents (FR-015).
    /// When set to true, running subagents wind down cooperatively.
    interrupt: Arc<AtomicBool>,
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
        }
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
        )
        .await
    }

    /// Internal dispatch with explicit overrides (used by batch dispatch).
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
    ) -> DelegationResult {
        let model = crate::subagent::resolve_model(
            None,
            req.model.as_deref(),
            default_model,
            &parent_config.model,
        );
        let ts_sum = crate::subagent::toolset_summary(&req.toolsets);

        // Emit SubagentSpawn event.
        if let Some(tx) = event_tx {
            let _ = tx.send(AgentEvent::SubagentSpawn {
                goal: req.goal.clone(),
                model: model.clone(),
                toolset_summary: ts_sum.clone(),
                depth: self.depth,
            });
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
        ) {
            Ok(s) => s,
            Err(e) => {
                let err_msg = format!("Failed to create subagent: {}", e);
                if let Some(tx) = event_tx {
                    let _ = tx.send(AgentEvent::SubagentFailed {
                        goal: req.goal.clone(),
                        error: err_msg.clone(),
                        duration_secs: 0.0,
                    });
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
        let result = subagent.run(event_tx).await;
        let elapsed = start.elapsed().as_secs_f64();

        // Emit completion/failure event.
        if let Some(tx) = event_tx {
            if result.success {
                let preview: String = result.summary.chars().take(100).collect();
                let _ = tx.send(AgentEvent::SubagentComplete {
                    goal: result.goal.clone(),
                    success: true,
                    summary_preview: preview,
                    token_usage: result.token_usage.clone(),
                    duration_secs: elapsed,
                });
            } else {
                let _ = tx.send(AgentEvent::SubagentFailed {
                    goal: result.goal.clone(),
                    error: result.error.clone().unwrap_or_default(),
                    duration_secs: elapsed,
                });
            }
        }

        result
    }

    /// Dispatch a batch of subagents in parallel (batch mode).
    /// One failure does not cancel others. The parent's semaphore is shared
    /// across all children (FR-018). Batches larger than max_concurrent_children
    /// are chunked so at most that many run simultaneously (FR-018).
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

        let total = requests.len();
        let start = Instant::now();

        let default_model = self.config.default_model.clone();
        let max_turns = self.config.default_max_turns;
        let max_spawn_depth = self.config.max_spawn_depth;
        let depth = self.depth;
        let max_children = self.config.max_concurrent_children.max(1);
        let shared_semaphore = self.semaphore.clone();

        let mut all_results = Vec::with_capacity(total);

        // Process in chunks of max_concurrent_children so excess tasks queue.
        // Collect each chunk into an owned Vec to avoid lifetime issues with
        // borrowed slice items moved into async blocks.
        let chunks: Vec<Vec<DelegationRequest>> = requests
            .into_iter()
            .collect::<Vec<_>>()
            .chunks(max_children)
            .map(|c| c.to_vec())
            .collect();

        for chunk in chunks {
            let mut join_set: JoinSet<DelegationResult> = JoinSet::new();

            for req in chunk {
                let parent_cfg = parent_config.clone();
                let config_tree = parent_config_tree.clone();
                let registry = base_registry.clone();
                let dm = default_model.clone();
                let tx = event_tx.cloned();
                let sem = shared_semaphore.clone();
                let interrupt = self.interrupt.clone();

                join_set.spawn(async move {
                    // Each child shares the PARENT's semaphore (FR-018).
                    let mgr = SubagentManager {
                        config: ManagerConfig::default(),
                        semaphore: sem,
                        depth,
                        interrupt,
                    };
                    mgr.dispatch_single_with_overrides(
                        &req,
                        &parent_cfg,
                        &config_tree,
                        &registry,
                        tx.as_ref(),
                        dm.as_deref(),
                        max_turns,
                        max_spawn_depth,
                    )
                    .await
                });
            }

            while let Some(res) = join_set.join_next().await {
                if let Ok(r) = res {
                    all_results.push(r);
                }
            }
        }

        // Sort results by original task order (goal matching).
        let mut results = all_results;
        let mut ordered = Vec::with_capacity(total);
        for task in tasks {
            if let Some(pos) = results.iter().position(|r| r.goal == task.goal) {
                ordered.push(results.remove(pos));
            }
        }
        ordered.extend(results);

        let elapsed = start.elapsed().as_secs_f64();
        let succeeded = ordered.iter().filter(|r| r.success).count();
        let failed = ordered.len().saturating_sub(succeeded);

        if let Some(tx) = event_tx {
            let _ = tx.send(AgentEvent::DelegationBatchComplete {
                total,
                succeeded,
                failed,
                total_duration_secs: elapsed,
            });
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
        assert_eq!(c.max_concurrent_children, 3);
        assert_eq!(c.default_max_turns, 50);
    }
}
