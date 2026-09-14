//! Integration test: T034 governance convergence for WAVES — batch and
//! background children dispatched through transient managers now INHERIT
//! the parent's governance (timeout/admission/dedup) and SHARE the parent's
//! records store, so every dispatch kind writes resource records
//! (feature 030, FR-002/004/007/008/011).
//!
//! Harness mirrors tests/governance_admission.rs verbatim (which mirrors
//! tests/concurrency_limiter.rs): ScriptedFinal mock provider (one scripted
//! response per HTTP connection, delay injection), in-flight probes,
//! TcpListener on 127.0.0.1:0, agent_config() helper, SubagentManager
//! construction. The background test mirrors the delegate_task tool surface
//! of tests/governance_priority.rs.
//!
//! Every test fn starts with `gov_wave_`.

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
    DelegateTask, ManagerConfig, SubagentManager, TaskSpec,
};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (governance_admission.rs harness)
// with in-flight concurrency probes.
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
// Wave-test helpers.
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

/// Manager with the T034 wave shape under test: 2 child slots, governance
/// forced ON with the per-test GovernanceConfig overrides, isolated data
/// dir (the governance_priority.rs helper convention).
fn governance_wave_manager(mut gov: GovernanceConfig, data_dir: std::path::PathBuf) -> SubagentManager {
    gov.enabled = true;
    gov.data_dir = Some(data_dir);
    SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: gov,
        ..Default::default()
    })
}

/// A trivial tool the scripted child can call (registered into the base
/// registry so the child's schema/dispatch both know it) — mirrors
/// tests/governance_priority.rs.
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

/// T034 (a): a dispatch_batch wave of 3 tasks writes exactly one Completed
/// resource record per child through the shared store (FR-011).
#[tokio::test]
async fn gov_wave_batch_children_get_records() {
    // ≥6 final steps, 300ms each: three children, each one turn.
    let steps = (0..6)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "done" })
        .collect();
    let (base_url, _starts, _ends, _max) = spawn_scripted_server(steps).await;

    let data_dir = tempfile::tempdir().unwrap();
    let mgr = governance_wave_manager(
        GovernanceConfig {
            max_queue_depth: 4,
            ..Default::default()
        },
        data_dir.path().to_path_buf(),
    );

    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));

    let tasks: Vec<TaskSpec> = (0..3)
        .map(|i| TaskSpec {
            goal: format!("gov-wave-batch-{i}"),
            context: None,
            model: None,
            toolsets: vec!["coding".to_string()],
            role: None,
            subagent_type: None,
            background: false,
            budgets: None,
        })
        .collect();

    let results = mgr
        .dispatch_batch(
            &tasks,
            None,
            &["coding".to_string()],
            &agent_config(base_url),
            &Config::defaults(),
            &base,
            None,
        )
        .await;

    assert_eq!(results.len(), 3);
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "batch child {i} must succeed against the mock provider, error: {:?}",
            r.error
        );
    }

    let records = ResourceRecordStore::open(Some(data_dir.path())).load();
    assert_eq!(
        records.len(),
        3,
        "exactly one record per batch child (3 goals → 3 signatures), got {}: {:?}",
        records.len(),
        records.iter().map(|r| (&r.task_signature, r.outcome)).collect::<Vec<_>>()
    );
    assert!(
        records.iter().all(|r| r.outcome == ResourceRecordOutcome::Completed),
        "all batch records must be Completed, got {:?}",
        records.iter().map(|r| r.outcome).collect::<Vec<_>>()
    );
    let sigs: std::collections::HashSet<&str> = records
        .iter()
        .map(|r| r.task_signature.as_str())
        .collect();
    assert_eq!(sigs.len(), 3, "3 distinct goals → 3 distinct signatures");
}

