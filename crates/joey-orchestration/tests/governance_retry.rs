//! Integration test: governance retry — wall-clock timeout, jittered
//! backoff under a global retry budget, and turn-boundary resume
//! (feature 030, US2 / FR-004/FR-005/FR-006, T010 TDD RED).
//!
//! RED contract pinned here (see specs/030-please-implement-features/):
//!   (a) a task exceeding its wall-clock budget is stopped and reported
//!       with a `[timeout] task exceeded Ns wall-clock budget` error, and
//!       the retry stays within the per-task allowance;
//!   (b) retries of fatal provider failures are spaced by jittered
//!       exponential backoff (base backoff_base_secs, jitter only ADDS),
//!       and a task can end fast-failed with `retry budget exhausted`;
//!   (c) a timed-out task with checkpointing on RESUMES from its last
//!       completed turn: the resumed child's provider requests carry the
//!       resume preamble marker `Continue the task from turn`, and the
//!       resumed run issues only the REMAINING turns' requests;
//!   (d) with checkpointing off, a timed-out task's retry is a FULL
//!       restart: no resume marker ever appears on the wire, and the
//!       replay issues the full turn count again.
//!
//! Harness conventions mirror tests/governance_admission.rs (which itself
//! mirrors tests/concurrency_limiter.rs): scripted mock OpenAI provider
//! over a TcpListener on 127.0.0.1:0 with delay injection, AtomicUsize
//! in-flight probes, openai_body()/read_http_body(), agent_config(), and
//! a governance_manager() helper. Multi-turn children are forced by
//! scripting `tool_calls` responses (the tests/budgets.rs convention).
//!
//! REQUIRED EXTENSION (this file only): the mock server is RESUME-AWARE.
//! Each request body is inspected; if it contains the exact marker
//! `Continue the task from turn` the server serves the scripted step at
//! index (resume_offset + served_marker_count), where resume_offset is a
//! per-server AtomicUsize the test sets to the number of already-
//! completed turns; every other request is served sequentially from
//! index 0. Total and marker request counts are AtomicUsize probes.
//! This pins the WIRE-VISIBLE resume behavior: a resumed child's first
//! provider request carries the resume preamble instead of re-eliciting
//! the completed turns.
//!
//! Every test fn starts with `gov_` so intermediate regression runs can
//! `--skip gov_` until T011/T012/T013 implement the mechanisms.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::{
    DelegationRequest, DelegationResult, ManagerConfig, SubagentManager,
};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The resume preamble marker a resumed child's first provider request
/// must carry (T013 contract: "Continue the task from turn ...").
const RESUME_MARKER: &str = "Continue the task from turn";

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (governance_admission.rs harness
// style + budgets.rs multi-turn tool-call scripting) with in-flight
// concurrency probes and RESUME-AWARE request routing.
// ---------------------------------------------------------------------------

/// One scripted response: content, an optional tool call (forcing another
/// iteration), reported usage, a delay, and an HTTP status (>= 500 serves
/// an error body — the always-failing provider).
#[derive(Clone)]
struct ScriptedStep {
    content: &'static str,
    /// Emit one tool call to this tool name (forces another iteration).
    tool_call: Option<&'static str>,
    /// (prompt, completion, total) usage reported for this call.
    usage: (u64, u64, u64),
    delay_ms: u64,
    http_status: u16,
}

/// A tool-call turn: the child executes `echo_tool` and iterates.
fn tool_step(delay_ms: u64) -> ScriptedStep {
    ScriptedStep {
        content: "",
        tool_call: Some("echo_tool"),
        usage: (100, 50, 150),
        delay_ms,
        http_status: 200,
    }
}

/// A final plain-text turn: the child finishes naturally if it gets here.
fn final_step(delay_ms: u64) -> ScriptedStep {
    ScriptedStep {
        content: "ALL DONE",
        tool_call: None,
        usage: (100, 50, 150),
        delay_ms,
        http_status: 200,
    }
}

/// A failing provider response: HTTP 500 with an error body.
fn fail_step(delay_ms: u64) -> ScriptedStep {
    ScriptedStep {
        content: "",
        tool_call: None,
        usage: (0, 0, 0),
        delay_ms,
        http_status: 500,
    }
}

fn openai_body(r: &ScriptedStep, call_index: usize) -> String {
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

struct ServerProbe {
    script: Arc<Vec<ScriptedStep>>,
    /// Sequential (non-resume) script position: the Nth plain request
    /// serves script[N] (clamped to the last entry once exhausted —
    /// budgets.rs convention; an all-fail script keeps failing).
    seq_index: Arc<AtomicUsize>,
    /// Number of marker-carrying (resume) requests served so far.
    marker_index: Arc<AtomicUsize>,
    /// Total requests served (marker + sequential).
    total_requests: Arc<AtomicUsize>,
    /// Requests whose body carried the resume marker.
    marker_requests: Arc<AtomicUsize>,
    /// Script index a resumed run starts from (= completed turns of the
    /// timed-out run); set by the test BEFORE the retry can happen (it is
    /// only consulted when a marker request arrives, i.e. during the
    /// resumed run — the initial run never reads it).
    resume_offset: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
}

/// Serve exactly one scripted request per connection, then close.
/// RESUME-AWARE routing: a request whose body contains the marker
/// `Continue the task from turn` serves script[resume_offset + k] (k =
/// marker requests already served); every other request serves the next
/// sequential step from index 0.
async fn serve_conn(mut stream: TcpStream, p: ServerProbe) {
    let Some(body) = read_http_body(&mut stream).await else {
        return;
    };
    p.total_requests.fetch_add(1, Ordering::SeqCst);
    let is_resume = body.contains(RESUME_MARKER);
    let idx = if is_resume {
        p.marker_requests.fetch_add(1, Ordering::SeqCst);
        let k = p.marker_index.fetch_add(1, Ordering::SeqCst);
        p.resume_offset.load(Ordering::SeqCst) + k
    } else {
        p.seq_index.fetch_add(1, Ordering::SeqCst)
    };
    let step = p.script[idx.min(p.script.len() - 1)].clone();
    let now_in_flight = p.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    p.max_in_flight.fetch_max(now_in_flight, Ordering::SeqCst);
    if step.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(step.delay_ms)).await;
    }
    let (status_line, body_out) = if step.http_status >= 500 {
        (
            "HTTP/1.1 500 Internal Server Error",
            json!({"error": {"message": "mock provider forced failure"}}).to_string(),
        )
    } else {
        ("HTTP/1.1 200 OK", openai_body(&step, idx))
    };
    let resp = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_out.len(),
        body_out
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
    p.in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// Test-side handle on the resume-aware scripted mock provider.
struct GovServer {
    base_url: String,
    total_requests: Arc<AtomicUsize>,
    marker_requests: Arc<AtomicUsize>,
    resume_offset: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
}

impl GovServer {
    fn url(&self) -> String {
        self.base_url.clone()
    }

    fn total_requests(&self) -> usize {
        self.total_requests.load(Ordering::SeqCst)
    }

    /// Requests whose body carried the resume marker (the resumed run's
    /// provider requests).
    fn marker_requests(&self) -> usize {
        self.marker_requests.load(Ordering::SeqCst)
    }

    /// Plain (non-resume) requests — the initial run's provider requests.
    fn seq_requests(&self) -> usize {
        self.total_requests() - self.marker_requests()
    }

    /// Set the script index a resumed run starts from (the number of
    /// turns the previous run completed).
    fn set_resume_offset(&self, n: usize) {
        self.resume_offset.store(n, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

/// Bind the resume-aware scripted mock provider on 127.0.0.1:0.
async fn spawn_scripted_server(script: Vec<ScriptedStep>) -> GovServer {
    assert!(!script.is_empty(), "scripted mock needs at least one response");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let script = Arc::new(script);
    let seq_index = Arc::new(AtomicUsize::new(0));
    let marker_index = Arc::new(AtomicUsize::new(0));
    let total_requests = Arc::new(AtomicUsize::new(0));
    let marker_requests = Arc::new(AtomicUsize::new(0));
    let resume_offset = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let ret_total = total_requests.clone();
    let ret_marker = marker_requests.clone();
    let ret_offset = resume_offset.clone();
    let ret_max = max_in_flight.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let probe = ServerProbe {
                script: script.clone(),
                seq_index: seq_index.clone(),
                marker_index: marker_index.clone(),
                total_requests: total_requests.clone(),
                marker_requests: marker_requests.clone(),
                resume_offset: resume_offset.clone(),
                in_flight: in_flight.clone(),
                max_in_flight: max_in_flight.clone(),
            };
            tokio::spawn(async move {
                serve_conn(stream, probe).await;
            });
        }
    });
    GovServer {
        base_url: format!("http://{addr}"),
        total_requests: ret_total,
        marker_requests: ret_marker,
        resume_offset: ret_offset,
        max_in_flight: ret_max,
    }
}

// ---------------------------------------------------------------------------
// Governance-retry test helpers.
// ---------------------------------------------------------------------------

/// The trivial tool the scripted children call (registered into the base
/// registry so the child's schema/dispatch both know it) — copied from
/// tests/concurrency_limiter.rs.
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
            "properties": {"note": {"type": "string"}}
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

/// Parent AgentConfig pointing at the scripted mock provider (mirrors
/// governance_admission.rs's agent_config).
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

/// Manager with the governance-retry shape under test: 2 child slots,
/// 8 request permits, governance ON with mechanism overrides per test,
/// isolated data dir. `enabled` and `data_dir` are forced; recovery
/// attempts stay at the ManagerConfig default (1).
fn governance_manager(
    data_dir: std::path::PathBuf,
    mut gov: GovernanceConfig,
) -> SubagentManager {
    gov.enabled = true;
    gov.data_dir = Some(data_dir);
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: gov,
        ..Default::default()
    })
}

