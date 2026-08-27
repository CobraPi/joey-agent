//! Feature 020, User Story 3 (T014 fail-first / T015): `subagent_control`.
//!
//! Drives the REAL tool + REAL manager plumbing against a scripted local
//! OpenAI-compatible HTTP server (same harness style as tests/background.rs,
//! extended with a per-connection response script + request-body log so steer
//! delivery is observable at the provider boundary):
//!   (a) steer is delivered before the child's next action — the steered
//!       text appears in the child's SECOND provider request (injected into
//!       the tool result via the out-of-band marker), FR-008
//!   (b) stop yields a partial result with the stop reason recorded —
//!       FR-010: the awaited DelegationResult carries the partial summary and
//!       the registry's terminal record is Stopped{OrchestratorRequested}
//!       (plus the SubagentStopped tap event)
//!   (c) selective stop SC-003: stop one of >=3 running children — only it
//!       stops, the siblings run to completion, FR-009
//!   (d) steer/stop on an already-finished child -> tool error containing
//!       "already finished" (spec edge cases)
//!   (e) unknown id -> tool error naming the id (spec edge cases)
//! plus tool-surface tests (action validation, id parsing, schema, and
//! registration via register_orchestration).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_orchestration::types::DelegationState;
use joey_orchestration::{
    DelegateTask, DelegationRequest, ManagerConfig, SubagentControl, SubagentManager, StopReason,
};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
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
// A trivial tool the scripted child can call (registered into the base
// registry so the child's schema/dispatch both know it).
// ---------------------------------------------------------------------------

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo_tool"
    }
    fn toolset(&self) -> &str {
        "coding"
    }
    fn description(&self) -> &str {
        "Echo a note back (test fixture)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"note": {"type": "string"}},
            "required": ["note"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let note = args
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or("(none)");
        ToolResult::Text(format!("echo ok: {note}"))
    }
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
/// registry that contains the echo_tool fixture.
fn make_harness(
    base_url: String,
) -> (
    Arc<SubagentManager>,
    DelegateTask,
    SubagentControl,
    ToolContext,
) {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));
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

// ---------------------------------------------------------------------------
// (a) Steer is delivered before the child's next action (FR-008)
// ---------------------------------------------------------------------------

/// Steer a slow scripted child right after spawn; the steered text must show
/// up in the child's SECOND provider request (not the first), wrapped in the
/// out-of-band user-message marker, and the child finishes normally.
#[tokio::test]
async fn t014a_steer_delivered_before_next_action() {
    // Request #1: sleep 700ms then call echo_tool (keeps the child running).
    // Request #2: final text (carries the injected steer in its messages).
    let (base, log) = spawn_scripted_server(vec![
        Step::ToolCall {
            tool: "echo_tool",
            args: r#"{"note":"one"}"#,
            delay_ms: 700,
        },
        Step::Final {
            text: "DONE-STEERED",
            delay_ms: 0,
        },
    ])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);

    let res = delegate
        .execute(json!({"goal": "steer-target", "background": true}), &ctx)
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "steer-target");

    // Steer while the child is still inside its first provider call.
    let ack = control
        .execute(
            json!({"action": "steer", "id": id, "message": "CORRECTION: PAINT IT BLUE"}),
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

    // The child completes normally (a steer never stops it).
    let rec = wait_terminal(&mgr, id).await;
    match rec.state {
        DelegationState::Completed { result } => {
            assert_eq!(result.summary, "DONE-STEERED");
        }
        other => panic!("expected Completed after steer, got {other:?}"),
    }

    // Delivery evidence: the steered text appears in a request AFTER the
    // first, inside the out-of-band marker (delivered before the next action).
    let logged = log.lock().unwrap().clone();
    assert!(logged.len() >= 2, "expected >=2 provider requests, got {logged:?}");
    let later = logged[1..].join("\n---REQ---\n");
    assert!(
        later.contains("PAINT IT BLUE"),
        "steered text not delivered in later request(s):\n{later}"
    );
    assert!(
        later.contains("OUT-OF-BAND USER MESSAGE"),
        "steer marker missing in later request(s):\n{later}"
    );
}

