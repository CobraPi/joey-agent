//! Feature 020, User Story 2 (T011/T012): background completion notices.
//!
//! T011 (FAIL-FIRST) — pinned per FR-003/FR-004/FR-016/SC-002/SC-006:
//!   (a) successful background child -> a completion notice is queued whose
//!       text starts with
//!       `[SUBAGENT COMPLETE] id=<id> goal=<goal> outcome=success tokens=<n> duration=<n>s`
//!       followed by a distilled summary of <=500 tokens (~2000 chars cap);
//!   (b) failing child -> `[SUBAGENT FAILED] ... outcome=failure ...` is
//!       PUSHED (failures are never silently dropped, US2-2/SC-002 at the
//!       delegation level: every finished failure produces a notice);
//!   (c) stopped child -> `[SUBAGENT STOPPED] ... outcome=<stop_reason_snake> ...`;
//!   (d) SC-006: a child producing a huge summary still yields a bounded
//!       notice — context grows with subagent count, not activity volume;
//!   (e) SC-002 queue semantics: >64 pending notices -> the EXISTING queue
//!       drops the OLDEST (documented behavior, pinned here); the retained
//!       64 include the newest failure notice.
//!
//! Harness: local mock OpenAI-compatible HTTP server (deterministic, no
//! real LLM) — same pattern as tests/background.rs (read, not edited).
//!
//! Notices are delivered through the EXISTING pending-completions queue on
//! `ToolContext` (cap 64, drop-oldest), drained by the agent at the start of
//! the next `run_turn` (the existing cross-turn delivery mechanism). The
//! notice text rides `output_tail`; `session_id` carries `subagent-<id>`.

use std::time::{Duration, Instant};

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::background::dispatch_background_with_notices;
use joey_orchestration::{DelegationRequest, ManagerConfig, StopReason, SubagentManager};
use joey_tools::context::ToolContext;
use joey_tools::ToolRegistry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Local mock OpenAI-compatible provider on 127.0.0.1 — deterministic, no LLM.
// (Same harness pattern as tests/background.rs.)
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum MockMode {
    /// 200 with the given assistant content.
    Ok(&'static str),
    /// 200 with the given content after a delay (keeps the child running).
    OkDelayed(&'static str, u64),
    /// 401 -> Auth -> non-retryable, empty fallback chain -> fatal child
    /// failure ("subagent turn failed (fatal provider error)").
    Unauthorized,
}

fn openai_body(content: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
    })
    .to_string()
}

/// Serve exactly one HTTP request on `stream` per the mode, then close.
async fn serve_conn(mut stream: TcpStream, mode: MockMode) {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 256 * 1024 {
            return;
        }
        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
        else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + 4 + content_length {
        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
        else {
            break;
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    let (status, body) = match mode {
        MockMode::Ok(text) => ("200 OK", openai_body(text)),
        MockMode::OkDelayed(text, ms) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            ("200 OK", openai_body(text))
        }
        MockMode::Unauthorized => (
            "401 Unauthorized",
            r#"{"error":{"message":"bad key"}}"#.to_string(),
        ),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Bind a mock provider on 127.0.0.1:0; returns its base URL.
async fn spawn_mock_server(mode: MockMode) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let mode = mode.clone();
            tokio::spawn(async move {
                serve_conn(stream, mode).await;
            });
        }
    });
    format!("http://{addr}")
}

/// A &'static str of `n` 'A's (mock a child whose summary blows past the
/// 500-token target by orders of magnitude — SC-006).
fn leak_long_summary(n: usize) -> &'static str {
    Box::leak("A".repeat(n).into_boxed_str())
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn agent_config(base_url: String) -> AgentConfig {
    AgentConfig {
        model: "test-model".to_string(),
        provider: "openrouter".to_string(),
        base_url,
        api_key: Some("test-key".to_string()),
        max_turns: 5,
        api_max_retries: 1,
        tool_delay: 0.0,
        reasoning: None,
        enabled_tools: vec![],
        max_tokens: None,
        stream: false,
        pass_session_id: false,
        model_pinned: false,
    }
}