/// Drain every queued event and collect the RetryAttempt wait_secs
/// (raw events, as dispatch_single's event_tx receives them; wrapped
/// SubagentEvent forms are unwrapped defensively).
fn drain_retry_waits(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<f64> {
    let mut waits = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        match ev {
            AgentEvent::RetryAttempt { wait_secs, .. } => waits.push(wait_secs),
            AgentEvent::SubagentEvent { event, .. } => {
                if let AgentEvent::RetryAttempt { wait_secs, .. } = *event {
                    waits.push(wait_secs);
                }
            }
            _ => {}
        }
    }
    waits
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// FR-004: a task over its 1s wall-clock budget is stopped and reported
/// with the exact `[timeout] task exceeded 1s wall-clock budget` error;
/// the manager retries within the per-task allowance (subagent_recovery_
/// attempts default 1), the retry (only slow steps available) also times
/// out, and after the allowance is exhausted the FINAL result still
/// carries `[timeout]`. RetryAttempt events observed for the task must
/// stay within the allowance (<= 1).
#[tokio::test]
async fn gov_timeout_reported_and_consumes_retry() {
    // Four slow tool-call steps (700ms each): turn 1 completes at ~0.7s,
    // turn 2 is in flight when the 1s budget expires — and every retry
    // faces the same slow script, so it times out too.
    let server = spawn_scripted_server(vec![
        tool_step(700),
        tool_step(700),
        tool_step(700),
        tool_step(700),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            task_timeout_secs: 1,
            retry_budget: 2,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut req = DelegationRequest::single("gov-timeout-task");
    req.max_turns = Some(4); // bound today's (pre-implementation) run
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));

    let result = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;

    let err = result.error.as_deref().unwrap_or("");
    assert!(
        err.contains("[timeout] task exceeded 1s wall-clock budget"),
        "a task over its 1s wall-clock budget must be reported with the exact timeout text, got success={} error={:?}",
        result.success,
        result.error
    );
    assert!(
        !result.success,
        "a timed-out task (allowance exhausted) must not report success"
    );

    let waits = drain_retry_waits(&mut event_rx);
    assert!(
        waits.len() <= 1,
        "timeout retries must stay within the per-task allowance (subagent_recovery_attempts default 1); saw {} RetryAttempt events ({:?})",
        waits.len(),
        waits
    );
}

/// FR-005: against an always-failing provider (HTTP 500 on every
/// response), every RetryAttempt is spaced by jittered exponential
/// backoff — wait_secs >= backoff_base_secs * 0.9 (jitter only adds) —
/// and at least one of two concurrent failing tasks ends with either the
/// fatal-failure text or the fast-fail `retry budget exhausted` reason.
/// (Exact budget concurrency is not observable from events alone; the
/// strong assertion lands in the implementation-wave lib tests. Behavioral
/// RED today: the only existing retry of provider-500s is subagent
/// recovery with wait_secs = 0.0, so the >= 0.9 spacing can never hold
/// and the budget-exhausted text can never appear.)
#[tokio::test]
async fn gov_retry_backoff_spacing_and_budget() {
    let server = spawn_scripted_server((0..12).map(|_| fail_step(50)).collect()).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            retry_budget: 1,
            backoff_base_secs: 1.0,
            backoff_max_secs: 60.0,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

    // Dispatch TWO always-failing tasks concurrently.
    let mut handles = Vec::with_capacity(2);
    for i in 0..2 {
        let mgr = mgr.clone();
        let cfg = agent_config(server.url());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        let tx = event_tx.clone();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(format!("gov-fail-task-{i}"));
            mgr.dispatch_single(&req, &cfg, &tree, &base, Some(&tx)).await
        }));
    }
    let mut results: Vec<DelegationResult> = Vec::with_capacity(2);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    assert_eq!(results.len(), 2);

    let waits = drain_retry_waits(&mut event_rx);
    assert!(
        !waits.is_empty(),
        "the failing tasks must surface RetryAttempt events (retry budget 1, recovery default 1)"
    );
    assert!(
        waits.iter().all(|w| *w >= 0.9),
        "every RetryAttempt must be spaced by jittered backoff (base 1.0s, jitter only adds): observed wait_secs {waits:?}"
    );
    assert!(
        results.iter().any(|r| {
            let e = r.error.as_deref().unwrap_or("");
            !r.success && (e.contains("fatal") || e.contains("retry budget exhausted"))
        }),
        "at least one failing task must end with the fatal-failure text or `retry budget exhausted`; results: {:?}",
        results.iter().map(|r| (r.success, r.error.clone())).collect::<Vec<_>>()
    );
}

