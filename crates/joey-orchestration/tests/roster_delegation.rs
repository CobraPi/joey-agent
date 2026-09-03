//! Feature 025 (User Story 2, T009–T012): the full OMO roster is callable
//! via `subagent_type`.
//!
//! Verifies the DelegateTask/CallOmoAgent tools:
//!   - Accept ALL eleven roster names through subagent_type resolution.
//!   - The unknown-type error lists every valid OMO agent name (T010).
//!   - call_omo_agent's schema enum carries the full roster (T009).
//!   - delegate_task's subagent_type description mentions the roster (T009).
//!   - load_skills does not break named-agent resolution (T011).
//!   - category + subagent_type remain mutually exclusive (BC-011).
//!
//! The full subagent dispatch requires a live provider, so these tests
//! exercise the resolution/validation layer — a successful resolution reaches
//! dispatch_single, which fails without credentials (same contract as
//! tests/category_delegation.rs).

use std::sync::Arc;

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::{
    CallOmoAgent, CategoryResolver, DelegateTask, ManagerConfig, ResolvedDelegation,
    SubagentManager,
};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::sync::mpsc;

/// The full 11-name OMO roster (feature 025, FR-003/FR-010), in the
/// canonical OMO_ROSTER order.
const ROSTER: [&str; 11] = [
    "sisyphus",
    "hephaestus",
    "prometheus",
    "atlas",
    "oracle",
    "librarian",
    "explore",
    "multimodal-looker",
    "metis",
    "momus",
    "sisyphus-junior",
];

/// A mock resolver that maps ALL eleven roster names to distinct models,
/// letting us assert resolution succeeds for every roster entry without a
/// real provider catalog.
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

/// A mock resolver that resolves nothing — for unknown-name error tests.
struct NullResolver;

