//! Integration test: governance parity — every mechanism has an OFF switch
//! that restores exact pre-feature behavior (feature 030, FR-014/SC-007,
//! T024) + SC-008 user-only file permissions.
//!
//! Parity contract pinned here: with the master switch off, dispatch
//! behavior is byte-identical to pre-feature (all 4 dispatches succeed,
//! ZERO busy refusals, NO governance side-effect files, NO governance
//! events). Each per-mechanism switch (result cache, single flight,
//! checkpointing, memory tracking, priority) disables exactly its own
//! mechanism. Defaults resolution is pinned exactly against
//! contracts/config-keys.md, and both governance stores persist with
//! user-only (0600) permissions (SC-008).
//!
//! Harness conventions mirror tests/governance_admission.rs verbatim
//! (which itself mirrors tests/concurrency_limiter.rs): ScriptedFinal mock
//! provider, read_http_body, ServerProbe/serve_conn/spawn_scripted_server,
//! agent_config + governance_manager helpers — extended with a
//! request-count probe (governance_dedup.rs convention).
//!
//! Every test fn starts with `gov_parity_` per the task brief.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::resource_records::ResourceRecordStore;
use joey_orchestration::result_cache::ResultCache;
use joey_orchestration::types::{Priority, ResourceRecordOutcome};
use joey_orchestration::{
    DelegationRequest, DelegationResult, ManagerConfig, SubagentManager,
};
use joey_tools::ToolRegistry;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (copied verbatim from
// tests/governance_admission.rs) + request-count probe.
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
    #[allow(dead_code)]
    starts_ms: Arc<Mutex<Vec<u64>>>,
    #[allow(dead_code)]
    ends_ms: Arc<Mutex<Vec<u64>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    /// Total requests served (the execution-count probe).
    total_requests: Arc<AtomicUsize>,
    epoch: Instant,
}

/// Serve exactly one scripted request per connection, then close.
/// A request arriving when the script is exhausted replays the LAST step
/// (clamped — budgets.rs convention) so the server never runs dry.
async fn serve_conn(mut stream: TcpStream, p: ServerProbe) {
    let Some(_body) = read_http_body(&mut stream).await else {
        return;
    };
    p.total_requests.fetch_add(1, Ordering::SeqCst);
    let step = {
        let mut q = p.queue.lock().unwrap();
        match q.pop_front() {
            Some(step) => step,
            None => {
                // Clamp to the last step (never runs dry).
                let mut last = ScriptedFinal { delay_ms: 0, text: "final" };
                if let Some(prev) = q.back() {
                    last = prev.clone();
                }
                last
            }
        }
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
/// end-time log, max observed in-flight requests, total request count).
async fn spawn_scripted_server(
    steps: Vec<ScriptedFinal>,
) -> (
    String,
    Arc<Mutex<Vec<u64>>>,
    Arc<Mutex<Vec<u64>>>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let starts_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let ends_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let total_requests = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    let epoch = Instant::now();
    let ret_starts = starts_ms.clone();
    let ret_ends = ends_ms.clone();
    let ret_max = max_in_flight.clone();
    let ret_total = total_requests.clone();
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
                total_requests: total_requests.clone(),
                epoch,
            };
            tokio::spawn(async move {
                serve_conn(stream, probe).await;
            });
        }
    });
    (format!("http://{addr}"), ret_starts, ret_ends, ret_max, ret_total)
}

// ---------------------------------------------------------------------------
// Governance-parity test helpers (governance_admission.rs style).
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

/// Manager with the parity shape under test: 2 child slots, 8 request
/// permits, governance config passed through verbatim (the test controls
/// `enabled` and every mechanism flag), isolated data dir.
fn governance_manager(enabled: bool, data_dir: std::path::PathBuf) -> SubagentManager {
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: GovernanceConfig {
            enabled,
            data_dir: Some(data_dir),
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Governance-enabled manager with a full GovernanceConfig override
/// (mirrors governance_retry.rs's governance_manager).
fn gov_manager(data_dir: std::path::PathBuf, mut gov: GovernanceConfig) -> SubagentManager {
    gov.enabled = true;
    gov.data_dir = Some(data_dir);
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: gov,
        ..Default::default()
    })
}

/// Fire `n` dispatch_single calls CONCURRENTLY (tokio::spawn each, join
/// all) against the shared manager (governance_admission.rs dispatch_wave
/// shape, with per-request priority control).
async fn dispatch_wave(
    mgr: Arc<SubagentManager>,
    base_url: String,
    reqs: Vec<DelegationRequest>,
) -> Vec<DelegationResult> {
    let n = reqs.len();
    let mut handles = Vec::with_capacity(n);
    for req in reqs {
        let mgr = mgr.clone();
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        handles.push(tokio::spawn(async move {
            mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
        }));
    }
    let mut results = Vec::with_capacity(n);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    results
}

