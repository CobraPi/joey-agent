//! Regression test: WS feature-id path-traversal guard (watch + meaning
//! stream). The feature id lands in `repo_root.join("specs").join(&id)` —
//! a percent-encoded `..` id must be rejected with 400 `invalid_path`
//! BEFORE the WS upgrade, instead of watching/reading outside the repo.
//!
//! Uses a real bound TCP listener + tokio-tungstenite client (same harness
//! as ws_run_execute.rs): axum 0.7's `WebSocketUpgrade` extractor rejects
//! synthetic oneshot requests with 426 before the handler runs, so the
//! guard can only be exercised by a genuine handshake. `..%2F..%2Fetc`
//! passes client-side URL normalization and route matching as one raw
//! segment, then percent-decodes in `Path<String>` to `../../etc` — the
//! exact input the guard must refuse. A guard rejection surfaces as
//! `connect_async` failing with the server's HTTP error response; a real
//! client always sends correct upgrade headers, so a 400 can only come
//! from the guard (the extractor's own rejections are 426/405).

mod common;

use std::time::Duration;

use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Error as WsError;

/// Serve the router on an ephemeral local port, returning its address.
async fn serve(app: axum::Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// Attempt a real WS handshake; return the server's HTTP rejection status.
/// Panics if the handshake unexpectedly succeeds.
async fn rejected_status(addr: &std::net::SocketAddr, path: &str) -> u16 {
    let url = format!("ws://{addr}{path}");
    let err = match tokio_tungstenite::connect_async(url).await {
        Err(e) => e,
        Ok(_) => panic!("traversal feature id must be rejected before WS upgrade"),
    };
    match err {
        WsError::Http(ref resp) => resp.status().as_u16(),
        other => panic!("expected HTTP rejection, got: {other}"),
    }
}

#[tokio::test]
async fn watch_rejects_traversal_feature_id() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);
    let addr = serve(app).await;

    // `..%2F..%2Fetc` decodes to `../../etc` in the `:id` capture — the
    // guard must answer 400 (invalid_path), never a 101 upgrade.
    let status = rejected_status(&addr, "/api/features/..%2F..%2Fetc/watch").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn meaning_stream_rejects_traversal_feature_id() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);
    let addr = serve(app).await;

    let status = rejected_status(&addr, "/api/features/..%2F..%2Fetc/meaning/stream").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn valid_feature_id_passes_guard_and_upgrades() {
    let dir = common::make_fixture_repo("001-test");
    let app = common::router_for(&dir);
    let addr = serve(app).await;

    // Control: a safe id must clear the guard and complete the upgrade.
    // `meaning_stream_loop` pushes the semantic graph immediately on
    // connect, so receiving `feature_id: 001-test` proves the request got
    // past the guard AND through the 101 upgrade.
    let url = format!("ws://{addr}/api/features/001-test/meaning/stream");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("valid feature id must pass the traversal guard and upgrade");

    let msg = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => return text,
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("ws error: {e}"),
                None => panic!("ws closed before any message"),
            }
        }
    })
    .await
    .expect("timed out waiting for initial semantic graph");

    let payload: serde_json::Value = serde_json::from_str(&msg).unwrap();
    assert_eq!(payload["feature_id"], "001-test");
}
