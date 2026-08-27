//! Feature 020, User Story 5 (T019 — FAIL-FIRST): per-child resource budgets.
//!
//! Pins (spec.md US5, FR-011/FR-012, SC-004, contracts/delegation-tools.md):
//!   (a) budgets.max_turns=2 — a parent-side watcher stops the child at the
//!       breach boundary: terminal `DelegationState::Stopped{BudgetExceeded}`,
//!       completion notice `[SUBAGENT STOPPED] … outcome=budget_exceeded`, and
//!       no tool actions beyond the budgeted turns (SC-004: the in-flight
//!       breach turn may finish winding down, nothing after);
//!   (b) FR-011 — budgets with a zero (or negative) value are rejected at the
//!       tool layer as `ToolResult::Error` naming the field ("must be > 0"),
//!       never a panic, never silently accepted (nothing dispatches);
//!   (c) FR-012 — after a child runs, its cumulative tokens are visible in
//!       `overview()` / `child_status()`;
//!   (d) budgets.max_tokens — mock-reported usage crosses the cap → child
//!       stopped with BudgetExceeded;
//!   (e) budgets.max_wall_clock_secs=1 against a slow mock (>1s per action)
//!       → stopped with BudgetExceeded.
//!
//! Harness: local scripted mock OpenAI-compatible TCP server + a counting
//! probe tool — same pattern as tests/background.rs and tests/notices.rs
//! (read, not edited). Multi-turn children are forced by scripting
//! `tool_calls` responses; mock response delays (250 ms) exceed the
//! interrupt-bridge poll period (50 ms), making the "no actions beyond
//! breach+1" bound deterministic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::types::DelegationState;
use joey_orchestration::{DelegateTask, ManagerConfig, StopReason, SubagentManager};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Local scripted mock OpenAI-compatible provider on 127.0.0.1 — deterministic,
// no LLM. Each scripted response decides content, whether a tool call is
// emitted (forcing another iteration), reported usage, and a delay.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ScriptedResp {
    content: &'static str,
    /// Emit one tool call to this tool name (forces another iteration).
    tool_call: Option<&'static str>,
    /// (prompt, completion, total) usage reported for this call.
    usage: (u64, u64, u64),
    delay_ms: u64,
}

/// A tool-call turn: the child executes `budget_probe` and iterates.
fn tool_turn(delay_ms: u64) -> ScriptedResp {
    ScriptedResp {
        content: "",
        tool_call: Some("budget_probe"),
        usage: (100, 50, 150),
        delay_ms,
    }
}

/// A final plain-text turn: the child finishes naturally if it gets here.
fn final_answer(delay_ms: u64) -> ScriptedResp {
    ScriptedResp {
        content: "ALL DONE",
        tool_call: None,
        usage: (100, 50, 150),
        delay_ms,
    }
}

fn openai_scripted_body(r: &ScriptedResp, call_index: usize) -> String {
    let mut message = json!({"role": "assistant", "content": r.content});
    let finish_reason = if let Some(name) = r.tool_call {
        message["tool_calls"] = json!([{
            "id": format!("call-{call_index}"),
            "type": "function",
            "function": {"name": name, "arguments": "{}"}
        }]);
        "tool_calls"
    } else {
        "stop"
    };
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
        "usage": {
            "prompt_tokens": r.usage.0,
            "completion_tokens": r.usage.1,
            "total_tokens": r.usage.2
        }
    })
    .to_string()
}

/// Serve exactly one HTTP request on `stream` with the i-th scripted
/// response (repeating the last once the script is exhausted), then close.
async fn serve_scripted_conn(
    mut stream: TcpStream,
    script: Vec<ScriptedResp>,
    counter: Arc<AtomicUsize>,
) {
    // Read headers + body (bounded, with a safety timeout so a bad client
    // can never hang the test) — same shape as tests/background.rs.
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

    let idx = counter.fetch_add(1, Ordering::SeqCst);
    let resp = &script[idx.min(script.len() - 1)];
    if resp.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(resp.delay_ms)).await;
    }
    let body = openai_scripted_body(resp, idx);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Bind a scripted mock provider on 127.0.0.1:0; returns its base URL.
