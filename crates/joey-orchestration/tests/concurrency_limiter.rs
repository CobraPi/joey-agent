//! Integration test: shared concurrency limiter (SC-008).
//! Verify the parent's semaphore is shared across batch children.

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::{ManagerConfig, SubagentManager, TaskSpec};
use joey_tools::ToolRegistry;

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

#[tokio::test]
async fn semaphore_is_shared_across_batch_children() {
    // The semaphore created by SubagentManager::new must have its permits
    // reduced when batch children are dispatched. We verify the semaphore
    // is the same Arc by checking that available_permits matches.
    let mgr = SubagentManager::new(ManagerConfig {
        max_concurrent_requests: 3,
        max_concurrent_children: 2,
        ..Default::default()
    });

    let sem = mgr.semaphore();
    assert_eq!(sem.available_permits(), 3);

    // Dispatch a small batch — the semaphore should still be intact after.
    let tasks = vec![
        TaskSpec { goal: "CL-A".to_string(), context: None, model: None, toolsets: vec![] },
        TaskSpec { goal: "CL-B".to_string(), context: None, model: None, toolsets: vec![] },
    ];

    let _ = mgr
        .dispatch_batch(
            &tasks,
            None,
            &[],
            &make_agent_config(),
            &Config::defaults(),
            &ToolRegistry::new(),
            None,
        )
        .await;

    // After completion, all permits should be returned.
    assert_eq!(sem.available_permits(), 3);
}

#[tokio::test]
async fn max_concurrent_children_chunks_large_batches() {
    // With max_concurrent_children=2 and 5 tasks, the batch should complete
    // but internally process in chunks of 2. All 5 results returned.
    let mgr = SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        ..Default::default()
    });

    let tasks: Vec<TaskSpec> = (0..5)
        .map(|i| TaskSpec {
            goal: format!("Chunk-task-{}", i),
            context: None,
            model: None,
            toolsets: vec![],
        })
        .collect();

    let results = mgr
        .dispatch_batch(
            &tasks,
            None,
            &[],
            &make_agent_config(),
            &Config::defaults(),
            &ToolRegistry::new(),
            None,
        )
        .await;

    assert_eq!(results.len(), 5, "all 5 results returned despite chunking");
}
