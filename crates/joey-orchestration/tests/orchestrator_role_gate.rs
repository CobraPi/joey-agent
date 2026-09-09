//! HyperCode orchestrator roles-only gate: when the process-global flag is
//! set (main orchestrator session active), delegate_task must ONLY accept
//! role:"explorer"/role:"implementor" routing — named-agent (subagent_type),
//! category, and load_skills are rejected at the top level AND per-task in
//! batch tasks[], and the tool schema omits those parameters. Flag off must
//! keep behavior byte-identical (public-surface regression tests, repo
//! constitution).
//!
//! NOTE: the flag is process-global and tests in one binary run in
//! parallel — every test here sets the flag EXPLICITLY at start and resets
//! it to false at the end (guard pattern), so cross-test contamination is
//! bounded to the assertion window.

use std::sync::Arc;

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::{
    set_orchestrator_roles_only, CategoryResolver, DelegateTask, ManagerConfig, ResolvedDelegation,
    SubagentManager,
};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};

/// The flag is process-global and tests in one binary run in parallel —
/// every test here sets it explicitly and resets it at the end. To make
/// those windows race-free, ALL tests in this binary serialize on this
/// mutex for their entire body (a reset from a finishing test must never
/// land inside another test's flag-on assertion window).
fn gate_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

/// Mock resolver mirroring roster_delegation.rs's AllRosterResolver (maps
/// every OMO roster name so named routing resolves without a provider
/// catalog — flag-off parity assertion).
struct AllRosterResolver;