///
/// The script position is tracked by ONE counter shared across connections:
/// each provider call opens a fresh connection (Connection: close), so a
/// per-connection counter would replay script[0] forever.
async fn spawn_scripted_server(script: Vec<ScriptedResp>) -> String {
    assert!(!script.is_empty(), "scripted mock needs at least one response");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Shared script position: connection N serves script[N] (clamped to the
    // last entry once exhausted).
    let counter = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let script = script.clone();
            let counter = counter.clone();
            tokio::spawn(async move {
                serve_scripted_conn(stream, script, counter).await;
            });
        }
    });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Counting probe tool: every execution increments `execs` (after an optional
// sleep) — the test's "action" counter for the SC-004 bounds.
// ---------------------------------------------------------------------------

struct ProbeTool {
    execs: Arc<AtomicUsize>,
    sleep_ms: u64,
}

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        "budget_probe"
    }

    fn toolset(&self) -> &str {
        "coding"
    }

    fn emoji(&self) -> &str {
        "🔧"
    }

    fn description(&self) -> &str {
        "Budget test probe (echoes ok)."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        if self.sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
        }
        self.execs.fetch_add(1, Ordering::SeqCst);
        ToolResult::Text("probe-ok".to_string())
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

/// Build the delegate_task tool with the probe registered, plus the manager
/// (for overview/status assertions) and the orchestrator context (for
/// completion-notice assertions).
fn make_budget_tool(
    base_url: String,
    probe_sleep_ms: u64,
) -> (
    Arc<SubagentManager>,
    DelegateTask,
    ToolContext,
    Arc<AtomicUsize>,
) {
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "budgets-test");
    let execs = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ProbeTool {
        execs: execs.clone(),
        sleep_ms: probe_sleep_ms,
    }));
    let tool = DelegateTask::new(
        mgr.clone(),
        agent_config(base_url),
        Config::defaults(),
        registry,
        None,
        None,
    );
    (mgr, tool, ctx, execs)
}

