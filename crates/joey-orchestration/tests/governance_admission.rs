//! Integration test: governance admission — bounded pool + busy refusal
//! (feature 030, US1 / FR-002, T007 TDD RED).
//!
//! RED contract pinned here (see specs/030-please-implement-features/
//! contracts/busy-and-outcomes.md): with governance enabled and
//! max_queue_depth = 4, 10 concurrent dispatch_single calls against a
//! 2-child-slot manager must produce exactly 4 busy refusals carrying the
//! exact text `[busy] delegation queue full (4 waiting, cap 4) — re-plan
//! or defer`, while the 2 running + 4 queued dispatches succeed.
//!
//! Harness conventions mirror tests/concurrency_limiter.rs verbatim:
//! ScriptedFinal mock provider (one scripted response per HTTP connection,
//! delay injection), in-flight AtomicUsize probes, TcpListener on
//! 127.0.0.1:0, agent_config() helper, SubagentManager construction.
//!
//! Every test fn starts with `gov_` so intermediate regression runs can
//! `--skip gov_` until T008 implements bounded admission.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::{
    DelegationRequest, DelegationResult, ManagerConfig, SubagentManager,
};
use joey_tools::ToolRegistry;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Exact busy-refusal contract text (FR-002, busy-and-outcomes.md): with
/// queue cap 4 and a full queue, N == 4.
const BUSY_TEXT: &str = "[busy] delegation queue full (4 waiting, cap 4) — re-plan or defer";

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (same harness style as
// tests/concurrency_limiter.rs) with in-flight concurrency probes.
// ---------------------------------------------------------------------------

/// One scripted response per HTTP connection: 200 with a plain assistant
/// text response, after `delay_ms`.
#[derive(Clone)]
struct ScriptedFinal {
    delay_ms: u64,
    text: &'static str,
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
    queue: Arc<Mutex<VecDeque<ScriptedFinal>>>,
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

/// Serve exactly one scripted request per connection, then close.
async fn serve_conn(mut stream: TcpStream, p: ServerProbe) {
    let Some(_body) = read_http_body(&mut stream).await else {
        return;
    };
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
    let body_out = openai_body(step.text);
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

/// Bind a scripted mock provider; returns (base_url, start-time log,
/// end-time log, max observed in-flight requests).
async fn spawn_scripted_server(
    steps: Vec<ScriptedFinal>,
) -> (String, Arc<Mutex<Vec<u64>>>, Arc<Mutex<Vec<u64>>>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let starts_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let ends_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    let epoch = Instant::now();
    let ret_starts = starts_ms.clone();
    let ret_ends = ends_ms.clone();
    let ret_max = max_in_flight.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let probe = ServerProbe {
                queue: queue.clone(),
                starts_ms: starts_ms.clone(),
                ends_ms: ends_ms.clone(),
                in_flight: in_flight.clone(),
                max_in_flight: max_in_flight.clone(),
                epoch,
            };
            tokio::spawn(async move {
                serve_conn(stream, probe).await;
            });
        }
    });
    (format!("http://{addr}"), ret_starts, ret_ends, ret_max)
}

// ---------------------------------------------------------------------------
// Governance-admission test helpers.
// ---------------------------------------------------------------------------

/// Parent AgentConfig pointing at the scripted mock provider (mirrors
/// concurrency_limiter.rs's agent_config).
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

/// Manager with the governance-admission shape under test: 2 child slots,
/// 8 request permits, governance ON with queue cap 4, isolated data dir.
/// `GovernanceConfig::default()` is DISABLED — enabled must be explicit.
fn governance_manager(enabled: bool, data_dir: std::path::PathBuf) -> SubagentManager {
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: GovernanceConfig {
            enabled,
            max_queue_depth: 4,
            data_dir: Some(data_dir),
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Fire `n` dispatch_single calls CONCURRENTLY (tokio::spawn each, join
/// all) against the shared manager — the shape of many delegate_task tool
/// calls arriving in one assistant message.
async fn dispatch_wave(
    mgr: Arc<SubagentManager>,
    base_url: String,
    n: usize,
) -> Vec<DelegationResult> {
    let mut handles = Vec::with_capacity(n);
    for i in 0..n {
        let mgr = mgr.clone();
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(format!("gov-task-{i}"));
            mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
        }));
    }
    let mut results = Vec::with_capacity(n);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    results
}

