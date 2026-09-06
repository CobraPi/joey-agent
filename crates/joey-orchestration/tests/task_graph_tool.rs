//! Integration tests for the `task_graph` tool (plan/update/status).
//!
//! Fixture: the strict planner-JSON contract example (quickstart §3),
//! reused verbatim from tests/task_graph_validation.rs. Manager/tap
//! construction mirrors tests/parallel_tap.rs; the ToolContext helper
//! mirrors tests/notices.rs.

use joey_agent_core::AgentEvent;
use joey_core::Config;
use joey_orchestration::tap::set_global_tap;
use joey_orchestration::{ManagerConfig, SubagentManager, TaskGraphTool};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

/// The strict planner-JSON contract example (quickstart §3): format tag,
/// baseline, single `task-auth` node with a scoped verification step.
const PLAN: &str = r#"{"format":"joey-taskgraph/1","baseline_revision":"abc123","tasks":[{"id":"task-auth","objective":"Implement token refresh","dependencies":[],"read_set":[],"write_set":["src/auth.rs"],"artifact_ids":[42,1337],"role":"implementor","model_tier":"economical","risk":"medium","acceptance":[{"criterion":"cargo test -p joey-core auth","kind":"command"}],"verification":{"steps":[{"name":"scoped-tests","command":"cargo test -p joey-core auth","parse":"plain","timeout_sec":300,"required":true}],"risk_triggered_review":false},"isolation":"isolated_worktree"}]}"#;

fn make_ctx() -> ToolContext {
    ToolContext::new(std::env::temp_dir(), Config::defaults(), "task-graph-tool-test")
}

fn make_tool() -> Arc<TaskGraphTool> {
    Arc::new(TaskGraphTool::new(Arc::new(SubagentManager::new(
        ManagerConfig::default(),
    ))))
}

fn plan_args() -> Value {
    json!({
        "action": "plan",
        "graph": serde_json::from_str::<Value>(PLAN).expect("fixture is valid JSON"),
    })
}

async fn plan(tool: &TaskGraphTool, ctx: &ToolContext) -> ToolResult {
    tool.execute(plan_args(), ctx).await
}

// (a) plan accepts the contract example and emits TaskGraphPublished
// through the process-global tap (manager-local tap unset ⇒ fallback).