// ---------------------------------------------------------------------------
// (b) Stop yields a partial result with the stop reason (FR-010)
// ---------------------------------------------------------------------------

/// Stop a running BLOCKING child via the tool; the awaited DelegationResult
/// carries a non-empty partial summary, the terminal record is
/// Stopped{OrchestratorRequested}, and the SubagentStopped tap event fires
/// with the reason + summary preview.
#[tokio::test]
async fn t014b_stop_yields_partial_result_with_stop_reason() {
    let (base, _log) = spawn_scripted_server(vec![Step::Final {
        text: "PARTIAL-B",
        delay_ms: 1500,
    }])
    .await;
    let (mgr, _delegate, control, ctx) = make_harness(base);

    // Tap the manager's events to learn the blocking child's id.
    let (tap_tx, mut tap_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    mgr.set_event_tap(Some(tap_tx));

    // Blocking dispatch in a side task so we can stop it mid-run and still
    // await its DelegationResult.
    let mgr_for_task = mgr.clone();
    let cfg = agent_config(format!("http://unused")); // replaced below
    let _ = cfg;
    let (base2, _log2) = spawn_scripted_server(vec![Step::Final {
        text: "PARTIAL-B",
        delay_ms: 1500,
    }])
    .await;
    let cfg = agent_config(base2);
    let mut base_reg = ToolRegistry::new();
    base_reg.register(Arc::new(EchoTool));
    let req = DelegationRequest::single("stop-target");
    let task_cfg = cfg;
    let task_tree = Config::defaults();
    let task_reg = base_reg;
    let handle = tokio::spawn(async move {
        mgr_for_task
            .dispatch_single(&req, &task_cfg, &task_tree, &task_reg, None)
            .await
    });

    // Wait for the spawn event, then for registration to land.
    let mut id: Option<u64> = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while id.is_none() {
        match tokio::time::timeout(Duration::from_millis(2000), tap_rx.recv()).await {
            Ok(Some(AgentEvent::SubagentSpawn { id: sid, goal, .. })) if goal == "stop-target" => {
                id = Some(sid);
            }
            Ok(Some(_)) => continue,
            Ok(None) => panic!("tap closed before spawn event"),
            Err(_) => panic!("timed out waiting for SubagentSpawn event"),
        }
        assert!(Instant::now() < deadline, "no spawn event in 5s");
    }
    let id = id.unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while mgr.child_status(id).is_none() {
        assert!(Instant::now() < deadline, "child {id} never registered");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Stop it via the tool (FR-009) — exact ack text.
    let ack = control
        .execute(json!({"action": "stop", "id": id}), &ctx)
        .await;
    match ack {
        ToolResult::Text(s) => assert_eq!(
            s,
            format!("stop requested for child {id} (reason: orchestrator-requested)")
        ),
        other => panic!("expected stop ack text, got: {other:?}"),
    }

    // Await the child's DelegationResult: a partial summary is present.
    let result = handle.await.expect("dispatch task panicked");
    assert!(
        !result.summary.is_empty(),
        "stopped child must yield a (partial) summary, got empty"
    );

    // FR-010: the stop reason is recorded on the terminal partial result.
    let rec = wait_terminal(&mgr, id).await;
    match rec.state {
        DelegationState::Stopped { reason } => assert_eq!(
            reason,
            StopReason::OrchestratorRequested,
            "wrong stop reason on terminal record"
        ),
        other => panic!("expected Stopped terminal record, got {other:?}"),
    }

    // And the SubagentStopped event carries the reason + a summary preview.
    let mut saw_stopped = false;
    while let Ok(Some(ev)) =
        tokio::time::timeout(Duration::from_millis(1000), tap_rx.recv()).await
    {
        if let AgentEvent::SubagentStopped {
            id: sid,
            reason,
            summary_preview,
            ..
        } = ev
        {
            if sid == id {
                assert_eq!(reason, "orchestrator_requested");
                assert!(!summary_preview.is_empty());
                saw_stopped = true;
                break;
            }
        }
    }
    assert!(saw_stopped, "SubagentStopped event never arrived for child {id}");
}

// ---------------------------------------------------------------------------
// (c) Selective stop — SC-003 / FR-009
// ---------------------------------------------------------------------------

/// Three background children run; stopping the middle one leaves the other
/// two to complete normally with their summaries.
#[tokio::test]
async fn t014c_selective_stop_only_target_stops() {
    // Three identical delayed finals: any mapping of connection to child is
    // fine because every survivor must see the same response.
    let (base, _log) = spawn_scripted_server(vec![
        Step::Final {
            text: "DONE-SIBLING",
            delay_ms: 900,
        };
        3
    ])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);

    let mut ids = Vec::new();
    for i in 0..3 {
        let res = delegate
            .execute(
                json!({"goal": format!("sc003-{i}"), "background": true}),
                &ctx,
            )
            .await;
        let line = match res {
            ToolResult::Text(s) => s,
            other => panic!("expected handle line, got: {other:?}"),
        };
        ids.push(parse_handle_id(&line, &format!("sc003-{i}")));
    }
    let (a, b, c) = (ids[0], ids[1], ids[2]);

    // Stop only the middle child.
    let ack = control
        .execute(json!({"action": "stop", "id": b}), &ctx)
        .await;
    assert!(matches!(ack, ToolResult::Text(_)), "stop ack failed: {ack:?}");

    // The stopped child: Stopped{OrchestratorRequested}.
    let rec_b = wait_terminal(&mgr, b).await;
    match rec_b.state {
        DelegationState::Stopped { reason } => assert_eq!(reason, StopReason::OrchestratorRequested),
        other => panic!("expected middle child Stopped, got {other:?}"),
    }

    // The siblings: complete normally with their summaries (SC-003).
    for sid in [a, c] {
        let rec = wait_terminal(&mgr, sid).await;
        match rec.state {
            DelegationState::Completed { result } => {
                assert_eq!(result.summary, "DONE-SIBLING");
            }
            other => panic!("expected sibling {sid} Completed, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// (d) Already-finished children respond gracefully
// ---------------------------------------------------------------------------

/// Steer + stop on a completed child return tool errors containing
/// "already finished" (spec edge case: graceful, never a panic).
#[tokio::test]
async fn t014d_already_finished_errors() {
    let (base, _log) = spawn_scripted_server(vec![Step::Final {
        text: "DONE-FAST",
        delay_ms: 0,
    }])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);

    let res = delegate
        .execute(json!({"goal": "finish-fast", "background": true}), &ctx)
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "finish-fast");
    wait_terminal(&mgr, id).await;

    let steer = control
        .execute(
            json!({"action": "steer", "id": id, "message": "too late"}),
            &ctx,
        )
        .await;
    match steer {
        ToolResult::Error(e) => assert!(
            e.contains("already finished"),
            "steer error must say 'already finished', got: {e}"
        ),
        other => panic!("expected Error for steer on finished child, got: {other:?}"),
    }

    let stop = control.execute(json!({"action": "stop", "id": id}), &ctx).await;
    match stop {
        ToolResult::Error(e) => assert!(
            e.contains("already finished"),
            "stop error must say 'already finished', got: {e}"
        ),
        other => panic!("expected Error for stop on finished child, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// (e) Unknown ids produce clear errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t014e_unknown_id_errors() {
    let (base, _log) = spawn_scripted_server(vec![]).await;
    let (_mgr, _delegate, control, ctx) = make_harness(base);

    let steer = control
        .execute(
            json!({"action": "steer", "id": 424242, "message": "hello?"}),
            &ctx,
        )
        .await;
    match steer {
        ToolResult::Error(e) => assert!(
            e.contains("No subagent with id 424242"),
            "steer unknown-id error must name the id, got: {e}"
        ),
        other => panic!("expected Error for steer on unknown id, got: {other:?}"),
    }

    let stop = control
        .execute(json!({"action": "stop", "id": 424243}), &ctx)
        .await;
    match stop {
        ToolResult::Error(e) => assert!(
            e.contains("No subagent with id 424243"),
            "stop unknown-id error must name the id, got: {e}"
        ),
        other => panic!("expected Error for stop on unknown id, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tool surface: validation, id parsing, schema, registration
// ---------------------------------------------------------------------------

/// Steer without a message, missing ids, and unknown actions all produce
/// clear tool errors (contract: "never panics").
#[tokio::test]
async fn t015_action_and_argument_validation() {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let control = SubagentControl::new(mgr.clone());
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "control-test");

    // steer without message
    match control
        .execute(json!({"action": "steer", "id": 1}), &ctx)
        .await
    {
        ToolResult::Error(e) => assert!(e.contains("message"), "got: {e}"),
        other => panic!("expected Error for steer without message, got: {other:?}"),
    }

    // missing id
    match control
        .execute(json!({"action": "stop"}), &ctx)
        .await
    {
        ToolResult::Error(e) => assert!(e.contains("id"), "got: {e}"),
        other => panic!("expected Error for stop without id, got: {other:?}"),
    }

    // missing action
    match control.execute(json!({"id": 1}), &ctx).await {
        ToolResult::Error(e) => assert!(e.contains("action"), "got: {e}"),
        other => panic!("expected Error for missing action, got: {other:?}"),
    }

    // unknown action (list/status/log/wait are implemented; pause is not)
    match control
        .execute(json!({"action": "pause"}), &ctx)
        .await
    {
        ToolResult::Error(e) => assert!(e.contains("Unknown action"), "got: {e}"),
        other => panic!("expected Error for unknown action, got: {other:?}"),
    }

    // wait without ids
    match control.execute(json!({"action": "wait"}), &ctx).await {
        ToolResult::Error(e) => assert!(e.contains("ids"), "got: {e}"),
        other => panic!("expected Error for wait without ids, got: {other:?}"),
    }

    // invalid last / timeout_secs values
    match control
        .execute(json!({"action": "log", "id": 1, "last": 0}), &ctx)
        .await
    {
        ToolResult::Error(e) => assert!(e.contains("last"), "got: {e}"),
        other => panic!("expected Error for last=0, got: {other:?}"),
    }
    match control
        .execute(json!({"action": "wait", "ids": [1], "timeout_secs": 0}), &ctx)
        .await
    {
        ToolResult::Error(e) => assert!(e.contains("timeout_secs"), "got: {e}"),
        other => panic!("expected Error for timeout_secs=0, got: {other:?}"),
    }

    // non-numeric id string
    match control
        .execute(json!({"action": "stop", "id": "not-a-number"}), &ctx)
        .await
    {
        ToolResult::Error(e) => assert!(e.contains("id"), "got: {e}"),
        other => panic!("expected Error for bad id, got: {other:?}"),
    }
}

/// Numeric-string ids are accepted (the handle line prints the id bare, and
/// models copy ids as strings).
#[tokio::test]
async fn t015_numeric_string_id_accepted() {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let control = SubagentControl::new(mgr);
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "control-test");

    match control
        .execute(json!({"action": "stop", "id": "424244"}), &ctx)
        .await
    {
        // Must reach the manager (unknown-id error), not an args error.
        ToolResult::Error(e) => assert!(e.contains("No subagent with id 424244"), "got: {e}"),
        other => panic!("expected unknown-id Error for string id, got: {other:?}"),
    }
}

/// The schema exposes exactly the implemented actions (US3 steer/stop +
/// US4 list/status/log/wait) and documents the US4 params.
#[test]
fn t015_schema_documents_actions() {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let control = SubagentControl::new(mgr);
    let params = control.parameters();
    let action = &params["properties"]["action"];
    assert_eq!(
        action["enum"],
        json!(["steer", "stop", "list", "status", "log", "wait"])
    );
    assert!(action["description"].as_str().unwrap_or("").len() > 10);
    assert_eq!(params["properties"]["id"]["type"], json!("integer"));
    assert_eq!(
        params["properties"]["message"]["type"],
        json!("string")
    );
    // US4 params: last (log), ids + timeout_secs (wait).
    assert_eq!(params["properties"]["last"]["type"], json!("integer"));
    assert_eq!(params["properties"]["ids"]["type"], json!("array"));
    assert_eq!(params["properties"]["timeout_secs"]["type"], json!("integer"));
    let required = params["required"].as_array().unwrap();
    assert!(required.contains(&json!("action")));
    // Tool metadata.
    assert_eq!(control.name(), "subagent_control");
    assert_eq!(control.toolset(), "delegation");
}

/// register_orchestration registers subagent_control alongside delegate_task.
#[test]
fn t015_registration_via_register_orchestration() {
    let mut registry = ToolRegistry::new();
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));
    joey_orchestration::register_orchestration(
        &mut registry,
        mgr,
        agent_config("http://127.0.0.1:9".to_string()),
        Config::defaults(),
        base,
        None,
    );
    let tool = registry
        .get("subagent_control")
        .expect("subagent_control must be registered by register_orchestration");
    assert_eq!(tool.toolset(), "delegation");
    assert!(registry.get("delegate_task").is_some());
}

// ---------------------------------------------------------------------------
// User Story 4 (T017 fail-first / T018): list / status / log / wait
// ---------------------------------------------------------------------------

/// Format helper mirroring the tool's elapsed rendering ("<n>s").
fn fmt_secs(d: Duration) -> String {
    format!("{}s", d.as_secs())
}

/// (a) list: one line per child with id, truncated goal, state, elapsed,
/// tokens (FR-005). Two children: one running, one completed — the
/// session-lifetime overview includes BOTH (FR-019).
#[tokio::test]
async fn t017a_list_shows_children_with_line_fields() {
    // Child 1 completes fast; child 2 runs 1.2s — list is taken while it
    // still runs, so both a terminal and a running record are visible.
    let (base, _log) = spawn_scripted_server(vec![
        Step::Final {
            text: "DONE-LIST",
            delay_ms: 50,
        },
        Step::Final {
            text: "DONE-SLOW",
            delay_ms: 1200,
        },
    ])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);

    let l1 = match delegate
        .execute(json!({"goal": "list-done", "background": true}), &ctx)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected handle, got: {other:?}"),
    };
    let id1 = parse_handle_id(&l1, "list-done");
    let l2 = match delegate
        .execute(json!({"goal": "list-slow", "background": true}), &ctx)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected handle, got: {other:?}"),
    };
    let id2 = parse_handle_id(&l2, "list-slow");
    wait_terminal(&mgr, id1).await;

    let res = control.execute(json!({"action": "list"}), &ctx).await;
    let text = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected list text, got: {other:?}"),
    };
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 2, "list must show >=2 children, got: {text}");

    // Each line carries the exact fields: id, goal=, state=, elapsed=, tokens=.
    for line in &lines {
        assert!(line.contains("id="), "line missing id=: {line}");
        assert!(line.contains("goal="), "line missing goal=: {line}");
        assert!(line.contains("state="), "line missing state=: {line}");
        assert!(line.contains("elapsed="), "line missing elapsed=: {line}");
        assert!(line.contains("tokens="), "line missing tokens=: {line}");
    }

    // The completed child's line shows state=completed; the running one
    // shows state=running (FR-016 distinguishable states).
    let line1 = lines.iter().find(|l| l.contains(&format!("id={id1} ")))
        .unwrap_or_else(|| panic!("no line for child {id1} in: {text}"));
    assert!(line1.contains("state=completed"), "got: {line1}");
    let line2 = lines.iter().find(|l| l.contains(&format!("id={id2} ")))
        .unwrap_or_else(|| panic!("no line for child {id2} in: {text}"));
    assert!(line2.contains("state=running"), "got: {line2}");
    assert!(line2.contains("goal=list-slow"), "got: {line2}");

    // Long goals are truncated in the list line (contract: goal truncated).
    let (base3, _l3) = spawn_scripted_server(vec![Step::Final {
        text: "X",
        delay_ms: 600,
    }])
    .await;
    let (mgr3, delegate3, control3, ctx3) = make_harness(base3);
    let long_goal = "g".repeat(200);
    let l3 = match delegate3
        .execute(json!({"goal": long_goal, "background": true}), &ctx3)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected handle, got: {other:?}"),
    };
    let id3 = parse_handle_id(
        &l3,
        &format!("{}", "g".repeat(200)),
    );
    let _ = mgr3;
    let res3 = control3.execute(json!({"action": "list"}), &ctx3).await;
    let text3 = match res3 {
        ToolResult::Text(s) => s,
        other => panic!("expected list text, got: {other:?}"),
    };
    let line3 = text3
        .lines()
        .find(|l| l.contains(&format!("id={id3} ")))
        .unwrap_or_else(|| panic!("no line for child {id3} in: {text3}"));
    assert!(
        line3.chars().count() < 120,
        "long goal must be truncated in list line: {line3}"
    );
}

