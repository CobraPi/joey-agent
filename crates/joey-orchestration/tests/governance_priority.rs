//! Integration test: governance priority lanes + degraded mode marking +
//! sustained-overload signal (feature 030, US6 / FR-013, T021 TDD RED).
//!
//! RED contracts pinned here (see specs/030-please-implement-features/
//! contracts/busy-and-outcomes.md):
//!   (a) under contention, a CRITICAL dispatch's first provider request
//!       arrives BEFORE the first requests of the queued NORMAL dispatches
//!       it jumped (critical jumps the queue line only — never preempts
//!       running work);
//!   (b) with degraded_mode.enabled=true and sample_rate=1.0, every
//!       sampled (background+normal) output carries the exact marker
//!       ` [degraded]` in its result summary and degraded=true in its
//!       resource record; CRITICAL work is never sampled (no marker,
//!       degraded=false);
//!   (c) with degraded mode disabled (the default), NO output carries the
//!       marker and no record is flagged (GREEN guard — passes today and
//!       must keep passing);
//!   (d) when busy refusals hit a rate of 3+ within 60 seconds, the busy
//!       text appends the exact suffix
//!       ` [overload] sustained saturation detected — consider degraded mode`
//!       AFTER the standard busy text (live RED: no suffix exists today).
//!
//! PRIORITY-FIELD GATING: `DelegationRequest` has NO `priority` field yet —
//! T022 adds it. The two tests that need it (critical-admission order and
//! the critical-half of degraded marking) are gated behind #[ignore] with
//! the exact usage kept as a commented line to uncomment when the field
//! lands. The FILE must compile today; the unignored tests (c)/(d) run.
//!
//! Harness mirrors tests/governance_admission.rs (which mirrors
//! tests/concurrency_limiter.rs): ScriptedFinal mock provider, in-flight
//! probes, TcpListener on 127.0.0.1:0 — PLUS request-arrival-order probing:
//! the server records, in arrival order, the first provider request per
//! goal (the goal tag is extracted from the request body — the child's
//! initial prompt embeds the goal verbatim).
//!
//! Every fn starts with `gov_` so intermediate regression runs can
//! `--skip gov_` until T022/T023 implement the mechanisms.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::resource_records::ResourceRecordStore;
use joey_orchestration::types::{Priority, ResourceRecordOutcome};
use joey_orchestration::{
    DelegateTask, DelegationRequest, DelegationResult, ManagerConfig, SubagentManager,
};
use joey_tools::ToolRegistry;
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Exact degraded marker (FR-013, busy-and-outcomes.md): leading space,
/// presented in the result text shown to the model.
const DEGRADED_MARKER: &str = " [degraded]";

/// Exact sustained-overload suffix (FR-002, busy-and-outcomes.md): appended
/// AFTER the standard busy text once busy refusals hit 3+ within 60s.
const OVERLOAD_SUFFIX: &str = " [overload] sustained saturation detected — consider degraded mode";

/// Standard busy text with queue cap 1 (refusal only happens when the queue
/// is FULL, so N == cap == 1 deterministically).
const BUSY_CAP1: &str = "[busy] delegation queue full (1 waiting, cap 1) — re-plan or defer";

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (governance_admission.rs harness)
// with in-flight concurrency probes AND first-request-per-goal arrival order.
// ---------------------------------------------------------------------------

/// One scripted response per HTTP connection: 200 with a plain assistant
/// text response, after `delay_ms`.
#[derive(Clone)]
struct ScriptedFinal {
    delay_ms: u64,
    text: &'static str,
}