/// FR-006: a task that times out with checkpointing ON resumes from its
/// last completed turn. Script: two 800ms tool turns complete inside the
/// 2s budget; the third (in-flight) step crosses it, so run 1 times out
/// after exactly 2 completed turns (checkpoint k = 2) having issued 3
/// provider requests. The internal retry re-dispatches with the same
/// parent config (same server URL); its requests ALL carry the resume
/// preamble marker `Continue the task from turn`, it issues ONLY the
/// remaining turns (script total 4 - offset 2 = 2 marker requests — NOT
/// the full 4), and it reaches the scripted final: success.
#[tokio::test]
async fn gov_timeout_resume_skips_completed_turns() {
    let server = spawn_scripted_server(vec![
        tool_step(800),
        tool_step(800),
        tool_step(800),
        final_step(0),
    ])
    .await;
    // Run 1 completes exactly 2 turns within the 2s budget; the resumed
    // run must continue at script[2]. Pre-set (rather than set between
    // runs, which the automatic internal retry makes impossible): the
    // offset is only consulted when a marker request arrives, i.e. during
    // the resumed run — the initial run never reads it.
    server.set_resume_offset(2);

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            task_timeout_secs: 2,
            retry_budget: 2,
            checkpointing: true,
            ..Default::default()
        },
    ));

    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut req = DelegationRequest::single("gov-resume-task");
    req.max_turns = Some(6); // never the binding constraint here
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));

    let result = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;

    assert_eq!(
        server.marker_requests(),
        2,
        "the resumed retry must issue exactly the REMAINING turns (script 4 - offset 2 = 2 marker requests), each carrying the resume preamble `{RESUME_MARKER}`; got {} marker requests — no resume exists today, so the marker never appears",
        server.marker_requests()
    );
    assert_eq!(
        server.seq_requests(),
        3,
        "run 1 must issue exactly 3 plain provider requests before the 2s timeout (2 completed turns + 1 in flight)"
    );
    assert_eq!(
        server.total_requests(),
        5,
        "total = 3 (run 1) + 2 (resumed run), NOT the full-turn 4+ a re-elicitation would produce"
    );
    assert!(
        result.success,
        "the resumed run must reach the scripted final and succeed; error: {:?}",
        result.error
    );
}

