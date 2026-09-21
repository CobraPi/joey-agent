//! Feature 035 integration tests: orchestrator-steered browser explorer
//! (US1: designation + steering loop; US2: [PAGE-REPORT] discipline).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::types::DelegationState;
use joey_orchestration::{DelegateTask, ManagerConfig, SubagentControl, SubagentManager};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider on 127.0.0.1 — deterministic.
// ---------------------------------------------------------------------------

/// One scripted response per HTTP connection (popped in order).
#[derive(Clone)]
enum Step {
    /// 200 with a plain assistant text response, after `delay_ms`.
    Final {
        text: &'static str,
        delay_ms: u64,
    },
    /// 200 with a single function tool_call, after `delay_ms`.
    ToolCall {
        tool: &'static str,
        args: &'static str,
        delay_ms: u64,
    },
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

fn tool_call_body(tool: &str, args: &str) -> String {
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_mock_1",
                    "type": "function",
                    "function": {"name": tool, "arguments": args}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
    })
    .to_string()
}

/// Read one HTTP request (headers + content-length body); returns the raw body.
async fn read_http_body(stream: &mut TcpStream) -> Option<String> {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 256 * 1024 {
            return None;
        }
        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
        else {
            return None;
        };
        if n == 0 {
            return None;
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
    Some(String::from_utf8_lossy(&buf[header_end + 4..]).to_string())
}

/// Serve exactly one scripted request per connection, then close.
async fn serve_conn(
    mut stream: TcpStream,
    queue: Arc<Mutex<VecDeque<Step>>>,
    log: Arc<Mutex<Vec<String>>>,
) {
    let Some(body) = read_http_body(&mut stream).await else {
        return;
    };
    log.lock().unwrap().push(body);
    let step = queue.lock().unwrap().pop_front();
    let (status, body_out) = match step {
        Some(Step::Final { text, delay_ms }) => {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            ("200 OK", openai_body(text))
        }
        Some(Step::ToolCall { tool, args, delay_ms }) => {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            ("200 OK", tool_call_body(tool, args))
        }
        None => (
            "500 Internal Server Error",
            r#"{"error":{"message":"script exhausted"}}"#.to_string(),
        ),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_out}",
        body_out.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Bind a scripted mock provider; returns (base_url, request-body log).
async fn spawn_scripted_server(steps: Vec<Step>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    let log_for_accept = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let log = log_for_accept.clone();
            let queue = queue.clone();
            tokio::spawn(async move {
                serve_conn(stream, queue, log).await;
            });
        }
    });
    (format!("http://{addr}"), log)
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

/// Manager + delegate_task + subagent_control + context, all sharing one
/// registry.
fn make_harness(
    base_url: String,
) -> (
    Arc<SubagentManager>,
    DelegateTask,
    SubagentControl,
    ToolContext,
) {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let base = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "control-test");
    let delegate = DelegateTask::new(
        mgr.clone(),
        agent_config(base_url),
        Config::defaults(),
        base,
        None,
        None,
    );
    let control = SubagentControl::new(mgr.clone());
    (mgr, delegate, control, ctx)
}

/// Extract + parse the child id from a handle line, asserting its shape.
fn parse_handle_id(line: &str, goal: &str) -> u64 {
    let tail = format!(" goal={goal} started");
    let id = line
        .strip_prefix("[BACKGROUND] id=")
        .and_then(|rest| rest.strip_suffix(&tail))
        .unwrap_or_else(|| panic!("not a handle line: {line:?}"));
    id.parse::<u64>()
        .unwrap_or_else(|e| panic!("bad child id {id:?} in handle line: {e}"))
}