fn gov_openai_body(content: &str) -> String {
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

/// Read one HTTP request (headers + content-length body); returns the raw body.
async fn gov_read_http_body(stream: &mut TcpStream) -> Option<String> {
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
    queue: Arc<Mutex<VecDeque<ScriptedFinal>>>,
    /// Goal tags to look for in request bodies (arrival-order probing).
    tags: Arc<Vec<String>>,
    /// First-request arrival order: one entry per goal tag, pushed when the
    /// tag is first seen in a request body.
    first_arrivals: Arc<Mutex<Vec<String>>>,
    /// Per-connection start time (millis since server bind), in
    /// completion order of the body read.
    #[allow(dead_code)]
    starts_ms: Arc<Mutex<Vec<u64>>>,
    /// Per-connection end time (millis since server bind), recorded after
    /// the response is written — in completion order of the connection.
    #[allow(dead_code)]
    ends_ms: Arc<Mutex<Vec<u64>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    epoch: Instant,
}

/// Serve exactly one scripted request per connection, then close. Records
/// the goal tag of the request body on its FIRST appearance (arrival-order
/// probe) before any delay is served.
async fn gov_serve_conn(mut stream: TcpStream, p: ServerProbe) {
    let Some(body) = gov_read_http_body(&mut stream).await else {
        return;
    };
    // Arrival-order probe: the child's initial prompt embeds the goal
    // verbatim, so the first request carrying each tag IS that goal's
    // first provider request.
    for tag in p.tags.iter() {
        if body.contains(tag.as_str()) {
            let mut arrivals = p.first_arrivals.lock().unwrap();
            if !arrivals.contains(tag) {
                arrivals.push(tag.clone());
            }
            break;
        }
    }
    let step = p.queue.lock().unwrap().pop_front();
    let Some(step) = step else {
        let body_out = r#"{"error":{"message":"script exhausted"}}"#.to_string();
        let resp = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_out.len(),
            body_out
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.shutdown().await;
        return;
    };
    p.starts_ms.lock().unwrap().push(p.epoch.elapsed().as_millis() as u64);
    let now_in_flight = p.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    p.max_in_flight.fetch_max(now_in_flight, Ordering::SeqCst);
    if step.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(step.delay_ms)).await;
    }
    let body_out = gov_openai_body(step.text);
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_out.len(),
        body_out
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
    p.ends_ms.lock().unwrap().push(p.epoch.elapsed().as_millis() as u64);
    p.in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// Bind the scripted mock provider with arrival-order probing. `tags` are
/// the goal strings to track; returns (base_url, starts, ends,
/// first-arrival order, max observed in-flight requests).
async fn gov_spawn_scripted_server(
    steps: Vec<ScriptedFinal>,
    tags: Vec<String>,
) -> (
    String,
    Arc<Mutex<Vec<u64>>>,
    Arc<Mutex<Vec<u64>>>,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicUsize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let starts_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let ends_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let first_arrivals: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    let tags = Arc::new(tags);
    let epoch = Instant::now();
    let ret_starts = starts_ms.clone();
    let ret_ends = ends_ms.clone();
    let ret_arrivals = first_arrivals.clone();
    let ret_max = max_in_flight.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let probe = ServerProbe {
                queue: queue.clone(),
                tags: tags.clone(),
                first_arrivals: first_arrivals.clone(),
                starts_ms: starts_ms.clone(),
                ends_ms: ends_ms.clone(),
                in_flight: in_flight.clone(),
                max_in_flight: max_in_flight.clone(),
                epoch,
            };
            tokio::spawn(async move {
                gov_serve_conn(stream, probe).await;
            });
        }
    });
    (
        format!("http://{addr}"),
        ret_starts,
        ret_ends,
        ret_arrivals,
        ret_max,
    )
}

// ---------------------------------------------------------------------------
// Governance-priority test helpers.
// ---------------------------------------------------------------------------

/// Parent AgentConfig pointing at the scripted mock provider (mirrors
/// governance_admission.rs's agent_config).
fn gov_agent_config(base_url: String) -> AgentConfig {
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

/// Manager with the governance shape under test: `children` child slots,
/// 8 request permits, governance forced ON with the per-test
/// GovernanceConfig overrides, isolated data dir (the retry-test helper
/// convention).
fn gov_priority_manager(
    children: usize,
    mut gov: GovernanceConfig,
    data_dir: std::path::PathBuf,
) -> SubagentManager {
    gov.enabled = true;
    gov.data_dir = Some(data_dir);
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: children,
        max_concurrent_requests: 8,
        governance: gov,
        ..Default::default()
    })
}

/// One sequential dispatch of a single-goal request (tests 2/3 shape).
async fn gov_dispatch_one(
    mgr: &SubagentManager,
    base_url: &str,
    goal: &str,
) -> DelegationResult {
    let req = DelegationRequest::single(goal);
    let cfg = gov_agent_config(base_url.to_string());
    let tree = Config::defaults();
    let base = ToolRegistry::new();
    mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
}

