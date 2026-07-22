//! End-to-end compression tests: the agent loop with a scripted transport +
//! scripted summary backend (413 recovery, disabled-compression guard,
//! preflight pressure, the 3-attempt cap, lock contention, in-place session
//! rewrite, anchors, manual /compress feedback).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use joey_core::{Config, SessionDb};
use joey_providers::{
    FinishReason, Message, NormalizedResponse, ProviderError, ProviderRequest, StreamEvent,
    ToolCall, Usage,
};
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::{ToolContext, ToolRegistry};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::anchors::{ensure_compressed_has_user_turn, is_real_user_message};
use super::compressor::ContextCompressor;
use super::test_support::ScriptedSummary;
use crate::agent::{Agent, AgentConfig, Transport};
use crate::events::AgentEvent;

// ── Scripted provider (mirror of the agent.rs test transport) ────────────

struct ScriptedTransport {
    responses: Mutex<VecDeque<Result<NormalizedResponse, ProviderError>>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl ScriptedTransport {
    fn new(script: Vec<Result<NormalizedResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(script.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl Transport for ScriptedTransport {
    async fn complete(&self, req: &ProviderRequest) -> Result<NormalizedResponse, ProviderError> {
        self.requests.lock().unwrap().push(req.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(NormalizedResponse::empty()))
    }
    async fn stream(
        &self,
        req: &ProviderRequest,
        _tx: mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<NormalizedResponse, ProviderError> {
        self.complete(req).await
    }
}

fn text_resp(text: &str) -> NormalizedResponse {
    NormalizedResponse {
        content: text.to_string(),
        finish_reason: FinishReason::Stop,
        ..NormalizedResponse::empty()
    }
}

fn text_resp_with_usage(text: &str, prompt_tokens: u64) -> NormalizedResponse {
    NormalizedResponse {
        content: text.to_string(),
        finish_reason: FinishReason::Stop,
        usage: Usage {
            prompt_tokens,
            completion_tokens: 5,
            total_tokens: prompt_tokens + 5,
            ..Usage::default()
        },
        ..NormalizedResponse::empty()
    }
}

fn tool_resp_with_usage(calls: Vec<ToolCall>, prompt_tokens: u64) -> NormalizedResponse {
    NormalizedResponse {
        tool_calls: calls,
        finish_reason: FinishReason::ToolCalls,
        usage: Usage {
            prompt_tokens,
            completion_tokens: 5,
            total_tokens: prompt_tokens + 5,
            ..Usage::default()
        },
        ..NormalizedResponse::empty()
    }
}

struct EchoTool;
#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn toolset(&self) -> &str {
        "test"
    }
    fn description(&self) -> &str {
        "echoes"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}})
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::Text(format!(
            "echo:{}",
            args.get("text").and_then(|t| t.as_str()).unwrap_or("")
        ))
    }
}

struct Fixture {
    agent: Agent,
    transport: Arc<ScriptedTransport>,
    _home: tempfile::TempDir,
    _cwd: tempfile::TempDir,
    _guard: joey_core::constants::HomeOverrideGuard,
}

fn fixture_with_config(
    script: Vec<Result<NormalizedResponse, ProviderError>>,
    config_yaml: Option<&str>,
) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let guard = joey_core::constants::HomeOverrideGuard::new(home.path().to_path_buf());
    let cwd = tempfile::tempdir().unwrap();
    let config = match config_yaml {
        Some(yaml) => {
            let path = home.path().join("config.yaml");
            std::fs::write(&path, yaml).unwrap();
            Config::load_from(path).unwrap()
        }
        None => Config::defaults(),
    };
    let ctx = ToolContext::new(cwd.path().to_path_buf(), config, "test-session");
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent_cfg = AgentConfig {
        model: "test-model".to_string(),
        provider: "openrouter".to_string(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        api_key: None,
        max_turns: 20,
        api_max_retries: 3,
        tool_delay: 0.0,
        reasoning: None,
        enabled_tools: vec!["echo".to_string()],
        max_tokens: None,
        stream: false,
        pass_session_id: false,
    };
    let mut agent = Agent::new(agent_cfg, registry, ctx).expect("agent");
    let transport = ScriptedTransport::new(script);
    agent.set_transport_for_tests(transport.clone());
    agent.set_summary_backend_for_tests(ScriptedSummary::ok("## Goal\nscripted summary body"));
    Fixture { agent, transport, _home: home, _cwd: cwd, _guard: guard }
}

