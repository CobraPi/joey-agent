//! Integration test: batch resilience — one failure doesn't abort others.
//!
//! SC-003: When one of 3 subagents returns an error, the other 2 results
//! are still delivered.

use joey_core::Config;
use joey_orchestration::{
    ManagerConfig, SubagentManager, TaskSpec,
};
use joey_agent_core::AgentConfig;
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
async fn batch_resilience_all_results_returned() {
    let mgr = SubagentManager::new(ManagerConfig::default());
    let parent_cfg = make_agent_config();
    let config_tree = Config::defaults();
    let registry = ToolRegistry::new();

    let tasks = vec![
        TaskSpec {
            goal: "Resilient task A".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
        },
        TaskSpec {
            goal: "Resilient task B".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
        },
        TaskSpec {
            goal: "Resilient task C".to_string(),
            context: None,
            model: None,
            toolsets: vec![],
        },
    ];

    let results = mgr
        .dispatch_batch(
            &tasks,
            None,
            &[],
            &parent_cfg,
            &config_tree,
            &registry,
            None,
        )
        .await;

    // All 3 results returned regardless of individual success/failure.
    assert_eq!(results.len(), 3, "all 3 results must be returned");

    // Each goal appears exactly once.
    let goals: Vec<&str> = results.iter().map(|r| r.goal.as_str()).collect();
    assert!(goals.contains(&"Resilient task A"));
    assert!(goals.contains(&"Resilient task B"));
    assert!(goals.contains(&"Resilient task C"));
}