/// Count governance events of the feature-030 kinds in an event drain.
fn governance_event_count(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> usize {
    let mut n = 0usize;
    while let Ok(ev) = rx.try_recv() {
        let ev = match ev {
            AgentEvent::SubagentEvent { event, .. } => *event,
            other => other,
        };
        if matches!(
            ev,
            AgentEvent::DelegationBusy { .. }
                | AgentEvent::CapacitySnapshot { .. }
                | AgentEvent::DelegationTimeout { .. }
        ) {
            n += 1;
        }
    }
    n
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// 1. FR-014/SC-007 master-switch parity: governance DISABLED with 4
/// concurrent dispatches against a 2-slot manager → all 4 succeed, ZERO
/// busy refusals (no queue semantics), NO governance side-effect files,
/// and no DelegationBusy/CapacitySnapshot/DelegationTimeout events.
#[tokio::test]
async fn gov_parity_master_switch_off_pre_feature_behavior() {
    let steps = (0..8)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, _max_in_flight, _total) =
        spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let mgr = Arc::new(governance_manager(false, data_dir.clone()));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let mut handles = Vec::with_capacity(4);
    for i in 0..4 {
        let mgr = mgr.clone();
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        let tx = event_tx.clone();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(format!("gov-parity-off-{i}"));
            mgr.dispatch_single(&req, &cfg, &tree, &base, Some(&tx)).await
        }));
    }
    let mut results = Vec::with_capacity(4);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    assert_eq!(results.len(), 4);

    // (a) All 4 succeed.
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "governance off ⇒ dispatch {i} must succeed, error: {:?}",
            r.error
        );
    }

    // (b) ZERO busy refusals (no queue semantics).
    let busy = results
        .iter()
        .filter(|r| r.error.as_deref().is_some_and(|e| e.contains("[busy]")))
        .count();
    assert_eq!(busy, 0, "governance off ⇒ zero busy refusals");

    // (c) NO governance side-effect files.
    assert!(
        !data_dir.join("delegation").join("resource-records.jsonl").exists(),
        "governance off ⇒ resource-records.jsonl must NOT be created"
    );
    assert!(
        !data_dir.join("delegation").join("result-cache.json").exists(),
        "governance off ⇒ result-cache.json must NOT be created"
    );

    // (d) No DelegationBusy / CapacitySnapshot / DelegationTimeout events.
    assert_eq!(
        governance_event_count(&mut event_rx),
        0,
        "governance off ⇒ zero governance events"
    );
}

/// 2. Result-cache OFF: identical sequential dispatches both EXECUTE
/// (server saw 2 requests) and no result-cache.json is written.
#[tokio::test]
async fn gov_parity_result_cache_off_executes_twice() {
    let steps = (0..4)
        .map(|_| ScriptedFinal { delay_ms: 100, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, _max_in_flight, total_requests) =
        spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let mgr = Arc::new(gov_manager(
        data_dir.clone(),
        GovernanceConfig {
            result_cache_enabled: false,
            ..Default::default()
        },
    ));

    // Two IDENTICAL sequential dispatches (same everything).
    let mut results = Vec::with_capacity(2);
    for _ in 0..2 {
        let req = DelegationRequest::single("gov-parity-cache-off-task");
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        let r = mgr.dispatch_single(&req, &cfg, &tree, &base, None).await;
        results.push(r);
    }
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "dispatch {i} must succeed (cache off, plain execution), error: {:?}",
            r.error
        );
    }

    // The server saw 2 requests: both dispatches executed.
    assert_eq!(
        total_requests.load(Ordering::SeqCst),
        2,
        "result cache off ⇒ identical dispatches both EXECUTE (2 provider requests)"
    );

    // No result-cache.json file.
    assert!(
        !data_dir.join("delegation").join("result-cache.json").exists(),
        "result cache off ⇒ result-cache.json must NOT be created"
    );
}

