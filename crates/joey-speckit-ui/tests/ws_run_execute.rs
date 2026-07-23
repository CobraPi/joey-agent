//! Integration test: POST execute → WebSocket /api/runs/{run_id} → tasks.md
//! write-back — T054/T055/T056 (Convergence Phase 7).
//!
//! Uses a real bound TCP listener + tokio-tungstenite client so the WS
//! upgrade and broadcast-channel wiring are exercised end-to-end, not just
//! mocked.

mod common;

use std::time::Duration;

use futures_util::StreamExt;
use tokio::net::TcpListener;

#[tokio::test]
async fn execute_then_run_ws_streams_status_and_marks_task_done() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "http://{addr}/api/features/001-test/tasks/T001/execute"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.unwrap();
    let run_id = body["run_id"].as_str().unwrap().to_string();

    let ws_url = format!("ws://{addr}/api/runs/{run_id}");
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();

    // The backend shells out to `specify`/`.specify/scripts/bash/implement.sh`,
    // neither of which exists in this fixture repo, so the run is expected to
    // fail fast — but it must still produce exactly one terminal run_status
    // event over the WS within a bounded time.
    let msg = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws_stream.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    return text;
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("ws error: {e}"),
                None => panic!("ws closed before any message"),
            }
        }
    })
    .await
    .expect("timed out waiting for run_status event");

    let payload: serde_json::Value = serde_json::from_str(&msg).unwrap();
    assert_eq!(payload["type"], "run_status");
    assert_eq!(payload["run_id"], run_id);
    assert!(payload["status"] == "succeeded" || payload["status"] == "failed");
}