#[tokio::test]
async fn plan_accepts_valid_document_and_emits_event() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    set_global_tap(Some(tx));

    let tool = make_tool();
    let ctx = make_ctx();
    let result = plan(&tool, &ctx).await;
    assert!(
        matches!(&result, ToolResult::Text(t) if t.contains("plan accepted")),
        "expected Text summary, got {:?}",
        result
    );

    // Drain until a TaskGraphPublished with a non-empty node map arrives.
    let mut found = None;
    for _ in 0..64 {
        match rx.try_recv() {
            Ok(AgentEvent::TaskGraphPublished { graph }) => {
                if graph["nodes"].as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    found = Some(graph);
                    break;
                }
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    set_global_tap(None);
    let graph = found.expect("tap must receive TaskGraphPublished with non-empty nodes");
    assert!(graph["nodes"]["task-auth"].is_object(), "graph: {graph}");
}

// (b) plan rejects an invalid document with the rejection detail.

#[tokio::test]
async fn plan_rejects_invalid_document() {
    let tool = make_tool();
    let ctx = make_ctx();
    let result = tool
        .execute(
            json!({"action": "plan", "graph": {}}),
            &ctx,
        )
        .await;
    match result {
        ToolResult::Error(e) => {
            assert!(e.contains("graph rejected"), "error: {e}");
            assert!(e.contains("unsupported_format"), "error: {e}");
        }
        other => panic!("expected Error, got {:?}", other),
    }
}

// (c) update applies a legal transition and rejects an illegal one.

#[tokio::test]
async fn update_applies_legal_transition_and_rejects_illegal() {
    let tool = make_tool();
    let ctx = make_ctx();
    assert!(matches!(plan(&tool, &ctx).await, ToolResult::Text(_)));

    // pending -> ready is legal.
    let legal = tool
        .execute(
            json!({"action": "update", "transitions": [{"id": "task-auth", "status": "ready"}]}),
            &ctx,
        )
        .await;
    match legal {
        ToolResult::Text(t) => assert!(t.contains("applied 1"), "text: {t}"),
        other => panic!("expected Text, got {:?}", other),
    }

    // ready -> completed is ILLEGAL (legal edges from ready: dispatched,
    // skipped).
    let illegal = tool
        .execute(
            json!({"action": "update", "transitions": [{"id": "task-auth", "status": "completed"}]}),
            &ctx,
        )
        .await;
    match illegal {
        ToolResult::Error(e) => assert!(e.contains("illegal"), "error: {e}"),
        other => panic!("expected Error, got {:?}", other),
    }
}

// (d) update without a prior plan errors.

#[tokio::test]
async fn update_without_plan_errors() {
    let tool = make_tool();
    let ctx = make_ctx();
    let result = tool
        .execute(
            json!({"action": "update", "transitions": [{"id": "task-auth", "status": "ready"}]}),
            &ctx,
        )
        .await;
    match result {
        ToolResult::Error(e) => assert_eq!(e, "no graph published yet — call action=plan first"),
        other => panic!("expected Error, got {:?}", other),
    }
}

// (e) status renders the graph after plan + one transition.

#[tokio::test]
async fn status_renders_graph() {
    let tool = make_tool();
    let ctx = make_ctx();
    assert!(matches!(plan(&tool, &ctx).await, ToolResult::Text(_)));
    assert!(
        matches!(
            tool.execute(
                json!({"action": "update", "transitions": [{"id": "task-auth", "status": "ready"}]}),
                &ctx
            )
            .await,
            ToolResult::Text(_)
        )
    );

    let result = tool.execute(json!({"action": "status"}), &ctx).await;
    match result {
        ToolResult::Text(t) => {
            assert!(t.contains("task-auth"), "text: {t}");
            assert!(t.contains("ready"), "text: {t}");
            assert!(t.contains("Implement token refresh"), "text: {t}");
            assert!(t.contains("ready now:"), "text: {t}");
        }
        other => panic!("expected Text, got {:?}", other),
    }
}

// (f) the description documents every REQUIRED task field and the exact
// enum variants — regression guard against the pre-wire-spec description
// that omitted artifact_ids/verification and never stated variants.

#[test]
fn description_documents_required_fields_and_enums() {
    let tool = make_tool();
    let d = tool.description();
    for needle in [
        "artifact_ids",
        "verification",
        "\"explorer\" | \"implementor\" | \"orchestrator\"",
        "\"economical\" | \"frontier\"",
        "joey-taskgraph/1",
        "risk_triggered_review",
    ] {
        assert!(d.contains(needle), "description must mention {needle}");
    }
}

// (g) concurrent-caller safety: two concurrent action=update calls on
// DISJOINT nodes both apply, the final graph shows both transitions, and
// each applied call re-publishes one TaskGraphPublished snapshot.
// Uses the manager-LOCAL tap (set_event_tap) so no process-global tap
// state is touched — the drain pattern mirrors (a) otherwise.

/// Two independent no-dependency tasks (disjoint write paths — unrelated
/// tasks sharing a write path are a schema violation).
const PLAN_TWO_NODES: &str = r#"{"format":"joey-taskgraph/1","baseline_revision":"abc123","tasks":[{"id":"task-auth","objective":"Implement token refresh","dependencies":[],"read_set":[],"write_set":["src/auth.rs"],"artifact_ids":[42,1337],"role":"implementor","model_tier":"economical","risk":"medium","acceptance":[{"criterion":"cargo test -p joey-core auth","kind":"command"}],"verification":{"steps":[{"name":"scoped-tests","command":"cargo test -p joey-core auth","parse":"plain","timeout_sec":300,"required":true}],"risk_triggered_review":false},"isolation":"isolated_worktree"},{"id":"task-docs","objective":"Document token refresh","dependencies":[],"read_set":[],"write_set":["docs/auth.md"],"artifact_ids":[],"role":"implementor","model_tier":"economical","risk":"low","acceptance":[{"criterion":"docs mention refresh","kind":"manual"}],"verification":{"steps":[],"risk_triggered_review":false}}]}"#;

#[tokio::test]
async fn concurrent_updates_on_disjoint_nodes_both_apply_and_republish() {
    // Manager-local tap: this tool's emissions land on our channel only.
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    mgr.set_event_tap(Some(tx));
    let tool = Arc::new(TaskGraphTool::new(mgr.clone()));
    let ctx = make_ctx();

    // Publish the two-node graph.
    let planned = tool
        .execute(
            json!({
                "action": "plan",
                "graph": serde_json::from_str::<Value>(PLAN_TWO_NODES).expect("fixture is valid JSON"),
            }),
            &ctx,
        )
        .await;
    assert!(
        matches!(&planned, ToolResult::Text(t) if t.contains("plan accepted: 2 tasks")),
        "plan must accept both nodes, got {:?}",
        planned
    );

    // Two concurrent action=update calls transitioning DISJOINT nodes
    // (pending -> ready on each). The tool is Arc-shared exactly as the
    // parent and child registries share it.
    let a = {
        let tool = tool.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            tool.execute(
                json!({"action": "update", "transitions": [{"id": "task-auth", "status": "ready"}]}),
                &ctx,
            )
            .await
        })
    };
    let b = {
        let tool = tool.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            tool.execute(
                json!({"action": "update", "transitions": [{"id": "task-docs", "status": "ready"}]}),
                &ctx,
            )
            .await
        })
    };
    let (ra, rb) = (a.await.expect("update A task joined"), b.await.expect("update B task joined"));
    for (label, r) in [("A", &ra), ("B", &rb)] {
        match r {
            ToolResult::Text(t) => assert!(
                t.contains("applied 1") && !t.contains("rejected"),
                "update {label} must report exactly one applied transition, got {t}"
            ),
            other => panic!("update {label} must succeed, got {:?}", other),
        }
    }

    // Final graph shows BOTH transitions.
    let status = tool.execute(json!({"action": "status"}), &ctx).await;
    match status {
        ToolResult::Text(t) => {
            assert!(t.contains("task-auth: ready"), "status: {t}");
            assert!(t.contains("task-docs: ready"), "status: {t}");
        }
        other => panic!("expected Text status, got {:?}", other),
    }

    // Tap capture: exactly the plan snapshot + ONE re-published snapshot
    // per applied update (Mutex-serialized mutations, snapshot re-emitted
    // under the lock after each call's transitions).
    let mut published = Vec::new();
    for _ in 0..16 {
        match rx.try_recv() {
            Ok(AgentEvent::TaskGraphPublished { graph }) => published.push(graph),
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    mgr.set_event_tap(None);
    assert_eq!(
        published.len(),
        3,
        "expected plan + 2 update snapshots, got {}",
        published.len()
    );
    // The LAST snapshot (the second update's, emitted under the lock after
    // both nodes are ready) must show both transitions.
    let last = published.last().expect("at least one snapshot");
    assert_eq!(last["nodes"]["task-auth"]["status"], "ready", "last: {last}");
    assert_eq!(last["nodes"]["task-docs"]["status"], "ready", "last: {last}");
}

// (h) recorder-tap mirror (T029): publish events feed the recorder tap
// ALONGSIDE the manager-local event tap — every emission site mirrors, so
// the subagent_control log ring keeps filling without shadowing the host
// tap.

#[tokio::test]
async fn publish_events_feed_recorder_tap_alongside_event_tap() {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    mgr.set_event_tap(Some(tx));
    let (rec_tx, mut rec_rx) = mpsc::unbounded_channel::<AgentEvent>();
    mgr.set_recorder_tap(Some(rec_tx));
    let tool = Arc::new(TaskGraphTool::new(mgr.clone()));
    let ctx = make_ctx();

    // Publish + one applied update ⇒ TWO TaskGraphPublished emissions.
    assert!(
        matches!(
            tool.execute(
                json!({
                    "action": "plan",
                    "graph": serde_json::from_str::<Value>(PLAN_TWO_NODES).expect("fixture is valid JSON"),
                }),
                &ctx
            )
            .await,
            ToolResult::Text(_)
        )
    );
    assert!(
        matches!(
            tool.execute(
                json!({ "action": "update", "transitions": [{ "id": "task-auth", "status": "ready" }] }),
                &ctx
            )
            .await,
            ToolResult::Text(_)
        )
    );

    let drain = |rx: &mut mpsc::UnboundedReceiver<AgentEvent>| -> usize {
        let mut n = 0;
        for _ in 0..16 {
            match rx.try_recv() {
                Ok(AgentEvent::TaskGraphPublished { .. }) => n += 1,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        n
    };
    let tap_count = drain(&mut rx);
    let rec_count = drain(&mut rec_rx);
    mgr.set_event_tap(None);
    mgr.set_recorder_tap(None);
    assert_eq!(tap_count, 2, "event tap: plan + update");
    assert_eq!(
        rec_count, tap_count,
        "recorder tap must mirror every published event"
    );
}