/// Dispatch one background child wired for completion notices; returns the
/// work handle plus the shared ToolContext the notices land in.
async fn dispatch_noticed_child(
    base_url: String,
    goal: &str,
) -> (std::sync::Arc<SubagentManager>, ToolContext, String) {
    let mgr = std::sync::Arc::new(SubagentManager::new(ManagerConfig::default()));
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "notices-test");
    let req = DelegationRequest::single(goal);
    let handle = dispatch_background_with_notices(
        &mgr,
        &req,
        &agent_config(base_url),
        &Config::defaults(),
        &ToolRegistry::new(),
        None,
        &ctx,
    );
    (mgr, ctx, handle.child_id)
}

/// Poll-drain the pending-completions queue until a notice for `child_id`
/// appears (or timeout). Returns ALL completions drained so far so callers
/// can assert on the whole retained set.
async fn wait_for_notice(
    ctx: &ToolContext,
    child_id: &str,
) -> Vec<joey_tools::context::BackgroundCompletion> {
    let needle = format!("id={child_id} ");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    loop {
        seen.extend(ctx.drain_pending_completions());
        if seen
            .iter()
            .any(|c| c.output_tail.contains(&needle))
        {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "no notice for child {child_id} within 20s; drained so far: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The notice entry for one child id (panics with context if absent).
fn notice_for<'a>(
    completions: &'a [joey_tools::context::BackgroundCompletion],
    child_id: &str,
) -> &'a joey_tools::context::BackgroundCompletion {
    completions
        .iter()
        .find(|c| c.output_tail.contains(&format!("id={child_id} ")))
        .unwrap_or_else(|| panic!("no notice for child {child_id} in {completions:?}"))
}

/// Split a notice into (header, summary body) and sanity-check the shape:
/// exactly the wire format `<header>\n<summary>` with a parseable duration.
fn split_notice<'a>(notice: &'a str) -> (&'a str, &'a str) {
    let (header, body) = notice
        .split_once('\n')
        .unwrap_or_else(|| panic!("notice has no header/body split: {notice:?}"));
    let dur = header
        .rsplit("duration=")
        .next()
        .unwrap_or_else(|| panic!("header lacks duration=: {header:?}"))
        .trim_end_matches('s');
    dur.parse::<f64>()
        .unwrap_or_else(|e| panic!("duration {dur:?} not a number ({e}): {header:?}"));
    (header, body)
}

// ---------------------------------------------------------------------------
// (a) Successful child -> [SUBAGENT COMPLETE] notice (FR-003/FR-016)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t011_success_child_pushes_complete_notice() {
    let base = spawn_mock_server(MockMode::Ok("All 3 modules refactored; tests green.")).await;
    let (_mgr, ctx, id) = dispatch_noticed_child(base, "refactor the modules").await;

    let drained = wait_for_notice(&ctx, &id).await;
    let entry = notice_for(&drained, &id);
    assert_eq!(entry.session_id, format!("subagent-{id}"));

    let (header, body) = split_notice(&entry.output_tail);
    assert!(
        header.starts_with(&format!(
            "[SUBAGENT COMPLETE] id={id} goal=refactor the modules outcome=success tokens=150 duration="
        )),
        "unexpected header: {header:?}"
    );
    assert!(header.ends_with('s'), "duration must end in 's': {header:?}");
    // Distilled summary: exactly the child's summary, within the 500-token
    // budget (~2000 chars cap — SC-006 budget applies to every notice).
    assert_eq!(body, "All 3 modules refactored; tests green.");
    assert!(
        body.chars().count() <= 2001,
        "summary exceeds the ~500-token/2000-char cap: {} chars",
        body.chars().count()
    );
}

// ---------------------------------------------------------------------------
// (b) Failing child -> [SUBAGENT FAILED] notice, PUSHED — never silently
//     dropped (US2-2/SC-002 at the delegation level).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t011_failed_child_pushes_failed_notice() {
    let base = spawn_mock_server(MockMode::Unauthorized).await;
    let (_mgr, ctx, id) = dispatch_noticed_child(base, "will fail hard").await;

    let drained = wait_for_notice(&ctx, &id).await;
    let entry = notice_for(&drained, &id);

    let (header, body) = split_notice(&entry.output_tail);
    assert!(
        header.starts_with(&format!(
            "[SUBAGENT FAILED] id={id} goal=will fail hard outcome=failure tokens="
        )),
        "unexpected header: {header:?}"
    );
    assert!(header.contains(" duration="), "header: {header:?}");
    // Failure notice carries the reason (US2 acceptance 2).
    assert!(
        body.contains("fatal provider error"),
        "failure notice must carry the reason, got body: {body:?}"
    );
}

