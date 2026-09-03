//! T3: state foundation for feature indicators (orchestration / omo /
//! systems) on the TUI App — task graph ingestion, goal/retry/compression
//! counters, MCP/browser flags, and the cron-job display copy.

use joey_agent_core::AgentEvent;
use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    AcceptanceCriterion, RiskLevel, TaskGraph, TaskId, TaskNode, WorkerRole,
};
use joey_tui::AppState;

fn node(id: &str, objective: &str) -> TaskNode {
    TaskNode {
        id: TaskId::new(id).expect("valid id"),
        objective: objective.to_string(),
        dependencies: Vec::new(),
        read_set: Vec::new(),
        write_set: Vec::new(),
        artifact_ids: Vec::new(),
        role: WorkerRole::Implementor,
        model_tier: joey_orchestration::task_graph::ModelTier::Frontier,
        risk: RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: format!("{} done", objective),
            kind: "manual".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: Default::default(),
        status: Default::default(),
        attempts: 0,
    }
}

fn graph_with(count: usize) -> TaskGraph {
    let mut g = TaskGraph::default();
    for i in 0..count {
        let id = format!("task-{}", i);
        let n = node(&id, &format!("objective {}", i));
        g.nodes.insert(n.id.clone(), n);
    }
    g
}

#[test]
fn set_task_graph_accepts_valid_graph() {
    let mut app = AppState::new("s1", "m");
    assert!(app.task_graph.is_none());
    let value = serde_json::to_value(graph_with(3)).unwrap();
    app.set_task_graph(value);
    let graph = app.task_graph.as_ref().expect("graph set");
    assert_eq!(graph.nodes.len(), 3);
    assert!(app.task_graph_updated_at.is_some());
}

#[test]
fn set_task_graph_garbage_keeps_previous() {
    let mut app = AppState::new("s1", "m");
    app.set_task_graph(serde_json::to_value(graph_with(2)).unwrap());
    assert!(app.task_graph.is_some());
    app.set_task_graph(serde_json::Value::String("garbage".into()));
    let graph = app.task_graph.as_ref().expect("previous graph kept");
    assert_eq!(graph.nodes.len(), 2, "bad payload must not blank the graph");
}

#[test]
fn clear_task_graph_resets() {
    let mut app = AppState::new("s1", "m");
    app.set_task_graph(serde_json::to_value(graph_with(1)).unwrap());
    app.clear_task_graph();
    assert!(app.task_graph.is_none());
    assert!(app.task_graph_updated_at.is_none());
}

#[test]
fn goal_set_and_clear_track_state() {
    let mut app = AppState::new("s1", "m");
    assert!(app.omo_goal.is_none() && app.goal_set_at.is_none());
    app.apply(AgentEvent::GoalSet { objective: "ship it".into() });
    assert_eq!(app.omo_goal.as_deref(), Some("ship it"));
    assert!(app.goal_set_at.is_some());
    app.apply(AgentEvent::GoalCleared);
    assert!(app.omo_goal.is_none());
    assert!(app.goal_set_at.is_none());
}

#[test]
fn retries_count_and_reset_on_turn_start() {
    let mut app = AppState::new("s1", "m");
    for i in 1..=2 {
        app.apply(AgentEvent::RetryAttempt {
            attempt: i,
            max_retries: 3,
            error: format!("boom {}", i),
            wait_secs: 1.0,
        });
    }
    assert_eq!(app.retries_this_turn, 2);
    assert_eq!(app.last_retry_error.as_deref(), Some("boom 2"));
    app.apply(AgentEvent::TurnStart { max_iterations: 8 });
    assert_eq!(app.retries_this_turn, 0);
    assert!(app.last_retry_error.is_none());
}

#[test]
fn compression_end_counts() {
    let mut app = AppState::new("s1", "m");
    assert_eq!(app.compression_count, 0);
    app.apply(AgentEvent::CompressionEnd { original_msgs: 40, new_msgs: 6 });
    assert_eq!(app.compression_count, 1);
    assert!(app.last_compression_at.is_some());
}

#[test]
fn mcp_and_browser_flags() {
    let mut app = AppState::new("s1", "m");
    assert!(app.mcp_servers.is_empty());
    assert!(!app.browser_connected);
    app.set_mcp_servers(vec!["fs".into(), "gh".into()]);
    assert_eq!(app.mcp_servers, vec!["fs".to_string(), "gh".to_string()]);
    app.set_browser_connected(true);
    assert!(app.browser_connected);
    app.set_browser_connected(false);
    assert!(!app.browser_connected);
}

#[test]
fn refresh_cron_jobs_never_panics() {
    let mut app = AppState::new("s1", "m");
    app.refresh_cron_jobs();
    // Whatever the machine's ~/.joey/cron/jobs.json holds (possibly nothing),
    // the call must succeed and leave a usable Vec.
    let _ = &app.cron_jobs;
}
