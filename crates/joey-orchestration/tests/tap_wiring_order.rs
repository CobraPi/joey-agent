//! Feature 020, T031: delegation event-tap WIRING ORDER regression test.
//!
//! Reproduces the REAL startup order of the hosts (repl.rs `build_agent_parts`
//! → tui.rs `set_global_tap`):
//!   1. `SubagentManager::new`
//!   2. tool registration constructs `SubagentControl::new(manager)` — which
//!      installs its activity recorder
//!   3. ONLY THEN does the host install the process-global tap
//!      (`tap::set_global_tap`)
//! and asserts the global tap still receives every delegation lifecycle event
//! (SubagentSpawn / SubagentEvent / SubagentComplete / SubagentStopped) —
//! i.e. the recorder must not SHADOW a tap installed after registration
//! (T029: pre-fix, the recorder became the manager-local tap and forwarded
//! to a `None` captured at construction, so late-installed host taps saw
//! zero events and TUI subagent panes never appeared).
//!
//! Also pins FR-005/FR-006: the recorder keeps feeding `subagent_control`
//! list/log introspection in the SAME wiring order.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_orchestration::types::DelegationState;
use joey_orchestration::{
    DelegateTask, DelegationRequest, ManagerConfig, SubagentControl, SubagentManager,
};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (same harness style as
// tests/control_tool.rs) — one scripted response per HTTP connection.
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Step {
    Final {
        text: &'static str,
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

async fn serve_conn(mut stream: TcpStream, queue: Arc<Mutex<VecDeque<Step>>>) {
    let Some(_body) = read_http_body(&mut stream).await else {
        return;
    };
    let step = queue.lock().unwrap().pop_front();
    let (status, body_out) = match step {
        Some(Step::Final { text, delay_ms }) => {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            ("200 OK", openai_body(text))
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

async fn spawn_scripted_server(steps: Vec<Step>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let queue = queue.clone();
            tokio::spawn(async move {
                serve_conn(stream, queue).await;
            });
        }
    });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Harness — mirrors the REAL host wiring order exactly.
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

/// RAII guard: the process-global tap is PROCESS state shared by every test
/// in this binary — always restore it to `None`, even on panic.
struct GlobalTapGuard;

impl GlobalTapGuard {
    fn install(tx: mpsc::UnboundedSender<AgentEvent>) -> Self {
        joey_orchestration::tap::set_global_tap(Some(tx));
        GlobalTapGuard
    }
}

impl Drop for GlobalTapGuard {
    fn drop(&mut self) {
        joey_orchestration::tap::set_global_tap(None);
    }
}

fn parse_handle_id(line: &str, goal: &str) -> u64 {
    let tail = format!(" goal={goal} started");
    let id = line
        .strip_prefix("[BACKGROUND] id=")
        .and_then(|rest| rest.strip_suffix(&tail))
        .unwrap_or_else(|| panic!("not a handle line: {line:?}"));
    id.parse::<u64>()
        .unwrap_or_else(|e| panic!("bad child id {id:?} in handle line: {e}"))
}

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

/// Collect every event currently queued on the tap receiver.
fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

// ---------------------------------------------------------------------------
// The wiring-order regression (T031 / T029)
// ---------------------------------------------------------------------------

/// THE regression: in the real startup order (manager → SubagentControl →
/// set_global_tap), the global tap must receive spawn, wrapped child
/// events, complete, AND stopped — while the recorder still feeds
/// subagent_control list/log (FR-005/FR-006).
#[tokio::test]
async fn global_tap_installed_after_control_sees_all_delegation_events() {
    // Child 1: completes fast (spawn + child events + complete).
    let base1 = spawn_scripted_server(vec![Step::Final {
        text: "DONE-WIRING",
        delay_ms: 50,
    }])
    .await;
    // Child 2: runs long enough to be stopped mid-flight (stopped event).
    let base2 = spawn_scripted_server(vec![Step::Final {
        text: "PARTIAL-WIRING",
        delay_ms: 8000,
    }])
    .await;

    // (1) manager — as the hosts build it.
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let mut base = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "wiring-order");
    // (2) registration order: delegate_task first, then subagent_control —
    // exactly what register_orchestration does (repl.rs build_agent_parts).
    let delegate = DelegateTask::new(
        mgr.clone(),
        agent_config(base1),
        Config::defaults(),
        base.clone(),
        None,
        None,
    );
    let control = SubagentControl::new(mgr.clone());
    // (3) ONLY NOW does the host install the global tap (tui.rs:239-241).
    let (gtx, mut grx) = mpsc::unbounded_channel::<AgentEvent>();
    let _tap_guard = GlobalTapGuard::install(gtx);

    // (4) real dispatch through the delegate tool (background child).
    let res = delegate
        .execute(json!({"goal": "wiring-complete", "background": true}), &ctx)
        .await;
    let line = match res {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line, got: {other:?}"),
    };
    let id1 = parse_handle_id(&line, "wiring-complete");
    let rec1 = wait_terminal(&mgr, id1).await;
    assert!(
        matches!(rec1.state, DelegationState::Completed { .. }),
        "child 1 must complete, got {:?}",
        rec1.state
    );

    // (5) a second child we STOP mid-run → SubagentStopped on the tap.
    let delegate2 = DelegateTask::new(
        mgr.clone(),
        agent_config(base2),
        Config::defaults(),
        base,
        None,
        None,
    );
    let res2 = delegate2
        .execute(json!({"goal": "wiring-stop", "background": true}), &ctx)
        .await;
    let line2 = match res2 {
        ToolResult::Text(s) => s,
        other => panic!("expected handle line, got: {other:?}"),
    };
    let id2 = parse_handle_id(&line2, "wiring-stop");
    // Wait for registration, then stop via the control tool.
    let deadline = Instant::now() + Duration::from_secs(5);
    while mgr.child_status(id2).is_none() {
        assert!(Instant::now() < deadline, "child {id2} never registered");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    match control.execute(json!({"action": "stop", "id": id2}), &ctx).await {
        ToolResult::Text(_) => {}
        other => panic!("stop ack failed: {other:?}"),
    }
    let rec2 = wait_terminal(&mgr, id2).await;
    assert!(
        matches!(rec2.state, DelegationState::Stopped { .. }),
        "child 2 must be stopped, got {:?}",
        rec2.state
    );

    // Give the just-archived stop event a moment to land on the tap, then
    // judge the tap's full contents.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let events = drain(&mut grx);

    // Spawn: one per child, correct goals.
    let spawns: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::SubagentSpawn { .. }))
        .collect();
    assert_eq!(
        spawns.len(),
        2,
        "T029: global tap (installed AFTER control) must see both spawns; got {} events: {:?}",
        events.len(),
        event_kinds(&events)
    );
    for goal in ["wiring-complete", "wiring-stop"] {
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::SubagentSpawn { goal: g, .. } if *g == goal
            )),
            "global tap missing SubagentSpawn for {goal}: {:?}",
            event_kinds(&events)
        );
    }

    // Complete: the finished child.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SubagentComplete { id, .. } if *id == id1
        )),
        "global tap missing SubagentComplete for child {id1}: {:?}",
        event_kinds(&events)
    );
    // Stopped: the stopped child.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SubagentStopped { id, .. } if *id == id2
        )),
        "global tap missing SubagentStopped for child {id2}: {:?}",
        event_kinds(&events)
    );
    // Wrapped child events: the completing child streams at least one
    // SubagentEvent carrying its id.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SubagentEvent { id, .. } if *id == id1)),
        "global tap missing wrapped SubagentEvent for child {id1}: {:?}",
        event_kinds(&events)
    );

    // (6) FR-005/FR-006: the recorder still feeds subagent_control in this
    // same wiring order — list shows both children, log shows activity.
    let list = match control.execute(json!({"action": "list"}), &ctx).await {
        ToolResult::Text(s) => s,
        other => panic!("list failed: {other:?}"),
    };
    assert!(
        list.contains(&format!("id={id1} ")) && list.contains("state=completed"),
        "recorder-backed list must show completed child {id1}: {list}"
    );
    assert!(
        list.contains(&format!("id={id2} ")) && list.contains("state=stopped"),
        "recorder-backed list must show stopped child {id2}: {list}"
    );
    let log = match control
        .execute(json!({"action": "log", "id": id1, "last": 20}), &ctx)
        .await
    {
        ToolResult::Text(s) => s,
        other => panic!("log failed: {other:?}"),
    };
    assert!(
        log.contains("spawned (goal: wiring-complete)"),
        "recorder-backed log must show the spawn line: {log}"
    );
    assert!(
        log.contains("completed"),
        "recorder-backed log must show the completion line: {log}"
    );
}