/// (b) status: single record detail incl. cumulative usage (FR-012).
#[tokio::test]
async fn t017b_status_returns_single_record() {
    let (base, _log) = spawn_scripted_server(vec![Step::Final {
        text: "DONE-STATUS",
        delay_ms: 400,
    }])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);
    let line = match delegate
        .execute(json!({"goal": "status-target", "background": true}), &ctx)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected handle, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "status-target");

    // While running.
    let res = control
        .execute(json!({"action": "status", "id": id}), &ctx)
        .await;
    let text = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected status text, got: {other:?}"),
    };
    assert!(text.contains(&format!("id={id}")), "got: {text}");
    assert!(text.contains("goal=status-target"), "got: {text}");
    assert!(text.contains("state=running"), "got: {text}");
    assert!(text.contains("tokens="), "got: {text}");

    // After completion: state=completed, usage fields present, and the
    // summary rides along (result detail beyond the list line).
    let rec = wait_terminal(&mgr, id).await;
    let res = control
        .execute(json!({"action": "status", "id": id}), &ctx)
        .await;
    let text = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected status text, got: {other:?}"),
    };
    assert!(text.contains("state=completed"), "got: {text}");
    assert!(text.contains("tokens=150"), "got: {text}"); // mock usage
    assert!(text.contains("DONE-STATUS"), "summary missing: {text}");
    assert!(text.contains(&fmt_secs(rec.elapsed)), "elapsed missing: {text}");
}