// ---------------------------------------------------------------------------
// (c) Stopped child -> [SUBAGENT STOPPED] with the snake_case stop reason
//     carried in outcome= (FR-010/FR-016).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t011_stopped_child_pushes_stopped_notice_with_reason() {
    let base = spawn_mock_server(MockMode::OkDelayed("PARTIAL WORK", 1500)).await;
    let (mgr, ctx, id) = dispatch_noticed_child(base, "long running goal").await;

    // Stop the child shortly after dispatch (mid-run for the delayed mock).
    mgr.stop_child(id.parse().unwrap(), StopReason::OrchestratorRequested)
        .expect("stop_child must succeed on a live pre-registered child");

    let drained = wait_for_notice(&ctx, &id).await;
    let entry = notice_for(&drained, &id);

    let (header, _body) = split_notice(&entry.output_tail);
    assert!(
        header.starts_with(&format!(
            "[SUBAGENT STOPPED] id={id} goal=long running goal outcome=orchestrator_requested tokens="
        )),
        "unexpected header: {header:?}"
    );
}

// ---------------------------------------------------------------------------
// (d) SC-006: huge child transcript/summary -> notice stays bounded.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t011_notice_bounded_for_huge_child_summary() {
    let base = spawn_mock_server(MockMode::Ok(leak_long_summary(100_000))).await;
    let (_mgr, ctx, id) = dispatch_noticed_child(base, "summarize everything").await;

    let drained = wait_for_notice(&ctx, &id).await;
    let entry = notice_for(&drained, &id);

    let (header, body) = split_notice(&entry.output_tail);
    assert!(
        header.starts_with(&format!(
            "[SUBAGENT COMPLETE] id={id} goal=summarize everything outcome=success tokens=150 duration="
        )),
        "unexpected header: {header:?}"
    );
    // The summary is hard-capped (~2000 chars ~ 500 tokens) regardless of
    // the child's 100_000-char transcript, and is marked truncated.
    assert!(
        body.chars().count() <= 2001,
        "summary must be capped at ~2000 chars, got {}",
        body.chars().count()
    );
    assert!(
        body.ends_with('…'),
        "truncated summary must be marked: tail={:?}",
        body.chars().rev().take(20).collect::<String>()
    );
    // Whole notice bounded: header + newline + capped body.
    assert!(
        entry.output_tail.chars().count() < 2200,
        "whole notice must stay bounded, got {}",
        entry.output_tail.chars().count()
    );
}

// ---------------------------------------------------------------------------
// (e) SC-002 queue semantics: cap 64, drop-OLDEST (the EXISTING queue's
//     documented behavior — pinned here). The retained 64 include the
//     newest failure notice; failures are not exempt from the cap (uniform
//     drop-oldest), which is why T012 pushes every failure the moment it
//     finishes and the agent drains the queue every turn.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t011_pending_notice_queue_cap_64_drops_oldest() {
    const CAP: usize = 64; // PENDING_COMPLETIONS_MAX in joey-tools context.rs
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "notices-cap");

    let mk = |i: usize, text: String| joey_tools::context::BackgroundCompletion {
        session_id: format!("subagent-{i}"),
        exit_code: if i == 69 { 1 } else { 0 },
        output_tail: text,
        elapsed_secs: 1.0,
    };
    for i in 0..=69 {
        let text = if i == 69 {
            format!("[SUBAGENT FAILED] id=69 goal=g outcome=failure tokens=0 duration=0.0s\nboom")
        } else {
            format!("[SUBAGENT COMPLETE] id={i} goal=g outcome=success tokens=1 duration=0.0s\nok")
        };
        ctx.push_background_completion(mk(i, text));
    }

    let drained = ctx.drain_pending_completions();
    assert_eq!(drained.len(), CAP, "queue must hold exactly the cap");
    // Drop-OLDEST: the first 6 pushes (0..=5) are gone.
    assert_eq!(drained.first().unwrap().session_id, "subagent-6");
    assert_eq!(drained.last().unwrap().session_id, "subagent-69");
    // The NEWEST failure notice is among the retained ones.
    assert!(
        drained
            .iter()
            .any(|c| c.output_tail.starts_with("[SUBAGENT FAILED] id=69")),
        "newest failure notice must be retained"
    );
    assert!(
        !drained.iter().any(|c| c.session_id == "subagent-5"),
        "oldest entries must be dropped"
    );
}