/// Fire dispatches for `goals` CONCURRENTLY (one tokio::spawn each); the
/// caller joins via [`gov_join_results`] (test 4 must keep pressure between
/// waves, test 1 fills slots+queue before the critical dispatch).
fn gov_spawn_goals(
    mgr: Arc<SubagentManager>,
    base_url: String,
    goals: &[String],
) -> Vec<tokio::task::JoinHandle<DelegationResult>> {
    goals
        .iter()
        .map(|goal| {
            let mgr = mgr.clone();
            let cfg = gov_agent_config(base_url.clone());
            let goal = goal.clone();
            let tree = Config::defaults();
            let base = ToolRegistry::new();
            tokio::spawn(async move {
                let req = DelegationRequest::single(goal);
                mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
            })
        })
        .collect()
}

async fn gov_join_results(
    handles: Vec<tokio::task::JoinHandle<DelegationResult>>,
) -> Vec<DelegationResult> {
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    results
}

// A trivial tool the scripted child can call (registered into the base
// registry so the child's schema/dispatch both know it) — mirrors
// tests/concurrency_limiter.rs.

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
// Tests.
// ---------------------------------------------------------------------------

/// (a) THE PRIORITY RED TEST (FR-001/FR-013 context, US6): 2 slots, queue
/// cap 4. Four NORMAL slow dispatches (goal tags N1..N4, 2s mock each) fill
/// the pool partially (N1/N2 running, N3/N4 queued). One CRITICAL dispatch
/// (goal tag C1) then arrives; when the first slot frees, the critical head
/// of the priority lane is admitted BEFORE the queued normals — C1's first
/// provider request must arrive strictly before BOTH queued normals' first
/// requests (deterministic single run).
///
/// DelegationRequest's `priority` field landed in T022 — both priority
/// tests are live.
#[tokio::test]
async fn gov_critical_admitted_before_queued_normal() {
    // 2s per request: the two running normals hold their slots long past
    // C1's arrival; 8 steps ≥ the 5 expected connections.
    let steps = (0..8)
        .map(|_| ScriptedFinal { delay_ms: 2000, text: "final" })
        .collect();
    let tags: Vec<String> = (1..=4)
        .map(|i| format!("gov-prio-N{i}"))
        .chain(std::iter::once("gov-prio-C1".to_string()))
        .collect();
    let (base_url, _starts, _ends, first_arrivals, _max) =
        gov_spawn_scripted_server(steps, tags).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(gov_priority_manager(
        2,
        GovernanceConfig {
            max_queue_depth: 4,
            priority_enabled: true,
            ..Default::default()
        },
        dir.path().to_path_buf(),
    ));

    // Fill slots + queue partially: 4 NORMAL dispatches → 2 running, 2 queued.
    let normals: Vec<String> = (1..=4).map(|i| format!("gov-prio-N{i}")).collect();
    let normal_handles = gov_spawn_goals(mgr.clone(), base_url.clone(), &normals);
    // Let admission settle (2 running, 2 queued) before the critical arrives.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The CRITICAL dispatch (T022 `DelegationRequest::priority`).
    let mut req = DelegationRequest::single("gov-prio-C1");
    req.priority = Some(Priority::Critical);
    let cfg = gov_agent_config(base_url.clone());
    let tree = Config::defaults();
    let base = ToolRegistry::new();
    let c1 = mgr.dispatch_single(&req, &cfg, &tree, &base, None).await;

    let mut results = gov_join_results(normal_handles).await;
    results.push(c1);
    assert_eq!(results.len(), 5, "one result per dispatched request");
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "dispatch {i} must succeed (queue cap 4 holds all five), error: {:?}",
            r.error
        );
    }

    // First-request arrival ORDER: C1 strictly before BOTH queued normals.
    let order = first_arrivals.lock().unwrap().clone();
    assert_eq!(
        order.len(),
        5,
        "each goal's first provider request must be recorded, got {order:?}"
    );
    let pos = |tag: &str| {
        order
            .iter()
            .position(|t| t == tag)
            .unwrap_or_else(|| panic!("goal {tag} missing from arrival order {order:?}"))
    };
    assert!(
        pos("gov-prio-C1") < pos("gov-prio-N3"),
        "critical C1's first request must arrive before queued normal N3's, arrival order {order:?}"
    );
    assert!(
        pos("gov-prio-C1") < pos("gov-prio-N4"),
        "critical C1's first request must arrive before queued normal N4's, arrival order {order:?}"
    );
}