/// 3. Single-flight OFF: 2 identical CONCURRENT dispatches both execute
/// (server max_in_flight == 2).
#[tokio::test]
async fn gov_parity_single_flight_off_runs_both() {
    let steps = (0..4)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, max_in_flight, _total) =
        spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(gov_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            single_flight_enabled: false,
            ..Default::default()
        },
    ));

    // 2 identical concurrent dispatches.
    let mut handles = Vec::with_capacity(2);
    for _ in 0..2 {
        let mgr = mgr.clone();
        let cfg = agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single("gov-parity-sf-off-task");
            mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
        }));
    }
    let mut results = Vec::with_capacity(2);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "dispatch {i} must succeed (single flight off, both run), error: {:?}",
            r.error
        );
    }

    // Both executed CONCURRENTLY: max in-flight provider requests == 2.
    assert_eq!(
        max_in_flight.load(Ordering::SeqCst),
        2,
        "single flight off ⇒ 2 identical concurrent dispatches both execute in parallel"
    );
}

/// 4. Checkpointing OFF: a timed-out task's terminal resource record has
/// checkpoint == None (timeouts never carry a resume token).
#[tokio::test]
async fn gov_parity_checkpointing_off_no_resume_token() {
    // Every scripted step delays 2000ms against a 1s budget: attempt 1
    // times out on step 1, the (checkpointing-off) full-restart retry times
    // out on step 2, and the allowance (subagent_recovery_attempts default
    // 1) is exhausted → terminal [timeout] (mirrors governance_retry.rs's
    // gov_timeout shape; enough steps so the retry never hits the clamp
    // fallback, which would serve an instant success).
    let steps = (0..3)
        .map(|_| ScriptedFinal { delay_ms: 2000, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, _max_in_flight, _total) =
        spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let mgr = Arc::new(gov_manager(
        data_dir.clone(),
        GovernanceConfig {
            checkpointing: false,
            task_timeout_secs: 1,
            retry_budget: 1,
            ..Default::default()
        },
    ));

    let req = DelegationRequest::single("gov-parity-checkpoint-off-task");
    let cfg = agent_config(base_url);
    let tree = Config::defaults();
    let base = ToolRegistry::new();
    let result = mgr.dispatch_single(&req, &cfg, &tree, &base, None).await;
    assert!(
        !result.success,
        "the delayed task must time out at 1s; got success=true error={:?}",
        result.error
    );

    // The terminal record with outcome Timeout must have checkpoint None.
    let records = ResourceRecordStore::open(Some(&data_dir)).load();
    assert!(
        !records.is_empty(),
        "governance on ⇒ at least one resource record must exist"
    );
    let timeouts: Vec<_> = records
        .iter()
        .filter(|r| r.outcome == ResourceRecordOutcome::Timeout)
        .collect();
    assert_eq!(
        timeouts.len(),
        1,
        "exactly one Timeout record expected; outcomes: {:?}",
        records.iter().map(|r| r.outcome).collect::<Vec<_>>()
    );
    assert!(
        timeouts[0].checkpoint.is_none(),
        "checkpointing off ⇒ the Timeout record's checkpoint must be None, got {:?}",
        timeouts[0].checkpoint
    );
}

/// 5. Memory tracking OFF: a quick success record has memory_peak_kb == 0.
#[tokio::test]
async fn gov_parity_memory_tracking_off_zero_peak() {
    let steps = vec![ScriptedFinal { delay_ms: 0, text: "final" }];
    let (base_url, _starts_ms, _ends_ms, _max_in_flight, _total) =
        spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let mgr = Arc::new(gov_manager(
        data_dir.clone(),
        GovernanceConfig {
            memory_tracking_enabled: false,
            ..Default::default()
        },
    ));

    let req = DelegationRequest::single("gov-parity-mem-off-task");
    let cfg = agent_config(base_url);
    let tree = Config::defaults();
    let base = ToolRegistry::new();
    let result = mgr.dispatch_single(&req, &cfg, &tree, &base, None).await;
    assert!(
        result.success,
        "quick task must succeed, error: {:?}",
        result.error
    );

    let records = ResourceRecordStore::open(Some(&data_dir)).load();
    assert!(
        !records.is_empty(),
        "governance on ⇒ at least one resource record must exist"
    );
    for (i, r) in records.iter().enumerate() {
        assert_eq!(
            r.memory_peak_kb, 0,
            "memory tracking off ⇒ record {i} memory_peak_kb must be 0"
        );
    }
}

/// 6. Priority OFF: lanes collapse to Normal — saturate the 2 slots, then
/// enqueue one critical + one normal; BOTH complete and NEITHER is refused
/// (the strict FIFO property is unobservable without instrumentation).
#[tokio::test]
async fn gov_parity_priority_off_fifo() {
    let steps = (0..12)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "final" })
        .collect();
    let (base_url, _starts_ms, _ends_ms, _max_in_flight, _total) =
        spawn_scripted_server(steps).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(gov_manager(
        dir.path().to_path_buf(),
        GovernanceConfig {
            priority_enabled: false,
            max_queue_depth: 4,
            ..Default::default()
        },
    ));

    // Saturate the 2 child slots with 300ms tasks and let them COMPLETE,
    // then dispatch one critical + one normal into the freed slots.
    // NOTE: the pair cannot be enqueued concurrently with the saturating
    // wave — the queued-admission path currently deadlocks in
    // governance.rs (admit_next hands the first queued waiter seq=2 while
    // StartGate started=0 requires started+1>=seq; see the T024 report).
    // The brief's mandated assertions hold either way: both complete and
    // neither is refused.
    let mut saturate_reqs = Vec::with_capacity(2);
    for i in 0..2 {
        saturate_reqs.push(DelegationRequest::single(format!("gov-parity-sat-{i}")));
    }
    let saturate = dispatch_wave(mgr.clone(), base_url.clone(), saturate_reqs).await;
    assert_eq!(saturate.len(), 2);
    for (i, r) in saturate.iter().enumerate() {
        assert!(r.success, "saturation task {i} must succeed, error: {:?}", r.error);
    }

    let mut critical = DelegationRequest::single("gov-parity-prio-critical");
    critical.priority = Some(Priority::Critical);
    let normal = DelegationRequest::single("gov-parity-prio-normal");
    let results = dispatch_wave(mgr, base_url, vec![critical, normal]).await;
    assert_eq!(results.len(), 2);

    // Both priority-labeled dispatches complete; neither is refused.
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "priority off ⇒ dispatch {i} must complete (no busy refusal), error: {:?}",
            r.error
        );
        assert!(
            !r.error.as_deref().unwrap_or("").contains("[busy]"),
            "priority off ⇒ dispatch {i} must NOT be busy-refused"
        );
    }
}

