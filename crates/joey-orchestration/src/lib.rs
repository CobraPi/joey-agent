//! `joey-orchestration` — the agentic orchestration engine for joey-agent.
//!
//! Provides subagent dispatch (single + parallel batch), isolated execution
//! contexts, shared concurrency limiting, per-subagent model/tool selection,
//! and structured lifecycle events. Ported from Hermes Agent's delegate_task
//! and Crush's coordinator patterns.

pub mod delegation_tool;
pub mod manager;
pub mod subagent;
pub mod types;

pub use delegation_tool::DelegateTask;
pub use manager::{ManagerConfig, SubagentManager};
pub use types::{DelegationRequest, DelegationResult, SubagentRole, TaskSpec};

/// Register the delegate_task tool into a tool registry.
/// Must be called after register_all() so it can see the full registry.
pub fn register_orchestration(
    registry: &mut joey_tools::ToolRegistry,
    manager: std::sync::Arc<SubagentManager>,
    parent_config: joey_agent_core::AgentConfig,
    parent_config_tree: joey_core::Config,
    base_registry: joey_tools::ToolRegistry,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
) {
    registry.register(std::sync::Arc::new(DelegateTask::new(
        manager,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
    )));
}