/// (b) THE DEGRADED-MARKING RED TEST (FR-013): degraded_mode.enabled=true,
/// sample_rate=1.0 (everything sampled), 1 slot. Two normal fast tasks:
/// every result summary CONTAINS ` [degraded]` (exact marker, leading
/// space) and both resource records (ResourceRecordStore) show
/// degraded==true. Then one CRITICAL dispatch: its summary does NOT carry
/// the marker and its record has degraded==false (critical is never
/// sampled).
#[tokio::test]
async fn gov_degraded_mode_marks_sampled_outputs() {
    let steps = (0..8)
        .map(|_| ScriptedFinal { delay_ms: 0, text: "final" })
        .collect();
    let (base_url, _starts, _ends, _arrivals, _max) =
        gov_spawn_scripted_server(steps, Vec::new()).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = gov_priority_manager(
        1,
        GovernanceConfig {
            max_queue_depth: 4,
            degraded_mode_enabled: true,
            degraded_sample_rate: 1.0,
            ..Default::default()
        },
        dir.path().to_path_buf(),
    );

    // Two normal fast tasks, sampled at rate 1.0 → both marked.
    let ra = gov_dispatch_one(&mgr, &base_url, "gov-degraded-A").await;
    let rb = gov_dispatch_one(&mgr, &base_url, "gov-degraded-B").await;
    assert!(ra.success, "normal task A must succeed: {:?}", ra.error);
    assert!(rb.success, "normal task B must succeed: {:?}", rb.error);
    assert!(
        ra.summary.contains(DEGRADED_MARKER),
        "sampled output (rate 1.0) must carry the exact ` [degraded]` marker, summary: {:?}",
        ra.summary
    );
    assert!(
        rb.summary.contains(DEGRADED_MARKER),
        "sampled output (rate 1.0) must carry the exact ` [degraded]` marker, summary: {:?}",
        rb.summary
    );

    // Both resource records show degraded==true (isolated data dir → every
    // record in the store comes from these dispatches).
    let store = ResourceRecordStore::open(Some(dir.path()));
    let records = store.load();
    assert_eq!(
        records.len(),
        2,
        "one resource record per completed task, got {} records",
        records.len()
    );
    assert!(
        records.iter().all(|r| r.degraded),
        "both sampled tasks' records must have degraded==true"
    );

    // A CRITICAL dispatch: never sampled → no marker, degraded==false.
    let mut req = DelegationRequest::single("gov-degraded-C");
    req.priority = Some(Priority::Critical);
    let cfg = gov_agent_config(base_url.clone());
    let tree = Config::defaults();
    let base = ToolRegistry::new();
    let rc = mgr.dispatch_single(&req, &cfg, &tree, &base, None).await;
    assert!(rc.success, "critical task must succeed: {:?}", rc.error);
    assert!(
        !rc.summary.contains(DEGRADED_MARKER),
        "critical work is never sampled — no degraded marker, summary: {:?}",
        rc.summary
    );
    let records = store.load();
    assert_eq!(records.len(), 3, "three records after the critical dispatch");
    assert_eq!(
        records.iter().filter(|r| !r.degraded).count(),
        1,
        "exactly the critical task's record has degraded==false"
    );
}