impl CategoryResolver for AllRosterResolver {
    fn resolve_category(&self, _name: &str) -> Option<ResolvedDelegation> {
        None
    }
    fn resolve_subagent_type(&self, name: &str) -> Option<ResolvedDelegation> {
        match name {
            "sisyphus" | "hephaestus" | "prometheus" | "atlas" | "oracle" | "librarian"
            | "explore" | "multimodal-looker" | "metis" | "momus" | "sisyphus-junior" => {
                Some(ResolvedDelegation {
                    model: format!("model-for-{name}"),
                    prompt_append: None,
                })
            }
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
        model_pinned: false,
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

/// 1. Flag on: the schema omits subagent_type/category/load_skills at the
/// top level and subagent_type in tasks[] items; role stays in BOTH places.
#[test]
fn schema_omits_named_routing_when_restricted() {
    let _guard = gate_lock().lock().unwrap_or_else(|p| p.into_inner());
    set_orchestrator_roles_only(true);
    let (tool, _ctx) = make_tool(None);
    let schema: Value = tool.parameters();
    let props = &schema["properties"];
    assert!(
        props.get("subagent_type").is_none(),
        "restricted schema must omit subagent_type"
    );
    assert!(
        props.get("category").is_none(),
        "restricted schema must omit category"
    );
    assert!(
        props.get("load_skills").is_none(),
        "restricted schema must omit load_skills"
    );
    assert!(
        props.get("role").is_some(),
        "restricted schema must keep role (top level)"
    );
    let items = &props["tasks"]["items"]["properties"];
    assert!(
        items.get("subagent_type").is_none(),
        "restricted tasks[] items must omit subagent_type"
    );
    assert!(
        items.get("role").is_some(),
        "restricted tasks[] items must keep role"
    );
    set_orchestrator_roles_only(false);
}

/// 2. Flag on: single-mode subagent_type is rejected with the restriction
/// error naming the mode and role routing.
#[tokio::test]
async fn single_mode_rejects_subagent_type() {
    let _guard = gate_lock().lock().unwrap_or_else(|p| p.into_inner());
    set_orchestrator_roles_only(true);
    let resolver: Arc<dyn CategoryResolver> = Arc::new(AllRosterResolver);
    let (tool, ctx) = make_tool(Some(resolver));
    let args = json!({ "goal": "trivial", "subagent_type": "oracle" });
    let result = tool.execute(args, &ctx).await;
    match result {
        ToolResult::Error(ref e) => {
            assert!(
                e.contains("not available in HyperCode orchestrator mode"),
                "error mentions orchestrator mode: {e}"
            );
            assert!(
                e.contains("role:'explorer'"),
                "error names role routing: {e}"
            );
        }
        other => panic!("expected error, got: {other:?}"),
    }
    set_orchestrator_roles_only(false);
}

/// 3. Flag on: single-mode category and load_skills are each rejected.
#[tokio::test]
async fn single_mode_rejects_category_and_load_skills() {
    let _guard = gate_lock().lock().unwrap_or_else(|p| p.into_inner());
    set_orchestrator_roles_only(true);
    let resolver: Arc<dyn CategoryResolver> = Arc::new(AllRosterResolver);
    let (tool, ctx) = make_tool(Some(resolver.clone()));
    let result = tool
        .execute(json!({ "goal": "trivial", "category": "quick" }), &ctx)
        .await;
    match result {
        ToolResult::Error(ref e) => assert!(
            e.contains("not available in HyperCode orchestrator mode"),
            "category rejected with mode error: {e}"
        ),
        other => panic!("expected error for category, got: {other:?}"),
    }
    let result = tool
        .execute(json!({ "goal": "trivial", "load_skills": ["x"] }), &ctx)
        .await;
    match result {
        ToolResult::Error(ref e) => assert!(
            e.contains("not available in HyperCode orchestrator mode"),
            "load_skills rejected with mode error: {e}"
        ),
        other => panic!("expected error for load_skills, got: {other:?}"),
    }
    set_orchestrator_roles_only(false);
}

/// 4. Flag on: per-task subagent_type inside tasks[] is rejected, naming
/// the field and tasks[] — before any dispatch.
#[tokio::test]
async fn batch_rejects_per_task_subagent_type() {
    let _guard = gate_lock().lock().unwrap_or_else(|p| p.into_inner());
    set_orchestrator_roles_only(true);
    let resolver: Arc<dyn CategoryResolver> = Arc::new(AllRosterResolver);
    let (tool, ctx) = make_tool(Some(resolver));
    let args = json!({ "tasks": [ { "goal": "g1", "subagent_type": "momus" } ] });
    let result = tool.execute(args, &ctx).await;
    match result {
        ToolResult::Error(ref e) => {
            assert!(e.contains("subagent_type"), "error names subagent_type: {e}");
            assert!(e.contains("tasks[]"), "error names tasks[]: {e}");
            assert!(
                e.contains("not available in HyperCode orchestrator mode"),
                "error mentions orchestrator mode: {e}"
            );
        }
        other => panic!("expected error, got: {other:?}"),
    }
    set_orchestrator_roles_only(false);
}

/// 5. Flag on: role dispatch itself is unaffected — a role:"explorer"
/// single-mode call must NOT produce the restriction error (it proceeds to
/// normal resolution/dispatch; a backend failure without credentials is
/// acceptable, mirroring roster_delegation.rs's role tests).
#[tokio::test]
async fn role_dispatch_unaffected_when_restricted() {
    let _guard = gate_lock().lock().unwrap_or_else(|p| p.into_inner());
    set_orchestrator_roles_only(true);
    let (tool, ctx) = make_tool(None);
    let args = json!({ "goal": "say ok", "role": "explorer", "toolsets": ["file-read"], "max_turns": 1 });
    let result = tool.execute(args, &ctx).await;
    assert!(
        !matches!(result, ToolResult::Error(ref e) if e.contains("orchestrator mode")),
        "role routing must not hit the restriction error, got: {result:?}"
    );
    set_orchestrator_roles_only(false);
}

/// 6. Flag off: the full schema (subagent_type et al.) is advertised —
/// flag-off parity (dispatch assertion optional; schema suffices).
#[test]
fn flag_off_single_mode_still_accepts_subagent_type() {
    let _guard = gate_lock().lock().unwrap_or_else(|p| p.into_inner());
    set_orchestrator_roles_only(false);
    let (tool, _ctx) = make_tool(None);
    let schema: Value = tool.parameters();
    let props = &schema["properties"];
    assert!(props.get("subagent_type").is_some(), "flag off keeps subagent_type");
    assert!(props.get("category").is_some(), "flag off keeps category");
    assert!(props.get("load_skills").is_some(), "flag off keeps load_skills");
    assert!(
        props["tasks"]["items"]["properties"].get("subagent_type").is_some(),
        "flag off keeps per-task subagent_type"
    );
    set_orchestrator_roles_only(false);
}

/// 6b. Flag on: call_omo_agent's execute returns the restriction error
/// (defense-in-depth — CallOmoAgent::new is public-constructible, so the
/// harness mirrors roster_delegation.rs's call_omo_agent test).
#[tokio::test]
async fn call_omo_agent_rejected_when_restricted() {
    use joey_orchestration::CallOmoAgent;
    let _guard = gate_lock().lock().unwrap_or_else(|p| p.into_inner());
    set_orchestrator_roles_only(true);
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let tool = CallOmoAgent::new(
        mgr,
        make_agent_config(),
        Config::defaults(),
        ToolRegistry::new(),
        None,
        None,
    );
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "test");
    let args = json!({ "goal": "research", "subagent_type": "oracle" });
    let result = tool.execute(args, &ctx).await;
    match result {
        ToolResult::Error(ref e) => assert!(
            e.contains("not available in HyperCode orchestrator mode"),
            "call_omo_agent restriction error: {e}"
        ),
        other => panic!("expected restriction error, got: {other:?}"),
    }
    set_orchestrator_roles_only(false);
}