/// Extract the `N` from a busy error's `N waiting, cap 4` segment
/// (regex-free). Returns None when the shape is absent.
fn busy_depth(err: &str) -> Option<usize> {
    let marker = " waiting, cap 4";
    let pos = err.find(marker)?;
    let before = &err[..pos];
    let mut digits: Vec<char> = Vec::new();
    for c in before.chars().rev() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return None;
    }
    digits.reverse();
    digits.into_iter().collect::<String>().parse::<usize>().ok()
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// THE RED TEST (FR-002/FR-004): 10 concurrent dispatches against a
/// 2-slot manager with queue cap 4 → exactly 4 busy refusals with the
/// exact contract text, 6 admitted successes, pool never exceeds 2.
#[tokio::test]
async fn gov_bounded_pool_and_busy_refusal_exact_text() {
    // Every provider request sleeps 300ms then returns a final text (no
    // tool calls): children hold their slots long enough for the wave to
    // saturate. 12 scripted steps ≥ the 10 expected connections.
    let steps = (0..12)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, max_in_flight) = spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(true, dir.path().to_path_buf()));

    let results = dispatch_wave(mgr, base_url, 10).await;
    assert_eq!(results.len(), 10, "one result per dispatched request");

    // (a) The child-slot pool still caps concurrent provider requests at 2.
    let observed_max = max_in_flight.load(Ordering::SeqCst);
    assert!(
        observed_max <= 2,
        "cap of 2 concurrently-running children must hold, observed {observed_max}"
    );

    // (b) Exactly 4 busy refusals carrying the EXACT contract text.
    let mut busy = 0usize;
    let mut ok = 0usize;
    for (i, r) in results.iter().enumerate() {
        if !r.success {
            assert_eq!(
                r.error.as_deref().unwrap().contains(BUSY_TEXT),
                true,
                "result {i}: non-success must be a busy refusal with the exact contract text, error: {:?}",
                r.error
            );
            busy += 1;
        } else {
            ok += 1;
        }
    }
    assert_eq!(
        busy, 4,
        "exactly 4 of 10 dispatches must be busy-refused (2 running + 4 queued of 10, cap 4)"
    );

    // (c) The other 6 (2 running + 4 queued) all succeed.
    assert_eq!(ok, 6, "the 6 admitted dispatches must succeed");
}

/// Complementary guard: every busy error that ever appears reports a
/// queue depth within the cap (matches `N waiting, cap 4`, N <= 4).
#[tokio::test]
async fn gov_queue_depth_never_exceeds_cap() {
    let steps = (0..12)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, _max_in_flight) = spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(true, dir.path().to_path_buf()));

    let results = dispatch_wave(mgr, base_url, 10).await;
    assert_eq!(results.len(), 10);

    for r in &results {
        if let Some(err) = r.error.as_deref() {
            if err.contains("queue full") {
                let depth = busy_depth(err).unwrap_or_else(|| {
                    panic!("busy error lacks the 'N waiting, cap 4' shape: {err}")
                });
                assert!(
                    depth <= 4,
                    "reported queue depth {depth} exceeds cap 4 in busy error: {err}"
                );
            }
        }
    }
}

/// Zero user config → safe capacity-derived defaults exist: queue depth
/// 2×children, governance on, 600s task timeout, 300s CPU ceiling.
/// (Unit-level guard; the FILE-level RED comes from the busy-refusal test.)
#[tokio::test]
async fn gov_defaults_capacity_derived_no_config() {
    let g = GovernanceConfig::from_config(&Config::defaults(), 2);
    assert_eq!(g.max_queue_depth, 4, "auto queue depth = 2 × children");
    assert!(g.enabled, "governance defaults ON with zero user config");
    assert_eq!(g.task_timeout_secs, 600);
    assert_eq!(g.cpu_ceiling_secs, 300);
}

/// FR-014/SC-007 regression: with governance disabled, none of the new
/// outcomes can occur — all 10 dispatches succeed (no refusals, no
/// queueing) and the pre-existing child-slot cap of 2 still holds.
#[tokio::test]
async fn gov_governance_off_zero_refusals() {
    let steps = (0..12)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, max_in_flight) = spawn_scripted_server(steps).await;

    // data_dir still set (isolated), but enabled = false.
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(false, dir.path().to_path_buf()));

    let results = dispatch_wave(mgr, base_url, 10).await;
    assert_eq!(results.len(), 10);
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "governance off ⇒ dispatch {i} must succeed (no refusals), error: {:?}",
            r.error
        );
    }

    // The pre-existing child-slot cap still applies.
    let observed_max = max_in_flight.load(Ordering::SeqCst);
    assert!(
        observed_max <= 2,
        "pre-existing cap of 2 must hold with governance off, observed {observed_max}"
    );
}

/// Busy refusal writes no side effects / the task is not started: every
/// refused result carries zero token usage and an empty summary.
/// (resource-records assertions land with T019/T020.)
#[tokio::test]
async fn gov_busy_refusal_writes_no_side_effects() {
    let steps = (0..12)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, _max_in_flight) = spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(true, dir.path().to_path_buf()));

    let results = dispatch_wave(mgr, base_url, 10).await;
    assert_eq!(results.len(), 10);

    for (i, r) in results.iter().enumerate() {
        if !r.success {
            assert!(
                r.error.as_deref().unwrap().contains("[busy]"),
                "result {i}: refused result must be a busy refusal, error: {:?}",
                r.error
            );
            assert_eq!(
                r.token_usage.total_tokens, 0,
                "refused dispatch {i} must consume zero tokens (task not started)"
            );
            assert!(
                r.summary.is_empty(),
                "refused dispatch {i} must carry an empty summary"
            );
        }
    }
}
