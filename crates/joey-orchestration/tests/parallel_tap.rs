//! Integration tests: parallel-subagent feature.
//!
//! 1. The event tap receives lifecycle + wrapped child events.
//! 2. Batch dispatch spawns children as one wave (wall-clock parallelism —
//!    verified via the shared-manager structure; real inference timing is
//!    environment-dependent, so the structural guarantee is what's pinned).
//! 3. Child ids are unique + monotonic per manager.

use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_orchestration::{ManagerConfig, SubagentManager, TaskSpec};
use joey_tools::ToolRegistry;
use tokio::sync::mpsc;

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

fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn tap_receives_lifecycle_and_wrapped_child_events() {
    let mgr = SubagentManager::new(ManagerConfig::default());
    let (tap_tx, mut tap_rx) = mpsc::unbounded_channel();
    mgr.set_event_tap(Some(tap_tx));

    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();

    let tasks: Vec<TaskSpec> = (0..2)
        .map(|i| TaskSpec {
            goal: format!("Tap task {}", i),
            context: None,
            model: None,
            toolsets: vec![],
            role: None,
            background: false,
            budgets: None,
        })
        .collect();

    // No API key → children fail fast, but lifecycle events still flow.
    let _ = mgr
        .dispatch_batch(&tasks, None, &[], &parent_cfg, &config_tree, &registry, None)
        .await;

    let events = drain(&mut tap_rx);

    // Lifecycle events on the tap.
    let spawns: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::SubagentSpawn { .. }))
        .collect();
    assert_eq!(spawns.len(), 2, "tap saw 2 spawns, got {}", spawns.len());
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::DelegationBatchComplete { .. })),
        "tap saw the batch completion"
    );

    // Wrapped child events carry the matching ids.
    let spawn_ids: Vec<u64> = spawns
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SubagentSpawn { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    let wrapped_ids: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SubagentEvent { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert!(
        wrapped_ids.iter().all(|id| spawn_ids.contains(id)),
        "every wrapped event's id matches a spawned child: spawn={:?} wrapped={:?}",
        spawn_ids,
        wrapped_ids
    );
}

#[tokio::test]
async fn ids_are_unique_and_monotonic() {
    let mgr = SubagentManager::new(ManagerConfig::default());
    let (tap_tx, mut tap_rx) = mpsc::unbounded_channel();
    mgr.set_event_tap(Some(tap_tx));

    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();

    let tasks: Vec<TaskSpec> = (0..4)
        .map(|i| TaskSpec {
            goal: format!("Id task {}", i),
            context: None,
            model: None,
            toolsets: vec![],
            role: None,
            background: false,
            budgets: None,
        })
        .collect();
    let _ = mgr
        .dispatch_batch(&tasks, None, &[], &parent_cfg, &config_tree, &registry, None)
        .await;

    let ids: Vec<u64> = drain(&mut tap_rx)
        .into_iter()
        .filter_map(|e| match e {
            AgentEvent::SubagentSpawn { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 4);
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "ids unique: {:?}", ids);
    assert_eq!(ids, sorted, "ids monotonic in spawn order: {:?}", ids);
}

#[tokio::test]
async fn child_ids_disjoint_across_concurrent_managers() {
    // T033 regression: the /hypercode surface (engine.rs) builds a SEPARATE
    // SubagentManager while the agent's delegate_task manager stays alive.
    // Pre-fix, each manager minted ids from its own counter starting at 1,
    // so a hypercode child id could equal a surviving delegate pane's id —
    // and the TUI routes wrapped events by FIRST-MATCH on child id, so
    // events cross-contaminated panes. Post-fix both draw from ONE
    // process-global counter, so two concurrently-alive managers can never
    // mint the same id. Pinned here at the orchestration layer: run two
    // managers CONCURRENTLY (tokio::join!) and prove their spawn-id sets
    // are disjoint through their taps.
    let mgr_a = SubagentManager::new(ManagerConfig::default());
    let mgr_b = SubagentManager::new(ManagerConfig::default());
    let (tap_a, mut rx_a) = mpsc::unbounded_channel();
    let (tap_b, mut rx_b) = mpsc::unbounded_channel();
    mgr_a.set_event_tap(Some(tap_a));
    mgr_b.set_event_tap(Some(tap_b));

    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();
    let tasks: Vec<TaskSpec> = (0..2)
        .map(|i| TaskSpec {
            goal: format!("T033 task {}", i),
            context: None,
            model: None,
            toolsets: vec![],
            role: None,
            background: false,
            budgets: None,
        })
        .collect();

    // No API key → children fail fast, but spawn lifecycle events still flow.
    let (a, b) = tokio::join!(
        mgr_a.dispatch_batch(&tasks, None, &[], &parent_cfg, &config_tree, &registry, None),
        mgr_b.dispatch_batch(&tasks, None, &[], &parent_cfg, &config_tree, &registry, None),
    );
    let _ = (a, b);

    let spawn_ids = |rx: &mut mpsc::UnboundedReceiver<AgentEvent>| {
        drain(rx)
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::SubagentSpawn { id, .. } => Some(id),
                _ => None,
            })
            .collect::<Vec<u64>>()
    };
    let ids_a = spawn_ids(&mut rx_a);
    let ids_b = spawn_ids(&mut rx_b);
    assert_eq!(ids_a.len(), 2, "manager A spawned 2, got {:?}", ids_a);
    assert_eq!(ids_b.len(), 2, "manager B spawned 2, got {:?}", ids_b);

    // Per-manager invariants preserved from the per-instance counter era.
    for (label, ids) in [("A", &ids_a), ("B", &ids_b)] {
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "manager {label} ids unique: {:?}", ids);
        assert_eq!(&sorted, ids, "manager {label} ids monotonic in spawn order: {:?}", ids);
        assert!(ids.iter().all(|id| *id >= 1), "manager {label} ids start at 1: {:?}", ids);
    }

    // THE fix: no id is shared between the two concurrently-alive managers.
    let overlap: Vec<u64> = ids_a.iter().filter(|id| ids_b.contains(id)).cloned().collect();
    assert!(
        overlap.is_empty(),
        "T033: concurrent managers must mint disjoint child ids, shared: {:?} (A={:?} B={:?})",
        overlap,
        ids_a,
        ids_b
    );
}

