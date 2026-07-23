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