/// FR-006 counterpart: with checkpointing OFF, a timed-out task's retry is
/// a FULL restart. Script: six 800ms tool steps — run 1 issues 3 requests
/// before the 2s budget expires; the retry (no checkpoint to resume from)
/// replays from turn 1 with NO resume marker on any request and issues
/// the full turn count again (3 more requests, total 6 — not the 5 a
/// 2-turn resume skip would produce); facing the same slow script it also
/// times out, so the final result stays `[timeout]`-failed after the
/// allowance is exhausted.
#[tokio::test]
async fn gov_no_checkpoint_timeout_is_full_restart() {
    let server = spawn_scripted_server(vec![
        tool_step(800),
        tool_step(800),
        tool_step(800),
        tool_step(800),
        tool_step(800),
        tool_step(800),
    ])
    .await;
    // resume_offset stays 0 — and must never be consulted, because no
    // request may carry the resume marker when checkpointing is off.

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            task_timeout_secs: 2,
            retry_budget: 2,
            checkpointing: false,
            ..Default::default()
        },
    ));

    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut req = DelegationRequest::single("gov-full-restart-task");
    req.max_turns = Some(6); // bound today's (pre-implementation) run
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));

    let result = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;

    assert_eq!(
        server.marker_requests(),
        0,
        "with checkpointing=false the retry is a full restart — no request may carry the resume marker `{RESUME_MARKER}`"
    );
    assert_eq!(
        server.total_requests(),
        6,
        "full restart: run 1 (3 requests) + replay from turn 1 (3 requests) = 6 — NOT the 5 a 2-turn resume skip would produce; got {}",
        server.total_requests()
    );
    assert!(
        !result.success,
        "with only slow steps available, both the initial run and the full-restart retry exceed the 2s budget — the final result must stay failed; got success=true error={:?}",
        result.error
    );
    let err = result.error.as_deref().unwrap_or("");
    assert!(
        err.contains("[timeout]"),
        "the allowance-exhausted final result must still carry the timeout outcome; got {err:?}"
    );
}

