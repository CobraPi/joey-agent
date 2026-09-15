//! THROWAWAY DIAGNOSTIC (delete after use): same harness as
//! governance_priority.rs test 1, plus arrival timestamps and record dump.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::resource_records::ResourceRecordStore;
use joey_orchestration::types::Priority;
use joey_orchestration::{
    DelegationRequest, DelegationResult, ManagerConfig, SubagentManager,
};
use joey_tools::ToolRegistry;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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
    tags: Arc<Vec<String>>,
    first_arrivals: Arc<Mutex<Vec<(String, u64)>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    epoch: Instant,
}

async fn gov_serve_conn(mut stream: TcpStream, p: ServerProbe) {
    let Some(body) = gov_read_http_body(&mut stream).await else {
        return;
    };
    for tag in p.tags.iter() {
        if body.contains(tag.as_str()) {
            let mut arrivals = p.first_arrivals.lock().unwrap();
            if !arrivals.iter().any(|(t, _)| t == tag) {
                arrivals.push((tag.clone(), p.epoch.elapsed().as_millis() as u64));
            }
            break;
        }
    }
    let step = p.queue.lock().unwrap().pop_front();
    let Some(step) = step else { return; };
    p.in_flight.fetch_add(1, Ordering::SeqCst);
    let now_in_flight = p.in_flight.fetch_add(0, Ordering::SeqCst) + 1;
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
    p.in_flight.fetch_sub(1, Ordering::SeqCst);
}

async fn gov_spawn_scripted_server(
    steps: Vec<ScriptedFinal>,
    tags: Vec<String>,
) -> (String, Arc<Mutex<Vec<(String, u64)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let first_arrivals: Arc<Mutex<Vec<(String, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(Mutex::new(VecDeque::from(steps)));
    let tags = Arc::new(tags);
    let epoch = Instant::now();
    let ret_arrivals = first_arrivals.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let probe = ServerProbe {
                queue: queue.clone(),
                tags: tags.clone(),
                first_arrivals: first_arrivals.clone(),
                in_flight: in_flight.clone(),
                max_in_flight: max_in_flight.clone(),
                epoch,
            };
            tokio::spawn(async move {
                gov_serve_conn(stream, probe).await;
            });
        }
    });
    (format!("http://{addr}"), ret_arrivals)
}

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

#[tokio::test]
async fn diag_critical_order() {
    let steps = (0..8)
        .map(|_| ScriptedFinal { delay_ms: 2000, text: "final" })
        .collect();
    let tags: Vec<String> = (1..=4)
        .map(|i| format!("gov-prio-N{i}"))
        .chain(std::iter::once("gov-prio-C1".to_string()))
        .collect();
    let (base_url, arrivals) = gov_spawn_scripted_server(steps, tags).await;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(SubagentManager::new(ManagerConfig {
        max_concurrent_children: 2,
        max_concurrent_requests: 8,
        governance: GovernanceConfig {
            enabled: true,
            max_queue_depth: 4,
            priority_enabled: true,
            data_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        },
        ..Default::default()
    }));

    let normals: Vec<String> = (1..=4).map(|i| format!("gov-prio-N{i}")).collect();
    let mut handles = Vec::new();
    for goal in normals {
        let mgr = mgr.clone();
        let cfg = gov_agent_config(base_url.clone());
        let tree = Config::defaults();
        let base = ToolRegistry::new();
        handles.push(tokio::spawn(async move {
            let req = DelegationRequest::single(goal);
            mgr.dispatch_single(&req, &cfg, &tree, &base, None).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut req = DelegationRequest::single("gov-prio-C1");
    req.priority = Some(Priority::Critical);
    let cfg = gov_agent_config(base_url.clone());
    let tree = Config::defaults();
    let base = ToolRegistry::new();
    let c1 = mgr.dispatch_single(&req, &cfg, &tree, &base, None).await;
    drop(c1);

    for h in handles {
        let _ = h.await;
    }

    let arr = arrivals.lock().unwrap().clone();
    eprintln!("ARRIVALS (tag, ms): {arr:?}");
    let store = ResourceRecordStore::open(Some(dir.path()));
    let records = store.load();
    for r in &records {
        eprintln!(
            "RECORD sig={:.60}.. prio={:?} outcome={:?} qwait={} compute={}",
            r.task_signature, r.priority, r.outcome, r.queue_wait_ms, r.compute_ms
        );
    }
}
