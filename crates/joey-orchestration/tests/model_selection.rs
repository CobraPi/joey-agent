//! Integration test: per-subagent model selection (SC-004).
//! Batch of 3 TaskSpecs with mixed models; each DelegationResult records
//! its assigned model.

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::{ManagerConfig, SubagentManager, TaskSpec};
use joey_tools::ToolRegistry;

fn make_agent_config() -> AgentConfig {
    AgentConfig {
        model: "parent-model".to_string(),
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
async fn mixed_model_batch_records_per_task_model() {
    let mgr = SubagentManager::new(ManagerConfig::default());

    let tasks = vec![
        TaskSpec {
            goal: "Heavy analysis task".to_string(),
            context: None,
            model: Some("heavy-model".to_string()),
            toolsets: vec![],
        },
        TaskSpec {
            goal: "Light task A".to_string(),
            context: None,
            model: Some("light-model".to_string()),
            toolsets: vec![],
        },
        TaskSpec {
            goal: "Light task B".to_string(),
            context: None,
            model: Some("light-model".to_string()),
            toolsets: vec![],
        },
    ];

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

    assert_eq!(results.len(), 3);

    // Each result should carry its assigned model, not the parent's.
    let heavy = results.iter().find(|r| r.goal == "Heavy analysis task").unwrap();
    assert_eq!(heavy.model, "heavy-model");

    let light_a = results.iter().find(|r| r.goal == "Light task A").unwrap();
    assert_eq!(light_a.model, "light-model");

    let light_b = results.iter().find(|r| r.goal == "Light task B").unwrap();
    assert_eq!(light_b.model, "light-model");
}