/// Root-cause pin (sync, no dispatch needed): after SubagentControl::new,
/// the manager's effective EXTERNAL tap resolution must not be captured or
/// shadowed by the recorder — a global tap installed later still resolves.
#[test]
fn control_recorder_does_not_shadow_external_tap_resolution() {
    let mgr = SubagentManager::new(ManagerConfig::default());
    let _control = SubagentControl::new(Arc::new(mgr.clone()));
    // Pre-fix: the recorder sat in the LOCAL tap slot, so event_tap()
    // returned it (or None-forwarded) and a later global tap never resolved.
    assert!(
        mgr.event_tap().is_none(),
        "no host tap installed → event_tap() must be None, not the recorder"
    );
    let (gtx, mut grx) = mpsc::unbounded_channel::<AgentEvent>();
    let _guard = GlobalTapGuard::install(gtx);
    let resolved = mgr
        .event_tap()
        .expect("global tap installed after control must resolve");
    resolved.send(AgentEvent::Notice("roundtrip".into())).unwrap();
    assert!(
        matches!(grx.try_recv(), Ok(AgentEvent::Notice(_))),
        "resolved tap must be the global tap, not the recorder"
    );
}

/// Companion order check: a LOCAL manager tap installed after control
/// creation (the line-REPL path, tests/control_tool.rs:400) still receives
/// events alongside the recorder.
#[tokio::test]
async fn local_tap_installed_after_control_still_receives_events() {
    let base = spawn_scripted_server(vec![Step::Final {
        text: "DONE-LOCAL-ORDER",
        delay_ms: 0,
    }])
    .await;
    let mgr = Arc::new(SubagentManager::new(ManagerConfig::default()));
    let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "wiring-local");
    let delegate = DelegateTask::new(
        mgr.clone(),
        agent_config(base),
        Config::defaults(),
        ToolRegistry::new(),
        None,
        None,
    );
    let control = SubagentControl::new(mgr.clone());
    // Local tap AFTER control construction (control_tool.rs t014b order).
    let (tap_tx, mut tap_rx) = mpsc::unbounded_channel::<AgentEvent>();
    mgr.set_event_tap(Some(tap_tx));

    // Blocking dispatch through the real manager plumbing.
    let req = DelegationRequest::single("local-order-target");
    let cfg = agent_config("http://unused".to_string());
    let result = mgr
        .dispatch_single(&req, &cfg, &Config::defaults(), &ToolRegistry::new(), None)
        .await;
    assert!(result.success, "child must succeed: {:?}", result.error);
    let _ = delegate;
    let _ = control;

    let events = drain(&mut tap_rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SubagentSpawn { goal, .. } if goal == "local-order-target")),
        "local tap installed after control must see the spawn: {:?}",
        event_kinds(&events)
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SubagentComplete { .. })),
        "local tap must see the completion: {:?}",
        event_kinds(&events)
    );
    mgr.set_event_tap(None);
}

