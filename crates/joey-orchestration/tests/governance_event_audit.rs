//! Integration test: governance event-emission audit (T026).
//!
//! Per-mechanism audit that every governance outcome is EMITTED through
//! the manager's event path (`dispatch_single`'s `event_tx`) — the tap
//! CLI/TUI surfaces consume. Audit of src/manager.rs emission sites:
//!   - DelegationBusy            (~1403, busy refusal branch)
//!   - DelegationTimeout         (~1909 spawned path / ~1995 inline path)
//!   - DelegationCacheHit        (~1277, persistent cache lookup)
//!   - DelegationDegradedOutput  (~2212, degraded-mode sampled output)
//!   - RetryAttempt              (~2137, governance retry loop)
//!
//! AUDIT GAPS FOUND (recorded, not fixed here):
//!   - GAP 1: `AgentEvent::DelegationRetryBudgetExhausted` is NEVER
//!     emitted by the manager (zero construction sites in src; the
//!     variant exists in joey-agent-core events.rs and is exercised only
//!     by tests/governance_event_compat.rs). Budget exhaustion surfaces
//!     solely as the `[retry budget exhausted]` error-text prefix on the
//!     terminal result (manager.rs ~2119). RetryAttempt is the only
//!     retry-path event — asserted as such in gov_audit_retry_events.
//!   - GAP 2: `AgentEvent::CapacitySnapshot` is NEVER emitted anywhere
//!     (no periodic loop, no admission-time emission). gov_audit_
//!     capacity_snapshot is an #[ignore]d placeholder recording this.
//!
//! Harness mirrors tests/governance_retry.rs verbatim (which mirrors
//! governance_admission.rs / concurrency_limiter.rs): scripted mock
//! OpenAI provider over a TcpListener on 127.0.0.1:0, resume-aware
//! request routing, agent_config(), governance_manager() forcing
//! enabled=true + isolated data_dir, and the unbounded_channel event
//! wiring `mgr.dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))`.
//!
//! Every test fn starts with `gov_audit_` (T026 convention).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::{
    DelegationRequest, DelegationResult, ManagerConfig, SubagentManager,
};
use joey_tools::ToolRegistry;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The resume preamble marker a resumed child's first provider request
/// must carry (kept from the governance_retry.rs harness this file
/// copies; the routing is inert for these scenarios).
const RESUME_MARKER: &str = "Continue the task from turn";

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (verbatim harness style from
// tests/governance_retry.rs) with in-flight concurrency probes and
// resume-aware request routing.
// ---------------------------------------------------------------------------

/// One scripted response: content, an optional tool call, reported usage,
/// a delay, and an HTTP status (>= 500 serves an error body).
#[derive(Clone)]
struct ScriptedStep {
    content: &'static str,
    tool_call: Option<&'static str>,
    usage: (u64, u64, u64),
    delay_ms: u64,
    http_status: u16,
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
    /// Sequential (non-resume) script position (clamped to the last entry
    /// once exhausted — budgets.rs convention; an all-fail script keeps
    /// failing).
    seq_index: Arc<AtomicUsize>,
    /// Number of marker-carrying (resume) requests served so far.
    marker_index: Arc<AtomicUsize>,
    /// Total requests served (marker + sequential).
    total_requests: Arc<AtomicUsize>,
    /// Requests whose body carried the resume marker.
    marker_requests: Arc<AtomicUsize>,
    /// Script index a resumed run starts from; only consulted when a
    /// marker request arrives (inert in this file's scenarios).
    resume_offset: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
}

/// Serve exactly one scripted request per connection, then close.
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

/// Test-side handle on the scripted mock provider (trimmed to what this
/// file's audits need: URL + total request count).
struct GovServer {
    base_url: String,
    total_requests: Arc<AtomicUsize>,
}

impl GovServer {
    fn url(&self) -> String {
        self.base_url.clone()
    }

    fn total_requests(&self) -> usize {
        self.total_requests.load(Ordering::SeqCst)
    }
}

/// Bind the scripted mock provider on 127.0.0.1:0.
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
    }
}

