//! US3 — cache-stable assembly (quickstart.md US3, research D10).

use super::support::fixture_with_tools;

fn canonical_messages(req: &joey_providers::ProviderRequest) -> String {
    let mut s = req.system.clone().unwrap_or_default();
    for m in &req.messages {
        s.push('\n');
        s.push_str(&serde_json::to_string(m).unwrap());
    }
    s
}

fn leading_identical_bytes(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

// Scripted-response adaptation (reasoning_prune.rs precedent): an EMPTY
// script returns NormalizedResponse::empty() every call, driving the loop
// into the 3x empty-response retry path — each turn then emits MULTIPLE
// requests and reqs[1] is a same-turn retry, not turn 2. One Stop response
// per turn makes the request↔turn mapping exact.
fn done_response() -> Result<joey_providers::NormalizedResponse, joey_providers::ProviderError> {
    let mut resp = joey_providers::NormalizedResponse::empty();
    resp.content = "done".into();
    Ok(resp)
}

// ORCHESTRATOR ADAPTATION (reported): config clamps `context_assembly.tool_top_k`
// to 5..=60 (joey-core config.rs `context_assembly_tool_top_k`), so the brief's
// literal `tool_top_k: 3` is unreachable via config — and with only 5 tools the
// clamped top_k=5 makes select_tools a no-op (`tools.len() <= top_k`), leaving
// the RED vacuous. 9 tools + top_k 5 + a narrow always-keep list makes selection
// genuinely query-dependent. The state block is disabled here so the gauge is
// the ONLY volatile tail (its ~1200 request-specific chars would sink the 95%
// leading-byte ratio even after GREEN; message ordering is pinned separately by
// `volatile_content_sits_at_the_tail`).
const ASM_YAML: &str = "context_assembly:\n  enabled: true\n  tool_schema_retrieval: true\n  tool_top_k: 5\n  always_keep_tools: [read_file]\nstate_block:\n  enabled: false\n";
const TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "search_files",
    "terminal",
    "web_search",
    "web_extract",
    "memory",
    "todo",
    "process",
];

#[tokio::test]
async fn consecutive_requests_share_95pct_leading_bytes_and_stable_tool_list() {
    let mut f = fixture_with_tools(ASM_YAML, vec![done_response(), done_response()], TOOLS.to_vec());
    // DIFFERENT queries per turn: selection is query-dependent, so without
    // the FR-004 guard the tool lists would differ between turns.
    f.turn("please update my todo list entries").await;
    f.turn("search").await;
    let reqs = f.transport.requests.lock().unwrap();
    assert!(reqs.len() >= 2, "need two requests");
    let c1 = canonical_messages(&reqs[0]);
    let c2 = canonical_messages(&reqs[1]);
    let lead = leading_identical_bytes(&c1, &c2);
    let ratio = lead as f64 / c1.len().max(1) as f64;
    assert!(ratio >= 0.95, "leading-byte identity {ratio:.3} < 0.95");
    let t1 = serde_json::to_string(&reqs[0].tools).unwrap();
    let t2 = serde_json::to_string(&reqs[1].tools).unwrap();
    assert_eq!(t1, t2, "tool list must be byte-identical across turns absent toolset change");
}

#[tokio::test]
async fn toolset_change_refreshes_selection() {
    let mut f = fixture_with_tools(ASM_YAML, vec![done_response(), done_response()], TOOLS.to_vec());
    f.turn("query about files").await;
    // Simulate an explicit toolset change between turns (the rebuild path):
    // restrict enabled tools, rebuild the prompt, run another turn.
    f.agent.set_enabled_tools(vec!["read_file".to_string(), "web_search".to_string()]);
    f.agent.rebuild_system_prompt();
    f.turn("query about files").await;
    let reqs = f.transport.requests.lock().unwrap();
    let t2 = serde_json::to_string(&reqs[1].tools).unwrap();
    let names: Vec<&str> = reqs[1].tools.iter().map(|t| t.function.name.as_str()).collect();
    assert!(
        names.iter().all(|n| ["read_file", "web_search"].contains(n)),
        "selection must respect the new toolset: {names:?}"
    );
    assert!(!t2.is_empty());
}

#[tokio::test]
async fn volatile_content_sits_at_the_tail() {
    // t014 placement audit: the gauge (volatile) is the LAST message; the
    // state block (if any) precedes it; stable history content first.
    let yaml = "state_block:\n  enabled: true\n";
    let mut f = fixture_with_tools(yaml, vec![done_response()], TOOLS.to_vec());
    f.turn("placement check").await;
    let reqs = f.transport.requests.lock().unwrap();
    let req = reqs.last().unwrap();
    let last = req.messages.last().expect("messages");
    let last_text = last.content.as_deref().unwrap_or("");
    assert!(
        last_text.contains("<total_tokens>"),
        "gauge must be the final message: {last_text}"
    );
    if req.messages.len() >= 2 {
        let prev = &req.messages[req.messages.len() - 2];
        let prev_text = prev.content.as_deref().unwrap_or("");
        assert!(!prev_text.contains("<total_tokens>"), "only one gauge line allowed");
    }
}