impl CategoryResolver for NullResolver {
    fn resolve_category(&self, _name: &str) -> Option<ResolvedDelegation> {
        None
    }
    fn resolve_subagent_type(&self, _name: &str) -> Option<ResolvedDelegation> {
        None
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

/// T012-1 / FR-003: every one of the eleven roster names resolves through
/// subagent_type (the subsequent dispatch failure without credentials is
/// expected — same as category_delegation.rs).
#[tokio::test]
async fn all_eleven_roster_names_are_accepted() {
    let resolver: Arc<dyn CategoryResolver> = Arc::new(AllRosterResolver);
    let (tool, ctx) = make_tool(Some(resolver));
    for name in ROSTER {
        let args = json!({ "goal": "trivial", "subagent_type": name });
        let result = tool.execute(args, &ctx).await;
        assert!(
            !matches!(result, ToolResult::Error(ref e) if e.contains("unknown or unavailable")),
            "roster name '{name}' must resolve, got: {result:?}"
        );
        assert!(
            !matches!(result, ToolResult::Error(ref e) if e.contains("requires an OMO category resolver")),
            "roster name '{name}' must not hit the resolver-missing error, got: {result:?}"
        );
    }
}

/// T012-2 / T010: the unknown-type error names the bogus agent and lists
/// every valid OMO agent name.
#[tokio::test]
async fn unknown_name_error_lists_valid_names() {
    let resolver: Arc<dyn CategoryResolver> = Arc::new(NullResolver);
    let (tool, ctx) = make_tool(Some(resolver));
    let args = json!({ "goal": "trivial", "subagent_type": "bogus-agent" });
    let result = tool.execute(args, &ctx).await;
    match result {
        ToolResult::Error(ref e) => {
            assert!(e.contains("bogus-agent"), "error names the bad agent: {e}");
            assert!(
                e.contains("Valid OMO agent names:"),
                "error lists valid names: {e}"
            );
            for name in ROSTER {
                assert!(e.contains(name), "error must list '{name}': {e}");
            }
        }
        other => panic!("expected error, got: {other:?}"),
    }
}

/// T012-3 / T009: call_omo_agent's schema enum is exactly the 11-name roster
/// in canonical order.
#[test]
fn call_omo_agent_schema_enum_lists_full_roster() {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let tool = CallOmoAgent::new(
        mgr,
        make_agent_config(),
        Config::defaults(),
        ToolRegistry::new(),
        None,
        None,
    );
    let schema: Value = tool.parameters();
    let enum_arr = schema["properties"]["subagent_type"]["enum"]
        .as_array()
        .expect("subagent_type enum present");
    let got: Vec<&str> = enum_arr
        .iter()
        .map(|v| v.as_str().expect("enum entries are strings"))
        .collect();
    assert_eq!(got, ROSTER.to_vec(), "enum must equal the full roster in order");
}

/// T012-4 / T009: delegate_task's subagent_type description advertises the
/// roster and the mutual exclusivity.
#[test]
fn delegate_task_subagent_type_description_mentions_roster() {
    let (tool, _ctx) = make_tool(None);
    let schema: Value = tool.parameters();
    let desc = schema["properties"]["subagent_type"]["description"]
        .as_str()
        .expect("subagent_type description present");
    assert!(desc.contains("sisyphus"), "description mentions sisyphus: {desc}");
    assert!(
        desc.contains("multimodal-looker"),
        "description mentions multimodal-looker: {desc}"
    );
    assert!(
        desc.contains("Mutually exclusive"),
        "description mentions mutual exclusivity: {desc}"
    );
}

/// T012-5 / T011 / FR-004: load_skills on the named-agent path does not
/// break resolution (the skills synthesis happens after a successful
/// resolve). The unit-level directive assertion lives inline in
/// delegation_tool.rs (named_agent_skill_directive is pub(crate)).
#[tokio::test]
async fn load_skills_directive_on_named_path() {
    let resolver: Arc<dyn CategoryResolver> = Arc::new(AllRosterResolver);
    let (tool, ctx) = make_tool(Some(resolver));
    let args = json!({
        "goal": "trivial",
        "subagent_type": "oracle",
        "load_skills": ["ulw-plan"],
    });
    let result = tool.execute(args, &ctx).await;
    assert!(
        !matches!(result, ToolResult::Error(ref e) if e.contains("unknown or unavailable")),
        "load_skills must not break named resolution, got: {result:?}"
    );
}

/// T012-6 / BC-011 regression: category + subagent_type still rejected.
#[tokio::test]
async fn category_and_subagent_type_still_mutually_exclusive_bc011() {
    let resolver: Arc<dyn CategoryResolver> = Arc::new(AllRosterResolver);
    let (tool, ctx) = make_tool(Some(resolver));
    let args = json!({
        "goal": "do work",
        "category": "quick",
        "subagent_type": "oracle",
    });
    let result = tool.execute(args, &ctx).await;
    assert!(
        matches!(result, ToolResult::Error(ref e) if e.contains("mutually exclusive")),
        "both category + subagent_type must be rejected, got: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Feature 025, T014/T016: OMO-chain role model defaults + FR-006 warning.
// ---------------------------------------------------------------------------

/// Drain an AgentEvent channel for up to ~2s, collecting every event. The
/// role-arm notice fires synchronously inside execute(); the bound just
/// tolerates a subsequent dispatch error's events arriving slightly later.
async fn drain_events(rx: &mut mpsc::UnboundedReceiver<joey_agent_core::AgentEvent>) -> Vec<joey_agent_core::AgentEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut events = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(ev) => events.push(ev),
            Err(mpsc::error::TryRecvError::Empty) => {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
    events
}

/// Build a DelegateTask with an explicit config tree, NullResolver, and a
/// real event channel (mirrors category_delegation.rs's channel setup).
fn make_tool_with_channel(
    config_tree: Config,
) -> (
    DelegateTask,
    ToolContext,
    mpsc::UnboundedReceiver<joey_agent_core::AgentEvent>,
) {
    let (tx, rx) = mpsc::unbounded_channel::<joey_agent_core::AgentEvent>();
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let parent_cfg = make_agent_config();
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "test");
    let resolver: Arc<dyn CategoryResolver> = Arc::new(NullResolver);
    let tool = DelegateTask::new(mgr, parent_cfg, config_tree, registry, Some(tx), Some(resolver));
    (tool, ctx, rx)
}

/// T016-a / FR-006: a role delegation whose OMO chain cannot resolve (no
/// configured role model, NullResolver) emits a Notice warning that the
/// default model is inherited. A subsequent dispatch error is fine — the
/// resolution-side notice fires first. (OMO specialists toggle pinned OFF:
/// this test pins the legacy chain-default path byte-identically.)
#[tokio::test]
async fn role_chain_unresolvable_emits_notice() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tmp.path(),
        "hypercode:\n  omo_specialists:\n    enabled: false\n",
    )
    .unwrap();
    let tree = Config::load_from(tmp.path().to_path_buf()).unwrap();
    let (tool, ctx, mut rx) = make_tool_with_channel(tree);
    let args = json!({ "goal": "g", "role": "explorer" });
    let _ = tool.execute(args, &ctx).await;
    let events = drain_events(&mut rx).await;
    let saw = events.iter().any(|ev| match ev {
        joey_agent_core::AgentEvent::Notice(n) =>
            n.contains("no OMO chain member") && n.contains("FR-006"),
        _ => false,
    });
    assert!(saw, "expected an FR-006 chain-warning Notice, got: {events:?}");
}

/// T016-b / FR-005 precedence 1: an explicit hypercode.<role>.<provider>.model
/// fills the model gap, so no chain warning fires even with a NullResolver.
#[tokio::test]
async fn role_configured_model_suppresses_chain_warning() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tmp.path(),
        "hypercode:\n  explorer:\n    openrouter:\n      model: configured-explorer-model\n",
    )
    .unwrap();
    let tree = Config::load_from(tmp.path().to_path_buf()).unwrap();
    let (tool, ctx, mut rx) = make_tool_with_channel(tree);
    let args = json!({ "goal": "g", "role": "explorer" });
    let _ = tool.execute(args, &ctx).await;
    let events = drain_events(&mut rx).await;
    let saw = events.iter().any(|ev| match ev {
        joey_agent_core::AgentEvent::Notice(n) => n.contains("no OMO chain member"),
        _ => false,
    });
    assert!(!saw, "configured model must suppress the chain warning, got: {events:?}");
}