// ---------------------------------------------------------------------------
// Audit test helpers.
// ---------------------------------------------------------------------------

/// Parent AgentConfig pointing at the scripted mock provider (mirrors
/// governance_retry.rs's agent_config).
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

/// Manager with governance forced ON and an isolated data dir; mechanism
/// overrides arrive per test (mirrors governance_retry.rs's
/// governance_manager verbatim).
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

/// Collect every event observed on the dispatch event channel. All
/// manager emission sites fire BEFORE dispatch_single returns, so after
/// the dispatch future(s) complete the events are already queued; a
/// short 2s grace window on the first receive guards against any late
/// send, then the queue is drained dry (drain-with-timeout convention
/// from governance_retry.rs's drain_retry_waits).
async fn drain_events(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        out.push(ev);
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests — one per governance mechanism, each asserting the mechanism's
// event actually arrives on the event_tx channel surfaces tap.
// ---------------------------------------------------------------------------

/// Mechanism 1 — bounded admission: queue cap 0, 2 child slots, 3
/// concurrent dispatches (distinct goals — identical goals would take
/// the single-flight follower path and skip admission) → 2 running,
/// third refused at a zero-cap queue → exactly one
/// DelegationBusy{queue_depth: 0, cap: 0} event on the channel.
///
/// NOTE (cap 0, not 1 — pre-existing src bug): with any queue waiter
/// ENQUEUED the dispatch deadlocks in governance.rs's StartGate —
/// `admit_next` hands the first-ever waiter seq=2 (`next_seq` starts
/// at 1, incremented before use), and `wait_turn` needs
/// `started + 1 >= seq` i.e. started >= 1, but `started` is only
/// bumped by a waiter that already passed — so the first queued waiter
/// waits forever. The pre-existing tests/governance_admission.rs
/// queued-waiter tests hang on this too (verified: 60s+ stalls).
/// Cap 0 refuses at `push` without enqueueing, exercising the busy
/// emission path the audit targets.
#[tokio::test]
async fn gov_audit_busy_event() {
    let server = spawn_scripted_server(vec![final_step(300); 3]).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            max_queue_depth: 0,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handles = Vec::with_capacity(3);
    for i in 0..3 {
        let mgr = mgr.clone();
        let cfg = agent_config(server.url());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        let tx = event_tx.clone();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(format!("gov-audit-busy-{i}"));
            mgr.dispatch_single(&req, &cfg, &tree, &base, Some(&tx)).await
        }));
    }
    let mut results: Vec<DelegationResult> = Vec::with_capacity(3);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    assert_eq!(results.len(), 3);

    // The refusal itself: exactly one non-success, carrying [busy].
    let refused: Vec<&DelegationResult> = results
        .iter()
        .filter(|r| !r.success)
        .collect();
    assert_eq!(
        refused.len(),
        1,
        "exactly one of 3 dispatches (2 slots, queue cap 0) must be refused; results: {:?}",
        results.iter().map(|r| (r.success, r.error.clone())).collect::<Vec<_>>()
    );
    assert!(
        refused[0].error.as_deref().unwrap_or("").contains("[busy]"),
        "the refused dispatch must carry the [busy] outcome text"
    );

    // THE AUDIT: the DelegationBusy event was emitted through the
    // manager's event path with appropriate nonzero fields.
    let events = drain_events(&mut event_rx).await;
    let busy: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::DelegationBusy { queue_depth, cap } => Some((*queue_depth, *cap)),
            _ => None,
        })
        .collect();
    assert_eq!(
        busy.len(),
        1,
        "the busy refusal must emit exactly one DelegationBusy event on event_tx; got {busy:?} among {} events",
        events.len()
    );
    assert_eq!(
        busy[0],
        (0, 0),
        "DelegationBusy must report the zero-cap queue exactly (depth 0, cap 0)"
    );
}