/// Assert the result is a background handle line and return it.
fn expect_handle_line(res: ToolResult, goal: &str) -> String {
    match res {
        ToolResult::Text(s) => {
            assert!(
                s.starts_with("[BACKGROUND] id=") && s.ends_with(" started"),
                "expected a handle line, got: {s:?}"
            );
            s
        }
        other => panic!("expected Text handle line for {goal:?}, got: {other:?}"),
    }
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

/// Poll overview() until the child reaches a terminal state (FR-019 records).
async fn wait_for_terminal(mgr: &SubagentManager, id: &str) -> joey_orchestration::types::DelegationOverview {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(r) = mgr
            .overview()
            .into_iter()
            .find(|r| r.child_id == id && r.state.is_terminal())
        {
            return r;
        }
        assert!(
            Instant::now() < deadline,
            "child {id} never reached a terminal state; overview={:?}",
            mgr.overview()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll-drain the pending-completions queue until a notice for `child_id`
/// appears (same pattern as tests/notices.rs).
async fn wait_for_notice(
    ctx: &ToolContext,
    child_id: &str,
) -> Vec<joey_tools::context::BackgroundCompletion> {
    let needle = format!("id={child_id} ");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    loop {
        seen.extend(ctx.drain_pending_completions());
        if seen.iter().any(|c| c.output_tail.contains(&needle)) {
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

// ---------------------------------------------------------------------------
// (a) max_turns breach (SC-004 / FR-011 / FR-016)
// ---------------------------------------------------------------------------

/// budgets.max_turns=2: the child performs two budgeted turns; when iteration
/// 3 starts, the watcher stops it (`stop_child(BudgetExceeded)`). Terminal
/// state `Stopped{BudgetExceeded}`, notice `outcome=budget_exceeded`, and no
/// tool actions beyond the budgeted turns (SC-004: the in-flight breach API
/// call may return, its queued tool call is cancelled at the pre-flight
/// interrupt check).
#[tokio::test]
async fn t019_max_turns_budget_stops_child_with_budget_exceeded() {
    let base = spawn_scripted_server(vec![
        tool_turn(250),
        tool_turn(250),
        tool_turn(250),
        tool_turn(250),
        final_answer(0),
    ])
    .await;
    let (mgr, tool, ctx, execs) = make_budget_tool(base, 0);

    let res = tool
        .execute(
            json!({"goal": "turn budgeted child", "background": true,
                   "budgets": {"max_turns": 2}}),
            &ctx,
        )
        .await;
    let line = expect_handle_line(res, "turn budgeted child");
    let id = parse_handle_line(&line, "turn budgeted child");

    let record = wait_for_terminal(&mgr, &id).await;
    assert!(
        matches!(
            record.state,
            DelegationState::Stopped {
                reason: StopReason::BudgetExceeded
            }
        ),
        "max_turns=2 breach must stop the child with BudgetExceeded, got {:?}",
        record.state
    );

    // SC-004: no tool actions beyond the budgeted turns — iteration 3's API
    // call is the allowed in-flight action; its tool call must be cancelled.
    let executed = execs.load(Ordering::SeqCst);
    assert!(
        executed <= 2,
        "SC-004 breach bound: at most max_turns tool actions, executed {executed}"
    );

    // The breach is reported through the completion-notice path (FR-016).
    let drained = wait_for_notice(&ctx, &id).await;
    let entry = notice_for(&drained, &id);
    assert!(
        entry
            .output_tail
            .contains(&format!("[SUBAGENT STOPPED] id={id} goal=turn budgeted child outcome=budget_exceeded")),
        "completion notice must report outcome=budget_exceeded, got: {:?}",
        entry.output_tail
    );
}

// ---------------------------------------------------------------------------
// (b) FR-011: invalid budgets rejected at the tool layer
// ---------------------------------------------------------------------------

/// Zero/negative budget values are rejected as ToolResult::Error naming the
/// field and the "> 0" rule — never a panic, never silently accepted (no
/// child dispatches). An all-empty budgets object stays valid.
#[tokio::test]
async fn t019_invalid_budgets_rejected_as_tool_error() {
    let base = spawn_scripted_server(vec![final_answer(0)]).await;
    let (mgr, tool, ctx, _execs) = make_budget_tool(base, 0);

    // Zero values: clear error naming the offending field (FR-011).
    for (bad, needle) in [
        (json!({"max_turns": 0}), "budgets.max_turns"),
        (json!({"max_tokens": 0}), "budgets.max_tokens"),
        (json!({"max_wall_clock_secs": 0}), "budgets.max_wall_clock_secs"),
    ] {
        let res = tool
            .execute(
                json!({"goal": "reject me", "background": true, "budgets": bad}),
                &ctx,
            )
            .await;
        match res {
            ToolResult::Error(e) => assert!(
                e.contains(needle) && e.contains("must be > 0"),
                "FR-011 error must name the field and the >0 rule, got: {e}"
            ),
            other => panic!(
                "zero budgets {bad} must be ToolResult::Error (FR-011), got: {other:?}"
            ),
        }
    }

    // Negative values are rejected at parse (serde type error) — still a
    // clean tool error, not a panic, not a dispatch.
    let res = tool
        .execute(
            json!({"goal": "reject me", "background": true,
                   "budgets": {"max_turns": -2}}),
            &ctx,
        )
        .await;
    assert!(
        matches!(res, ToolResult::Error(_)),
        "negative budget must error, got: {res:?}"
    );

    // The blocking path rejects identically (parse happens before dispatch).
    let res = tool
        .execute(json!({"goal": "reject me", "budgets": {"max_tokens": 0}}), &ctx)
        .await;
    match res {
        ToolResult::Error(e) => assert!(
            e.contains("must be > 0"),
            "blocking path must reject zero budgets too, got: {e}"
        ),
        other => panic!("blocking path must reject zero budgets, got: {other:?}"),
    }

    // Nothing from the rejected requests dispatched (no child with that goal).
    assert!(
        !mgr.overview().iter().any(|r| r.goal == "reject me"),
        "rejected budget requests must not dispatch children; overview={:?}",
        mgr.overview()
    );

    // An all-empty budgets object is valid (no caps) and dispatches normally.
    let res = tool
        .execute(
            json!({"goal": "empty ok", "background": true, "budgets": {}}),
            &ctx,
        )
        .await;
    assert!(
        matches!(res, ToolResult::Text(_)),
        "empty budgets object is valid, got: {res:?}"
    );
}

// ---------------------------------------------------------------------------
// (c) FR-012: cumulative usage visible after a child runs
// ---------------------------------------------------------------------------

/// After a child runs (2 mock calls × 150 tokens), its cumulative tokens are
/// visible in the overview record and child_status (FR-012).
#[tokio::test]
async fn t019_cumulative_tokens_visible_after_run() {
    let base = spawn_scripted_server(vec![tool_turn(0), final_answer(0)]).await;
    let (mgr, tool, ctx, _execs) = make_budget_tool(base, 0);

    let res = tool
        .execute(json!({"goal": "usage child", "background": true}), &ctx)
        .await;
    let line = expect_handle_line(res, "usage child");
    let id = parse_handle_line(&line, "usage child");

    let record = wait_for_terminal(&mgr, &id).await;
    match record.state {
        DelegationState::Completed { ref result } => {
            assert_eq!(
                result.token_usage.total_tokens, 300,
                "two mock calls à 150 tokens"
            );
        }
        ref other => panic!("expected Completed, got {other:?}"),
    }
    // FR-012: the session-lifetime record carries cumulative usage.
    assert_eq!(record.tokens, 300, "overview record tokens");
    let status = mgr
        .child_status(id.parse().unwrap())
        .expect("status for finished child");
    assert_eq!(status.tokens, 300, "child_status tokens");
    assert!(status.state.is_terminal());
}

// ---------------------------------------------------------------------------
// (d) max_tokens breach (FR-011)
// ---------------------------------------------------------------------------

/// budgets.max_tokens=250 with 150 tokens reported per call: after the second
/// ApiCallEnd (cumulative 300 > 250) the watcher stops the child. The probe
/// sleeps 300 ms so the in-flight iteration-2 tool is the only action that
/// completes post-detection; iteration 3 never starts (deterministic bound).
#[tokio::test]
async fn t019_max_tokens_budget_stops_child_with_budget_exceeded() {
    let base = spawn_scripted_server(vec![
        tool_turn(250),
        tool_turn(250),
        tool_turn(250),
        tool_turn(250),
        final_answer(0),
    ])
    .await;
    let (mgr, tool, ctx, execs) = make_budget_tool(base, 300);

    let res = tool
        .execute(
            json!({"goal": "token budgeted child", "background": true,
                   "budgets": {"max_tokens": 250}}),
            &ctx,
        )
        .await;
    let line = expect_handle_line(res, "token budgeted child");
    let id = parse_handle_line(&line, "token budgeted child");

    let record = wait_for_terminal(&mgr, &id).await;
    assert!(
        matches!(
            record.state,
            DelegationState::Stopped {
                reason: StopReason::BudgetExceeded
            }
        ),
        "max_tokens=250 breach (300 consumed) must stop the child with BudgetExceeded, got {:?}",
        record.state
    );

    let executed = execs.load(Ordering::SeqCst);
    assert!(
        executed <= 2,
        "token breach bound: at most the in-flight tool completes, executed {executed}"
    );

    let drained = wait_for_notice(&ctx, &id).await;
    let entry = notice_for(&drained, &id);
    assert!(
        entry.output_tail.contains("outcome=budget_exceeded"),
        "completion notice must report outcome=budget_exceeded, got: {:?}",
        entry.output_tail
    );
}

// ---------------------------------------------------------------------------
// (e) wall-clock breach (FR-011)
// ---------------------------------------------------------------------------

/// budgets.max_wall_clock_secs=1 against a mock that delays every response by
/// 1.5 s: the watcher's tick detects the wall breach while iteration 1 is
/// still in flight; the child winds down when the response returns (queued
/// tool calls cancelled) and archives Stopped{BudgetExceeded}.
#[tokio::test]
async fn t019_wall_clock_budget_stops_child_with_budget_exceeded() {
    let base = spawn_scripted_server(vec![tool_turn(1500), tool_turn(1500), final_answer(0)]).await;
    let (mgr, tool, ctx, execs) = make_budget_tool(base, 0);

    let res = tool
        .execute(
            json!({"goal": "wall budgeted child", "background": true,
                   "budgets": {"max_wall_clock_secs": 1}}),
            &ctx,
        )
        .await;
    let line = expect_handle_line(res, "wall budgeted child");
    let id = parse_handle_line(&line, "wall budgeted child");

    let record = wait_for_terminal(&mgr, &id).await;
    assert!(
        matches!(
            record.state,
            DelegationState::Stopped {
                reason: StopReason::BudgetExceeded
            }
        ),
        "wall-clock breach (1s cap, >1.5s child) must stop the child with BudgetExceeded, got {:?}",
        record.state
    );

    let executed = execs.load(Ordering::SeqCst);
    assert!(
        executed <= 1,
        "wall breach bound: at most the in-flight action completes, executed {executed}"
    );

    let drained = wait_for_notice(&ctx, &id).await;
    let entry = notice_for(&drained, &id);
    assert!(
        entry.output_tail.contains("outcome=budget_exceeded"),
        "completion notice must report outcome=budget_exceeded, got: {:?}",
        entry.output_tail
    );
}