/// (c) GREEN GUARD (must pass today AND after T023): degraded_mode disabled
/// (the default) → no output carries ` [degraded]` and no record has
/// degraded==true.
#[tokio::test]
async fn gov_no_degraded_output_when_disabled() {
    let steps = (0..8)
        .map(|_| ScriptedFinal { delay_ms: 0, text: "final" })
        .collect();
    let (base_url, _starts, _ends, _arrivals, _max) =
        gov_spawn_scripted_server(steps, Vec::new()).await;

    let dir = tempfile::tempdir().unwrap();
    // degraded_mode_enabled defaults to false — explicit for the contract.
    let mgr = gov_priority_manager(
        2,
        GovernanceConfig {
            max_queue_depth: 4,
            degraded_mode_enabled: false,
            ..Default::default()
        },
        dir.path().to_path_buf(),
    );

    let ra = gov_dispatch_one(&mgr, &base_url, "gov-nodegraded-A").await;
    let rb = gov_dispatch_one(&mgr, &base_url, "gov-nodegraded-B").await;
    assert!(ra.success, "task A must succeed: {:?}", ra.error);
    assert!(rb.success, "task B must succeed: {:?}", rb.error);
    assert!(
        !ra.summary.contains(DEGRADED_MARKER),
        "degraded mode off ⇒ no marker, summary: {:?}",
        ra.summary
    );
    assert!(
        !rb.summary.contains(DEGRADED_MARKER),
        "degraded mode off ⇒ no marker, summary: {:?}",
        rb.summary
    );

    let store = ResourceRecordStore::open(Some(dir.path()));
    let records = store.load();
    assert!(
        records.iter().all(|r| !r.degraded),
        "degraded mode off ⇒ no record may have degraded==true, saw {}/{} flagged",
        records.iter().filter(|r| r.degraded).count(),
        records.len()
    );
}