/// Mechanism 2 — wall-clock task timeout: task_timeout_secs: 1 against
/// 2000ms provider steps → the budget expires mid-step and a
/// DelegationTimeout{timeout_secs: 1, ..} event arrives on the channel
/// (with the dispatch's goal). The retry allowance (default 1) re-faces
/// the same slow script, so the event may arrive twice — at least once
/// is the audit contract.
#[tokio::test]
async fn gov_audit_timeout_event() {
    let server = spawn_scripted_server(vec![final_step(2000); 4]).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            task_timeout_secs: 1,
            retry_budget: 2,
            // Short backoff keeps the retry attempt inside the test
            // budget (audit target is the event, not the spacing).
            backoff_base_secs: 0.1,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut req = DelegationRequest::single("gov-audit-timeout-goal");
    req.max_turns = Some(4);
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let base = ToolRegistry::new();

    let result = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;
    assert!(
        !result.success
            && result.error.as_deref().unwrap_or("").contains("[timeout]"),
        "the slow task must end with the [timeout] outcome; got success={} error={:?}",
        result.success,
        result.error
    );

    // THE AUDIT: DelegationTimeout{timeout_secs: 1} was emitted through
    // the manager's event path.
    let events = drain_events(&mut event_rx).await;
    let timeouts: Vec<(u64, &str, u64)> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::DelegationTimeout { child_id, goal, timeout_secs } => {
                Some((*child_id, goal.as_str(), *timeout_secs))
            }
            _ => None,
        })
        .collect();
    assert!(
        !timeouts.is_empty(),
        "a timed-out dispatch must emit DelegationTimeout on event_tx ({} other events observed)",
        events.len()
    );
    assert!(
        timeouts
            .iter()
            .all(|(_, _, secs)| *secs == 1),
        "every DelegationTimeout must carry timeout_secs == 1; got {timeouts:?}"
    );
    assert!(
        timeouts.iter().any(|(_, goal, _)| *goal == "gov-audit-timeout-goal"),
        "the DelegationTimeout event must carry the dispatch's goal; got {timeouts:?}"
    );
}

/// Mechanism 3 — persistent result cache: result_cache_enabled: true;
/// one successful dispatch stores the result, and re-dispatching the
/// IDENTICAL request (same goal/budgets → same signature) serves from
/// the cache with NO new provider request and emits exactly one
/// DelegationCacheHit{signature} (signature nonempty) on the channel.
#[tokio::test]
async fn gov_audit_cache_hit_event() {
    let server = spawn_scripted_server(vec![final_step(100)]).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            result_cache_enabled: true,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let base = ToolRegistry::new();

    // Dispatch 1: executes against the provider and stores the result.
    let req = DelegationRequest::single("gov-audit-cache-goal");
    let r1 = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;
    assert!(
        r1.success,
        "the first (executing) dispatch must succeed; error: {:?}",
        r1.error
    );

    // Dispatch 2: the IDENTICAL request — served from the cache.
    let r2 = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;
    assert!(
        r2.success,
        "the cached re-dispatch must succeed; error: {:?}",
        r2.error
    );
    assert_eq!(
        server.total_requests(),
        1,
        "a cache hit must NOT re-execute (exactly one provider request ever)"
    );

    // THE AUDIT: DelegationCacheHit{signature} was emitted through the
    // manager's event path, signature nonempty.
    let events = drain_events(&mut event_rx).await;
    let sigs: Vec<&str> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::DelegationCacheHit { signature } => Some(signature.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sigs.len(),
        1,
        "the second (cached) dispatch must emit exactly one DelegationCacheHit on event_tx"
    );
    assert!(
        !sigs[0].is_empty(),
        "the DelegationCacheHit signature must be nonempty"
    );
}

