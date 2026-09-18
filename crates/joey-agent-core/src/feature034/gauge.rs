//! US1 — live context gauge (quickstart.md US1).

use super::support::{fixture, fixture_with_tools};

fn last_msg_text(req: &joey_providers::ProviderRequest) -> String {
    req.messages.last().and_then(|m| m.content.clone()).unwrap_or_default()
}

fn gauge_remaining(text: &str) -> i64 {
    let open = "<total_tokens>";
    let start = text.find(open).expect("gauge tag") + open.len();
    let end = start + text[start..].find(" tokens left").expect("tokens-left suffix");
    text[start..end].parse().expect("gauge number")
}

#[tokio::test]
async fn gauge_line_present_in_assembled_request() {
    let mut f = fixture("\n", vec![]);
    f.turn("hello").await;
    let reqs = f.transport.requests.lock().unwrap();
    let text = last_msg_text(reqs.last().unwrap());
    assert!(text.contains("<total_tokens>"), "gauge tag missing in: {text}");
    assert!(text.contains("tokens left</total_tokens>"), "in: {text}");
}

#[tokio::test]
async fn gauge_figure_is_window_minus_max_of_real_or_estimate() {
    let mut f = fixture("\n", vec![]);
    f.turn("hello world").await;
    let reqs = f.transport.requests.lock().unwrap();
    let req = reqs.last().unwrap();
    let text = last_msg_text(req);
    let window = crate::compression::get_model_context_length("test-model", None);
    let estimate = crate::compression::estimate_request_tokens_rough(&req.messages[..req.messages.len() - 1], "", None);
    let expected = window - estimate;
    assert_eq!(gauge_remaining(&text), expected.max(0));
}

#[tokio::test]
async fn gauge_decreases_as_history_grows() {
    let mut f = fixture("\n", vec![]);
    f.turn("first question").await;
    let g1 = { let reqs = f.transport.requests.lock().unwrap(); gauge_remaining(&last_msg_text(reqs.last().unwrap())) };
    f.turn("second question, deliberately longer than the first one").await;
    let g2 = { let reqs = f.transport.requests.lock().unwrap(); gauge_remaining(&last_msg_text(reqs.last().unwrap())) };
    assert!(g2 < g1, "gauge must decrease as history grows: {g2} !< {g1}");
}

#[tokio::test]
async fn gauge_clamps_at_zero_and_flags_low_on_tiny_window() {
    // ORCHESTRATOR RULING: compression disabled for this test (mechanism per
    // loop_tests.rs e2e_disabled_compression_guard_messages: yaml
    // `compression:\n  enabled: false\n`) so the compressor cannot rewrite
    // history mid-turn; the gauge reads compressor.context_length/
    // threshold_tokens regardless.
    let yaml = "model:\n  context_length: 400\ncompression:\n  enabled: false\n";
    let mut f = fixture(yaml, vec![]);
    let seed: Vec<joey_providers::Message> = (0..40)
        .map(|i| joey_providers::Message::user(format!("seed message number {i} with plenty of padding text to consume tokens")))
        .collect();
    f.agent.set_history(seed);
    f.turn("go").await;
    let reqs = f.transport.requests.lock().unwrap();
    let text = last_msg_text(reqs.last().unwrap());
    assert_eq!(gauge_remaining(&text), 0, "negative remaining must clamp to zero: {text}");
    assert!(text.contains("LOW"), "low flag must fire at/below the compression threshold: {text}");
}

#[tokio::test]
async fn no_gauge_when_context_gauge_disabled() {
    let yaml = "context_gauge:\n  enabled: false\n";
    let mut f = fixture(yaml, vec![]);
    f.turn("hello").await;
    let reqs = f.transport.requests.lock().unwrap();
    for m in &reqs.last().unwrap().messages {
        if let Some(c) = &m.content {
            assert!(!c.contains("<total_tokens"), "gauge must be absent when disabled: {c}");
        }
    }
}

#[tokio::test]
async fn assembly_log_records_gauge_fields() {
    let yaml = "context_assembly:\n  enabled: true\n  log_assembly: true\n";
    let mut f = fixture(yaml, vec![]);
    f.turn("log me").await;
    let root = f.home.path().join("context-assembly");
    let dir = std::fs::read_dir(&root).expect("assembly log dir exists").next().expect("session dir").unwrap();
    let content = std::fs::read_to_string(dir.path().join("assembly.jsonl")).expect("assembly.jsonl");
    let last = content.lines().last().unwrap();
    let v: serde_json::Value = serde_json::from_str(last).unwrap();
    for key in ["request_turn", "gauge_remaining", "gauge_low", "notice_channel_on", "tool_list_hash"] {
        assert!(v.get(key).is_some(), "missing {key} in: {last}");
    }
}

#[tokio::test]
async fn tool_list_byte_identical_across_turns_absent_toolset_change() {
    // ORCHESTRATOR RULING: identical query both turns — selection is
    // query-dependent by design (feature 028); differing-query stability is
    // guaranteed only after the t013 snapshot guard.
    let yaml = "context_assembly:\n  enabled: true\n  tool_schema_retrieval: true\n  tool_top_k: 3\n";
    let mut f = fixture_with_tools(yaml, vec![], vec!["read_file", "write_file", "search_files", "terminal", "web_search"]);
    f.turn("turn about files and search").await;
    f.turn("turn about files and search").await;
    let reqs = f.transport.requests.lock().unwrap();
    let t1 = serde_json::to_string(&reqs[0].tools).unwrap();
    let t2 = serde_json::to_string(&reqs[1].tools).unwrap();
    assert_eq!(t1, t2, "tool lists must be byte-identical absent toolset change");
}
