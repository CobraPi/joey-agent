//! Integration test: cooperative interrupt propagation (FR-015).
//! Verify the interrupt handle is available and can be signaled.

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
async fn interrupt_handle_is_shareable_and_signalable() {
    let mgr = SubagentManager::new(ManagerConfig::default());

    // The interrupt handle must be accessible before any dispatch.
    let handle = mgr.interrupt_handle();
    assert!(!mgr.is_interrupted());

    // Signal interrupt.
    mgr.signal_interrupt();
    assert!(mgr.is_interrupted());
    assert!(handle.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn pre_signaled_interrupt_propagates_to_batch_results() {
    let mgr = SubagentManager::new(ManagerConfig::default());

    // Signal interrupt before dispatch.
    mgr.signal_interrupt();
    assert!(mgr.is_interrupted());

    let tasks = vec![
        TaskSpec {
            goal: "Interrupt-task".to_string(),
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
            &make_agent_config(),
            &Config::defaults(),
            &ToolRegistry::new(),
            None,
        )
        .await;

    // The batch should still return results (not hang or panic).
    assert_eq!(results.len(), 1);
    // With interrupt pre-signaled, the subagent should be marked interrupted.
    let r = &results[0];
    assert!(!r.success, "subagent should be interrupted when flag is pre-set");
}