/// T016-c / FR-005 precedence 0: an explicit `model` arg wins before the role
/// gap-fill, so no chain warning fires.
#[tokio::test]
async fn explicit_model_arg_suppresses_chain_warning() {
    let (tool, ctx, mut rx) = make_tool_with_channel(Config::defaults());
    let args = json!({ "goal": "g", "role": "implementor", "model": "explicit-model" });
    let _ = tool.execute(args, &ctx).await;
    let events = drain_events(&mut rx).await;
    let saw = events.iter().any(|ev| match ev {
        joey_agent_core::AgentEvent::Notice(n) => n.contains("no OMO chain member"),
        _ => false,
    });
    assert!(!saw, "explicit model arg must suppress the chain warning, got: {events:?}");
}

/// T016-d / BC guard: combining `role` with `category` is unaffected by the
/// chain logic — the result must not be a role/chain error (no chain warning
/// Notice; no Error mentioning "role" as the failure cause).
#[tokio::test]
async fn category_mutual_exclusivity_unchanged_with_role() {
    let (tool, ctx, mut rx) = make_tool_with_channel(Config::defaults());
    let args = json!({ "goal": "g", "role": "explorer", "category": "quick" });
    let result = tool.execute(args, &ctx).await;
    let events = drain_events(&mut rx).await;
    let saw_chain_warning = events.iter().any(|ev| match ev {
        joey_agent_core::AgentEvent::Notice(n) => n.contains("no OMO chain member"),
        _ => false,
    });
    assert!(
        !saw_chain_warning,
        "role+category must not produce a chain warning, got: {events:?}"
    );
    if let ToolResult::Error(ref e) = result {
        assert!(
            !e.contains("role"),
            "must not be a role/chain error, got: {e}"
        );
    }
}

/// T030 / FR-004: role enrichment composes with named routing — the named
/// path's resolved model fills the model gap BEFORE the role chain applies
/// (explicit values win), so the delegation must resolve and NO FR-006
/// "no OMO chain member" warning may fire (the AllRosterResolver serves
/// every roster name, so the chain never hits precedence 3).
#[tokio::test]
async fn role_enrichment_composes_with_named_routing() {
    let (tx, mut rx) = mpsc::unbounded_channel::<joey_agent_core::AgentEvent>();
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let parent_cfg = make_agent_config();
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "test");
    let resolver: Arc<dyn CategoryResolver> = Arc::new(AllRosterResolver);
    let tool = DelegateTask::new(mgr, parent_cfg, Config::defaults(), registry, Some(tx), Some(resolver));
    let args = json!({
        "goal": "g",
        "subagent_type": "oracle",
        "role": "explorer",
        "load_skills": ["ulw-plan"],
    });
    let result = tool.execute(args, &ctx).await;
    assert!(
        !matches!(result, ToolResult::Error(ref e) if e.contains("unknown or unavailable")),
        "named + role + skills must resolve, got: {result:?}"
    );
    let events = drain_events(&mut rx).await;
    let saw_chain_warning = events.iter().any(|ev| match ev {
        joey_agent_core::AgentEvent::Notice(n) => n.contains("no OMO chain member"),
        _ => false,
    });
    assert!(
        !saw_chain_warning,
        "named routing fills the model gap; no FR-006 chain warning expected, got: {events:?}"
    );
}
