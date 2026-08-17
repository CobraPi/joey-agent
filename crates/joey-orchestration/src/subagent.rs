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
    #[allow(clippy::too_many_arguments)] // deviation: domain-shaped construction, parameter bag would be speculative abstraction
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
        semaphore: Arc<tokio::sync::Semaphore>,
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
            // The child model was resolved by the delegation layer (per-task
            // override or fallback chain) — that resolution is authoritative,
            // so pin it against NeuroCode tier rewrites.
            model_pinned: true,
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

        // Share the PARENT's provider-request semaphore (FR-018). Without
        // this, a batch of N children fires unbounded concurrent provider
        // requests — the manager's semaphore exists but was never wired
        // into child agents, so capacity_requests() never throttled.
        agent.set_provider_semaphore(semaphore);

        // T060/T149: Inject category prompt_append as extra instructions.
        // This is prepended to the system prompt alongside any loaded skills.
        if let Some(ref append) = req.prompt_append {
            if !append.is_empty() {
                let mut overlay = String::new();
                for skill in &req.load_skills {
                    overlay.push_str(&format!(
                        "--- Loaded Skill: {} ---\n(Load and follow this skill's guidance.)\n\n",
                        skill
                    ));
                }
                overlay.push_str("--- Category Directive ---\n");
                overlay.push_str(append);
                overlay.push_str("\n--- End Category Directive ---\n");
                agent.set_extra_instructions(Some(overlay));
            }
        }

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
            match joey_core::state::SessionDb::open_default() {
                Ok(db) => {
                    // Use the id create_session RETURNS (it has a real
                    // sessions row). The old code discarded it and used an
                    // unregistered "subagent-<uuid>" string — persisted
                    // messages then referenced a session that no query
                    // (session search, resume) could resolve.
                    match db.create_session("subagent", Some(&model), None) {
                        Ok(real_sid) => {
                            agent.set_session_store(db, real_sid.clone());
                            Some(real_sid)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to create session row for subagent persist: {}",
                                e
                            );
                            None
                        }
                    }
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
    ///
    /// Backward-compatible wrapper: no live tap, events forwarded raw to the
    /// per-dispatch channel (legacy behavior for direct callers/tests).
    #[allow(dead_code)] // legacy entry kept for direct callers/tests; manager uses run_with_tap
    pub(crate) async fn run(
        self,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
    ) -> DelegationResult {
        self.run_with_tap(0, event_tx, None).await
    }

    /// Run the subagent's turn loop, forwarding every child event to the
    /// tap wrapped as `AgentEvent::SubagentEvent { id, event }` (parallel-
    /// subagent feature) while the per-dispatch channel keeps receiving the
    /// RAW events (legacy behavior preserved).
    ///
    /// Id `0` + no tap is byte-identical to the pre-feature `run`.
    pub(crate) async fn run_with_tap(
        mut self,
        id: u64,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
        tap: Option<&tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
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

        // Parallel-subagent feature: intercept the child's event stream.
        // When a tap is installed, every event the child emits is FIRST
        // mirrored to the tap (wrapped with the child's stable id) and ALSO
        // forwarded raw to the legacy per-dispatch channel so existing
        // consumers are unaffected. Implementation: wrap the sender with a
        // fan-out via an mpsc channel + forwarding task.
        let result: TurnResult = if tap.is_some() {
            let (child_tx, mut child_rx) =
                tokio::sync::mpsc::unbounded_channel::<joey_agent_core::AgentEvent>();
            let tap_tx = tap.cloned().unwrap();
            let legacy_tx = tx_for_run.clone();
            let fanout = tokio::spawn(async move {
                while let Some(ev) = child_rx.recv().await {
                    if id != 0 {
                        let _ = tap_tx.send(joey_agent_core::AgentEvent::SubagentEvent {
                            id,
                            event: Box::new(ev.clone()),
                        });
                    }
                    let _ = legacy_tx.send(ev);
                }
            });
            let r = self.agent.run_turn(&initial_prompt, child_tx).await;
            // child_tx drops here → fanout drains → task ends.
            let _ = fanout.await;
            r
        } else {
            self.agent.run_turn(&initial_prompt, tx_for_run).await
        };

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

        // Determine outcome: interrupted beats fatal beats clean.
        let was_interrupted = result.interrupted || batch_interrupt.load(Ordering::SeqCst);
        let fatal = result.fatal && !was_interrupted;

        DelegationResult {
            goal,
            summary,
            success: !was_interrupted && !fatal,
            error: if was_interrupted {
                Some("subagent was interrupted".to_string())
            } else if fatal {
                Some("subagent turn failed (fatal provider error)".to_string())
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
        category: None,
        subagent_type: None,
        load_skills: Vec::new(),
        prompt_append: None,
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
