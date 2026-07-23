//! Integration test: orchestration events are emitted correctly.
//!
//! SC-009: SubagentSpawn, SubagentComplete/SubagentFailed,
//! DelegationBatchComplete events emitted via AgentEvent channel.

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
    }
}

fn drain_events(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn batch_emits_all_lifecycle_events() {
    let mgr = SubagentManager::new(ManagerConfig::default());
    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();

    let (tx, mut rx) = mpsc::unbounded_channel();

    let tasks = vec![
        TaskSpec {
            goal: "Event task A".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
        },
        TaskSpec {
            goal: "Event task B".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
        },
    ];

    let _results = mgr
        .dispatch_batch(
            &tasks,
            None,
            &[],
            &parent_cfg,
            &config_tree,
            &registry,
            Some(&tx),
        )
        .await;

    let events = drain_events(&mut rx);

    // Should have SubagentSpawn events for each child.
    let spawns: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::SubagentSpawn { .. }))
        .collect();
    assert_eq!(spawns.len(), 2, "expected 2 SubagentSpawn events");

    // Should have exactly 1 DelegationBatchComplete event.
    let batch_completes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::DelegationBatchComplete { .. }))
        .collect();
    assert_eq!(
        batch_completes.len(),
        1,
        "expected 1 DelegationBatchComplete event"
    );

    // Verify DelegationBatchComplete fields.
    if let AgentEvent::DelegationBatchComplete {
        total,
        succeeded,
        failed,
        ..
    } = batch_completes[0]
    {
        assert_eq!(*total, 2usize);
        assert_eq!(*succeeded + *failed, *total);
    }

    // Each spawn should carry a goal and model.
    for spawn in &spawns {
        if let AgentEvent::SubagentSpawn {
            goal, model, depth, ..
        } = spawn
        {
            assert!(!goal.is_empty());
            assert!(!model.is_empty());
            assert_eq!(*depth, 0); // top-level parent depth
        }
    }
}
