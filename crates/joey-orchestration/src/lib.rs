//! `joey-orchestration` — the agentic orchestration engine for joey-agent.
//!
//! Provides subagent dispatch (single + parallel batch), isolated execution
//! contexts, shared concurrency limiting, per-subagent model/tool selection,
//! and structured lifecycle events. Ported from Hermes Agent's delegate_task
//! and Crush's coordinator patterns.

pub mod background;
pub mod capacity;
pub mod control_tool;
pub mod delegation_tool;
pub mod manager;
pub mod subagent;
pub mod tap;
pub mod types;

pub use capacity::{
    capacity_children, capacity_requests, SystemCapacity, DEFAULT_MEM_MAX_FRACTION,
    DEFAULT_MEM_RESERVE_MB_PER_CHILD, HARD_CHILD_CEILING,
};
pub use control_tool::SubagentControl;
pub use delegation_tool::{CallOmoAgent, DelegateTask};
pub use manager::{ManagerConfig, SubagentManager};
pub use types::{
    Budgets, DelegationOverview, DelegationRequest, DelegationResult, DelegationState,
    StopReason, SubagentRole, TaskSpec, WorkHandle,
};

/// Result of resolving a category or subagent_type to its model + prompt_append.
/// Returned by the CategoryResolver trait so joey-orchestration can resolve
/// OMO categories without depending on joey-omo (avoids circular dependency).
#[derive(Debug, Clone)]
pub struct ResolvedDelegation {
    /// The resolved model ID.
    pub model: String,
    /// Optional prompt_append text to prepend to the subagent's system prompt.
    pub prompt_append: Option<String>,
}

/// Trait for resolving OMO category/subagent_type names to model + prompt_append.
/// Implemented by the CLI layer (which depends on both joey-omo and
/// joey-orchestration), injected into the DelegateTask tool. This avoids a
/// circular dependency between joey-orchestration and joey-omo (T057/T135).
pub trait CategoryResolver: Send + Sync {
    /// Resolve a category name to its model + prompt_append.
    /// Returns None if the category is unknown or its model chain doesn't resolve.
    fn resolve_category(&self, name: &str) -> Option<ResolvedDelegation>;

    /// Resolve a subagent_type to its model.
    /// Returns None if the agent type is unknown or unavailable.
    fn resolve_subagent_type(&self, name: &str) -> Option<ResolvedDelegation>;
}

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
    register_orchestration_inner(
        registry,
        manager,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        None,
        None,
    );
}

/// Register orchestration with a dynamic model allocator (feature 011, T028).
/// The allocator is threaded into the delegate_task tool so subagent dispatch
/// consults `ModuleId::Subagent` when the resolved model is `auto`.
pub fn register_orchestration_with_allocator(
    registry: &mut joey_tools::ToolRegistry,
    manager: std::sync::Arc<SubagentManager>,
    parent_config: joey_agent_core::AgentConfig,
    parent_config_tree: joey_core::Config,
    base_registry: joey_tools::ToolRegistry,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
    allocator: Option<std::sync::Arc<dyn joey_llm_selector::ModelAllocator>>,
) {
    register_orchestration_inner(
        registry,
        manager,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        None,
        allocator,
    );
}

fn register_orchestration_inner(
    registry: &mut joey_tools::ToolRegistry,
    manager: std::sync::Arc<SubagentManager>,
    parent_config: joey_agent_core::AgentConfig,
    parent_config_tree: joey_core::Config,
    base_registry: joey_tools::ToolRegistry,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
    resolver: Option<std::sync::Arc<dyn CategoryResolver>>,
    allocator: Option<std::sync::Arc<dyn joey_llm_selector::ModelAllocator>>,
) {
    let mut delegate = DelegateTask::new(
        manager.clone(),
        parent_config.clone(),
        parent_config_tree.clone(),
        base_registry.clone(),
        event_tx.clone(),
        resolver.clone(),
    );
    if let Some(alloc) = allocator {
        delegate.set_model_allocator(alloc);
    }
    registry.register(std::sync::Arc::new(delegate));
    // subagent_control (feature 020, US3): acts on the SAME manager
    // registry as delegate_task — steer/stop any child this manager
    // dispatched (blocking or background).
    registry.register(std::sync::Arc::new(SubagentControl::new(manager.clone())));
    // Register call_omo_agent WITH the resolver: it requires
    // `subagent_type` resolution to succeed — a None resolver made the
    // tool permanently error ("requires an OMO category resolver"),
    // killing Junior's research path (T153) entirely.
    registry.register(std::sync::Arc::new(CallOmoAgent::new(
        manager,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        resolver,
    )));
}

/// Register the delegate_task tool with OMO category resolution support.
/// The CategoryResolver is provided by the CLI layer, which has access to
/// both joey-omo (for resolve_category) and joey-orchestration.
/// Also registers call_omo_agent (research-only delegation for Junior, T153).
pub fn register_orchestration_with_resolver(
    registry: &mut joey_tools::ToolRegistry,
    manager: std::sync::Arc<SubagentManager>,
    parent_config: joey_agent_core::AgentConfig,
    parent_config_tree: joey_core::Config,
    base_registry: joey_tools::ToolRegistry,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
    resolver: std::sync::Arc<dyn CategoryResolver>,
) {
    register_orchestration_inner(
        registry,
        manager,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        Some(resolver.clone()),
        None, // no allocator — use register_orchestration_with_resolver_and_allocator
    );
}

/// Register with BOTH an OMO category resolver and a dynamic model allocator
/// (feature 011, T028). This is the full-feature registration path used by
/// the REPL when both OMO categories and the dynamic selector are active.
pub fn register_orchestration_with_resolver_and_allocator(
    registry: &mut joey_tools::ToolRegistry,
    manager: std::sync::Arc<SubagentManager>,
    parent_config: joey_agent_core::AgentConfig,
    parent_config_tree: joey_core::Config,
    base_registry: joey_tools::ToolRegistry,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<joey_agent_core::AgentEvent>>,
    resolver: std::sync::Arc<dyn CategoryResolver>,
    allocator: Option<std::sync::Arc<dyn joey_llm_selector::ModelAllocator>>,
) {
    register_orchestration_inner(
        registry,
        manager,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        Some(resolver),
        allocator,
    );
}
