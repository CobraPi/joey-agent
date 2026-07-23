//! Subagent: an isolated Agent instance with its own history, toolset, and budget.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use joey_agent_core::{Agent, AgentConfig, TurnResult};
use joey_core::Config;
use joey_providers::ProviderError;
use joey_tools::toolsets as ts;
use joey_tools::{ToolContext, ToolRegistry};

use crate::types::{DelegationRequest, DelegationResult, SubagentRole, TaskSpec};

/// Build a child `AgentConfig` from a delegation request and parent config.
///
/// Model resolution chain: per-TaskSpec.model > DelegationRequest.model >
/// config delegation.default_model > parent AgentConfig.model.
pub(crate) fn resolve_model(
    task_model: Option<&str>,
    req_model: Option<&str>,
    default_model: Option<&str>,
    parent_model: &str,
) -> String {
    task_model
        .map(String::from)
        .or_else(|| req_model.map(String::from))
        .or_else(|| default_model.map(String::from))
        .unwrap_or_else(|| parent_model.to_string())
}

/// Build a toolset summary string for events (e.g. "file, web").
pub(crate) fn toolset_summary(toolsets: &[String]) -> String {
    if toolsets.is_empty() {
        "all".to_string()
    } else {
        toolsets.join(", ")
    }
}

/// Create a fresh ToolContext for a subagent (isolated SessionState).
pub(crate) fn child_context(
    parent_config: &Config,
    workdir: Option<&std::path::Path>,
    session_id: &str,
) -> ToolContext {
    let cwd = workdir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")));
    ToolContext::new(cwd, parent_config.clone(), session_id)
}

/// Build a filtered ToolRegistry containing only the requested toolsets' tools.
pub(crate) fn filtered_registry(
    base: &ToolRegistry,
    toolsets: &[String],
    role: SubagentRole,
    depth: usize,
    max_spawn_depth: usize,
) -> ToolRegistry {
    let mut filtered = ToolRegistry::new();

    let registered: std::collections::HashSet<String> = base.names().into_iter().collect();
    let tool_names: Vec<String> = if toolsets.is_empty() {
        registered.iter().cloned().collect()
    } else {
        let mut names: Vec<String> = Vec::new();
        for t in toolsets {
            names.extend(ts::resolve(t));
        }
        names
            .into_iter()
            .filter(|n| registered.contains(n))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    };

    for name in &tool_names {
        if let Some(tool) = base.get(name) {
            filtered.register(tool);
        }
    }

    // Leaf role always excludes delegate_task; Orchestrator at depth >=
    // max_spawn_depth is also treated as Leaf.
    if role == SubagentRole::Leaf || depth >= max_spawn_depth {
        // delegate_task won't be in the filtered set unless explicitly
        // registered — the tool filtering above already handles this since
        // delegate_task lives in joey-orchestration, not joey-tools.
    }

    filtered
}

/// Resolve the enabled tool list for a subagent based on requested toolsets.
fn resolve_enabled_tools(
    req: &DelegationRequest,
    base_registry: &ToolRegistry,
    max_spawn_depth: usize,
    depth: usize,
) -> Vec<String> {
    let registered: std::collections::HashSet<String> = base_registry.names().into_iter().collect();
    let names: Vec<String> = if req.toolsets.is_empty() {
        registered.iter().cloned().collect()
    } else {
        let mut all = Vec::new();
        for t in &req.toolsets {
            all.extend(ts::resolve(t));
        }
        all
    };

    let mut enabled: Vec<String> = names.into_iter().filter(|n| registered.contains(n)).collect();

    if req.role == SubagentRole::Leaf || depth >= max_spawn_depth {
        enabled.retain(|n| n != "delegate_task");
    }

    enabled.sort();
    enabled.dedup();
    enabled
}

/// An isolated subagent instance.
#[allow(dead_code)]
pub(crate) struct Subagent {
    pub agent: Agent,
    pub goal: String,
    pub context: Option<String>,
    pub model: String,
    pub toolset_summary: String,
    pub depth: usize,
    pub interrupt: Arc<AtomicBool>,
    pub persist: bool,
    pub session_id: Option<String>,
}

