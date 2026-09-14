//! Integration test: governance dedup — identical work runs exactly once
//! (feature 030, US4 / FR-007/FR-008, T017 TDD RED).
//!
//! RED contract pinned here (see specs/030-please-implement-features/):
//!   (a) 5 IDENTICAL simultaneous dispatch_single calls produce exactly
//!       ONE child execution (total provider requests == script length,
//!       not 5×) and all 5 callers receive successful, IDENTICAL results
//!       (single-flight coalescing, FR-008);
//!   (b) after simulating a process restart (drop the manager, build a
//!       new one over the SAME governance data_dir), dispatching the SAME
//!       request again is served from the persistent exact-signature
//!       result cache with ZERO new executions (server request count
//!       unchanged) and returns the first run's result (FR-007);
//!   (c) dispatches differing ONLY in budget fields never cross-serve:
//!       task_signature includes governance.task_timeout_secs, so two
//!       managers over the same data_dir with differing timeouts are
//!       DIFFERENT tasks — the second dispatch must EXECUTE (server
//!       request count increases). NOTE: this test asserts EXECUTION
//!       happens — the inverse pin. Today nothing dedups, so both
//!       dispatches execute and it passes; after T018 the differing
//!       signature misses the cache, executes, and it still passes —
//!       stable on both sides of the implementation.
//!
//! Harness conventions mirror tests/governance_admission.rs (which itself
//! mirrors tests/concurrency_limiter.rs): scripted mock OpenAI provider
//! over a TcpListener on 127.0.0.1:0 with delay injection and
//! AtomicUsize request-count probes, agent_config() and
//! governance_manager() helpers, GovernanceConfig { enabled: true,
//! data_dir: Some(tempdir) }. Multi-turn children are forced by
//! scripting `tool_calls` responses (the tool_step/final_step convention
//! from tests/governance_retry.rs, budgets.rs clamping: the sequential
//! script index clamps to the last entry once exhausted, so the server
//! never runs dry while pre-implementation behavior executes everything).
//!
//! Every test fn starts with `gov_` so intermediate regression runs can
//! `--skip gov_` until T018 implements dedup.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::{DelegationRequest, DelegationResult, ManagerConfig, SubagentManager};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (governance_admission.rs harness
// style + governance_retry.rs multi-turn tool-call scripting) with a
// request-count AtomicUsize probe.
// ---------------------------------------------------------------------------

/// One scripted response: content, an optional tool call (forcing another
/// iteration), reported usage, a delay, and an HTTP status.
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
    /// Sequential script position: the Nth request serves script[N]
    /// (clamped to the last entry once exhausted — budgets.rs convention).
    seq_index: Arc<AtomicUsize>,
    /// Total requests served (the execution-count probe).
    total_requests: Arc<AtomicUsize>,
}

/// Serve exactly one scripted request per connection, then close.
async fn serve_conn(mut stream: TcpStream, p: ServerProbe) {
    let Some(_body) = read_http_body(&mut stream).await else {
        return;
    };
    p.total_requests.fetch_add(1, Ordering::SeqCst);
    let idx = p.seq_index.fetch_add(1, Ordering::SeqCst);
    let step = p.script[idx.min(p.script.len() - 1)].clone();
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
}

/// Test-side handle on the scripted mock provider.
struct GovServer {
    base_url: String,
    total_requests: Arc<AtomicUsize>,
}

impl GovServer {
    fn url(&self) -> String {
        self.base_url.clone()
    }

