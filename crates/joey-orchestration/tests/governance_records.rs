//! Integration test: governance resource records — one JSONL record per
//! terminal task outcome (feature 030, US5 / FR-011..FR-013, T019 TDD RED).
//!
//! RED contract pinned here (see specs/030-please-implement-features/
//! contracts/resource-record.md): with governance enabled, EVERY terminal
//! task outcome (completed, failed, timeout, aborted_by_resource_limit,
//! busy_refused, cache_hit) appends exactly one record carrying the full
//! contract field set; records join with token telemetry via
//! task_signature + token_usage; synthetic pathology fixtures
//! (stuck-task burn vs aggregate load; retry amplification; control-plane
//! starvation via elevated parent_starved_ms) are distinguishable by
//! querying records alone.
//!
//! Harness conventions mirror tests/governance_admission.rs (which itself
//! mirrors tests/concurrency_limiter.rs): scripted mock OpenAI provider
//! over a TcpListener on 127.0.0.1:0, one scripted response per HTTP
//! connection with delay injection (extended here with an HTTP status —
//! the always-500 fatal-failure mock), openai_body()/read_http_body(),
//! agent_config(), governance_manager(), and a concurrent dispatch_wave.
//!
//! Every test fn starts with `gov_` so intermediate regression runs can
//! `--skip gov_` until T016/T018/T020 wire record emission in.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::resource_records::{new_record, ResourceRecordStore};
use joey_orchestration::types::{Priority, ResourceRecord, ResourceRecordOutcome};
use joey_orchestration::{DelegationRequest, DelegationResult, ManagerConfig, SubagentManager};
use joey_tools::ToolRegistry;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use ResourceRecordOutcome::{
    AbortedByResourceLimit, BusyRefused, CacheHit, Completed, Failed, Timeout,
};

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (governance_admission.rs harness
// style, plus an HTTP status per step for the always-failing mock).
// ---------------------------------------------------------------------------

/// One scripted response per HTTP connection: `status` with a plain
/// assistant text response after `delay_ms`; status >= 500 serves an error
/// body (the always-failing provider).
#[derive(Clone)]
struct ScriptedFinal {
    delay_ms: u64,
    text: &'static str,
    status: u16,
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

/// Serve exactly one scripted request per connection, then close. A
/// script exhausted also serves 500 (so an all-fail script fails ALWAYS).
async fn serve_conn(mut stream: TcpStream, queue: Arc<Mutex<VecDeque<ScriptedFinal>>>) {
    let Some(_body) = read_http_body(&mut stream).await else {
        return;
    };
    let step = queue.lock().unwrap().pop_front();
    let (status_line, body_out) = match step {
        Some(step) => {
            if step.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(step.delay_ms)).await;
            }
            if step.status >= 500 {
                (
                    "HTTP/1.1 500 Internal Server Error",
                    json!({"error": {"message": "mock provider forced failure"}}).to_string(),
                )
            } else {
                ("HTTP/1.1 200 OK", openai_body(step.text))
            }
        }
        None => (
            "HTTP/1.1 500 Internal Server Error",
            json!({"error": {"message": "script exhausted"}}).to_string(),
        ),
    };
    let resp = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_out.len(),
        body_out
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Bind the scripted mock provider on 127.0.0.1:0; returns the base_url.
async fn spawn_server(steps: Vec<ScriptedFinal>) -> String {
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
// Governance-records test helpers.
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

/// Manager with the governance shape under test: 2 child slots, 8 request
/// permits, governance ON with mechanism overrides per test, isolated
/// data dir (the resource-records JSONL lands under
/// `<data_dir>/delegation/resource-records.jsonl`).
fn governance_manager(data_dir: std::path::PathBuf, mut gov: GovernanceConfig) -> SubagentManager {
    gov.enabled = true;
    gov.data_dir = Some(data_dir);
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: gov,
        ..Default::default()
    })
}

/// One dispatch against the manager (no event tap needed — all assertions
/// are on the records file).
async fn dispatch_one(
    mgr: &SubagentManager,
    base_url: &str,
    goal: &str,
) -> DelegationResult {
    let cfg = agent_config(base_url.to_string());
    let tree = Config::defaults();
    let base = ToolRegistry::new();
    let req = DelegationRequest::single(goal);
    mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
}

/// Fire `n` dispatch_single calls CONCURRENTLY (tokio::spawn each, join
/// all) against the shared manager — the shape of many delegate_task tool
/// calls arriving in one assistant message.
async fn dispatch_wave(
    mgr: Arc<SubagentManager>,
    base_url: String,
    prefix: &str,
    n: usize,
) -> Vec<DelegationResult> {
    let mut handles = Vec::with_capacity(n);
    for i in 0..n {
        let mgr = mgr.clone();
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        let goal = format!("{prefix}-{i}");
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(goal);
            mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
        }));
    }
    let mut results = Vec::with_capacity(n);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    results
}

