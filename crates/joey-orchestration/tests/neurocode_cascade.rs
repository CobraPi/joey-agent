//! Orchestrator-only NeuroCode injection policy: the parent session's
//! NeuroCode engine is installed on the orchestrator/main agent ONLY.
//! Dispatched subagents receive NO NeuroCode Context — the manager no
//! longer carries or propagates an engine, so even with a live engine in
//! the parent session the child's provider request carries no context
//! block. The orchestrator pastes any code-map facts its children need
//! directly into their briefs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::parse::ingest_project;
use joey_neurocode::{DefaultEngine, NeuroCodeConfig, NeuroCodeEngine};
use joey_orchestration::{DelegateTask, SubagentManager};
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolRegistry;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Local mock OpenAI-compatible provider on 127.0.0.1 that CAPTURES bodies.
// ---------------------------------------------------------------------------

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

/// Serve exactly one HTTP request on `stream`, pushing the raw request
/// body into `bodies`, then respond 200 with a chat completion.
async fn serve_conn(mut stream: TcpStream, bodies: Arc<Mutex<Vec<String>>>) {
    // Read headers + body (bounded, with a safety timeout so a bad client
    // can never hang the test).
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 256 * 1024 {
            return;
        }
        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
        else {
            return;
        };
        if n == 0 {
            return;
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

    let body = String::from_utf8_lossy(&buf[header_end + 4..]).to_string();
    bodies.lock().unwrap().push(body);

    let resp_body = openai_body("cascade-done");
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
        resp_body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Bind a capturing mock provider on 127.0.0.1:0; returns its base URL.
async fn spawn_capturing_server(bodies: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let bodies = bodies.clone();
            tokio::spawn(async move {
                serve_conn(stream, bodies).await;
            });
        }
    });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

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

/// A tiny two-artifact Java project: an interface and its implementation.
fn write_project(root: &std::path::Path) {
    let src = root.join("src/main/java/com/example");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("UserService.java"),
        "package com.example;\n\npublic interface UserService {\n    String findUser(String id);\n}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("UserServiceImpl.java"),
        "package com.example;\n\npublic class UserServiceImpl implements UserService {\n    @Override\n    public String findUser(String id) {\n        return \"user-\" + id;\n    }\n}\n",
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Orchestrator-only policy: even with a live NeuroCode engine built for the parent session, the dispatched child's provider request carries NO NeuroCode Context.
#[tokio::test]
async fn subagents_never_receive_neurocode_context() {
    let tmp = TempDir::new().unwrap();
    write_project(tmp.path());

    // Open + ingest the graph ONCE (the "parent session's" index).
    let graph = DependencyGraph::open_for_project(tmp.path()).unwrap();
    let ingest = ingest_project(&graph, tmp.path());
    assert!(ingest.errors.is_empty());
    assert!(ingest.artifacts_seen > 0);
    drop(graph);

    // The parent session's engine — children must share this exact engine.
    let mut nc = NeuroCodeConfig::default();
    nc.enabled = true;
    let _engine: Arc<dyn NeuroCodeEngine> = Arc::new(DefaultEngine::new(nc, tmp.path().to_path_buf()));

    let manager = Arc::new(SubagentManager::new(Default::default()));

    let bodies = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_capturing_server(bodies.clone()).await;
    let tool = DelegateTask::new(
        manager,
        agent_config(base),
        Config::defaults(),
        ToolRegistry::new(),
        None,
        None,
    );
    let ctx = ToolContext::new(tmp.path().to_path_buf(), Config::defaults(), "neurocode-cascade-test");
    let res = tool
        .execute(
            json!({"goal": "Refactor com.example.UserServiceImpl to add caching. Locate UserServiceImpl and its interface UserService first."}),
            &ctx,
        )
        .await;
    match res {
        ToolResult::Text(s) => assert_eq!(s, "cascade-done"),
        other => panic!("expected Text \"cascade-done\", got: {other:?}"),
    }

    let body = bodies.lock().unwrap()[0].clone();
    assert!(
        !body.contains("NeuroCode Context"),
        "child request body must NOT contain a NeuroCode Context (orchestrator-only injection):\n{body}"
    );
}

/// FR-020 byte-parity: with NO engine installed, the child's provider
/// request contains no NeuroCode Context (pre-feature behavior).
#[tokio::test]
async fn no_engine_means_no_neurocode_context() {
    let tmp = TempDir::new().unwrap();
    write_project(tmp.path());

    // Same indexed graph on disk, but NO engine installed on the manager.
    let graph = DependencyGraph::open_for_project(tmp.path()).unwrap();
    let ingest = ingest_project(&graph, tmp.path());
    assert!(ingest.errors.is_empty());
    assert!(ingest.artifacts_seen > 0);
    drop(graph);

    let manager = Arc::new(SubagentManager::new(Default::default()));

    let bodies = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_capturing_server(bodies.clone()).await;
    let tool = DelegateTask::new(
        manager,
        agent_config(base),
        Config::defaults(),
        ToolRegistry::new(),
        None,
        None,
    );
    let ctx = ToolContext::new(tmp.path().to_path_buf(), Config::defaults(), "neurocode-cascade-test");
    let res = tool
        .execute(
            json!({"goal": "Refactor com.example.UserServiceImpl to add caching. Locate UserServiceImpl and its interface UserService first."}),
            &ctx,
        )
        .await;
    match res {
        ToolResult::Text(s) => assert_eq!(s, "cascade-done"),
        other => panic!("expected Text \"cascade-done\", got: {other:?}"),
    }

    let body = bodies.lock().unwrap()[0].clone();
    assert!(
        !body.contains("NeuroCode Context"),
        "child request body must not contain a NeuroCode Context without an engine:\n{body}"
    );
}