    /// Total provider requests served so far — the child-execution count.
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
    let total_requests = Arc::new(AtomicUsize::new(0));
    let ret_total = total_requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let probe = ServerProbe {
                script: script.clone(),
                seq_index: seq_index.clone(),
                total_requests: total_requests.clone(),
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
// Governance-dedup test helpers.
// ---------------------------------------------------------------------------

/// The trivial tool the scripted children call (registered into the base
/// registry so the child's schema/dispatch both know it) — copied from
/// tests/governance_retry.rs.
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

/// Manager with the governance-dedup shape under test: 2 child slots,
/// 8 request permits, governance ON (result cache + single flight at
/// their enabled defaults), isolated data dir. `enabled` and `data_dir`
/// are forced (mirrors governance_retry.rs's governance_manager).
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

/// The IDENTICAL request every dedup test dispatches: same goal, context,
/// toolsets, model, and role — byte-identical signature inputs.
fn dedup_request() -> DelegationRequest {
    let mut req = DelegationRequest::single("gov-dedup-identical-task");
    req.context = Some("shared dedup context".to_string());
    req.max_turns = Some(4); // bound today's (pre-implementation) runs
    req
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// FR-008 (single-flight): 5 IDENTICAL simultaneous dispatch_single calls
/// (same goal/context/toolsets/model/role, fired concurrently) must produce
/// exactly ONE child execution — total provider requests == script length
/// (2 tool steps + 1 final = 3) EXACTLY once, not 5× — and all 5 callers
/// receive success==true with IDENTICAL summaries (the one execution's
/// result coalesced across the wave).
#[tokio::test]
async fn gov_five_identical_simultaneous_single_execution() {
    // Script: 2 tool steps + 1 final, 200ms each (clamped once exhausted so
    // today's execute-everything behavior still terminates and succeeds).
    let server = spawn_scripted_server(vec![
        tool_step(200),
        tool_step(200),
        final_step(200),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(governance_manager(
        dir.path().to_path_buf(),
        GovernanceConfig::default(),
    ));

    // Fire 5 dispatch_single calls with the IDENTICAL request concurrently.
    let mut handles = Vec::with_capacity(5);
    for _ in 0..5 {
        let mgr = mgr.clone();
        let cfg = agent_config(server.url());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        handles.push(tokio::spawn(async move {
            let req = dedup_request();
            mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
        }));
    }
    let mut results: Vec<DelegationResult> = Vec::with_capacity(5);
    for h in handles {
        results.push(h.await.expect("dispatch task panicked"));
    }
    assert_eq!(results.len(), 5, "one result per dispatched request");

    // (a) RED headline: exactly ONE execution — total server requests ==
    // script length (3), NOT 5× (≈15) as execute-everything produces.
    assert_eq!(
        server.total_requests(),
        3,
        "5 identical simultaneous dispatches must execute the 3-request script EXACTLY once (single-flight), got {} provider requests — no dedup exists today, so every dispatch executes",
        server.total_requests()
    );

    // (b) All 5 results succeed.
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "coalesced result {i} must succeed (the one execution reached the scripted final), error: {:?}",
            r.error
        );
    }

    // (c) All 5 summaries are IDENTICAL (the same execution's summary).
    let first = results[0].summary.clone();
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.summary, first,
            "coalesced result {i} must carry the IDENTICAL summary as result 0"
        );
    }
}

/// FR-007 (persistent result cache): run one dispatch (script executes
/// once, 3 requests), drop manager M1, build manager M2 over the SAME
/// governance data_dir (governance enabled, result cache enabled), and
/// dispatch the SAME request again — M2 must serve it from the cache with
/// ZERO new executions (server count still 3) and return the first run's
/// result (success + same summary string).
#[tokio::test]
async fn gov_cache_hit_after_restart_zero_executions() {
    let server = spawn_scripted_server(vec![
        tool_step(200),
        tool_step(200),
        final_step(200),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // M1: run the dispatch once — the script executes (3 requests).
    let m1 = governance_manager(data_dir.clone(), GovernanceConfig::default());
    let req = dedup_request();
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));
    let result1 = m1.dispatch_single(&req, &cfg, &tree, &base, None).await;
    assert!(result1.success, "first run must succeed, error: {:?}", result1.error);
    assert_eq!(
        server.total_requests(),
        3,
        "first run executes the 3-request script exactly once"
    );
    let count_after_first = server.total_requests();

    // Simulated process restart: drop M1, build M2 over the same data_dir.
    drop(m1);
    let m2 = governance_manager(data_dir, GovernanceConfig::default());

    // Same request again — must come from the persistent cache.
    let req2 = dedup_request();
    let cfg2 = agent_config(server.url());
    let tree2 = Config::defaults();
    let mut base2 = ToolRegistry::new();
    base2.register(Arc::new(EchoTool));
    let result2 = m2.dispatch_single(&req2, &cfg2, &tree2, &base2, None).await;

    // (a) RED headline: zero new executions — the count did NOT increase.
    assert_eq!(
        server.total_requests(),
        count_after_first,
        "after restart the same signature must return from the persistent cache with ZERO new executions (still {count_after_first}), got {} — no result cache exists today, so the dispatch re-executes",
        server.total_requests()
    );

    // (b) The cached result equals the first run's: success + same summary.
    assert!(
        result2.success,
        "cache-hit result must be success like the first run, error: {:?}",
        result2.error
    );
    assert_eq!(
        result2.summary, result1.summary,
        "cache-hit result must carry the first run's summary string"
    );
}

/// FR-007/FR-008 guard (no cross-serving across differing budgets): two
/// managers over the SAME data_dir, identical request, differing ONLY the
/// budget that participates in the signature via task_signature(req,
/// timeout_secs, cpu_ceiling_secs) — M1 with task_timeout_secs=600, M2
/// with task_timeout_secs=60. Run A on M1 (executes once); the same
/// request on M2 must ALSO execute (server count increases): a differing
/// timeout budget is a DIFFERENT task and must never be cross-served from
/// M1's cache entry.
///
/// NOTE — this test asserts EXECUTION happens (the inverse pin): today
/// nothing caches so both dispatches execute and it passes; after T018
/// the differing signature misses the cache → executes → count increases
/// → it STILL passes. Stable on both sides of the implementation.
#[tokio::test]
async fn gov_differing_budgets_never_cross_serve() {
    // Two full rounds (2 tool steps + 1 final each) so a second execution
    // replays the whole script deterministically.
    let server = spawn_scripted_server(vec![
        tool_step(200),
        tool_step(200),
        final_step(200),
        tool_step(200),
        tool_step(200),
        final_step(200),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // M1: 600s timeout budget. Run A executes once.
    let m1 = governance_manager(
        data_dir.clone(),
        GovernanceConfig {
            task_timeout_secs: 600,
            ..Default::default()
        },
    );
    let req_a = dedup_request();
    let cfg = agent_config(server.url());
    let tree = Config::defaults();
    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));
    let result_a = m1.dispatch_single(&req_a, &cfg, &tree, &base, None).await;
    assert!(result_a.success, "run A must succeed, error: {:?}", result_a.error);
    let count_after_a = server.total_requests();
    assert!(
        count_after_a >= 3,
        "run A must execute the script (>= 3 requests), got {count_after_a}"
    );

    // M2 over the SAME data_dir but a DIFFERENT budget: 60s timeout. The
    // identical request under a differing budget is a different task
    // signature — it must EXECUTE, not be served from M1's entry.
    let m2 = governance_manager(
        data_dir,
        GovernanceConfig {
            task_timeout_secs: 60,
            ..Default::default()
        },
    );
    let req_b = dedup_request();
    let cfg_b = agent_config(server.url());
    let tree_b = Config::defaults();
    let mut base_b = ToolRegistry::new();
    base_b.register(Arc::new(EchoTool));
    let result_b = m2.dispatch_single(&req_b, &cfg_b, &tree_b, &base_b, None).await;

    assert!(
        result_b.success,
        "the differently-budgeted dispatch must still succeed by executing, error: {:?}",
        result_b.error
    );
    assert!(
        server.total_requests() > count_after_a,
        "differing timeout budgets are DIFFERENT task signatures and must never cross-serve: M2's dispatch must EXECUTE (server count must increase beyond {count_after_a}), got {}",
        server.total_requests()
    );
}