/// T034 (b): the inherited task timeout applies to batch children — a 1s
/// budget against 2s mock steps fails both children with the exact
/// `[timeout]` error and records exactly two Timeout outcomes (FR-002).
#[tokio::test]
async fn gov_wave_batch_child_timeout_recorded() {
    // ≥4 steps of 2000ms: each child's single turn exceeds the 1s timeout.
    let steps = (0..4)
        .map(|_| ScriptedFinal { delay_ms: 2000, text: "slow" })
        .collect();
    let (base_url, _starts, _ends, _max) = spawn_scripted_server(steps).await;

    let data_dir = tempfile::tempdir().unwrap();
    let mgr = governance_wave_manager(
        GovernanceConfig {
            task_timeout_secs: 1,
            retry_budget: 1,
            max_queue_depth: 4,
            ..Default::default()
        },
        data_dir.path().to_path_buf(),
    );

    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));

    let tasks: Vec<TaskSpec> = (0..2)
        .map(|i| TaskSpec {
            goal: format!("gov-wave-timeout-{i}"),
            context: None,
            model: None,
            toolsets: vec!["coding".to_string()],
            role: None,
            subagent_type: None,
            background: false,
            budgets: None,
        })
        .collect();

    let results = mgr
        .dispatch_batch(
            &tasks,
            None,
            &["coding".to_string()],
            &agent_config(base_url),
            &Config::defaults(),
            &base,
            None,
        )
        .await;

    assert_eq!(results.len(), 2);
    for (i, r) in results.iter().enumerate() {
        assert!(
            !r.success,
            "batch child {i} must fail under the 1s inherited timeout"
        );
        assert!(
            r.error.as_deref().unwrap_or("").contains("[timeout]"),
            "batch child {i} error must contain [timeout], got: {:?}",
            r.error
        );
    }

    let records = ResourceRecordStore::open(Some(data_dir.path())).load();
    assert_eq!(
        records.len(),
        2,
        "exactly one Timeout record per timed-out batch child, got {}: {:?}",
        records.len(),
        records.iter().map(|r| (&r.task_signature, r.outcome)).collect::<Vec<_>>()
    );
    assert!(
        records.iter().all(|r| r.outcome == ResourceRecordOutcome::Timeout),
        "all records must be Timeout, got {:?}",
        records.iter().map(|r| r.outcome).collect::<Vec<_>>()
    );
}

/// T034 (c): a background FLAG dispatch through the delegate_task tool
/// writes a resource record through the shared store — outcome Completed,
/// priority Background (FR-011).
#[tokio::test]
async fn gov_wave_background_children_get_records() {
    // Same server shape as (a): ≥6 final steps, 300ms each.
    let steps = (0..6)
        .map(|_| ScriptedFinal { delay_ms: 300, text: "done" })
        .collect();
    let (base_url, _starts, _ends, _max) = spawn_scripted_server(steps).await;

    let data_dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_wave_manager(
        GovernanceConfig {
            max_queue_depth: 4,
            ..Default::default()
        },
        data_dir.path().to_path_buf(),
    ));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let tool = DelegateTask::new(
        mgr.clone(),
        agent_config(base_url),
        Config::defaults(),
        registry,
        Some(event_tx),
        None,
    );

    let ctx_dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(
        ctx_dir.path().to_path_buf(),
        Config::defaults(),
        "t034",
    );

    let record_count_before = ResourceRecordStore::open(Some(data_dir.path())).load().len();

    let res = tool
        .execute(json!({"goal": "bg wave record", "background": true}), &ctx)
        .await;
    match &res {
        ToolResult::Text(t) => assert!(
            t.contains("[BACKGROUND]") && t.contains("bg wave record"),
            "background dispatch must return a non-error handle/notice line, got: {t:?}"
        ),
        ToolResult::Error(e) => panic!("background dispatch must start, got: {e}"),
        other => panic!("background dispatch must return Text, got: {other:?}"),
    }

    // Bounded-wait for the background completion notice (30s deadline):
    // any SubagentComplete for this goal (strict match on goal; the event
    // carries it verbatim from the dispatch result).
    let mut saw_complete = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, event_rx.recv()).await {
        if let AgentEvent::SubagentComplete { goal, .. } = &ev {
            if goal.contains("bg wave record") {
                saw_complete = true;
                break;
            }
        }
    }
    assert!(
        saw_complete,
        "a SubagentComplete for 'bg wave record' must arrive within 30s"
    );

    // Grace: let any straggling record append land before counting.
    tokio::time::sleep(Duration::from_secs(2)).await;
    // T034: background children dispatch via shared_child_manager, which now
    // inherits governance and shares the records store — background waves write
    // resource records like every other dispatch kind (FR-011).
    let records = ResourceRecordStore::open(Some(data_dir.path())).load();
    assert!(
        records.len() >= record_count_before + 1,
        "background child must write a resource record (T034 shared store), before={record_count_before} after={}",
        records.len()
    );
    assert!(
        records[record_count_before..]
            .iter()
            .any(|r| r.priority == Priority::Background
                && r.outcome == ResourceRecordOutcome::Completed),
        "at least one new record must be Completed with priority==Background, records: {:?}",
        records
            .iter()
            .map(|r| (r.priority, r.outcome))
            .collect::<Vec<_>>()
    );
}