/// Mechanism 4 — degraded mode: degraded_mode_enabled: true with
/// degraded_sample_rate: 1.0 (every normal-lane executed output sampled)
/// → one successful dispatch emits exactly one
/// DelegationDegradedOutput{goal, sample_rate: 1.0} on the channel.
#[tokio::test]
async fn gov_audit_degraded_event() {
    let server = spawn_scripted_server(vec![final_step(100)]).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            degraded_mode_enabled: true,
            degraded_sample_rate: 1.0,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let req = DelegationRequest::single("gov-audit-degraded-goal");
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let base = ToolRegistry::new();

    let result = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;
    assert!(
        result.success,
        "the degraded-mode dispatch must still succeed; error: {:?}",
        result.error
    );

    // THE AUDIT: DelegationDegradedOutput was emitted through the
    // manager's event path with the configured sample rate and goal.
    let events = drain_events(&mut event_rx).await;
    let degraded: Vec<(&str, f64)> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::DelegationDegradedOutput { goal, sample_rate, .. } => {
                Some((goal.as_str(), *sample_rate))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        degraded.len(),
        1,
        "one degraded-sampled output must emit exactly one DelegationDegradedOutput on event_tx"
    );
    assert_eq!(degraded[0].0, "gov-audit-degraded-goal");
    assert_eq!(degraded[0].1, 1.0, "sample_rate must echo the configured 1.0");
}

/// Mechanism 5 — retry loop: always-failing provider (HTTP 500), retry
/// config mirroring governance_retry.rs scenario (b) (retry_budget 1,
/// backoff base 1.0/max 60.0) with the default recovery allowance
/// (subagent_recovery_attempts = 1) → attempt 1 fails, the loop emits
/// RetryAttempt{attempt: 1, max_retries: 1} on the channel, then the
/// final attempt fails terminally.
///
/// AUDIT NOTE (GAP 1): the retry path emits ONLY RetryAttempt —
/// `DelegationRetryBudgetExhausted` has ZERO construction sites in src/
/// (budget exhaustion surfaces solely as the `[retry budget exhausted]`
/// error-text prefix on the terminal result, manager.rs ~2119). This
/// test asserts what the code DOES emit.
#[tokio::test]
async fn gov_audit_retry_events() {
    let server = spawn_scripted_server((0..8).map(|_| fail_step(50)).collect()).await;

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
    let req = DelegationRequest::single("gov-audit-retry-goal");
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let base = ToolRegistry::new();

    let result = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;
    assert!(
        !result.success,
        "an always-failing provider must end failed; got success=true"
    );

    // THE AUDIT: RetryAttempt was emitted through the manager's event
    // path with the expected attempt accounting.
    let events = drain_events(&mut event_rx).await;
    let attempts: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::RetryAttempt { attempt, max_retries, .. } => {
                Some((*attempt, *max_retries))
            }
            _ => None,
        })
        .collect();
    assert!(
        !attempts.is_empty(),
        "a retried fatal failure must emit RetryAttempt on event_tx ({} other events observed)",
        events.len()
    );
    assert!(
        attempts.contains(&(1, 1)),
        "the retry after attempt 1 must emit RetryAttempt{{attempt: 1, max_retries: 1}}; got {attempts:?}"
    );
}