/// Compact kind names for assertion messages.
fn event_kinds(events: &[AgentEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            AgentEvent::SubagentSpawn { .. } => "SubagentSpawn",
            AgentEvent::SubagentEvent { .. } => "SubagentEvent",
            AgentEvent::SubagentComplete { .. } => "SubagentComplete",
            AgentEvent::SubagentFailed { .. } => "SubagentFailed",
            AgentEvent::SubagentStopped { .. } => "SubagentStopped",
            AgentEvent::DelegationBatchComplete { .. } => "DelegationBatchComplete",
            AgentEvent::TurnStart { .. } => "TurnStart",
            AgentEvent::IterationStart { .. } => "IterationStart",
            AgentEvent::AssistantMessage(_) => "AssistantMessage",
            AgentEvent::ToolStart { .. } => "ToolStart",
            AgentEvent::ToolEnd { .. } => "ToolEnd",
            AgentEvent::ApiCallStart { .. } => "ApiCallStart",
            AgentEvent::ApiCallEnd { .. } => "ApiCallEnd",
            AgentEvent::Done { .. } => "Done",
            AgentEvent::Failed(_) => "Failed",
            AgentEvent::Notice(_) => "Notice",
            _ => "other",
        })
        .collect()
}

// Re-export for the unused-variable silences above.
#[allow(unused)]
fn _silence(_: &Value) {}