#[tokio::test]
async fn removing_tap_stops_events() {
    let mgr = SubagentManager::new(ManagerConfig::default());
    let (tap_tx, mut tap_rx) = mpsc::unbounded_channel();
    mgr.set_event_tap(Some(tap_tx));
    mgr.set_event_tap(None);

    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();
    let _ = mgr
        .dispatch_single(
            &joey_orchestration::DelegationRequest::single("no tap"),
            &parent_cfg,
            &config_tree,
            &registry,
            None,
        )
        .await;
    assert!(drain(&mut tap_rx).is_empty(), "tap removed → no events");
}

#[tokio::test]
async fn global_tap_used_when_local_unset() {
    let (gtx, mut grx) = mpsc::unbounded_channel();
    joey_orchestration::tap::set_global_tap(Some(gtx));
    let mgr = SubagentManager::new(ManagerConfig::default());

    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();
    let _ = mgr
        .dispatch_single(
            &joey_orchestration::DelegationRequest::single("global tap"),
            &parent_cfg,
            &config_tree,
            &registry,
            None,
        )
        .await;
    assert!(
        drain(&mut grx)
            .iter()
            .any(|e| matches!(e, AgentEvent::SubagentSpawn { .. })),
        "global tap received the spawn"
    );
    joey_orchestration::tap::set_global_tap(None);
}

#[test]
fn capacity_config_auto_sizing() {
    // max_concurrent_children = 0 (or absent) → capacity-driven ≥ FLOOR.
    let cfg = Config::defaults();
    let c = ManagerConfig::from_config(&cfg);
    assert!(
        c.max_concurrent_children >= joey_orchestration::capacity::FLOOR_CHILDREN,
        "auto sizing picks at least the floor, got {}",
        c.max_concurrent_children
    );
    assert!(
        c.max_concurrent_requests >= c.max_concurrent_children,
        "requests semaphore ≥ children"
    );
}

#[test]
fn capacity_config_explicit_override() {
    // An explicit positive value wins over detection.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, "delegation:\n  max_concurrent_children: 7\n").unwrap();
    let cfg = joey_core::Config::load_from(path).unwrap();
    let c = ManagerConfig::from_config(&cfg);
    assert_eq!(c.max_concurrent_children, 7);
}
