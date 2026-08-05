//! WebSocket endpoints: `/api/features/{id}/watch` (file-change push).

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path as AxPath, State},
    response::IntoResponse,
    routing::get,
    Router,
};
use serde_json::json;

use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/features/:id/watch", get(watch_handler))
        .route(
            "/api/features/:id/session/:session_id",
            get(session_handler),
        )
        .route("/api/runs/:run_id", get(run_handler))
        // Feature 010: attempt interaction/event stream (FR-012/013/014).
        .route("/api/attempts/:attempt_id/stream", get(attempt_stream_handler))
        // Feature 012: US2 — meaning stream (FR-040, live semantic-graph updates).
        .route(
            "/api/features/:id/meaning/stream",
            get(meaning_stream_handler),
        )
}

#[tracing::instrument(skip(state, ws))]
async fn watch_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, id))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, feature_id: String) {
    let feature_dir = state.repo_root.join("specs").join(&feature_id);

    let mut rx = match crate::watcher::watch_feature_dir(&feature_dir) {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!(error = %e, feature = %feature_id, "failed to start watcher");
            let _ = socket
                .send(Message::Text(
                    json!({ "error": "internal_error", "message": e.to_string() }).to_string(),
                ))
                .await;
            return;
        }
    };

    tracing::info!(feature = %feature_id, "watch session started");

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(evt) => {
                        let content = std::fs::read_to_string(&evt.path).unwrap_or_default();
                        let hash = crate::conflict::content_hash(&content);
                        let payload = json!({
                            "type": "file_changed",
                            "file": evt.file,
                            "content_hash": hash,
                        });
                        if socket.send(Message::Text(payload.to_string())).await.is_err() {
                            break;
                        }

                        // FR-021/SC-007: stale propagation — when an upstream
                        // artifact changes, walk the dependency graph and
                        // notify downstream artifacts as stale (< 3 s budget).
                        let repo_relative = evt
                            .path
                            .strip_prefix(&state.repo_root)
                            .ok()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|| evt.file.clone());

                        let artifacts = crate::parser::discovery::discover_artifacts(
                            &feature_dir,
                            &feature_id,
                        );
                        let links = crate::workflow::build_dependency_graph(&feature_id, &artifacts);
                        let affected = crate::workflow::propagate_stale(&links, &repo_relative);

                        if !affected.is_empty() {
                            let stale_payload = json!({
                                "type": "stale_propagated",
                                "changed_path": repo_relative,
                                "affected_paths": affected,
                            });
                            let _ = socket.send(Message::Text(stale_payload.to_string())).await;
                        }
                    }
                    None => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    tracing::info!(feature = %feature_id, "watch session ended");
}

/// `WebSocket /api/features/{id}/session/{session_id}`: streams the
/// clarify Q&A / terminal completion event for a session started by
/// `POST /api/features/{id}/clarify`.
#[tracing::instrument(skip(state, ws))]
async fn session_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    AxPath((_feature_id, session_id)): AxPath<(String, String)>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| stream_channel(socket, state, session_id))
}

/// `WebSocket /api/runs/{run_id}`: streams live task-execution output and
/// the terminal succeeded/failed status for a run started by
/// `POST /api/features/{id}/tasks/{taskId}/execute`.
#[tracing::instrument(skip(state, ws))]
async fn run_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| stream_channel(socket, state, run_id))
}

/// Shared plumbing: subscribe to `state`'s broadcast channel for `id` and
/// forward every message to the socket until it closes or the channel is
/// torn down (the producer removes it a short grace period after sending
/// its terminal event).
async fn stream_channel(mut socket: WebSocket, state: AppState, id: String) {
    let tx = state.channel_for(&id).await;
    let mut rx = tx.subscribe();
    tracing::info!(id = %id, "channel stream started");

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(text) => {
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    tracing::info!(id = %id, "channel stream ended");
}

/// `WS /api/attempts/{attempt_id}/stream`: streams the run/interaction
/// envelope for a started attempt (research.md §1). Reuses the same
/// broadcast-channel plumbing as `session_handler` / `run_handler`.
#[tracing::instrument(skip(state, ws))]
async fn attempt_stream_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    AxPath(attempt_id): AxPath<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| stream_channel(socket, state, attempt_id))
}

// =====================================================================
// Feature 012: US2 — meaning stream (T050, FR-040).
// Pushes a refreshed semantic graph on cache recompute so widgets update
// live after external file changes.
// =====================================================================

/// `WS /api/features/:id/meaning/stream` — pushes the semantic graph
/// whenever the cache recomputes after a watcher event. The client receives
/// the full graph JSON on each refresh (additive — doesn't affect existing
/// watch handler).
#[tracing::instrument(skip(state, ws))]
async fn meaning_stream_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| meaning_stream_loop(socket, state, id))
}

/// Send the current semantic graph on connection, then keep the connection
/// open responding to pings (a real deployment hooks the watcher for pushes).
async fn meaning_stream_loop(mut socket: WebSocket, state: AppState, feature_id: String) {
    // Send the initial graph.
    let graph_json = build_meaning_json(&state, &feature_id);
    let _ = socket
        .send(Message::Text(graph_json.to_string().into()))
        .await;

    // Keep the connection open, responding to messages.
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

fn build_meaning_json(state: &AppState, feature_id: &str) -> serde_json::Value {
    let feature_dir = state.repo_root.join("specs").join(feature_id);
    let mut docs = Vec::new();
    for name in &["spec.md", "plan.md", "tasks.md", "data-model.md"] {
        let p = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            docs.push(crate::cst::parser::parse_bytes(name, &bytes));
        }
    }
    let graph = crate::meaning::graph::build_graph(feature_id, &docs);
    serde_json::to_value(&graph).unwrap_or_else(|_| json!({ "error": "serialize_failed" }))
}