fn fixture(script: Vec<Result<NormalizedResponse, ProviderError>>) -> Fixture {
    fixture_with_config(script, None)
}

/// Seed a long alternating transcript so compress() has a compactable middle.
fn seed_history(agent: &mut Agent, n: usize) {
    let mut msgs = Vec::new();
    for i in 0..n {
        if i % 2 == 0 {
            msgs.push(Message::user(format!("user turn {} {}", i, "x".repeat(200))));
        } else {
            msgs.push(Message::assistant(format!("assistant turn {} {}", i, "y".repeat(200))));
        }
    }
    agent.set_history(msgs);
}

fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn notices(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Notice(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

fn lock<'a>() -> std::sync::MutexGuard<'a, ()> {
    crate::TEST_HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn history_has_summary(agent: &Agent) -> bool {
    agent.history().iter().any(|m| {
        ContextCompressor::is_context_summary_content(&m.content.clone().unwrap_or_default())
    })
}

// ── (a) 413 → compress → retry succeeds ─────────────────────────────────

#[tokio::test(start_paused = true)]
async fn e2e_413_compress_then_retry_succeeds() {
    let _l = lock();
    let mut fx = fixture(vec![
        Err(ProviderError::from_status(413, "payload too large", None)),
        Ok(text_resp("recovered")),
    ]);
    seed_history(&mut fx.agent, 30);
    let pre_len = fx.agent.history().len(); // +1 user message during the turn

    let (tx, mut rx) = mpsc::unbounded_channel();
    let result = fx.agent.run_turn("go", tx).await;
    assert_eq!(result.final_text, "recovered");
    assert!(!result.interrupted);
    assert_eq!(fx.transport.request_count(), 2, "one failed call + one retry");
    // The history was compacted between the calls.
    assert!(history_has_summary(&fx.agent));
    assert!(fx.agent.history().len() < pre_len + 2);
    let events = drain(&mut rx);
    let ns = notices(&events);
    assert!(
        ns.iter()
            .any(|n| n == "⚠️  Request payload too large (413) — compression attempt 1/3..."),
        "413 notice missing: {:?}",
        ns
    );
    assert!(
        ns.iter().any(|n| n.starts_with("🗜️ Compressed ") && n.ends_with("retrying...")),
        "compressed/retrying notice missing: {:?}",
        ns
    );
}

// ── (b) compression disabled → exact guard messages, no compress ────────

#[tokio::test(start_paused = true)]
async fn e2e_disabled_compression_guard_messages() {
    let _l = lock();
    let mut fx = fixture_with_config(
        vec![Err(ProviderError::from_status(
            400,
            "prompt is too long: 250000 tokens > 200000 maximum",
            None,
        ))],
        Some("compression:\n  enabled: false\n"),
    );
    assert!(!fx.agent.compression_enabled());
    seed_history(&mut fx.agent, 30);
    let pre_len = fx.agent.history().len();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _ = fx.agent.run_turn("go", tx).await;
    assert_eq!(fx.transport.request_count(), 1, "no compress-retry when disabled");
    assert!(!history_has_summary(&fx.agent));
    // user msg + assistant error sentinel only — nothing compacted away.
    assert_eq!(fx.agent.history().len(), pre_len + 2);

    let events = drain(&mut rx);
    let ns = notices(&events);
    // Byte-exact guard messages (conversation_loop.py:3237-3246).
    assert!(ns.iter().any(|n| n
        == "❌ Context overflow, but auto-compaction is disabled (compression.enabled: false)."));
    assert!(ns.iter().any(|n| n
        == "   💡 Run /compress to compact manually, /new to start fresh, switch to a \
            larger-context model, or reduce attachments."));
    let failed = events.iter().find_map(|e| match e {
        AgentEvent::Failed(msg) => Some(msg.clone()),
        _ => None,
    });
    assert_eq!(
        failed.as_deref(),
        Some(
            "Context overflow and auto-compaction is disabled (compression.enabled: false). \
             Run /compress to compact manually, /new to start fresh, or switch to a \
             larger-context model."
        )
    );
}

// ── (c) preflight pressure trigger ──────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn e2e_preflight_pressure_triggers_compression() {
    let _l = lock();
    let mut fx = fixture(vec![
        // First call reports REAL usage well under the (lowered) threshold,
        // so the post-response gate stays quiet and only the rough preflight
        // estimate (system prompt + history + schemas) exceeds it.
        Ok(tool_resp_with_usage(
            vec![ToolCall::new("call_1", "echo", r#"{"text": "hi"}"#)],
            100,
        )),
        Ok(text_resp_with_usage("done", 120)),
    ]);
    seed_history(&mut fx.agent, 30);
    fx.agent.compressor_mut().threshold_tokens = 2_000;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let result = fx.agent.run_turn("go", tx).await;
    assert_eq!(result.final_text, "done");
    let events = drain(&mut rx);
    let ns = notices(&events);
    assert!(
        ns.iter().any(|n| n.starts_with("📦 Pre-API compression: ~")
            && n.ends_with("tokens near the context/output limit. Compacting before the next model call.")),
        "preflight notice missing: {:?}",
        ns
    );
    assert!(history_has_summary(&fx.agent));
}

// ── (d) 3-attempt cap ───────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn e2e_three_attempt_cap() {
    let _l = lock();
    let overflow = || {
        Err(ProviderError::from_status(
            400,
            "the input exceeds the context window",
            None,
        ))
    };
    let mut fx = fixture(vec![overflow(), overflow(), overflow(), overflow()]);
    seed_history(&mut fx.agent, 60);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _ = fx.agent.run_turn("go", tx).await;
    assert_eq!(fx.transport.request_count(), 4, "initial + 3 compress-retries");
    let events = drain(&mut rx);
    let ns = notices(&events);
    assert!(ns.iter().any(|n| n == "❌ Max compression attempts (3) reached."), "{:?}", ns);
    assert!(ns.iter().any(|n| n
        == "   💡 Try /new to start a fresh conversation, or /compress to retry compression."));
    // No provider-reported limit in the error → the window is never guessed
    // down (context probe stays at the catalog value).
    assert!(ns.iter().any(|n| n.starts_with(
        "⚠️  Context length exceeded, but provider did not report a max context length;"
    )));
    let failed = events.iter().find_map(|e| match e {
        AgentEvent::Failed(msg) => Some(msg.clone()),
        _ => None,
    });
    assert_eq!(
        failed.as_deref(),
        Some("Context length exceeded: max compression attempts (3) reached.")
    );
}

// ── Context probe: provider-reported limit updates the compressor ───────

#[tokio::test(start_paused = true)]
async fn e2e_context_probe_updates_window_from_error_text() {
    let _l = lock();
    let mut fx = fixture(vec![
        Err(ProviderError::from_status(
            400,
            "This model's maximum context length is 128000 tokens. However, your messages \
             resulted in 143222 tokens.",
            None,
        )),
        Ok(text_resp("ok")),
    ]);
    seed_history(&mut fx.agent, 30);
    assert_eq!(fx.agent.compressor().context_length, 256_000); // catalog default

    let (tx, mut rx) = mpsc::unbounded_channel();
    let result = fx.agent.run_turn("go", tx).await;
    assert_eq!(result.final_text, "ok");
    // The compressor was recalibrated to the provider-reported window.
    assert_eq!(fx.agent.compressor().context_length, 128_000);
    let events = drain(&mut rx);
    let ns = notices(&events);
    assert!(ns.iter().any(|n| n == "Context limit detected from API: 128,000 tokens (was 256,000)"));
    assert!(ns
        .iter()
        .any(|n| n == "⚠️  Context length exceeded — using provider limit: 256,000 → 128,000 tokens"));
}

// ── Lock contention: a concurrent holder makes compression sit out ──────

#[tokio::test(start_paused = true)]
async fn lock_contention_skips_compression_and_warns_once() {
    let _l = lock();
    let mut fx = fixture(vec![]);
    let db = SessionDb::open_in_memory().unwrap();
    let sid = db.create_session("cli", None, None).unwrap();
    fx.agent.set_session_store(db, sid.clone());
    seed_history(&mut fx.agent, 30);

    // A competing path holds the lock.
    {
        let guard = fx.agent.session_db().unwrap();
        assert!(guard.try_acquire_compression_lock(&sid, "other-holder", 300.0));
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let before = fx.agent.history().len();
    let changed = fx.agent.compress_context(Some(999_999), None, true, Some(&tx)).await;
    assert!(!changed, "contended compression must sit out");
    assert_eq!(fx.agent.history().len(), before);
    let ns = notices(&drain(&mut rx));
    assert!(ns.iter().any(|n| n
        == "⚠ Skipping concurrent compression — another path is already compressing this \
            session. Will retry after it finishes."));

    // Second contended attempt stays quiet (warned once per session id).
    let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
    let changed = fx.agent.compress_context(Some(999_999), None, true, Some(&tx2)).await;
    assert!(!changed);
    let ns2 = notices(&drain(&mut rx2));
    assert!(!ns2.iter().any(|n| n.starts_with("⚠ Skipping concurrent compression")));

    // Release → compression proceeds and the lock is freed afterwards.
    {
        let guard = fx.agent.session_db().unwrap();
        guard.release_compression_lock(&sid, "other-holder");
    }
    let (tx3, mut rx3) = mpsc::unbounded_channel::<AgentEvent>();
    let changed = fx.agent.compress_context(Some(999_999), None, true, Some(&tx3)).await;
    assert!(changed);
    drain(&mut rx3);
    {
        let guard = fx.agent.session_db().unwrap();
        assert!(guard.get_compression_lock_holder(&sid).is_none(), "lock released after compaction");
    }
}

// ── In-place session rewrite (conversation_history_after_compression) ───

#[tokio::test(start_paused = true)]
async fn compress_context_rewrites_history_and_session_store_in_place() {
    let _l = lock();
    let mut fx = fixture(vec![]);
    let db = SessionDb::open_in_memory().unwrap();
    let sid = db.create_session("cli", None, None).unwrap();
    fx.agent.set_session_store(db, sid.clone());

    // Persist a long transcript through the normal append path.
    let mut msgs = Vec::new();
    for i in 0..30 {
        let m = if i % 2 == 0 {
            Message::user(format!("user turn {} {}", i, "x".repeat(200)))
        } else {
            Message::assistant(format!("assistant turn {} {}", i, "y".repeat(200)))
        };
        msgs.push(m.clone());
        // Write the row exactly as the loop does.
        {
            let guard = fx.agent.session_db().unwrap();
            let mut row = joey_core::StoredMessage::new(
                sid.clone(),
                joey_core::Role::from_str(&m.role),
                m.text_content(),
            );
            row.timestamp = 1000.0 + i as f64;
            guard.add_message(&row).unwrap();
        }
    }
    fx.agent.set_history(msgs);

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let changed = fx.agent.compress_context(Some(999_999), None, true, Some(&tx)).await;
    assert!(changed);
    drain(&mut rx);

    // The live history was rebuilt around the summary…
    assert!(history_has_summary(&fx.agent));
    let live: Vec<(String, String)> = fx
        .agent
        .history()
        .iter()
        .map(|m| (m.role.clone(), m.text_content()))
        .collect();
    // …and the session store's ACTIVE rows are exactly the compacted
    // transcript (same session id — in-place, no rotation).
    let stored: Vec<(String, String)> = {
        let guard = fx.agent.session_db().unwrap();
        guard
            .messages(&sid)
            .unwrap()
            .iter()
            .map(|r| (r.role.as_str().to_string(), r.content.clone()))
            .collect()
    };
    assert_eq!(live, stored, "active rows must equal the live compacted transcript");
    assert!(stored.len() < 30);
    // The pre-compaction rows were soft-archived, not deleted: they are still
    // discoverable via FTS search (active=0, compacted=1 rows stay indexed).
    // "user turn 10" sits in the summarized-away middle — it is gone from
    // the live rows but its archived (active=0, compacted=1) row still hits.
    assert!(!stored.iter().any(|(_, c)| c.contains("user turn 10 ")));
    let hits = {
        let guard = fx.agent.session_db().unwrap();
        guard.search("\"user turn 10\"", 5).unwrap()
    };
    assert!(!hits.is_empty(), "archived pre-compaction turns must stay searchable");
}

// ── Anchor insertion (`_ensure_compressed_has_user_turn`) ───────────────

#[test]
fn anchor_restores_latest_real_user_turn() {
    // Compressed output with zero real user turns (summary pinned to user +
    // assistant tail).
    let summary_text = ContextCompressor::with_summary_prefix("## Goal\nbody");
    let mut summary_msg = Message::user(summary_text);
    summary_msg.compressed_summary = true;
    let mut compressed = vec![summary_msg, Message::assistant("tail reply")];
    assert!(!compressed.iter().any(is_real_user_message));

    let originals = vec![
        Message::user("first ask"),
        Message::assistant("did it"),
        Message::user("the REAL latest ask"),
        Message::assistant("working on it"),
    ];
    ensure_compressed_has_user_turn(&originals, &mut compressed);
    // The summary is user-ROLE, so the assistant is "user-preceded" and the
    // boundary slot is skipped; the anchor lands at the end — which is
    // exactly what the handoff prefix instructs the model to obey ("respond
    // ONLY to the latest user message AFTER this summary").
    let last = compressed.last().unwrap();
    assert_eq!(last.role, "user");
    assert_eq!(last.content.as_deref(), Some("the REAL latest ask"));
    assert!(compressed.iter().any(is_real_user_message));

    // Idempotent: a second call does not duplicate the anchor.
    let len = compressed.len();
    ensure_compressed_has_user_turn(&originals, &mut compressed);
    assert_eq!(compressed.len(), len);
}

#[test]
fn anchor_placeholder_when_no_real_user_exists() {
    let mut compressed = vec![Message::assistant("only assistant")];
    let originals = vec![Message::assistant("nothing human here")];
    ensure_compressed_has_user_turn(&originals, &mut compressed);
    let last = compressed.last().unwrap();
    assert_eq!(last.role, "user");
    assert_eq!(
        last.content.as_deref(),
        Some(
            "Continue from the compressed conversation context above. \
             This marker exists because no human user turn was available."
        )
    );
}

#[test]
fn synthetic_and_summary_user_turns_are_not_real() {
    let mut todo = Message::user(
        "[Your active task list was preserved across context compression]\n- [ ] 1. do it (pending)",
    );
    assert!(!is_real_user_message(&todo));
    todo.synthetic = true;
    assert!(!is_real_user_message(&todo));
    let summary = Message::user(ContextCompressor::with_summary_prefix("## Goal\nbody"));
    assert!(!is_real_user_message(&summary));
    assert!(is_real_user_message(&Message::user("hello")));
    assert!(!is_real_user_message(&Message::user("   ")));
}

// ── Manual /compress feedback strings ───────────────────────────────────

#[tokio::test(start_paused = true)]
async fn manual_compress_feedback_shapes() {
    let _l = lock();
    let mut fx = fixture(vec![]);
    seed_history(&mut fx.agent, 30);
    let summary = fx.agent.manual_compress(None, None).await;
    assert!(!summary.noop);
    assert!(!summary.aborted);
    assert!(summary.headline.starts_with("Compressed: 30 → "));
    assert!(summary.token_line.starts_with("Approx request size: ~"));
    assert!(summary.token_line.contains(" → ~"));

    // Aborted shape (auth failure): messages preserved.
    let mut fx2 = fixture(vec![]);
    seed_history(&mut fx2.agent, 30);
    fx2.agent
        .set_summary_backend_for_tests(ScriptedSummary::script(vec![Err("auth")]));
    let summary = fx2.agent.manual_compress(None, None).await;
    assert!(summary.aborted);
    assert_eq!(summary.headline, "Compression aborted: 30 messages preserved");
    assert_eq!(
        summary.note.as_deref().map(|n| n.starts_with(
            "Summary generation failed; no messages were removed. Reason: "
        )),
        Some(true)
    );

    // Fallback shape ("format" error → deterministic fallback).
    let mut fx3 = fixture(vec![]);
    seed_history(&mut fx3.agent, 30);
    fx3.agent
        .set_summary_backend_for_tests(ScriptedSummary::script(vec![Err("format")]));
    let summary = fx3.agent.manual_compress(None, None).await;
    assert!(summary.fallback_used);
    assert!(summary.headline.starts_with("Compressed with fallback: 30 → "));
    assert!(summary
        .note
        .as_deref()
        .unwrap()
        .contains("Joey used limited fallback context and removed"));
}

// ── Focus topic reaches the summarizer prompt ───────────────────────────

#[tokio::test(start_paused = true)]
async fn focus_topic_is_injected_into_the_summary_prompt() {
    let _l = lock();
    let mut fx = fixture(vec![]);
    seed_history(&mut fx.agent, 30);
    let backend = ScriptedSummary::ok("## Goal\nfocused");
    fx.agent.set_summary_backend_for_tests(backend.clone());
    let _ = fx.agent.manual_compress(Some("the database migration"), None).await;
    assert_eq!(backend.call_count(), 1);
    let prompt = backend.prompts.lock().unwrap()[0].clone();
    assert!(prompt.contains("FOCUS TOPIC: \"the database migration\""));
    assert!(prompt.contains("roughly 60-70% of the summary token budget"));
    // The structured template headings ride along verbatim.
    assert!(prompt.contains("## Historical Task Snapshot"));
    assert!(prompt.contains("## Historical Pending User Asks"));
    assert!(prompt.contains("Write only the summary body. Do not include any preamble or prefix."));
}
