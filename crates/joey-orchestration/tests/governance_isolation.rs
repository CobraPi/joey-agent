//! Integration test: governance isolation — parent responsiveness under
//! saturation + runaway-child CPU-ceiling abort (feature 030, US3, T014
//! TDD RED).
//!
//! RED contract pinned here (see specs/030-please-implement-features/):
//!   (a) SC-004 guard — with every child slot occupied by slow children,
//!       the parent's scheduling-inspection surface (`overview()`) stays
//!       responsive: ≥ 95% of samples complete within 2× the idle-manager
//!       baseline median (this may already pass — it guards the property);
//!   (b) a runaway child is aborted at `cpu_ceiling_secs` with the EXACT
//!       contract error `[resource-limit] task exceeded 2s CPU budget`
//!       (RED today: no CPU-ceiling abort exists — the child runs to
//!       natural completion);
//!   (c) a sibling dispatched alongside the runaway finishes unaffected
//!       (RED today: the runaway is never aborted at all).
//!
//! Harness conventions mirror tests/governance_admission.rs (scripted mock
//! OpenAI-compatible provider over a TcpListener on 127.0.0.1:0, delay
//! injection, AtomicUsize in-flight probes, agent_config() helper,
//! governance_manager helper with GovernanceConfig{enabled:true,
//! data_dir:Some(tempdir), ...}) extended with `tool_calls` scripting (the
//! tests/budgets.rs convention) so a child can be forced through multiple
//! slow tool steps.
//!
//! Every test fn starts with `gov_`. All tests use
//! `#[tokio::test(flavor = "multi_thread")]` — real threading matters here
//! (an OS spin thread burns CPU; tokio workers must keep scheduling).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::{DelegationRequest, ManagerConfig, SubagentManager};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Exact resource-limit contract text pinned by this test (US3): the CPU
/// ceiling abort error must carry this prefix-shaped message with the
/// configured ceiling inlined.
const RESOURCE_LIMIT_TEXT: &str = "[resource-limit] task exceeded 2s CPU budget";

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (governance_admission.rs harness
// style + budgets.rs `tool_calls` scripting): one scripted response per HTTP
// connection, shared script position counter, delay injection, in-flight
// AtomicUsize probes.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ScriptedStep {
    content: &'static str,
    /// Emit one tool call to this tool name (forces another child turn).
    tool_call: Option<&'static str>,
    delay_ms: u64,
}

/// A tool-call turn: the child executes `gov_probe` and iterates.
fn tool_step(delay_ms: u64) -> ScriptedStep {
    ScriptedStep { content: "", tool_call: Some("gov_probe"), delay_ms }
}

/// A final plain-text turn: the child finishes naturally if it gets here.
fn final_step(delay_ms: u64) -> ScriptedStep {
    ScriptedStep { content: "ALL DONE", tool_call: None, delay_ms }
}

fn openai_body(step: &ScriptedStep, call_index: usize) -> String {
    let mut message = json!({"role": "assistant", "content": step.content});
    let finish_reason = if let Some(name) = step.tool_call {
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

/// Serve exactly one scripted request on this connection, then close.
/// Script position is the SHARED counter (connection N serves
/// script[N], clamped to the last entry once exhausted) — the
/// tests/budgets.rs convention: each provider call opens a fresh
/// connection (Connection: close).
async fn serve_conn(
    mut stream: TcpStream,
    script: Arc<Vec<ScriptedStep>>,
    counter: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
) {
    let Some(_body) = read_http_body(&mut stream).await else {
        return;
    };
    let idx = counter.fetch_add(1, Ordering::SeqCst);
    let step = script[idx.min(script.len() - 1)].clone();
    let now_in_flight = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    max_in_flight.fetch_max(now_in_flight, Ordering::SeqCst);
    if step.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(step.delay_ms)).await;
    }
    let body = openai_body(&step, idx);
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
    in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// Bind a scripted mock provider; returns (base_url, in-flight probe,
/// max observed in-flight requests).
async fn spawn_scripted_server(
    steps: Vec<ScriptedStep>,
) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    assert!(!steps.is_empty(), "scripted mock needs at least one response");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let script = Arc::new(steps);
    let counter = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let ret_in_flight = in_flight.clone();
    let ret_max = max_in_flight.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let script = script.clone();
            let counter = counter.clone();
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            tokio::spawn(async move {
                serve_conn(stream, script, counter, in_flight, max_in_flight).await;
            });
        }
    });
    (format!("http://{addr}"), ret_in_flight, ret_max)
}