/// (c) log: bounded recent-activity slice (FR-006) — last defaults to 10,
/// `last=N` bounds it further, and the output never grows with transcript
/// length.
#[tokio::test]
async fn t017c_log_bounded_recent_activity() {
    // A child that makes several tool calls → a stream of tap events.
    let steps = vec![
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"1"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"2"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"3"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"4"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"5"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"6"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"7"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"8"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"9"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"10"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"11"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"12"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"13"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"14"}"#, delay_ms: 30 },
        Step::ToolCall { tool: "echo_tool", args: r#"{"note":"15"}"#, delay_ms: 30 },
        Step::Final { text: "DONE-LOG", delay_ms: 0 },
    ];
    let (base, _log) = spawn_scripted_server(steps).await;
    let (mgr, delegate, control, ctx) = make_harness(base);
    let line = match delegate
        .execute(json!({"goal": "log-target", "background": true}), &ctx)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected handle, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "log-target");
    wait_terminal(&mgr, id).await;

    // Default last=10: at most 10 activity lines regardless of the 15+ calls.
    let res = control
        .execute(json!({"action": "log", "id": id}), &ctx)
        .await;
    let text = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected log text, got: {other:?}"),
    };
    let activity = text
        .lines()
        .filter(|l| !l.starts_with("last") && !l.starts_with('['))
        .count();
    assert!(
        activity <= 10,
        "log must be bounded to <=10 lines by default, got {activity}:\n{text}"
    );
    assert!(!text.is_empty(), "log should carry some activity");

    // last=3 bounds further.
    let res = control
        .execute(json!({"action": "log", "id": id, "last": 3}), &ctx)
        .await;
    let text3 = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected log text, got: {other:?}"),
    };
    let activity3 = text3
        .lines()
        .filter(|l| !l.starts_with("last") && !l.starts_with('['))
        .count();
    assert!(
        activity3 <= 3,
        "log last=3 must bound to <=3 lines, got {activity3}:\n{text3}"
    );
}