/// (d) THE OVERLOAD-SIGNAL RED TEST (FR-002): 2 slots, queue cap 1. Five
/// concurrent slow dispatches → 2 run, 1 queued, 2 refused. A second wave
/// 100ms later keeps the pressure (slots + queue still saturated) for 2+
/// more refusals — ≥ 4 total within 60s. The FIRST TWO refusals carry the
/// standard busy text WITHOUT the overload suffix; from the 3rd refusal
/// onward the error CONTAINS ` [overload] sustained saturation detected —
/// consider degraded mode` appended AFTER the standard busy text.
/// RED today: no suffix is appended (the mechanism is T023).
#[tokio::test]
async fn gov_overload_signal_appended_to_busy_refusals() {
    // 500ms per request: the two running tasks hold their slots well past
    // the second wave at +100ms (200ms margin), so every wave-2 dispatch
    // finds slots AND queue full.
    let steps = (0..16)
        .map(|_| ScriptedFinal { delay_ms: 500, text: "final" })
        .collect();
    let (base_url, _starts, _ends, _arrivals, _max) =
        gov_spawn_scripted_server(steps, Vec::new()).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(gov_priority_manager(
        2,
        GovernanceConfig {
            max_queue_depth: 1,
            ..Default::default()
        },
        dir.path().to_path_buf(),
    ));

    // Wave 1: 5 concurrent slow dispatches → 2 running, 1 queued, 2 refused.
    let w1_goals: Vec<String> = (0..5).map(|i| format!("gov-ovl-w1-{i}")).collect();
    let w1_handles = gov_spawn_goals(mgr.clone(), base_url.clone(), &w1_goals);

    // Wave 2 after 100ms: slots and queue still saturated → 2+ more
    // refusals (all 5 refused in practice), ≥ 4 total within 60s.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let w2_goals: Vec<String> = (0..5).map(|i| format!("gov-ovl-w2-{i}")).collect();
    let w2_handles = gov_spawn_goals(mgr.clone(), base_url.clone(), &w2_goals);

    let w1 = gov_join_results(w1_handles).await;
    let w2 = gov_join_results(w2_handles).await;

    let is_refusal =
        |r: &DelegationResult| !r.success && r.error.as_deref().unwrap_or("").contains("[busy]");

    // The first two refusals (wave 1): standard busy text, NO suffix.
    let w1_refusals: Vec<&DelegationResult> = w1.iter().filter(|r| is_refusal(r)).collect();
    assert_eq!(
        w1_refusals.len(),
        2,
        "wave 1 (5 dispatches, 2 slots, cap 1) must produce exactly 2 busy refusals"
    );
    for (i, r) in w1_refusals.iter().enumerate() {
        let err = r.error.as_deref().unwrap();
        assert!(
            err.contains(BUSY_CAP1),
            "wave-1 refusal {i} must carry the standard busy text, error: {err:?}"
        );
        assert!(
            !err.contains(OVERLOAD_SUFFIX),
            "the first two refusals must NOT carry the overload suffix, error: {err:?}"
        );
    }

    // From the 3rd refusal onward (wave 2): the suffix is appended AFTER
    // the standard busy text.
    let w2_refusals: Vec<&DelegationResult> = w2.iter().filter(|r| is_refusal(r)).collect();
    assert!(
        w2_refusals.len() >= 2,
        "wave 2 must add at least 2 more refusals (>= 4 total), got {} wave-2 refusals",
        w2_refusals.len()
    );
    assert!(
        w1_refusals.len() + w2_refusals.len() >= 4,
        "sustained pressure must produce >= 4 refusals within 60s"
    );
    for (i, r) in w2_refusals.iter().enumerate() {
        let err = r.error.as_deref().unwrap();
        assert!(
            err.contains(BUSY_CAP1),
            "wave-2 refusal {i} must carry the standard busy text, error: {err:?}"
        );
        assert!(
            err.contains(OVERLOAD_SUFFIX),
            "RED: refusal {} (3rd onward within 60s) must carry the exact overload suffix `{OVERLOAD_SUFFIX}`, error: {err:?}",
            i + 3
        );
        let busy_pos = err.find(BUSY_CAP1).expect("busy text present");
        let suffix_pos = err.find(OVERLOAD_SUFFIX).expect("overload suffix present");
        assert!(
            suffix_pos > busy_pos,
            "the overload suffix must be appended AFTER the standard busy text, error: {err:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// T029: `priority` on the delegate_task TOOL surface (FR-013) — the tool arg
// must reach the admission lane and the resource record; the background FLAG
// keeps flag > arg precedence; the schema pins the additive contract.
// ---------------------------------------------------------------------------

/// T029 contract 1: the tool schema exposes `priority` additively — exact
/// shape (string enum critical|normal|background, default normal) and NOT in
/// `required`.
#[test]
fn gov_tool_surface_schema_pins_priority() {
    let tool = DelegateTask::new(
        Arc::new(SubagentManager::new(ManagerConfig::default())),
        gov_agent_config("http://127.0.0.1:9/v1".to_string()),
        Config::defaults(),
        ToolRegistry::new(),
        None,
        None,
    );
    use joey_tools::Tool as _;
    let p = tool.parameters();
    assert_eq!(
        p["properties"]["priority"],
        json!({
            "type": "string",
            "enum": ["critical", "normal", "background"],
            "default": "normal",
            "description": "Admission priority lane (feature 030, FR-013): critical jumps the queued line (never preempts running work); background defers to idle capacity; default normal. Ignored when governance or priority lanes are disabled."
        }),
        "the priority parameter must be exposed with the exact additive shape"
    );
    if let Some(required) = p.get("required").and_then(|r| r.as_array()) {
        assert!(
            !required.iter().any(|v| v == "priority"),
            "priority must not be a required parameter"
        );
    }
}

/// T029 contract 2: `priority: "critical"` / `"background"` tool args reach
/// the admission record — a governed blocking dispatch lands exactly one
/// Completed resource record carrying the requested priority.
#[tokio::test]
async fn gov_tool_surface_priority_reaches_admission_record() {
    // ≥3 final steps, 300ms each: two blocking children, each one turn.
    let steps = (0..4)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "done" })
        .collect();
    let (base_url, _starts, _ends, _arrivals, _max) =
        gov_spawn_scripted_server(steps, Vec::new()).await;

    let data_dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(gov_priority_manager(
        2,
        GovernanceConfig { ..Default::default() },
        data_dir.path().to_path_buf(),
    ));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));

    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let tool = DelegateTask::new(
        mgr.clone(),
        gov_agent_config(base_url),
        Config::defaults(),
        registry,
        Some(event_tx),
        None,
    );

    let ctx_dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(
        ctx_dir.path().to_path_buf(),
        Config::defaults(),
        "t029",
    );

    let crit = tool
        .execute(
            json!({"goal": "critical via tool", "priority": "critical"}),
            &ctx,
        )
        .await;
    match &crit {
        ToolResult::Text(t) => assert!(!t.is_empty(), "critical dispatch must return a summary"),
        ToolResult::Error(e) => panic!("critical dispatch must succeed, got: {e}"),
        other => panic!("critical dispatch must return Text, got: {other:?}"),
    }

    let bg = tool
        .execute(
            json!({"goal": "bg via arg", "priority": "background"}),
            &ctx,
        )
        .await;
    match &bg {
        ToolResult::Text(t) => assert!(!t.is_empty(), "background-arg dispatch must return a summary"),
        ToolResult::Error(e) => panic!("background-arg dispatch must succeed, got: {e}"),
        other => panic!("background-arg dispatch must return Text, got: {other:?}"),
    }

    let records = ResourceRecordStore::open(Some(data_dir.path())).load();
    let completed: Vec<_> = records
        .iter()
        .filter(|r| r.outcome == ResourceRecordOutcome::Completed)
        .collect();
    assert_eq!(
        completed.len(),
        2,
        "exactly two Completed records (one per blocking dispatch), got {} records: {:?}",
        records.len(),
        records.iter().map(|r| (r.outcome, r.priority)).collect::<Vec<_>>()
    );
    assert_eq!(
        completed
            .iter()
            .filter(|r| r.priority == Priority::Critical)
            .count(),
        1,
        "exactly one Completed record with priority==Critical"
    );
    assert_eq!(
        completed
            .iter()
            .filter(|r| r.priority == Priority::Background)
            .count(),
        1,
        "exactly one Completed record with priority==Background"
    );
}