/// Poll the overview until `id` reaches a terminal state (15s deadline).
async fn wait_terminal(mgr: &SubagentManager, id: u64) -> joey_orchestration::DelegationOverview {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(rec) = mgr.child_status(id) {
            if rec.state.is_terminal() {
                return rec;
            }
        }
        assert!(
            Instant::now() < deadline,
            "child {id} never reached a terminal state"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Fetch a child's recent activity log via subagent_control (last 50).
async fn fetch_log(control: &SubagentControl, ctx: &ToolContext, id: u64) -> String {
    match control
        .execute(json!({"action": "log", "id": id, "last": 50}), ctx)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected log text for child {id}, got: {other:?}"),
    }
}

/// Poll the child's activity log until it contains `needle` (deadline in
/// seconds); panics with the last log text on timeout (timing guard).
async fn poll_log_contains(
    control: &SubagentControl,
    ctx: &ToolContext,
    id: u64,
    needle: &str,
    secs: u64,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let text = fetch_log(control, ctx, id).await;
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "child {id} log never contained {needle:?}; last log:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ---------------------------------------------------------------------------
// US1 (a): designation + steering loop end-to-end
// ---------------------------------------------------------------------------

/// Dispatch a browser explorer, verify the designation names the child,
/// steer it through the wait boundary (the steer text is returned as the
/// browser_explorer_wait tool result and so reaches the child's NEXT
/// provider request), see it complete, and see the registry auto-clear.
#[tokio::test]
async fn us1_designation_and_steering_loop() {
    // Request #1: call browser_explorer_wait with a valid success report
    // (blocks in the tool until the steer lands). Request #2: final text.
    let (base, log) = spawn_scripted_server(vec![
        Step::ToolCall {
            tool: "browser_explorer_wait",
            args: r#"{"report":{"instruction":"navigate to https://example.com","url":"https://example.com","title":"Example Domain","snapshot":"- heading: Example Domain","outcome":"success"},"timeout_secs":60}"#,
            delay_ms: 0,
        },
        Step::Final {
            text: "DONE-EXPLORER",
            delay_ms: 0,
        },
    ])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);

    let res = delegate
        .execute(
            json!({"goal": "explore example.com", "role": "explorer", "browser_explorer": true, "background": true}),
            &ctx,
        )
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "explore example.com");

    // The dispatch designated exactly this child.
    let active = mgr
        .browser_explorer_registry()
        .active()
        .unwrap_or_else(|| panic!("expected an active explorer designation for child {id}"));
    assert_eq!(active.child_id, id);
    assert_eq!(active.goal, "explore example.com");

    // Steer while the child is blocked inside browser_explorer_wait.
    let ack = control
        .execute(
            json!({"action": "steer", "id": id, "message": "click the Docs link"}),
            &ctx,
        )
        .await;
    match ack {
        ToolResult::Text(s) => assert_eq!(
            s,
            format!("steer queued for child {id}: delivered at next action boundary")
        ),
        other => panic!("expected steer ack text, got: {other:?}"),
    }

    // The child completes normally with the scripted final summary.
    let rec = wait_terminal(&mgr, id).await;
    match rec.state {
        DelegationState::Completed { result } => {
            assert_eq!(result.summary, "DONE-EXPLORER");
        }
        other => panic!("expected Completed after steer, got: {other:?}"),
    }

    // Terminal state auto-clears the designation.
    assert!(
        mgr.browser_explorer_registry().active().is_none(),
        "registry must auto-clear on terminal state"
    );

    // Delivery evidence: the steer text was returned at the wait boundary
    // into the SAME child — it appears in that child's later provider
    // request bodies (as the wait tool's result).
    let logged = log.lock().unwrap().clone();
    assert!(
        logged.len() >= 2,
        "expected >=2 provider requests, got {logged:?}"
    );
    let later = logged[1..].join("\n---REQ---\n");
    assert!(
        later.contains("click the Docs link"),
        "steered text not delivered into the child's later request(s):\n{later}"
    );
}

// ---------------------------------------------------------------------------
// US1 (b): single-active designation + plain arg gates
// ---------------------------------------------------------------------------

