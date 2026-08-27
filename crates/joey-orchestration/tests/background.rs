//! Feature 020, User Story 1 (T007/T008): `delegate_task` background mode.
//!
//! T007 (blocking parity, FR-002/SC-005) — pins TODAY'S background=false
//! behavior byte-for-byte through the real tool, against a local mock
//! OpenAI-compatible HTTP server (deterministic; no real LLM):
//!   (a) single success -> ToolResult::Text(result.summary) verbatim
//!   (b) single failure -> ToolResult::Error("Subagent failed: ...")
//!   (c) batch (tasks array) -> the exact multi-line plain-text report
//!   (d) the blocking path actually blocks until child completion
//!
//! T008 (fail-first, FR-001/SC-001/FR-013) — background=true behavior.
//! These MUST FAIL before T009/T010 land (they do: the parameter is
//! ignored today, so the call blocks and returns the child summary /
//! batch report instead of a handle line) and PASS after.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::types::DelegationState;
use joey_orchestration::{DelegateTask, ManagerConfig, SubagentManager};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Local mock OpenAI-compatible provider on 127.0.0.1 — deterministic, no LLM.
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
    json!({
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
    // Read headers + body (bounded, with a safety timeout so a bad client
    // can never hang the test).
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
        MockMode::Unauthorized => ("401 Unauthorized", r#"{"error":{"message":"bad key"}}"#.to_string()),
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

fn make_tool(base_url: String) -> (std::sync::Arc<SubagentManager>, DelegateTask, ToolContext) {
    make_tool_with(ManagerConfig::default(), base_url)
}

fn make_tool_with(
    mgr_config: ManagerConfig,
    base_url: String,
) -> (std::sync::Arc<SubagentManager>, DelegateTask, ToolContext) {
    let mgr = std::sync::Arc::new(SubagentManager::new(mgr_config));
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "background-test");
    let tool = DelegateTask::new(
        mgr.clone(),
        agent_config(base_url),
        Config::defaults(),
        ToolRegistry::new(),
        None,
        None,
    );
    (mgr, tool, ctx)
}

/// Extract the child id from a handle line, asserting the line's exact shape.
fn parse_handle_line(line: &str, goal: &str) -> String {
    let expected_tail = format!(" goal={goal} started");
    let id = line
        .strip_prefix("[BACKGROUND] id=")
        .and_then(|rest| rest.strip_suffix(&expected_tail))
        .unwrap_or_else(|| panic!("not a handle line: {line:?}"));
    assert!(!id.is_empty(), "empty child id in handle line: {line:?}");
    id.to_string()
}

// ---------------------------------------------------------------------------
// T007 — blocking parity (MUST pass pre-implementation)
// ---------------------------------------------------------------------------

/// (a) Single-task delegate with background unset returns the child summary
/// verbatim as ToolResult::Text.
#[tokio::test]
async fn t007_blocking_single_returns_summary_verbatim() {
    let base = spawn_mock_server(MockMode::Ok("MOCK-SUMMARY")).await;
    let (_mgr, tool, ctx) = make_tool(base);
    let res = tool
        .execute(json!({"goal": "summarize this"}), &ctx)
        .await;
    match res {
        ToolResult::Text(s) => assert_eq!(s, "MOCK-SUMMARY"),
        other => panic!("expected Text summary, got: {other:?}"),
    }
}

/// (a2) background explicitly false behaves identically (FR-002 opt-in only).
#[tokio::test]
async fn t007_blocking_single_explicit_false_same_bytes() {
    let base = spawn_mock_server(MockMode::Ok("MOCK-SUMMARY")).await;
    let (_mgr, tool, ctx) = make_tool(base);
    let res = tool
        .execute(json!({"goal": "summarize this", "background": false}), &ctx)
        .await;
    match res {
        ToolResult::Text(s) => assert_eq!(s, "MOCK-SUMMARY"),
        other => panic!("expected Text summary, got: {other:?}"),
    }
}

/// (b) A failing child surfaces as the exact error string.
#[tokio::test]
async fn t007_blocking_single_failure_error_string() {
    let base = spawn_mock_server(MockMode::Unauthorized).await;
    let (_mgr, tool, ctx) = make_tool(base);
    let res = tool.execute(json!({"goal": "will fail"}), &ctx).await;
    match res {
        ToolResult::Error(e) => assert_eq!(
            e, "Subagent failed: subagent turn failed (fatal provider error)"
        ),
        other => panic!("expected Error, got: {other:?}"),
    }
}

/// (c) Batch delegate produces the exact multi-line plain-text report.
#[tokio::test]
async fn t007_blocking_batch_exact_format() {
    let base = spawn_mock_server(MockMode::Ok("MOCK-SUMMARY")).await;
    let (_mgr, tool, ctx) = make_tool(base);
    let res = tool
        .execute(
            json!({"tasks": [{"goal": "Task A"}, {"goal": "Task B"}]}),
            &ctx,
        )
        .await;
    let out = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected Text batch report, got: {other:?}"),
    };
    let expected = "[1/2] goal: \"Task A\"\n      status: success\n      summary: MOCK-SUMMARY\n      tokens: 150 | duration: N.Ns\n\n[2/2] goal: \"Task B\"\n      status: success\n      summary: MOCK-SUMMARY\n      tokens: 150 | duration: N.Ns\n";
    assert_eq!(
        normalize_durations(&out), expected,
        "batch output not byte-identical to the contract format:\n{out}"
    );
}

/// The batch report pins the byte-exact contract format; only the duration
/// VALUE is wall-clock dependent (a mock child can take 0.0-0.3s under
/// parallel full-suite load). Normalize each `duration: X.Xs` to `N.Ns`
/// while asserting the one-decimal rendering stays intact.
fn normalize_durations(out: &str) -> String {
    let mut normalized = String::with_capacity(out.len());
    for line in out.lines() {
        if let Some(idx) = line.find("duration: ") {
            let (head, tail) = line.split_at(idx + "duration: ".len());
            let end = tail
                .find('s')
                .unwrap_or_else(|| panic!("no 's' suffix in duration: {line:?}"));
            let val = &tail[..end];
            let parsed: f64 = val
                .parse()
                .unwrap_or_else(|_| panic!("non-numeric duration {val:?} in {line:?}"));
            assert_eq!(val, format!("{parsed:.1}"), "duration not one-decimal: {line:?}");
            normalized.push_str(head);
            normalized.push_str("N.Ns");
        } else {
            normalized.push_str(line);
        }
        normalized.push('\n');
    }
    normalized
}

/// (d) The blocking path actually blocks until the child completes: a child
/// whose provider responds only after 800 ms makes the tool take >= 700 ms.
#[tokio::test]
async fn t007_blocking_waits_for_child_completion() {
    let base = spawn_mock_server(MockMode::OkDelayed("SLOW-SUMMARY", 800)).await;
    let (_mgr, tool, ctx) = make_tool(base);
    let start = Instant::now();
    let res = tool.execute(json!({"goal": "slow child"}), &ctx).await;
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(700),
        "blocking path returned in {elapsed:?} before the child finished"
    );
    match res {
        ToolResult::Text(s) => assert_eq!(s, "SLOW-SUMMARY"),
        other => panic!("expected Text summary, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// T008 — background mode (FAIL-FIRST; passes after T009/T010)
// ---------------------------------------------------------------------------

/// (a) background=true returns in <2s with the exact handle line, the child
/// is visible in overview(), and it archives a terminal record on completion
/// (SC-001/FR-001).
#[tokio::test]
async fn t008_background_single_returns_handle_fast() {
    let base = spawn_mock_server(MockMode::OkDelayed("LATER", 1500)).await;
    let (mgr, tool, ctx) = make_tool(base);

    let start = Instant::now();
    let res = tool
        .execute(
            json!({"goal": "bg goal here", "background": true}),
            &ctx,
        )
        .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "background dispatch took {elapsed:?} (SC-001 budget is <2s)"
    );

    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected Text handle line, got: {other:?}"),
    };
    let id = parse_handle_line(&line, "bg goal here");
    assert_eq!(line, format!("[BACKGROUND] id={id} goal=bg goal here started"));

    // The child is registered (running) from submit time.
    assert!(
        mgr.overview().iter().any(|r| r.child_id == id),
        "handle id {id} not present in overview()"
    );

    // It completes in the background and archives a terminal record
    // exactly like a blocking child (FR-019 one-way history).
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let record = mgr.overview().into_iter().find(|r| r.child_id == id);
        if let Some(r) = record {
            if r.state.is_terminal() {
                match r.state {
                    DelegationState::Completed { result } => {
                        assert_eq!(result.goal, "bg goal here");
                        assert_eq!(result.summary, "LATER");
                    }
                    ref other => panic!("expected Completed, got {other:?}"),
                }
                break;
            }
        }
        assert!(Instant::now() < deadline, "child {id} never reached a terminal state");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// (b) FR-013: more background tasks than max_concurrent_children are all
/// accepted fast (none rejected), and overview() eventually shows all of
/// them — excess work queues under the same limits.
#[tokio::test]
async fn t008_background_overflow_queues_without_rejection() {
    const N: usize = 2; // max_concurrent_children
    const TOTAL: usize = N + 2;
    let base = spawn_mock_server(MockMode::OkDelayed("Q", 300)).await;
    let (mgr, tool, ctx) = make_tool_with(
        ManagerConfig {
            max_concurrent_children: N,
            max_concurrent_requests: 2,
            parent_reserved_permits: 0,
            ..Default::default()
        },
        base,
    );

    let submit_start = Instant::now();
    let mut ids: HashSet<String> = HashSet::new();
    for i in 0..TOTAL {
        let res = tool
            .execute(
                json!({"goal": format!("queued-{i}"), "background": true}),
                &ctx,
            )
            .await;
        let line = match res {
            ToolResult::Text(s) => s,
            ToolResult::Error(e) => panic!("background task {i} was REJECTED (FR-013): {e}"),
            other => panic!("expected Text handle line, got: {other:?}"),
        };
        let id = parse_handle_line(&line, &format!("queued-{i}"));
        assert!(ids.insert(id), "duplicate child id in handle: {line:?}");
    }
    let submit_elapsed = submit_start.elapsed();
    assert!(
        submit_elapsed < Duration::from_secs(2),
        "{TOTAL} background submissions took {submit_elapsed:?} — must accept fast"
    );

    // All children are visible (queued tasks included) right away.
    assert!(
        mgr.overview().len() >= TOTAL,
        "overview must list all {TOTAL} accepted tasks, got {}",
        mgr.overview().len()
    );

    // Every one eventually reaches a terminal state (queued work runs under
    // the same concurrency limits; none is dropped).
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let overview = mgr.overview();
        let terminal_ids: HashSet<String> = overview
            .iter()
            .filter(|r| r.state.is_terminal())
            .map(|r| r.child_id.clone())
            .collect();
        if ids.iter().all(|id| terminal_ids.contains(id)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "not all background children reached terminal state; ids={ids:?} overview={overview:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
