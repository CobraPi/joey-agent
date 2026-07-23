//! HTTP + WebSocket API surface.

pub mod rest;
pub mod ws;

use axum::Router;

use crate::AppState;

/// Build the full axum router for the backend, bound by the caller to
/// `127.0.0.1`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .merge(rest::routes())
        .merge(ws::routes())
        .with_state(state)
}
