//! US6 — reasoning-history pruning (quickstart.md US6, research D9).

use super::support::fixture;
use joey_providers::Message;

fn msg_from(v: serde_json::Value) -> Message {
    serde_json::from_value(v).expect("message roundtrip")
}

fn seed_history(agent: &mut crate::agent::Agent) {
    let history = vec![
        msg_from(serde_json::json!({
            "role": "user",
            "content": "first question"
        })),
        msg_from(serde_json::json!({
            "role": "assistant",
            "content": "first answer",
            "reasoning": "completed-turn thinking that must be pruned"
        })),
        msg_from(serde_json::json!({
            "role": "user",
            "content": "second question"
        })),
        msg_from(serde_json::json!({
            "role": "assistant",
            "content": "second answer",
            "reasoning": "also completed, also pruned"
        })),
    ];
    agent.set_history(history);
}

#[tokio::test]
async fn completed_turn_thinking_pruned_when_enabled() {
    let yaml = "reasoning_prune:\n  enabled: true\n";
    // Scripted response deviation from the task sketch: an EMPTY script
    // (vec![]) drives the loop into the empty-response retry path, which
    // appends a synthetic "(empty)" sentinel assistant (reasoning: None)
    // to history — breaking the append-only assertion below for reasons
    // unrelated to pruning. Script a real final response that itself
    // carries reasoning so every stored assistant keeps thinking.
    let mut resp = joey_providers::NormalizedResponse::empty();
    resp.reasoning = Some("current-turn thinking".into());
    resp.content = "done".into();
    let mut f = fixture(yaml, vec![Ok(resp)]);
    seed_history(&mut f.agent);
    f.turn("go").await;
    let reqs = f.transport.requests.lock().unwrap();
    let req = reqs.last().unwrap();
    for m in &req.messages {
        if m.role == "assistant" && m.content.as_deref() != Some("go") {
            assert!(
                m.reasoning.is_none(),
                "completed-turn thinking must be pruned from the request: {:?}",
                m.reasoning
            );
        }
    }
    // History itself is append-only: the agent's stored history keeps thinking.
    drop(reqs);
    for m in f.agent.history_for_inspection() {
        if m.role == "assistant" {
            assert!(m.reasoning.is_some(), "self.history must never be mutated by the prune pass");
        }
    }
}

#[tokio::test]
async fn thinking_replayed_when_disabled() {
    let mut f = fixture("\n", vec![]);
    seed_history(&mut f.agent);
    f.turn("go").await;
    let reqs = f.transport.requests.lock().unwrap();
    let req = reqs.last().unwrap();
    let with_thinking = req
        .messages
        .iter()
        .filter(|m| m.role == "assistant" && m.reasoning.is_some())
        .count();
    assert_eq!(with_thinking, 2, "default-off: thinking replays as today");
}

#[tokio::test]
async fn in_progress_turn_thinking_retained_when_enabled() {
    let yaml = "reasoning_prune:\n  enabled: true\n";
    // Scripted-response deviation from the task sketch: with a single Stop
    // response the turn ends after ONE provider call — the live assistant
    // message is pushed to history only AFTER that request is built, so it
    // can never appear in any captured request. Script a first response
    // carrying live reasoning PLUS a tool call (invalid name against the
    // empty registry → error-result → loop continues) so a SECOND request
    // contains the current turn's assistant message.
    let mut resp = joey_providers::NormalizedResponse::empty();
    resp.reasoning = Some("live in-progress thinking".into());
    resp.content = "live answer".into();
    resp.tool_calls = vec![joey_providers::ToolCall::new(
        "no_such_tool",
        "no_such_tool",
        "{}",
    )];
    resp.finish_reason = joey_providers::FinishReason::ToolCalls;
    let mut done = joey_providers::NormalizedResponse::empty();
    done.content = "done".into();
    let mut f = fixture(yaml, vec![Ok(resp), Ok(done)]);
    seed_history(&mut f.agent);
    f.turn("go").await;
    let reqs = f.transport.requests.lock().unwrap();
    let req = reqs.last().unwrap();
    assert!(
        req.messages.iter().any(|m| m.role == "assistant"
            && m.reasoning.as_deref() == Some("live in-progress thinking")),
        "in-progress-turn thinking must be retained"
    );
}

#[tokio::test]
async fn pruned_requests_keep_tool_calls_and_results_intact() {
    let yaml = "reasoning_prune:\n  enabled: true\n";
    let mut f = fixture(yaml, vec![]);
    let history = vec![
        msg_from(serde_json::json!({"role": "user", "content": "run a tool"})),
        msg_from(serde_json::json!({
            "role": "assistant",
            "content": "calling",
            "reasoning": "prune me",
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}]
        })),
        msg_from(serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": "tool result payload"})),
    ];
    f.agent.set_history(history);
    f.turn("next").await;
    let reqs = f.transport.requests.lock().unwrap();
    let req = reqs.last().unwrap();
    let caller = req.messages.iter().find(|m| m.role == "assistant" && m.content.as_deref() == Some("calling")).expect("assistant");
    assert!(caller.reasoning.is_none(), "thinking pruned");
    assert_eq!(caller.tool_calls.len(), 1, "tool calls must survive the prune");
    assert!(req.messages.iter().any(|m| m.role == "tool" && m.content.as_deref() == Some("tool result payload")), "tool results must survive");
}