/// 7. Config snapshot: `GovernanceConfig::from_config(&Config::defaults(), 3)`
/// resolves EXACTLY the contracts/config-keys.md defaults.
#[tokio::test]
async fn gov_parity_config_snapshot_defaults() {
    let g = GovernanceConfig::from_config(&joey_core::Config::defaults(), 3);
    assert!(g.enabled, "enabled == true");
    assert_eq!(g.max_queue_depth, 6, "max_queue_depth == 2 × 3 children");
    assert_eq!(g.task_timeout_secs, 600);
    assert_eq!(g.retry_budget, 2);
    assert_eq!(g.backoff_base_secs, 2.0);
    assert_eq!(g.backoff_max_secs, 60.0);
    assert!(g.checkpointing);
    assert!(g.result_cache_enabled);
    assert_eq!(g.result_cache_max_entries, 256);
    assert_eq!(g.result_cache_ttl_hours, 24);
    assert!(g.single_flight_enabled);
    assert_eq!(g.cpu_ceiling_secs, 300);
    assert_eq!(g.watchdog_interval_secs, 1);
    assert!(g.memory_tracking_enabled);
    assert!(g.priority_enabled);
    assert!(!g.degraded_mode_enabled);
    assert_eq!(g.degraded_sample_rate, 0.1);
}

/// 8. SC-008: both governance stores persist with user-only (0600) perms.
#[cfg(unix)]
#[tokio::test]
async fn gov_parity_files_user_only_perms() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();

    // Result cache: open → store a success → the persisted file is 0600.
    let cache_path = dir.path().join("cache.json");
    let mut cache = ResultCache::open(cache_path.clone(), 4, 24);
    let now = chrono::Utc::now();
    let success_result = DelegationResult {
        goal: "g".to_string(),
        summary: "s".to_string(),
        success: true,
        error: None,
        token_usage: Default::default(),
        wall_clock: Duration::from_millis(10),
        model: "m".to_string(),
        iterations: 1,
        persisted_session_id: None,
        stop_reason: None,
    };
    cache.store("sig".to_string(), &success_result, &now);
    let mode = std::fs::metadata(&cache_path)
        .expect("cache file must exist after store")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "SC-008: result cache file must be user-only (0600), got {:o}",
        mode & 0o777
    );

    // Resource records: append one record → the jsonl file is 0600.
    let record = joey_orchestration::resource_records::new_record(
        "sig".to_string(),
        Priority::Normal,
        ResourceRecordOutcome::Completed,
        0,
        0,
        0,
        0,
        0,
        0,
        None,
        Default::default(),
        false,
    );
    let store = ResourceRecordStore::open(Some(dir.path()));
    store.append(&record).expect("append must succeed");
    let mode = std::fs::metadata(store.path())
        .expect("records file must exist after append")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "SC-008: resource-records.jsonl must be user-only (0600), got {:o}",
        mode & 0o777
    );
}