// ---------------------------------------------------------------------------
// Probe tool (tests/budgets.rs convention): the tool the scripted mock
// forces the child to call. Instant — the slowness lives in the mock's
// injected delays.
// ---------------------------------------------------------------------------

struct ProbeTool {
    execs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        "gov_probe"
    }

    fn toolset(&self) -> &str {
        "coding"
    }

    fn emoji(&self) -> &str {
        "🔧"
    }

    fn description(&self) -> &str {
        "Governance isolation probe (echoes ok)."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        self.execs.fetch_add(1, Ordering::SeqCst);
        ToolResult::Text("probe-ok".to_string())
    }
}

/// Base registry with the probe registered (children resolve `toolsets: []`
/// to "everything registered" — the mock's `gov_probe` calls resolve).
fn probe_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ProbeTool { execs: Arc::new(AtomicUsize::new(0)) }));
    registry
}

// ---------------------------------------------------------------------------
// Governance-isolation test helpers.
// ---------------------------------------------------------------------------

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

/// Manager with 2 child slots, 8 request permits, governance ON with the
/// given config (always carries an isolated data_dir — tempdir).
fn governance_manager(gov: GovernanceConfig) -> SubagentManager {
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: gov,
        ..Default::default()
    })
}

/// Admission-shaped governance (test 1): defaults besides enabled + dir.
fn admission_governance(data_dir: std::path::PathBuf) -> GovernanceConfig {
    GovernanceConfig {
        enabled: true,
        max_queue_depth: 4,
        data_dir: Some(data_dir),
        ..Default::default()
    }
}

/// CPU-ceiling-shaped governance (tests 2-3): ceiling 2s sampled every 1s,
/// wall-clock timeout a generous 30s so the CPU ceiling must trigger first.
fn cpu_ceiling_governance(data_dir: std::path::PathBuf) -> GovernanceConfig {
    GovernanceConfig {
        enabled: true,
        data_dir: Some(data_dir),
        cpu_ceiling_secs: 2,
        watchdog_interval_secs: 1,
        task_timeout_secs: 30,
        ..Default::default()
    }
}

/// Start ONE OS thread spinning pure computation (`spin_loop` under an
/// AtomicBool stop flag) to give the process real CPU burn for the CPU
/// ceiling to sample. Returns (join handle, stop flag).
fn spawn_spin_thread() -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            std::hint::spin_loop();
        }
    });
    (handle, stop)
}

/// Stop a spin thread started by `spawn_spin_thread` (flag + join).
fn stop_spin_thread(handle: std::thread::JoinHandle<()>, stop: &Arc<AtomicBool>) {
    stop.store(true, Ordering::SeqCst);
    handle.join().expect("spin thread must join cleanly");
}

