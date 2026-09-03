//! Integration test: shared concurrency limiter (SC-008).
//! Verify the parent's semaphore is shared across batch children.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::{ManagerConfig, SubagentManager, TaskSpec};
use joey_tools::ToolRegistry;
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn make_agent_config() -> AgentConfig {
    AgentConfig {
        model: "test-model".to_string(),
        provider: "openrouter".to_string(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        api_key: None,
        max_turns: 10,
        api_max_retries: 3,
        tool_delay: 0.0,
        reasoning: None,
        enabled_tools: vec![],
        max_tokens: None,
        stream: false,
        pass_session_id: false,
        model_pinned: false,
    }
}

#[tokio::test]
async fn semaphore_is_shared_across_batch_children() {
    // The semaphore created by SubagentManager::new must have its permits
    // reduced when batch children are dispatched. We verify the semaphore
    // is the same Arc by checking that available_permits matches.
    let mgr = SubagentManager::new(ManagerConfig {
        max_concurrent_requests: 3,
        max_concurrent_children: 2,
        ..Default::default()
    });

    let sem = mgr.semaphore();
    assert_eq!(sem.available_permits(), 3);

    // Dispatch a small batch — the semaphore should still be intact after.
    let tasks = vec![
        TaskSpec { goal: "CL-A".to_string(), context: None, model: None, toolsets: vec![], role: None, subagent_type: None, background: false, budgets: None },
        TaskSpec { goal: "CL-B".to_string(), context: None, model: None, toolsets: vec![], role: None, subagent_type: None, background: false, budgets: None },
    ];

    let _ = mgr
        .dispatch_batch(
            &tasks,
            None,
            &[],
            &make_agent_config(),
            &Config::defaults(),
            &ToolRegistry::new(),
            None,
        )
        .await;

    // After completion, all permits should be returned.
    assert_eq!(sem.available_permits(), 3);
}

#[tokio::test]
async fn max_concurrent_children_caps_large_batches() {
    // With max_concurrent_children=2 and 5 tasks, the batch should complete
    // but internally process in chunks of 2. All 5 results returned.
    let mgr = SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        ..Default::default()
    });

    let tasks: Vec<TaskSpec> = (0..5)
        .map(|i| TaskSpec {
            goal: format!("Chunk-task-{}", i),
            context: None,
            model: None,
            toolsets: vec![],
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
            &[],
            &make_agent_config(),
            &Config::defaults(),
            &ToolRegistry::new(),
            None,
        )
        .await;

    assert_eq!(results.len(), 5, "all 5 results returned despite the cap");
}

// ---------------------------------------------------------------------------
// Scripted mock OpenAI-compatible provider (same harness style as
// tests/control_tool.rs) with in-flight concurrency + start-time probes.
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
    starts_ms: Arc<Mutex<Vec<u64>>>,
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
    p.in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// Bind a scripted mock provider; returns (base_url, start-time log,
/// max observed in-flight requests).
async fn spawn_scripted_server(
    steps: Vec<ScriptedFinal>,
) -> (String, Arc<Mutex<Vec<u64>>>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let starts_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    let epoch = Instant::now();
    let ret_starts = starts_ms.clone();
    let ret_max = max_in_flight.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let probe = ServerProbe {
                queue: queue.clone(),
                starts_ms: starts_ms.clone(),
                in_flight: in_flight.clone(),
                max_in_flight: max_in_flight.clone(),
                epoch,
            };
            tokio::spawn(async move {
                serve_conn(stream, probe).await;
            });
        }
    });
    (format!("http://{addr}"), ret_starts, ret_max)
}

// A trivial tool the scripted child can call (registered into the base
// registry so the child's schema/dispatch both know it).

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

#[tokio::test]
async fn no_chunk_barrier_slow_child_does_not_block_later_children() {
    // OLD chunked behavior: 4 children, cap 2 -> chunk [slow(500ms),
    // fast(80ms)] then [fast, fast]: the 3rd/4th children could not START
    // until the whole first chunk drained (>= 500ms).
    // NEW slot behavior: the fast child hands its slot straight to the 3rd
    // child (~80ms in). Assert a later child starts well before the slow
    // one finishes, while the cap of 2 concurrently-running children holds.
    let steps = vec![
        ScriptedFinal { delay_ms: 500, text: "slow" },
        ScriptedFinal { delay_ms: 80, text: "fast-a" },
        ScriptedFinal { delay_ms: 80, text: "fast-b" },
        ScriptedFinal { delay_ms: 80, text: "fast-c" },
    ];
    let (base_url, starts_ms, max_in_flight) = spawn_scripted_server(steps).await;

    let mgr = SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        // Keep the request pool wide so the slot semaphore is the only
        // binding constraint in this test.
        max_concurrent_requests: 8,
        ..Default::default()
    });

    let mut base = ToolRegistry::new();
    base.register(Arc::new(EchoTool));

    let tasks: Vec<TaskSpec> = (0..4)
        .map(|i| TaskSpec {
            goal: format!("Slot-task-{i}"),
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
            &[],
            &agent_config(base_url),
            &Config::defaults(),
            &base,
            None,
        )
        .await;

    assert_eq!(results.len(), 4);
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "child {i} must succeed against the mock provider, error: {:?}",
            r.error
        );
    }

    let observed_max = max_in_flight.load(Ordering::SeqCst);
    assert!(
        observed_max <= 2,
        "cap of 2 concurrently-running children must hold, observed {observed_max}"
    );
    assert!(
        observed_max >= 2,
        "the wave must genuinely run 2-wide, observed {observed_max}"
    );

    let mut starts = starts_ms.lock().unwrap().clone();
    starts.sort_unstable();
    // RELATIVE timing (immune to setup latency before the first provider
    // request): the first two children start together; the slow one holds
    // its slot for 500ms. Chunked: the 3rd child cannot start until the
    // whole first chunk drains, i.e. starts[2] - starts[0] >= 500.
    // Slotted: the fast sibling hands its slot straight over, observed
    // ~100-170ms. 450ms separates the two regimes with margin on both
    // sides.
    let third_start_offset = starts[2] - starts[0];
    assert!(
        third_start_offset < 450,
        "3rd child must start before the slow child's chunk would have drained (offset {third_start_offset}ms, starts: {starts:?})"
    );

    // All provider-request permits return after the wave.
    assert_eq!(mgr.semaphore().available_permits(), 8);
}