impl Subagent {
    /// Construct a subagent from a delegation request.
    pub(crate) fn new(
        req: &DelegationRequest,
        parent_config: &AgentConfig,
        parent_config_tree: &Config,
        base_registry: &ToolRegistry,
        default_model: Option<&str>,
        default_max_turns: usize,
        depth: usize,
        max_spawn_depth: usize,
        workdir: Option<&std::path::Path>,
        interrupt: Arc<AtomicBool>,
    ) -> Result<Self, ProviderError> {
        let model = resolve_model(
            None,
            req.model.as_deref(),
            default_model,
            &parent_config.model,
        );

        let child_agent_cfg = AgentConfig {
            model: model.clone(),
            provider: parent_config.provider.clone(),
            base_url: parent_config.base_url.clone(),
            api_key: parent_config.api_key.clone(),
            max_turns: req.max_turns.unwrap_or(default_max_turns),
            api_max_retries: parent_config.api_max_retries,
            tool_delay: parent_config.tool_delay,
            reasoning: parent_config.reasoning.clone(),
            enabled_tools: resolve_enabled_tools(req, base_registry, max_spawn_depth, depth),
            max_tokens: parent_config.max_tokens,
            stream: parent_config.stream,
            pass_session_id: false,
        };

        let child_ctx = child_context(
            parent_config_tree,
            workdir.or(req.workdir.as_deref()),
            &format!("subagent-{}", uuid::Uuid::new_v4().simple()),
        );

        let child_registry =
            filtered_registry(base_registry, &req.toolsets, req.role, depth, max_spawn_depth);

        let ts_sum = toolset_summary(&req.toolsets);
        let mut agent = Agent::new(child_agent_cfg, child_registry, child_ctx)?;

        // Wire the cooperative interrupt handle into the child Agent so
        // setting the batch-level interrupt flag stops the subagent's turn
        // loop at the next check point (FR-015).
        let agent_interrupt = agent.interrupt_handle();
        let batch_interrupt = interrupt.clone();
        // Immediately propagate if already interrupted.
        if batch_interrupt.load(Ordering::SeqCst) {
            agent_interrupt.store(true, Ordering::SeqCst);
        }

        // Persist: attach a SessionDb to the child agent when persist=true (FR-017).
        let session_id = if req.persist {
            let child_sid = format!("subagent-{}", uuid::Uuid::new_v4().simple());
            match joey_core::state::SessionDb::open_default() {
                Ok(db) => {
                    let _ = db.create_session(
                        "subagent",
                        Some(&model),
                        None,
                    );
                    agent.set_session_store(db, child_sid.clone());
                    Some(child_sid)
                }
                Err(e) => {
                    tracing::warn!("Failed to open session DB for subagent persist: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            agent,
            goal: req.goal.clone(),
            context: req.context.clone(),
            model,
            toolset_summary: ts_sum,
            depth,
            interrupt: batch_interrupt,
            persist: req.persist,
            session_id,
        })
    }

    /// Run the subagent's turn loop and produce a DelegationResult.
    pub(crate) async fn run(
        mut self,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
    ) -> DelegationResult {
        let start = Instant::now();
        let goal = self.goal.clone();
        let model = self.model.clone();
        let batch_interrupt = self.interrupt.clone();
        let session_id = self.session_id.clone();

        let (fallback_tx, _fallback_rx) = tokio::sync::mpsc::unbounded_channel();
        let tx_for_run = if let Some(parent_tx) = event_tx {
            parent_tx.clone()
        } else {
            fallback_tx
        };

        // Build the initial user message: goal + context (FR-003).
        let initial_prompt = match &self.context {
            Some(ctx) if !ctx.is_empty() => {
                format!(
                    "{goal}\n\n\
                     --- Additional Context ---\n\
                     {ctx}\n\n\
                     --- End Context ---\n\
                     \n\
                     Work on the goal above. Keep your final summary under 500 tokens."
                )
            }
            _ => format!(
                "{goal}\n\n\
                 Keep your final summary under 500 tokens."
            ),
        };

        // Spawn a mid-turn interrupt forwarder: polls the batch interrupt flag
        // and propagates it to the agent's interrupt handle (FR-015).
        let agent_interrupt = self.agent.interrupt_handle();
        let forward_interrupt = batch_interrupt.clone();
        let forwarder_handle = tokio::spawn(async move {
            loop {
                if forward_interrupt.load(Ordering::SeqCst) {
                    agent_interrupt.store(true, Ordering::SeqCst);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });

        let result: TurnResult = self.agent.run_turn(&initial_prompt, tx_for_run).await;
        
        // Stop the forwarder.
        forwarder_handle.abort();
        let elapsed = start.elapsed();

        let summary = if !result.final_text.is_empty() {
            result.final_text
        } else {
            "(subagent produced no output)".to_string()
        };

        let summary_chars = summary.chars().count();
        if summary_chars > 2000 {
            tracing::warn!(
                "Subagent summary for '{}' is {} chars (~{} tokens) — exceeds 500 token target",
                goal,
                summary_chars,
                summary_chars / 4
            );
        }

        // Determine if interrupted by the batch-level flag.
        let was_interrupted = result.interrupted || batch_interrupt.load(Ordering::SeqCst);

        DelegationResult {
            goal,
            summary,
            success: !was_interrupted,
            error: if was_interrupted {
                Some("subagent was interrupted".to_string())
            } else {
                None
            },
            token_usage: result.usage,
            wall_clock: elapsed,
            model,
            iterations: result.iterations,
            persisted_session_id: session_id,
        }
    }
}

/// Create a batch of TaskSpec-derived DelegationRequests for parallel dispatch.
pub(crate) fn specs_to_requests(
    tasks: &[TaskSpec],
    batch_model: Option<&str>,
    batch_toolsets: &[String],
    batch_max_turns: Option<usize>,
    persist: bool,
    role: SubagentRole,
) -> Vec<DelegationRequest> {
    tasks
        .iter()
        .map(|spec| DelegationRequest {
            goal: spec.goal.clone(),
            context: spec.context.clone(),
            tasks: Vec::new(),
            model: spec
                .model
                .clone()
                .or_else(|| batch_model.map(|s| s.to_string())),
            toolsets: if spec.toolsets.is_empty() {
                batch_toolsets.to_vec()
            } else {
                spec.toolsets.clone()
            },
            max_turns: batch_max_turns,
            persist,
            role,
            workdir: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_resolution_chain() {
        assert_eq!(
            resolve_model(Some("task-model"), Some("req-model"), Some("default-model"), "parent"),
            "task-model"
        );
        assert_eq!(
            resolve_model(None, Some("req-model"), Some("default-model"), "parent"),
            "req-model"
        );
        assert_eq!(
            resolve_model(None, None, Some("default-model"), "parent"),
            "default-model"
        );
        assert_eq!(resolve_model(None, None, None, "parent"), "parent");
    }

    #[test]
    fn toolset_summary_formats() {
        assert_eq!(toolset_summary(&[]), "all");
        assert_eq!(toolset_summary(&["file".into(), "web".into()]), "file, web");
    }
}