/// SC-002/T012: with retry_budget=1 and several ALWAYS-FAILING tasks
/// dispatched concurrently, at most ONE retry may be in flight at any
/// moment system-wide; every other task that fails while the budget is
/// held must fast-fail with the exact exhaustion text instead of adding
/// load. (The per-task allowance stays at its default 1.)
#[tokio::test]
async fn gov_concurrent_failures_respect_global_budget() {
    let steps: Vec<ScriptedStep> = (0..24).map(|_| fail_step(80)).collect();
    let server = spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            retry_budget: 1,
            max_queue_depth: 8,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut handles = Vec::new();
    for i in 0..4 {
        let mgr = Arc::clone(&mgr);
        let url = server.url();
        let tx = event_tx.clone();
        handles.push(tokio::spawn(async move {
            let ac = agent_config(url);
            let tree = Config::defaults();
            let mut base = ToolRegistry::new();
            base.register(Arc::new(EchoTool));
            let req = DelegationRequest::single(format!("always-fail-{}", i));
            mgr.dispatch_single(&req, &ac, &tree, &base, Some(&tx)).await
        }));
    }
    drop(event_tx);
    let mut results: Vec<DelegationResult> = Vec::with_capacity(4);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }

    let mut exhausted = 0usize;
    for res in &results {
        assert!(!res.success, "fail_step tasks must fail: {:?}", res.error);
        if res
            .error
            .as_deref()
            .unwrap_or("")
            .contains("[retry budget exhausted]")
        {
            exhausted += 1;
        }
    }
    assert!(
        exhausted >= 1,
        "with budget=1 and 4 concurrent always-fail tasks, at least one must fast-fail on the exhausted budget (observed {exhausted})"
    );
    // Drain events: DelegationRetryBudgetExhausted must have been emitted
    // for the exhausted tasks (raw form, or wrapped in SubagentEvent —
    // matched defensively, mirroring drain_retry_waits).
    let mut budget_events = 0usize;
    while let Ok(ev) = event_rx.try_recv() {
        let hit = matches!(ev, AgentEvent::DelegationRetryBudgetExhausted { .. })
            || matches!(
                ev,
                AgentEvent::SubagentEvent { ref event, .. }
                    if matches!(**event, AgentEvent::DelegationRetryBudgetExhausted { .. })
            );
        if hit {
            budget_events += 1;
        }
    }
    assert_eq!(
        budget_events, exhausted,
        "one DelegationRetryBudgetExhausted event per fast-failed task"
    );
}