/// Median of an even/odd-length sample vector (sorts in place).
fn median_of(mut v: Vec<Duration>) -> Duration {
    assert!(!v.is_empty());
    v.sort();
    v[v.len() / 2]
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// SC-004 (may already pass — it guards the property): with 2 slow children
/// (5s mock delay each) occupying both slots, the parent-side scheduling
/// inspection (`SubagentManager::overview()`) must stay responsive — at
/// least 95% of 30 samples within 2× the idle-manager baseline median.
///
/// Measurement design notes (kept honest, per the pinned assertion):
///   - Baseline is taken on an IDLE manager that has already run 2
///     fast-completing children, so its `overview()` builds the SAME
///     number of records (2) as under saturation — otherwise the
///     comparison would measure record-count work, not saturation.
///   - Each sample measures a fixed batch of 1000 calls: a single call is
///     sub-microsecond, so per-call samples are dominated by timer noise,
///     and short batches are dominated by fixed-duration scheduler spikes
///     (~30µs preemption ≈ 66% of a 100-call batch but only ~7% of a
///     1000-call batch); batching raises the signal well above both.
#[tokio::test(flavor = "multi_thread")]
async fn gov_parent_scheduling_responsive_under_saturation() {
    // Warm-up wave first: 2 fast-final children (~50ms) complete and land
    // in history, so the idle baseline builds the same 2 overview records
    // the saturated phase will (apples-to-apples).
    let steps = vec![final_step(50), final_step(50), final_step(5000), final_step(5000)];
    let (base_url, _in_flight, _max_in_flight) = spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(admission_governance(dir.path().to_path_buf())));

    for i in 0..2 {
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        let req = DelegationRequest::single(format!("gov-warmup-{i}"));
        let r = mgr.dispatch_single(&req, &cfg, &tree, &base, None).await;
        assert!(r.success, "warm-up child {i} must finish, error: {:?}", r.error);
    }

    // (a) Baseline: idle manager (2 terminal history records), 30 samples
    // (each a 1000-call batch) of overview() latency.
    let mut baseline: Vec<Duration> = Vec::with_capacity(30);
    for _ in 0..30 {
        let t0 = Instant::now();
        for _ in 0..1000 {
            let n = mgr.overview().len();
            assert_eq!(n, 2, "idle manager must show its 2 terminal records");
        }
        baseline.push(t0.elapsed());
    }
    let baseline_median = median_of(baseline);

    // (b) Saturate: dispatch 2 children that each take ~5s (both slots).
    let mut handles = Vec::with_capacity(2);
    for i in 0..2 {
        let mgr = mgr.clone();
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(format!("gov-saturation-{i}"));
            mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
        }));
    }
    // Wait until both slow children are actually Running (bounded).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let running = mgr
            .overview()
            .iter()
            .filter(|r| matches!(r.state, joey_orchestration::types::DelegationState::Running))
            .count();
        if running == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the two slow children never saturated both slots"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 30 samples (each a 1000-call batch) of the same call under saturation
    // (2 running + 2 terminal records = 4 total per snapshot).
    let mut saturated: Vec<Duration> = Vec::with_capacity(30);
    for _ in 0..30 {
        let t0 = Instant::now();
        for _ in 0..1000 {
            let n = mgr.overview().len();
            assert!(
                n >= 4,
                "saturated manager must show its 2 running + 2 terminal records"
            );
        }
        saturated.push(t0.elapsed());
    }
    let threshold = baseline_median * 2;
    let within = saturated.iter().filter(|d| **d <= threshold).count();
    assert!(
        within >= 29, // 29/30 = 96.7% ≥ 95%
        "parent scheduling decisions must stay within 2x baseline under saturation: \
         only {within}/30 samples ≤ {threshold:?} (baseline median {baseline_median:?}, \
         saturated max {:?})",
        saturated.last().copied().unwrap_or_default()
    );

    // Wind down: both saturating children succeed naturally.
    for (i, h) in handles.into_iter().enumerate() {
        let r = h.await.expect("saturating dispatch panicked");
        assert!(r.success, "saturating child {i} must finish naturally, error: {:?}", r.error);
    }
}

/// THE RED TEST (US3, part 1): a runaway child (slow-but-finite 3×3s script
/// while an OS thread spins pure computation) must be ABORTED at the 2s CPU
/// ceiling with the exact contract error `[resource-limit] task exceeded
/// 2s CPU budget`. RED today: no CPU-ceiling abort exists — the child runs
/// to natural completion (~9s) and reports success.
#[tokio::test(flavor = "multi_thread")]
async fn gov_runaway_child_aborted_at_cpu_ceiling() {
    // Slow-but-finite script: 3 tool steps x 3s ≈ 9s total — long enough
    // for the 2s ceiling to hit, short enough to bound the test.
    let steps = vec![tool_step(3000), tool_step(3000), tool_step(3000), final_step(0)];
    let (base_url, _in_flight, _max_in_flight) = spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(cpu_ceiling_governance(dir.path().to_path_buf())));

    // Real CPU burn: one OS thread spinning pure computation.
    let (spin_handle, stop_flag) = spawn_spin_thread();

    let cfg = agent_config(base_url);
    let tree = Config::defaults();
    let base = probe_registry();
    let req = DelegationRequest::single("gov-runaway-cpu-burn");
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
    })
    .await
    .expect("dispatch must return within the 30s outer bound");

    stop_spin_thread(spin_handle, &stop_flag);

    assert!(
        !result.success,
        "RED: the runaway child must be aborted at the 2s CPU ceiling, but it ran to \
         natural completion (success=true, summary: {:?}, wall_clock {:?})",
        result.summary,
        result.wall_clock
    );
    let err = result.error.as_deref().unwrap_or_else(|| {
        panic!("aborted child must carry an error containing {RESOURCE_LIMIT_TEXT:?}, got none")
    });
    assert!(
        err.contains(RESOURCE_LIMIT_TEXT),
        "abort error must contain the exact contract text {RESOURCE_LIMIT_TEXT:?}, got: {err:?}"
    );
}