/// T029 contract 3: the background FLAG keeps precedence over the `priority`
/// arg (flag > arg). The dispatch returns a background handle line, the
/// completion notice arrives — and background children write NO resource
/// records (known FR-011 gap, pinned for follow-up).
#[tokio::test]
async fn gov_tool_surface_background_flag_dispatches_without_records() {
    let steps = (0..4)
        .map(|_| ScriptedFinal { delay_ms: 0, text: "done" })
        .collect();
    let (base_url, _starts, _ends, _arrivals, _max) =
        gov_spawn_scripted_server(steps, Vec::new()).await;

    let data_dir2 = tempfile::tempdir().unwrap();
    let mgr = Arc::new(gov_priority_manager(
        2,
        GovernanceConfig { ..Default::default() },
        data_dir2.path().to_path_buf(),
    ));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let tool = DelegateTask::new(
        mgr.clone(),
        gov_agent_config(base_url),
        Config::defaults(),
        registry,
        Some(event_tx),
        None,
    );

    let ctx_dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(
        ctx_dir.path().to_path_buf(),
        Config::defaults(),
        "t029",
    );

    let record_count_before = ResourceRecordStore::open(Some(data_dir2.path())).load().len();

    let res = tool
        .execute(
            json!({"goal": "bg flag wins", "background": true, "priority": "critical"}),
            &ctx,
        )
        .await;
    match &res {
        ToolResult::Text(t) => assert!(
            t.contains("[BACKGROUND]") && t.contains("bg flag wins"),
            "background dispatch must return a non-error handle/notice line, got: {t:?}"
        ),
        ToolResult::Error(e) => panic!("background dispatch must start, got: {e}"),
        other => panic!("background dispatch must return Text, got: {other:?}"),
    }

    // Bounded-wait for the background completion notice: any
    // SubagentComplete for this goal (strict match on goal; the event
    // carries it verbatim from the dispatch result).
    let mut saw_complete = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, event_rx.recv()).await {
        if let AgentEvent::SubagentComplete { goal, .. } = &ev {
            if goal.contains("bg flag wins") {
                saw_complete = true;
                break;
            }
        }
    }
    assert!(
        saw_complete,
        "a SubagentComplete for 'bg flag wins' must arrive within 30s"
    );

    // Grace: let any straggling record append land before counting.
    tokio::time::sleep(Duration::from_secs(2)).await;
    // Background children dispatch via shared_child_manager (governance off,
    // gov_records: None) — they emit no resource records today (FR-011 gap,
    // surfaced for follow-up converge; see manager.rs shared_child_manager).
    assert_eq!(
        ResourceRecordStore::open(Some(data_dir2.path())).load().len(),
        record_count_before,
        "background children must not write resource records (known FR-011 gap)"
    );
}