/// Fixture token usage for the synthetic-record test.
fn usage() -> joey_providers::Usage {
    joey_providers::Usage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// THE FILE-LEVEL RED TEST (FR-011): with governance on, each terminal
/// kind appends exactly one record. Kinds produced here:
///   (a) completed — normal fast dispatch;
///   (b) failed — mock returns HTTP 500 always (fatal failure);
///   (c) timeout — task_timeout_secs=1 with a slow mock;
///   (d) busy_refused — 2 slots + queue cap 1, 5 concurrent slow
///       dispatches → at least one busy refusal;
///   (e) cache_hit — the SAME request dispatched twice (the second must
///       hit the cache once T018 lands); today both execute → no cache_hit
///       record → RED counts.
/// After all complete: exactly one record per dispatch, the outcome set
/// includes Completed/Failed/Timeout/BusyRefused, a cache_hit record
/// exists, every record carries non-empty record_id/task_signature/
/// created_at with queue_wait_ms/compute_ms present and priority Normal,
/// and completed records' token_usage.total_tokens join the successful
/// results' token_usage.total_tokens.
#[tokio::test]
async fn gov_every_terminal_outcome_recorded() {
    // One shared data dir: every scenario's records accumulate in ONE
    // JSONL file, and the total record count must equal total dispatches.
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let mut results: Vec<DelegationResult> = Vec::new();

    // (a) completed: normal fast dispatch.
    let ok_url = spawn_server(vec![ScriptedFinal {
        delay_ms: 50,
        text: "ALL DONE",
        status: 200,
    }])
    .await;
    let mgr_ok = Arc::new(governance_manager(
        data_dir.clone(),
        GovernanceConfig::default(),
    ));
    results.push(dispatch_one(&mgr_ok, &ok_url, "gov-rec-completed").await);

    // (b) failed: HTTP 500 always (fatal failure).
    let fail_url = spawn_server(
        (0..8)
            .map(|_| ScriptedFinal {
                delay_ms: 50,
                text: "",
                status: 500,
            })
            .collect(),
    )
    .await;
    let mgr_fail = Arc::new(governance_manager(
        data_dir.clone(),
        GovernanceConfig::default(),
    ));
    results.push(dispatch_one(&mgr_fail, &fail_url, "gov-rec-failed").await);

    // (c) timeout: 1s wall-clock budget vs 1500ms-slow mock steps.
    let slow_url = spawn_server(
        (0..4)
            .map(|_| ScriptedFinal {
                delay_ms: 1500,
                text: "ALL DONE",
                status: 200,
            })
            .collect(),
    )
    .await;
    let mgr_timeout = Arc::new(governance_manager(
        data_dir.clone(),
        GovernanceConfig {
            task_timeout_secs: 1,
            ..Default::default()
        },
    ));
    results.push(dispatch_one(&mgr_timeout, &slow_url, "gov-rec-timeout").await);

    // (d) busy_refused: 2 slots + queue cap 1, 5 concurrent slow
    // dispatches → at least one busy refusal (2 running + 1 queued).
    let busy_url = spawn_server(
        (0..6)
            .map(|_| ScriptedFinal {
                delay_ms: 400,
                text: "ALL DONE",
                status: 200,
            })
            .collect(),
    )
    .await;
    let mgr_busy = Arc::new(governance_manager(
        data_dir.clone(),
        GovernanceConfig {
            max_queue_depth: 1,
            ..Default::default()
        },
    ));
    results.extend(dispatch_wave(mgr_busy, busy_url, "gov-rec-busy", 5).await);

    // (e) cache_hit: the SAME request twice (second must hit the cache
    // once T018 lands); today both execute → no cache_hit record.
    let cache_url = spawn_server(
        (0..2)
            .map(|_| ScriptedFinal {
                delay_ms: 50,
                text: "ALL DONE",
                status: 200,
            })
            .collect(),
    )
    .await;
    let mgr_cache = Arc::new(governance_manager(
        data_dir.clone(),
        GovernanceConfig {
            result_cache_enabled: true,
            ..Default::default()
        },
    ));
    results.push(dispatch_one(&mgr_cache, &cache_url, "gov-rec-cache").await);
    results.push(dispatch_one(&mgr_cache, &cache_url, "gov-rec-cache").await);

    let total_dispatches = results.len();
    assert_eq!(
        total_dispatches, 10,
        "harness sanity: 1+1+1+5+2 dispatches were made"
    );

    let store = ResourceRecordStore::open(Some(&data_dir));
    let records = store.load();

    // EXACTLY ONE record per dispatch.
    assert_eq!(
        records.len(),
        total_dispatches,
        "exactly one resource record per dispatch ({total_dispatches} made); got {} — today (RED) the dispatch path writes no records",
        records.len()
    );

    // The outcome set includes Completed, Failed, Timeout, BusyRefused.
    for want in [Completed, Failed, Timeout, BusyRefused] {
        assert!(
            records.iter().any(|r| r.outcome == want),
            "records must include at least one {want:?} outcome; got {:?}",
            records.iter().map(|r| r.outcome).collect::<Vec<_>>()
        );
    }

    // (e) a cache_hit record exists (RED today: both dispatches execute).
    assert!(
        records.iter().any(|r| r.outcome == CacheHit),
        "the second identical dispatch must append a cache_hit record once T018 lands; outcomes: {:?}",
        records.iter().map(|r| r.outcome).collect::<Vec<_>>()
    );

    // Field sanity: non-empty identity fields, timing fields present
    // (non-optional), default-priority lane.
    for r in &records {
        assert!(!r.record_id.is_empty(), "record_id must be non-empty");
        assert!(
            !r.task_signature.is_empty(),
            "task_signature must be non-empty"
        );
        assert!(!r.created_at.is_empty(), "created_at must be non-empty");
        let _ = (r.queue_wait_ms, r.compute_ms); // present (structural u64s)
        assert_eq!(
            r.priority,
            Priority::Normal,
            "dispatches without an explicit lane must record priority normal"
        );
    }

    // Joinability: every Completed record's token_usage.total_tokens must
    // be drawn from the successful results' token_usage.total_tokens.
    let mut result_tokens: Vec<u64> = results
        .iter()
        .filter(|r| r.success)
        .map(|r| r.token_usage.total_tokens)
        .collect();
    result_tokens.sort_unstable();
    let completed: Vec<&ResourceRecord> =
        records.iter().filter(|r| r.outcome == Completed).collect();
    assert!(
        !completed.is_empty(),
        "at least one completed record must exist"
    );
    let mut ri = 0usize;
    for rec in &completed {
        let ct = rec.token_usage.total_tokens;
        while ri < result_tokens.len() && result_tokens[ri] < ct {
            ri += 1;
        }
        assert!(
            ri < result_tokens.len() && result_tokens[ri] == ct,
            "completed record (sig {}) total_tokens={ct} must join a successful result's token_usage.total_tokens; result tokens: {result_tokens:?}",
            rec.task_signature
        );
        ri += 1;
    }
}

/// FR-012 RED TEST: a runaway task (slow child + a spinning thread burning
/// process CPU, cpu_ceiling_secs=2 — the governance_isolation runaway
/// scenario) is aborted by the watchdog, and the records contain exactly
/// one aborted_by_resource_limit record for that goal (the data dir is
/// isolated to this single dispatch) with cpu_ms >= 1500 (sampled CPU
/// attribution near the 2s ceiling; generous tolerance) and a sampled
/// advisory memory_peak_kb > 0.
#[tokio::test]
async fn gov_aborted_by_resource_limit_recorded() {
    // Slow child: provider responses far past the 2s CPU ceiling, so the
    // child is still running when the watchdog fires.
    let url = spawn_server(
        (0..4)
            .map(|_| ScriptedFinal {
                delay_ms: 4000,
                text: "slow",
                status: 200,
            })
            .collect(),
    )
    .await;

    // Spin thread: burn one core so process-level sampled CPU attribution
    // (apportioned to the running child) climbs toward the ceiling.
    let stop = Arc::new(AtomicBool::new(false));
    let spinner_stop = stop.clone();
    let spinner = std::thread::spawn(move || {
        while !spinner_stop.load(Ordering::Relaxed) {
            std::hint::spin_loop();
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            cpu_ceiling_secs: 2,
            watchdog_interval_secs: 1,
            task_timeout_secs: 30, // the CPU ceiling, not the wall clock, must win
            memory_tracking_enabled: true,
            ..Default::default()
        },
    ));

    let _result = dispatch_one(&mgr, &url, "gov-rec-runaway").await;

    stop.store(true, Ordering::Relaxed);
    spinner.join().unwrap();

    let store = ResourceRecordStore::open(Some(dir.path()));
    let records = store.load();
    let aborted: Vec<&ResourceRecord> = records
        .iter()
        .filter(|r| r.outcome == AbortedByResourceLimit)
        .collect();
    assert_eq!(
        aborted.len(),
        1,
        "exactly one aborted_by_resource_limit record for the runaway goal; got {} record(s) total, {} aborted — today (RED) no watchdog aborts and no records are written",
        records.len(),
        aborted.len()
    );
    assert!(
        aborted[0].cpu_ms >= 1500,
        "sampled CPU attribution must sit near the 2s ceiling (generous tolerance): cpu_ms={}",
        aborted[0].cpu_ms
    );
    assert!(
        aborted[0].memory_peak_kb > 0,
        "advisory memory peak must be sampled: memory_peak_kb={}",
        aborted[0].memory_peak_kb
    );
}

/// FR-013: the record vocabulary alone distinguishes the pathologies.
/// Four SYNTHETIC fixture sets written via the store API (no delegation):
///   A stuck-task burn — 2 records cpu_ms 25000/22000 + 8 records cpu_ms 800;
///   B aggregate load — 10 records all cpu_ms ~1500;
///   C retry amplification — 4 records retries 0,1,2,3, same signature;
///   D control-plane starvation — 3 records parent_starved_ms 4000+.
/// Classifier predicates live IN THE TEST; all sets load from one
/// combined file in append order. (Mostly GREEN via the store API; the
/// FILE-level RED comes from gov_every_terminal_outcome_recorded.)
#[test]
fn gov_pathologies_distinguishable_from_records_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResourceRecordStore::open(Some(dir.path()));

    let rec = |sig: String, cpu_ms: u64, retries: u16, starved: u64| {
        new_record(
            sig,
            Priority::Normal,
            Completed,
            10,
            100,
            cpu_ms,
            512,
            starved,
            retries,
            None,
            usage(),
            false,
        )
    };

    // A: stuck-task burn — a couple of huge-cpu outliers over a low median.
    let mut a = Vec::new();
    for (i, cpu) in [25000u64, 22000].into_iter().enumerate() {
        a.push(rec(format!("stuck-burn-{i}"), cpu, 0, 0));
    }
    for i in 0..8 {
        a.push(rec(format!("stuck-burn-sibling-{i}"), 800, 0, 0));
    }

    // B: aggregate load — uniformly ~1500ms of CPU, no outliers.
    let b: Vec<ResourceRecord> = (0..10)
        .map(|i| rec(format!("aggregate-{i}"), 1450 + (i % 3) as u64 * 50, 0, 0))
        .collect();

    // C: retry amplification — escalating retries on ONE task signature.
    let c: Vec<ResourceRecord> = [0u16, 1, 2, 3]
        .into_iter()
        .enumerate()
        .map(|(i, retries)| rec(format!("retry-amp-{i}"), 300, retries, 0))
        .collect();

    // D: control-plane starvation — elevated parent_starved_ms.
    let d: Vec<ResourceRecord> = [4000u64, 4500, 5000]
        .into_iter()
        .enumerate()
        .map(|(i, starved)| rec(format!("starved-{i}"), 300, 0, starved))
        .collect();

    // Classifier predicates (the diagnostic queries over records alone).
    fn stuck_vs_aggregate(records: &[ResourceRecord]) -> bool {
        let mut cpus: Vec<u64> = records.iter().map(|r| r.cpu_ms).collect();
        if cpus.is_empty() {
            return false;
        }
        cpus.sort_unstable();
        let mid = cpus.len() / 2;
        let median = if cpus.len() % 2 == 1 {
            cpus[mid] as f64
        } else {
            (cpus[mid - 1] + cpus[mid]) as f64 / 2.0
        };
        let max = *cpus.last().unwrap() as f64;
        max > 5.0 * median
    }

    fn retry_amplified(records: &[ResourceRecord]) -> bool {
        records.iter().any(|r| r.retries >= 2)
            || records.iter().map(|r| r.retries).sum::<u16>() >= 4
    }

    fn starved(records: &[ResourceRecord]) -> bool {
        records.iter().any(|r| r.parent_starved_ms >= 2000)
    }

    // A is stuck, B is not (the burn-vs-load distinction).
    assert!(
        stuck_vs_aggregate(&a),
        "set A (stuck-task burn) must classify as stuck"
    );
    assert!(
        !stuck_vs_aggregate(&b),
        "set B (aggregate load) must NOT classify as stuck"
    );
    // C is retry-amplified.
    assert!(
        retry_amplified(&c),
        "set C (retries 0,1,2,3) must classify as retry-amplified"
    );
    // D is starved.
    assert!(starved(&d), "set D (parent_starved_ms 4000+) must classify as starved");

    // All four sets load from ONE combined file in append order.
    let mut expected = Vec::new();
    for set in [&a, &b, &c, &d] {
        for r in set.iter() {
            store.append(r).unwrap();
            expected.push(r.clone());
        }
    }
    let loaded = store.load();
    assert_eq!(
        loaded.len(),
        a.len() + b.len() + c.len() + d.len(),
        "combined file must hold every fixture record"
    );
    assert_eq!(
        loaded, expected,
        "records must load in exact append order"
    );
}