/// THE RED TEST (US3, part 2): a sibling dispatched concurrently with the
/// runaway must finish unaffected. B (~300ms fast final) succeeds with a
/// real summary; A (runaway-prone, ceiling applies to it) is aborted with
/// the exact `[resource-limit]` error. RED today: A is never aborted — it
/// completes naturally (~9s) with success=true.
#[tokio::test(flavor = "multi_thread")]
async fn gov_siblings_unaffected_by_runaway_abort() {
    // Separate deterministic servers: A's script is runaway-prone, B's is
    // a fast final (~300ms).
    let (url_a, _in_a, _max_a) =
        spawn_scripted_server(vec![tool_step(3000), tool_step(3000), tool_step(3000), final_step(0)])
            .await;
    let (url_b, _in_b, _max_b) = spawn_scripted_server(vec![final_step(300)]).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(cpu_ceiling_governance(dir.path().to_path_buf())));

    let (spin_handle, stop_flag) = spawn_spin_thread();

    // Dispatch A (runaway-prone) and B (fast sibling) concurrently.
    let mgr_a = mgr.clone();
    let cfg_a = agent_config(url_a);
    let tree_a = Config::defaults();
    let base_a = probe_registry();
    let handle_a = tokio::spawn(async move {
        let req = DelegationRequest::single("gov-runaway-sibling-A");
        mgr_a.dispatch_single(&req, &cfg_a, &tree_a, &base_a, None).await
    });

    let mgr_b = mgr.clone();
    let cfg_b = agent_config(url_b);
    let tree_b = Config::defaults();
    let base_b = probe_registry();
    let handle_b = tokio::spawn(async move {
        let req = DelegationRequest::single("gov-fast-sibling-B");
        mgr_b.dispatch_single(&req, &cfg_b, &tree_b, &base_b, None).await
    });

    let (result_a, result_b) = tokio::time::timeout(Duration::from_secs(30), async {
        let a = handle_a.await.expect("runaway dispatch panicked");
        let b = handle_b.await.expect("sibling dispatch panicked");
        (a, b)
    })
    .await
    .expect("both dispatches must return within the 30s outer bound");

    stop_spin_thread(spin_handle, &stop_flag);

    // Sibling B: unaffected — real success with a real summary.
    assert!(
        result_b.success,
        "sibling B must finish unaffected by A's abort, error: {:?}",
        result_b.error
    );
    assert!(
        result_b.summary.contains("ALL DONE"),
        "sibling B must carry a real summary, got: {:?}",
        result_b.summary
    );

    // Runaway A: aborted at the CPU ceiling with the exact contract error.
    assert!(
        !result_a.success,
        "RED: runaway-prone child A must be aborted at the 2s CPU ceiling, but it ran \
         to natural completion (success=true, summary: {:?}, wall_clock {:?})",
        result_a.summary,
        result_a.wall_clock
    );
    let err_a = result_a.error.as_deref().unwrap_or_else(|| {
        panic!("aborted child A must carry an error containing {RESOURCE_LIMIT_TEXT:?}, got none")
    });
    assert!(
        err_a.contains(RESOURCE_LIMIT_TEXT),
        "child A abort error must contain the exact contract text {RESOURCE_LIMIT_TEXT:?}, got: {err_a:?}"
    );
}