/// A second browser_explorer dispatch while one is active is rejected with
/// an error naming the active child and its goal; the cheap argument-gate
/// rejections (batch / missing role / foreground) hold in the same test.
#[tokio::test]
async fn us1_second_designation_rejected_naming_active_child() {
    let (base, _log) = spawn_scripted_server(vec![]).await;
    let (mgr, delegate, _control, ctx) = make_harness(base);

    // Pre-designate a standing explorer.
    mgr.browser_explorer_registry()
        .designate(99, "open example.com")
        .unwrap();

    // Second designation: rejected, naming the active child + goal.
    match delegate
        .execute(
            json!({"goal": "another sweep", "role": "explorer", "browser_explorer": true, "background": true}),
            &ctx,
        )
        .await
    {
        ToolResult::Error(e) => assert_eq!(
            e,
            "browser explorer already active: child 99 (goal: open example.com); steer it via subagent_control or stop it first"
        ),
        other => panic!("expected single-active Error, got: {other:?}"),
    }

    // Batch mode is rejected for browser_explorer.
    match delegate
        .execute(json!({"tasks": [{"goal": "a"}], "browser_explorer": true}), &ctx)
        .await
    {
        ToolResult::Error(e) => assert!(
            e.contains("browser_explorer is single-task mode only"),
            "batch gate error must say 'single-task mode only', got: {e}"
        ),
        other => panic!("expected batch gate Error, got: {other:?}"),
    }

    // Missing role: explorer is required.
    match delegate
        .execute(
            json!({"goal": "g", "browser_explorer": true, "background": true}),
            &ctx,
        )
        .await
    {
        ToolResult::Error(e) => assert!(
            e.contains("browser_explorer requires role: explorer"),
            "role gate error must name role explorer, got: {e}"
        ),
        other => panic!("expected role gate Error, got: {other:?}"),
    }

    // Foreground dispatch is rejected.
    match delegate
        .execute(
            json!({"goal": "g", "role": "explorer", "browser_explorer": true, "background": false}),
            &ctx,
        )
        .await
    {
        ToolResult::Error(e) => assert!(
            e.contains("browser explorer requires background dispatch"),
            "background gate error must require background, got: {e}"
        ),
        other => panic!("expected background gate Error, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// US1 (c): config-off rejects browser_explorer but leaves passthrough intact
// ---------------------------------------------------------------------------

/// With hypercode.browser_explorer.enabled=false the browser_explorer gate
/// rejects, but a NORMAL background delegation through the same tool and
/// config tree still dispatches and completes; the registry stays empty
/// throughout.
#[tokio::test]
async fn us1_config_off_rejects_but_passthrough_unchanged() {
    // Config tree with the capability disabled, from a temp config file.
    let cfg_path = std::env::temp_dir().join(format!(
        "joey_be_cfg_off_{}.yaml",
        std::process::id()
    ));
    std::fs::write(&cfg_path, "hypercode:\n  browser_explorer:\n    enabled: false\n").unwrap();
    let tree = Config::load_from(cfg_path.clone()).unwrap();
    let _ = std::fs::remove_file(&cfg_path);

    let (base, _log) =
        spawn_scripted_server(vec![Step::Final { text: "PLAIN-DONE", delay_ms: 0 }]).await;
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "control-test");
    let delegate = DelegateTask::new(
        mgr.clone(),
        agent_config(base),
        tree,
        ToolRegistry::with_builtins(),
        None,
        None,
    );

    // (a) browser_explorer delegation is rejected by the config gate.
    match delegate
        .execute(
            json!({"goal": "explore example.com", "role": "explorer", "browser_explorer": true, "background": true}),
            &ctx,
        )
        .await
    {
        ToolResult::Error(e) => assert_eq!(
            e,
            "browser explorer capability disabled by configuration"
        ),
        other => panic!("expected config-off Error, got: {other:?}"),
    }
    assert!(
        mgr.browser_explorer_registry().active().is_none(),
        "rejected dispatch must not designate"
    );

    // (b) a plain background delegation through the SAME tool + tree still
    // dispatches, completes, and never touches the registry.
    let res = delegate
        .execute(json!({"goal": "plain work", "background": true}), &ctx)
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line for plain dispatch, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "plain work");
    assert!(
        mgr.browser_explorer_registry().active().is_none(),
        "plain dispatch must not designate an explorer"
    );

    let rec = wait_terminal(&mgr, id).await;
    match rec.state {
        DelegationState::Completed { result } => {
            assert_eq!(result.summary, "PLAIN-DONE");
        }
        other => panic!("expected Completed for plain dispatch, got: {other:?}"),
    }
    assert!(
        mgr.browser_explorer_registry().active().is_none(),
        "registry must stay empty for plain delegations"
    );
}

// ---------------------------------------------------------------------------
// US1 (d): steering a finished child errors explicitly
// ---------------------------------------------------------------------------

/// Steer on an already-finished (plain, non-explorer) background child is a
/// tool error containing "already finished" — never silent.
#[tokio::test]
async fn us1_steer_after_finish_errors_explicitly() {
    let (base, _log) = spawn_scripted_server(vec![Step::Final {
        text: "DONE-PLAIN",
        delay_ms: 0,
    }])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);

    let res = delegate
        .execute(json!({"goal": "finish-plain", "background": true}), &ctx)
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "finish-plain");
    wait_terminal(&mgr, id).await;

    match control
        .execute(
            json!({"action": "steer", "id": id, "message": "too late"}),
            &ctx,
        )
        .await
    {
        ToolResult::Error(e) => assert!(
            e.contains("already finished"),
            "steer-after-finish error must say 'already finished', got: {e}"
        ),
        other => panic!("expected Error for steer on finished child, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// US2: [PAGE-REPORT] discipline + seq traceability
// ---------------------------------------------------------------------------

/// Two steered steps produce exactly two [PAGE-REPORT] blocks with seq 1
/// and 2; a failure report carries its error line BEFORE the unchanged-page
/// snapshot payload; every instruction field echoes one of the steered
/// texts (SC-003 traceability).
#[tokio::test]
async fn us2_report_discipline_and_seq_traceability() {
    let report_a = r#"{"report":{"instruction":"navigate to https://example.com","url":"https://example.com","title":"Example Domain","snapshot":"- heading: Example Domain","outcome":"success"},"timeout_secs":60}"#;
    let report_b = r#"{"report":{"instruction":"click the missing link","url":"https://example.com","title":"Example Domain","snapshot":"- link: More information...","outcome":"failure","error":"no element matching 'missing link'"},"timeout_secs":60}"#;
    let (base, _log) = spawn_scripted_server(vec![
        Step::ToolCall {
            tool: "browser_explorer_wait",
            args: report_a,
            delay_ms: 0,
        },
        Step::ToolCall {
            tool: "browser_explorer_wait",
            args: report_b,
            delay_ms: 0,
        },
        Step::Final {
            text: "FIN",
            delay_ms: 0,
        },
    ])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);

    let res = delegate
        .execute(
            json!({"goal": "explore example.com", "role": "explorer", "browser_explorer": true, "background": true}),
            &ctx,
        )
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "explore example.com");

    // Steer 1 lands while the child blocks in wait#1 (report A recorded).
    let ack1 = control
        .execute(
            json!({"action": "steer", "id": id, "message": "navigate to https://example.com"}),
            &ctx,
        )
        .await;
    assert!(matches!(ack1, ToolResult::Text(_)), "steer 1 failed: {ack1:?}");

    // Wait until wait#1 returned the steer and the model entered wait#2
    // (report B, seq=2, recorded) — only then deliver steer 2, so the two
    // steers cannot overwrite each other in the single steer slot.
    let _ = poll_log_contains(
        &control,
        &ctx,
        id,
        &format!("[PAGE-REPORT] child={id} seq=2"),
        10,
    )
    .await;
    let ack2 = control
        .execute(
            json!({"action": "steer", "id": id, "message": "click the missing link"}),
            &ctx,
        )
        .await;
    assert!(matches!(ack2, ToolResult::Text(_)), "steer 2 failed: {ack2:?}");

    // Terminal state (15s timing guard; log text on failure aids debugging).
    let deadline = Instant::now() + Duration::from_secs(15);
    let rec = loop {
        if let Some(rec) = mgr.child_status(id) {
            if rec.state.is_terminal() {
                break rec;
            }
        }
        assert!(
            Instant::now() < deadline,
            "child {id} never reached a terminal state; log:\n{}",
            fetch_log(&control, &ctx, id).await
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    match rec.state {
        DelegationState::Completed { result } => {
            assert_eq!(result.summary, "FIN");
        }
        other => panic!("expected Completed, got: {other:?}"),
    }

    // Read the recorder ring via subagent_control.
    let log_text = fetch_log(&control, &ctx, id).await;

    // Exactly TWO [PAGE-REPORT] blocks for this child (the ring also holds
    // spawn/turn/tool lines — only block starts count).
    let lines: Vec<&str> = log_text.lines().collect();
    let block_starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(&format!("[PAGE-REPORT] child={id} seq=")))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        block_starts.len(),
        2,
        "expected exactly 2 [PAGE-REPORT] blocks, got {}:\n{log_text}",
        block_starts.len()
    );

    // A block spans from its start line to the next '#'-prefixed ring line
    // (multi-line ring entries carry the #N prefix only on their first line).
    let block_end = |start: usize| -> usize {
        lines[start + 1..]
            .iter()
            .position(|l| l.starts_with('#'))
            .map(|p| start + 1 + p)
            .unwrap_or(lines.len())
    };
    let block1 = &lines[block_starts[0]..block_end(block_starts[0])];
    let block2 = &lines[block_starts[1]..block_end(block_starts[1])];

    // Block 1: seq=1, success, full field set, complete snapshot payload.
    assert!(
        block1[0].contains(&format!("[PAGE-REPORT] child={id} seq=1")),
        "block 1 header wrong: {block1:?}"
    );
    assert!(
        block1.iter().any(|l| l.contains("instruction: navigate to https://example.com")),
        "block 1 instruction missing/wrong: {block1:?}"
    );
    assert!(
        block1.iter().any(|l| l.contains("outcome: success")),
        "block 1 outcome missing/wrong: {block1:?}"
    );
    assert!(
        block1.iter().any(|l| l.contains("url: https://example.com")),
        "block 1 url missing/wrong: {block1:?}"
    );
    assert!(
        block1.iter().any(|l| l.contains("title: Example Domain")),
        "block 1 title missing/wrong: {block1:?}"
    );
    assert!(
        block1.iter().any(|l| l.contains("- heading: Example Domain")),
        "block 1 snapshot payload missing: {block1:?}"
    );

    // Block 2: seq=2, failure with the error line BEFORE the snapshot payload.
    assert!(
        block2[0].contains(&format!("[PAGE-REPORT] child={id} seq=2")),
        "block 2 header wrong: {block2:?}"
    );
    assert!(
        block2.iter().any(|l| l.contains("instruction: click the missing link")),
        "block 2 instruction missing/wrong: {block2:?}"
    );
    assert!(
        block2.iter().any(|l| l.contains("outcome: failure")),
        "block 2 outcome missing/wrong: {block2:?}"
    );
    let err_pos = block2
        .iter()
        .position(|l| l.contains("error: no element matching 'missing link'"))
        .unwrap_or_else(|| panic!("block 2 missing error line: {block2:?}"));
    let snap_pos = block2
        .iter()
        .position(|l| l.contains("- link: More information..."))
        .unwrap_or_else(|| panic!("block 2 missing unchanged-page snapshot: {block2:?}"));
    assert!(
        err_pos < snap_pos,
        "failure error must precede the unchanged-page snapshot: {block2:?}"
    );

    // SC-003 traceability: every instruction echoes one of the steered texts.
    for steered in ["navigate to https://example.com", "click the missing link"] {
        assert!(
            log_text.contains(&format!("instruction: {steered}")),
            "SC-003: no report echoes steered instruction {steered:?}:\n{log_text}"
        );
    }
}
