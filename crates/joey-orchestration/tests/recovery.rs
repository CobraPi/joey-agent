//! Feature 020 addendum: subagent self-recovery from fatal provider errors.
//!
//! A child turn that dies with `TurnResult::fatal_provider_error` (401 →
//! Auth → non-retryable, empty fallback chain) is re-run with a clean
//! history up to `delegation.subagent_recovery_attempts` times. These tests
//! pin: recovery success, the 0-attempts legacy failure string, exhaustion
//! after N attempts, and usage/iteration accumulation across attempts.

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
// Scripted mock OpenAI-compatible provider: serves responses in order
// (counter shared across connections, clamped to the last step) — the same
// pattern as tests/budgets.rs, so attempt N of a recovery loop can be made
// deterministic.
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Step {
    Ok(&'static str),
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

/// Serve exactly one HTTP request per the step, then close.
async fn serve_conn(mut stream: TcpStream, step: Step) {
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

    let (status, body) = match step {
        Step::Ok(text) => ("200 OK", openai_body(text)),
        Step::Unauthorized => (
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

/// Bind a scripted mock provider on 127.0.0.1:0; returns its base URL.
/// Request i gets `script[i]` (clamped to the last step).
async fn spawn_scripted_server(script: Vec<Step>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let script = script.clone();
            let counter = counter.clone();
            tokio::spawn(async move {
                let idx = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let step = script[idx.min(script.len() - 1)].clone();
                serve_conn(stream, step).await;
            });
        }
    });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Harness (mirrors tests/background.rs)
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

fn make_tool_with(
    mgr_config: ManagerConfig,
    base_url: String,
) -> (std::sync::Arc<SubagentManager>, DelegateTask, ToolContext) {
    let mgr = std::sync::Arc::new(SubagentManager::new(mgr_config));
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "recovery-test");
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
// Tests
// ---------------------------------------------------------------------------

/// One fatal provider error, then success: the child recovers and the
/// blocking path returns the recovered summary verbatim.
#[tokio::test]
async fn recovery_recovers_after_fatal_provider_error() {
    let base = spawn_scripted_server(vec![Step::Unauthorized, Step::Ok("RECOVERED")]).await;
    let (_mgr, tool, ctx) = make_tool_with(
        ManagerConfig {
            subagent_recovery_attempts: 1,
            ..Default::default()
        },
        base,
    );
    let res = tool.execute(json!({"goal": "recover me"}), &ctx).await;
    match res {
        ToolResult::Text(s) => assert_eq!(s, "RECOVERED"),
        other => panic!("expected recovered Text summary, got: {other:?}"),
    }
}

/// 0 attempts = pre-feature behavior: the exact legacy failure string.
#[tokio::test]
async fn recovery_disabled_keeps_legacy_failure() {
    let base = spawn_scripted_server(vec![Step::Unauthorized]).await;
    let (_mgr, tool, ctx) = make_tool_with(
        ManagerConfig {
            subagent_recovery_attempts: 0,
            ..Default::default()
        },
        base,
    );
    let res = tool.execute(json!({"goal": "will fail"}), &ctx).await;
    match res {
        ToolResult::Error(e) => assert_eq!(
            e, "Subagent failed: subagent turn failed (fatal provider error)"
        ),
        other => panic!("expected Error, got: {other:?}"),
    }
}

/// Attempts exhausted: still the exact legacy failure string.
#[tokio::test]
async fn recovery_exhausted_reports_fatal_failure() {
    let base = spawn_scripted_server(vec![Step::Unauthorized, Step::Unauthorized]).await;
    let (_mgr, tool, ctx) = make_tool_with(
        ManagerConfig {
            subagent_recovery_attempts: 1,
            ..Default::default()
        },
        base,
    );
    let res = tool.execute(json!({"goal": "keeps failing"}), &ctx).await;
    match res {
        ToolResult::Error(e) => assert_eq!(
            e, "Subagent failed: subagent turn failed (fatal provider error)"
        ),
        other => panic!("expected Error, got: {other:?}"),
    }
}

/// Usage and iterations accumulate across attempts: the fatal attempt
/// burned 1 API call, the recovered attempt another — the archived
/// DelegationResult reports both (150 tokens come from the successful
/// attempt only; the 401 carries no usage).
#[tokio::test]
async fn recovery_accumulates_iterations_across_attempts() {
    let base = spawn_scripted_server(vec![Step::Unauthorized, Step::Ok("RECOVERED")]).await;
    let (mgr, tool, ctx) = make_tool_with(
        ManagerConfig {
            subagent_recovery_attempts: 1,
            ..Default::default()
        },
        base,
    );
    let res = tool
        .execute(json!({"goal": "accumulate", "background": true}), &ctx)
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected Text handle line, got: {other:?}"),
    };
    let id = parse_handle_line(&line, "accumulate");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let record = mgr.overview().into_iter().find(|r| r.child_id == id);
        if let Some(r) = record {
            if r.state.is_terminal() {
                match r.state {
                    DelegationState::Completed { result } => {
                        assert_eq!(result.summary, "RECOVERED");
                        assert_eq!(result.iterations, 2, "1 fatal attempt + 1 recovered attempt");
                        assert_eq!(result.token_usage.total_tokens, 150);
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