/// Mechanism 6 — capacity snapshot (T026 GAP 2 closed): a busy refusal
/// (queue cap 0, 2 slots busy, third dispatch refused) now emits BOTH a
/// DelegationBusy AND a CapacitySnapshot{running: 2, queued: 0,
/// queue_cap: 0, max_children: 2} — the snapshot makes running-vs-caps
/// observable at the exact moment capacity is exhausted.
#[tokio::test]
async fn gov_audit_capacity_snapshot() {
    let server = spawn_scripted_server(vec![final_step(300); 3]).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            max_queue_depth: 0,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handles = Vec::with_capacity(3);
    for i in 0..3 {
        let mgr = mgr.clone();
        let cfg = agent_config(server.url());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        let tx = event_tx.clone();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(format!("gov-audit-snapshot-{i}"));
            mgr.dispatch_single(&req, &cfg, &tree, &base, Some(&tx)).await
        }));
    }
    let mut results: Vec<DelegationResult> = Vec::with_capacity(3);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    assert_eq!(results.len(), 3);
    assert_eq!(
        results.iter().filter(|r| !r.success).count(),
        1,
        "exactly one of 3 dispatches (2 slots, queue cap 0) must be refused; results: {:?}",
        results.iter().map(|r| (r.success, r.error.clone())).collect::<Vec<_>>()
    );

    // THE AUDIT: the busy refusal emits BOTH DelegationBusy and a
    // CapacitySnapshot carrying the exact capacity picture.
    let events = drain_events(&mut event_rx).await;
    let busy: Vec<(usize, usize)> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::DelegationBusy { queue_depth, cap } => Some((*queue_depth, *cap)),
            _ => None,
        })
        .collect();
    assert_eq!(
        busy.len(),
        1,
        "the busy refusal must emit exactly one DelegationBusy event on event_tx; got {busy:?} among {} events",
        events.len()
    );
    assert_eq!(busy[0], (0, 0));
    let snapshots: Vec<(usize, usize, usize, usize)> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::CapacitySnapshot { running, queued, queue_cap, max_children } => {
                Some((*running, *queued, *queue_cap, *max_children))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        snapshots.len(),
        1,
        "the busy refusal must emit exactly one CapacitySnapshot on event_tx; got {snapshots:?} among {} events",
        events.len()
    );
    assert_eq!(
        snapshots[0],
        (2, 0, 0, 2),
        "CapacitySnapshot must report running 2 (both slots held), queued 0, queue_cap 0, max_children 2"
    );
}

/// Mechanism 7 — retry budget exhaustion (T026 GAP 1 closed, R10):
/// retry_budget: 0 + recovery allowance 1 and always-failing steps —
/// attempt 1 fails, the retry is refused by the zero budget, and the
/// terminal result carries `[retry budget exhausted]` AND a
/// DelegationRetryBudgetExhausted{goal, budget: 0, in_flight: 0} event
/// arrives on the channel.
#[tokio::test]
async fn gov_audit_retry_budget_exhausted_event() {
    let server = spawn_scripted_server((0..4).map(|_| fail_step(50)).collect()).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            retry_budget: 0,
            backoff_base_secs: 0.1,
            backoff_max_secs: 0.1,
            ..Default::default()
        },
    ));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let req = DelegationRequest::single("gov-audit-budget-goal");
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let base = ToolRegistry::new();

    let result = mgr
        .dispatch_single(&req, &cfg, &tree, &base, Some(&event_tx))
        .await;
    assert!(
        !result.success
            && result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("[retry budget exhausted]"),
        "a zero retry budget must end with the [retry budget exhausted] outcome; got success={} error={:?}",
        result.success,
        result.error
    );

    // THE AUDIT: DelegationRetryBudgetExhausted was emitted through the
    // manager's event path, carrying the dispatch's goal.
    let events = drain_events(&mut event_rx).await;
    let exhausted: Vec<(String, usize, usize)> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::DelegationRetryBudgetExhausted { goal, budget, in_flight } => {
                Some((goal.clone(), *budget, *in_flight))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        exhausted.len(),
        1,
        "budget exhaustion must emit exactly one DelegationRetryBudgetExhausted on event_tx; got {exhausted:?} among {} events",
        events.len()
    );
    assert_eq!(
        exhausted[0].0, "gov-audit-budget-goal",
        "the DelegationRetryBudgetExhausted event must carry the dispatch's goal"
    );
    assert_eq!(exhausted[0].1, 0, "budget must echo the configured retry_budget 0");
    assert_eq!(exhausted[0].2, 0, "in_flight reports the exhausted capacity (== budget 0)");
}