/// (d) wait: returns when the child reaches a terminal state; on timeout it
/// returns partial statuses (FR-007).
#[tokio::test]
async fn t017d_wait_returns_on_terminal_and_partial_on_timeout() {
    // Fast child for the success path.
    let (base, _log) = spawn_scripted_server(vec![Step::Final {
        text: "DONE-WAIT",
        delay_ms: 200,
    }])
    .await;
    let (mgr, delegate, control, ctx) = make_harness(base);
    let line = match delegate
        .execute(json!({"goal": "wait-fast", "background": true}), &ctx)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected handle, got: {other:?}"),
    };
    let id = parse_handle_id(&line, "wait-fast");

    let res = control
        .execute(
            json!({"action": "wait", "ids": [id], "timeout_secs": 10}),
            &ctx,
        )
        .await;
    let text = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected wait text, got: {other:?}"),
    };
    assert!(text.contains(&format!("id={id}")), "got: {text}");
    assert!(text.contains("state=completed"), "got: {text}");
    assert!(text.contains("DONE-WAIT"), "summary missing: {text}");
    let _ = mgr;

    // Timeout path: a slow child + short timeout → partial (running) status,
    // clearly marked as timed out — NOT an error.
    let (base2, _log2) = spawn_scripted_server(vec![Step::Final {
        text: "DONE-SLOW-2",
        delay_ms: 3000,
    }])
    .await;
    let (mgr2, delegate2, control2, ctx2) = make_harness(base2);
    let line2 = match delegate2
        .execute(json!({"goal": "wait-slow", "background": true}), &ctx2)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("expected handle, got: {other:?}"),
    };
    let id2 = parse_handle_id(&line2, "wait-slow");
    let res2 = control2
        .execute(
            json!({"action": "wait", "ids": [id2], "timeout_secs": 1}),
            &ctx2,
        )
        .await;
    let text2 = match res2 {
        ToolResult::Text(s) => s,
        other => panic!("wait timeout must return partial statuses, got: {other:?}"),
    };
    assert!(text2.contains("timed out"), "must mark timeout: {text2}");
    assert!(
        text2.contains(&format!("id={id2}")),
        "partial status must include the id: {text2}"
    );
    assert!(text2.contains("state=running"), "got: {text2}");
    let _ = mgr2;
}

/// (e) unknown ids → tool errors for status/log/wait.
#[tokio::test]
async fn t017e_unknown_id_errors_for_inspection_actions() {
    let (base, _log) = spawn_scripted_server(vec![]).await;
    let (_mgr, _delegate, control, ctx) = make_harness(base);

    for action in ["status", "log"] {
        let res = control
            .execute(json!({"action": action, "id": 987001}), &ctx)
            .await;
        match res {
            ToolResult::Error(e) => assert!(
                e.contains("No subagent with id 987001"),
                "{action} unknown-id error must name the id, got: {e}"
            ),
            other => panic!("expected Error for {action} unknown id, got: {other:?}"),
        }
    }

    let res = control
        .execute(json!({"action": "wait", "ids": [987002]}), &ctx)
        .await;
    match res {
        ToolResult::Error(e) => assert!(
            e.contains("No subagent with id 987002"),
            "wait unknown-id error must name the id, got: {e}"
        ),
        other => panic!("expected Error for wait unknown id, got: {other:?}"),
    }
}
