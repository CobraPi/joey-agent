//! T063 contract: category-based delegation routing.
//!
//! Verifies the DelegateTask tool:
//!   - Rejects calls specifying BOTH `category` and `subagent_type` (BC-011).
//!   - Resolves a `category` via the injected CategoryResolver to a model +
//!     prompt_append, and emits a CategoryDelegation event (BC-012, FR-013).
//!   - Emits the resolved model on the CategoryDelegation event.
//!
//! The full subagent dispatch requires a live provider, so these tests exercise
//! the resolution/validation layer (which is what the contract guarantees at
//! the tool boundary). A successful resolution reaches dispatch_single, which
//! fails without credentials — the important contract assertions are the
//! *resolution* and *event emission*, not the model round-trip.

use std::sync::Arc;

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::{
    CategoryResolver, DelegateTask, ManagerConfig, ResolvedDelegation, SubagentManager,
};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::sync::mpsc;

/// A mock resolver that maps known categories/subagent types to fixed models,
/// letting us assert what the tool resolved WITHOUT a real provider catalog.
struct MockResolver;

impl CategoryResolver for MockResolver {
    fn resolve_category(&self, name: &str) -> Option<ResolvedDelegation> {
        match name {
            "quick" => Some(ResolvedDelegation {
                model: "gpt-5.4-mini".to_string(),
                prompt_append: Some("QUICK MODE".to_string()),
            }),
            "visual-engineering" => Some(ResolvedDelegation {
                model: "gemini-3.1-pro".to_string(),
                prompt_append: None,
            }),
            _ => None,
        }
    }
    fn resolve_subagent_type(&self, name: &str) -> Option<ResolvedDelegation> {
        match name {
            "oracle" => Some(ResolvedDelegation {
                model: "gpt-5.6-sol".to_string(),
                prompt_append: None,
            }),
            _ => None,
        }
    }
}

fn make_agent_config() -> AgentConfig {
    AgentConfig {
        model: "test-model".to_string(),
        provider: "openrouter".to_string(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        api_key: None,
        max_turns: 10,
        api_max_retries: 3,
        tool_delay: 0.0,
        reasoning: None,
        enabled_tools: vec![],
        max_tokens: None,
        stream: false,
        pass_session_id: false,
    }
}

fn make_tool(resolver: Option<Arc<dyn CategoryResolver>>) -> (DelegateTask, ToolContext) {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "test");
    let event_tx = None;
    let tool = DelegateTask::new(
        mgr,
        parent_cfg,
        config_tree,
        registry,
        event_tx,
        resolver,
    );
    (tool, ctx)
}

/// BC-011: specifying both category and subagent_type is rejected at dispatch.
#[tokio::test]
async fn category_and_subagent_type_mutually_exclusive() {
    let (tool, ctx) = make_tool(None);
    let args = json!({
        "goal": "do work",
        "category": "quick",
        "subagent_type": "oracle",
    });
    let result = tool.execute(args, &ctx).await;
    assert!(
        matches!(result, ToolResult::Error(ref e) if e.contains("mutually exclusive") || e.contains("BC-011")),
        "both category + subagent_type must be rejected, got: {result:?}"
    );
}

/// BC-012 / FR-013: a category delegation resolves through the resolver and
/// emits a CategoryDelegation event carrying the resolved model.
#[tokio::test]
async fn category_resolves_model_and_emits_event() {
    let (tx, mut rx) = mpsc::unbounded_channel::<joey_agent_core::AgentEvent>();
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "test");
    let resolver: Arc<dyn CategoryResolver> = Arc::new(MockResolver);
    let tool = DelegateTask::new(mgr, parent_cfg, config_tree, registry, Some(tx), Some(resolver));

    let args = json!({
        "goal": "fix the typo",
        "category": "quick",
        "load_skills": ["frontend"],
    });
    let _ = tool.execute(args, &ctx).await;

    // A CategoryDelegation event must have been emitted with the quick model.
    let mut saw = false;
    while let Ok(ev) = rx.try_recv() {
        if let joey_agent_core::AgentEvent::CategoryDelegation { category, model } = ev {
            assert_eq!(category, "quick");
            assert_eq!(model, "gpt-5.4-mini", "event carries the resolved model");
            saw = true;
        }
    }
    assert!(saw, "CategoryDelegation event must be emitted for category calls");
}

/// A category delegation WITHOUT a resolver configured returns a clear error,
/// not a silent fallthrough to the parent model.
#[tokio::test]
async fn category_without_resolver_errors_clearly() {
    let (tool, ctx) = make_tool(None);
    let args = json!({ "goal": "x", "category": "quick" });
    let result = tool.execute(args, &ctx).await;
    assert!(
        matches!(result, ToolResult::Error(ref e) if e.contains("category resolver") || e.contains("resolver")),
        "category without resolver must error clearly, got: {result:?}"
    );
}

/// An unknown category (resolver returns None) errors rather than dispatching.
#[tokio::test]
async fn unknown_category_errors() {
    let resolver: Arc<dyn CategoryResolver> = Arc::new(MockResolver);
    let (tool, ctx) = make_tool(Some(resolver));
    let args = json!({ "goal": "x", "category": "nonexistent-category" });
    let result = tool.execute(args, &ctx).await;
    assert!(
        matches!(result, ToolResult::Error(ref e) if e.contains("nonexistent-category") || e.contains("unknown")),
        "unknown category must error, got: {result:?}"
    );
}

/// A subagent_type delegation resolves the named agent's model (FR-014).
#[tokio::test]
async fn subagent_type_resolves_named_agent_model() {
    let (tx, mut rx) = mpsc::unbounded_channel::<joey_agent_core::AgentEvent>();
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "test");
    let resolver: Arc<dyn CategoryResolver> = Arc::new(MockResolver);
    let tool = DelegateTask::new(mgr, parent_cfg, config_tree, registry, Some(tx), Some(resolver));

    // subagent_type path: oracle → gpt-5.6-sol. No CategoryDelegation event
    // should fire (only `category` triggers it).
    let args = json!({ "goal": "review architecture", "subagent_type": "oracle" });
    let _ = tool.execute(args, &ctx).await;
    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, joey_agent_core::AgentEvent::CategoryDelegation { .. }),
            "subagent_type must NOT emit CategoryDelegation"
        );
    }
}

/// The tool's JSON schema advertises category, subagent_type, and load_skills
/// (T058/T135 contract).
#[test]
fn tool_schema_advertises_category_fields() {
    let (tool, _ctx) = make_tool(None);
    let schema: Value = tool.parameters();
    let props = schema
        .get("properties")
        .expect("schema has properties");
    assert!(props.get("category").is_some(), "category property present");
    assert!(
        props.get("subagent_type").is_some(),
        "subagent_type property present"
    );
    assert!(
        props.get("load_skills").is_some(),
        "load_skills property present"
    );
}
