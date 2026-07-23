//! REST endpoints per `contracts/speckit-ui-api.md`.

use axum::{
    extract::{Path as AxPath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{commands, conflict, list_feature_ids, load_feature, writer, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/features", get(list_features))
        .route("/api/features/:id", get(get_feature))
        .route("/api/features/:id/spec", patch(patch_spec))
        .route("/api/features/:id/tasks/:task_id", patch(patch_task))
        .route("/api/features/:id/clarify", post(post_clarify))
        .route(
            "/api/features/:id/clarify/:session_id/answer",
            post(post_clarify_answer),
        )
        .route("/api/features/:id/analyze", post(post_analyze))
        .route(
            "/api/features/:id/tasks/:task_id/execute",
            post(post_task_execute),
        )
        .route("/api/init", post(post_init))
}

/// Shared error body shape: `{ "error": ..., "message": ... }`.
fn error_body(code: &str, message: impl Into<String>) -> Json<serde_json::Value> {
    Json(json!({ "error": code, "message": message.into() }))
}

// ---------------------------------------------------------------------
// GET /api/features
// ---------------------------------------------------------------------

#[tracing::instrument(skip(state))]
async fn list_features(State(state): State<AppState>) -> impl IntoResponse {
    let ids = match list_feature_ids(&state.repo_root) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::error!(error = %e, "failed to list features");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_body("internal_error", e.to_string()),
            )
                .into_response();
        }
    };

    let mut features = Vec::new();
    for id in ids {
        match load_feature(&state.repo_root, &id) {
            Ok(feature) => {
                let title = feature
                    .specification
                    .as_ref()
                    .map(|s| s.title.clone())
                    .unwrap_or_else(|| id.clone());
                let status = feature
                    .specification
                    .as_ref()
                    .map(|s| s.status.clone())
                    .unwrap_or(crate::model::Status::Unparsed);
                features.push(json!({ "id": id, "title": title, "status": status }));
            }
            Err(e) => {
                tracing::warn!(feature = %id, error = %e, "skipping unloadable feature");
            }
        }
    }

    (StatusCode::OK, Json(json!({ "features": features }))).into_response()
}

// ---------------------------------------------------------------------
// GET /api/features/{id}
// ---------------------------------------------------------------------

#[tracing::instrument(skip(state))]
async fn get_feature(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    match load_feature(&state.repo_root, &id) {
        Ok(feature) => (StatusCode::OK, Json(json!(feature))).into_response(),
        Err(e) => {
            tracing::info!(feature = %id, error = %e, "feature not found");
            (
                StatusCode::NOT_FOUND,
                error_body("not_found", format!("feature '{id}' not found")),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------
// PATCH /api/features/{id}/spec
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PatchTarget {
    #[serde(default)]
    #[allow(dead_code)]
    r#type: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
struct PatchSpecRequest {
    target: PatchTarget,
    new_text: String,
    based_on_hash: String,
}

#[tracing::instrument(skip(state, body))]
async fn patch_spec(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<PatchSpecRequest>,
) -> impl IntoResponse {
    let spec_path = state.repo_root.join("specs").join(&id).join("spec.md");
    if !spec_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("spec.md not found for feature '{id}'")),
        )
            .into_response();
    }

    // PATCH /spec applies a targeted single-line replacement identified by
    // `target.id` (e.g. a requirement/user-story id like "FR-012" or "US1"),
    // never a whole-file overwrite — the frontend only ever sends the single
    // changed line as `new_text`. This mirrors `patch_task`'s single-line
    // replace-by-id behavior below.
    let current = std::fs::read_to_string(&spec_path).unwrap_or_default();
    let target_line = current
        .lines()
        .find(|l| l.trim_start().contains(body.target.id.as_str()))
        .map(|l| l.to_string());

    let Some(target_line) = target_line else {
        return (
            StatusCode::NOT_FOUND,
            error_body(
                "not_found",
                format!("target '{}' not found in spec.md", body.target.id),
            ),
        )
            .into_response();
    };

    match writer::replace_line_if_unchanged(
        &spec_path,
        &target_line,
        &body.new_text,
        &body.based_on_hash,
    ) {
        Ok(new_hash) => {
            (StatusCode::OK, Json(json!({ "content_hash": new_hash }))).into_response()
        }
        Err(crate::writer::WriteError::Conflict(conflict::ConflictError::Conflict {
            current_hash,
        })) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "conflict",
                "current_hash": current_hash,
                "message": "spec.md changed on disk. Reload and reapply your edit."
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("internal_error", e.to_string()),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------
// PATCH /api/features/{id}/tasks/{taskId}
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PatchTaskRequest {
    new_text: String,
    based_on_hash: String,
}

#[tracing::instrument(skip(state, body))]
async fn patch_task(
    State(state): State<AppState>,
    AxPath((id, task_id)): AxPath<(String, String)>,
    Json(body): Json<PatchTaskRequest>,
) -> impl IntoResponse {
    let tasks_path = state.repo_root.join("specs").join(&id).join("tasks.md");
    if !tasks_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            error_body(
                "not_found",
                format!("tasks.md not found for feature '{id}'"),
            ),
        )
            .into_response();
    }

    let current = std::fs::read_to_string(&tasks_path).unwrap_or_default();
    let target_line = current
        .lines()
        .find(|l| l.trim_start().contains(task_id.as_str()))
        .map(|l| l.to_string());

    let Some(target_line) = target_line else {
        return (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("task '{task_id}' not found")),
        )
            .into_response();
    };

    match writer::replace_line_if_unchanged(
        &tasks_path,
        &target_line,
        &body.new_text,
        &body.based_on_hash,
    ) {
        Ok(new_hash) => {
            (StatusCode::OK, Json(json!({ "content_hash": new_hash }))).into_response()
        }
        Err(crate::writer::WriteError::Conflict(conflict::ConflictError::Conflict {
            current_hash,
        })) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "conflict",
                "current_hash": current_hash,
                "message": "tasks.md changed on disk. Reload and reapply your edit."
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("internal_error", e.to_string()),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------
// POST /api/features/{id}/clarify (+/answer)
// ---------------------------------------------------------------------

#[tracing::instrument(skip(state))]
async fn post_clarify(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let session_id = uuid::Uuid::new_v4().to_string();
    let repo_root = state.repo_root.clone();
    let feature_id = id.clone();
    let tx = state.channel_for(&session_id).await;
    let state_for_cleanup = state.clone();
    let session_id_for_cleanup = session_id.clone();
    tokio::spawn(async move {
        let result = commands::run_clarify(&repo_root, &feature_id).await;
        match &result {
            Ok(r) => tracing::info!(success = r.success, "clarify run completed"),
            Err(e) => tracing::error!(error = %e, "clarify run failed"),
        }
        let payload = match result {
            Ok(r) => json!({
                "type": "clarify_complete",
                "success": r.success,
                "stdout": r.stdout,
                "stderr": r.stderr,
            }),
            Err(e) => {
                json!({ "type": "clarify_complete", "success": false, "error": e.to_string() })
            }
        };
        let _ = tx.send(payload.to_string());
        // Give any late-connecting subscriber a moment to observe the terminal
        // event before the channel is torn down.
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        state_for_cleanup.remove_channel(&session_id_for_cleanup).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "session_id": session_id })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ClarifyAnswerRequest {
    #[allow(dead_code)]
    answer: String,
}

#[tracing::instrument(skip(state, _body))]
async fn post_clarify_answer(
    State(state): State<AppState>,
    AxPath((id, _session_id)): AxPath<(String, String)>,
    Json(_body): Json<ClarifyAnswerRequest>,
) -> impl IntoResponse {
    let spec_path = state.repo_root.join("specs").join(&id).join("spec.md");
    let hash = if spec_path.exists() {
        std::fs::read_to_string(&spec_path)
            .ok()
            .map(|c| conflict::content_hash(&c))
    } else {
        None
    };

    (
        StatusCode::OK,
        Json(json!({
            "updated_line": "",
            "spec_content_hash": hash,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------
// POST /api/features/{id}/analyze
// ---------------------------------------------------------------------

#[tracing::instrument(skip(state))]
async fn post_analyze(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    match commands::run_analyze(&state.repo_root, &id).await {
        Ok(result) => {
            let findings: Vec<crate::model::AnalysisFinding> = Vec::new();
            let compliance = if result.success { "Pass" } else { "Fail" };
            (
                StatusCode::OK,
                Json(json!({
                    "findings": findings,
                    "constitution_compliance": compliance,
                    "output": result.stdout,
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "analyze failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_body("internal_error", e.to_string()),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------
// POST /api/features/{id}/tasks/{taskId}/execute
// ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ExecuteResponse {
    run_id: String,
}

#[tracing::instrument(skip(state))]
async fn post_task_execute(
    State(state): State<AppState>,
    AxPath((id, task_id)): AxPath<(String, String)>,
) -> impl IntoResponse {
    let run_id = uuid::Uuid::new_v4().to_string();
    let repo_root = state.repo_root.clone();
    let feature_id = id.clone();
    let task = task_id.clone();
    let tx = state.channel_for(&run_id).await;
    let state_for_cleanup = state.clone();
    let run_id_for_cleanup = run_id.clone();
    let run_id_for_task = run_id.clone();

    tokio::spawn(async move {
        let result = commands::run_implement_task(&repo_root, &feature_id, &task).await;
        match &result {
            Ok(r) => {
                tracing::info!(success = r.success, task = %task, "task execute completed")
            }
            Err(e) => tracing::error!(error = %e, task = %task, "task execute failed"),
        }

        let (status, output, error_message) = match &result {
            Ok(r) if r.success => ("succeeded", r.stdout.clone(), None),
            Ok(r) => ("failed", r.stdout.clone(), Some(r.stderr.clone())),
            Err(e) => ("failed", String::new(), Some(e.to_string())),
        };

        // On success, mark the single executed task complete in tasks.md
        // (never cascades to other tasks — Clarifications Q3).
        if status == "succeeded" {
            if let Err(e) = crate::writer::mark_task_complete(&repo_root, &feature_id, &task) {
                tracing::error!(error = %e, task = %task, "failed to write task completion to tasks.md");
            }
        }

        let payload = json!({
            "type": "run_status",
            "run_id": run_id_for_task,
            "status": status,
            "output": output,
            "error": error_message,
        });
        let _ = tx.send(payload.to_string());
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        state_for_cleanup.remove_channel(&run_id_for_cleanup).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!(ExecuteResponse { run_id })),
    )
        .into_response()
}

// ---------------------------------------------------------------------
// POST /api/init
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct InitRequest {
    integration: String,
    script: String,
}

#[tracing::instrument(skip(state))]
async fn post_init(
    State(state): State<AppState>,
    Json(body): Json<InitRequest>,
) -> impl IntoResponse {
    match commands::run_init(&state.repo_root, &body.integration, &body.script).await {
        Ok(result) => (
            StatusCode::OK,
            Json(json!({
                "success": result.success,
                "output": format!("{}{}", result.stdout, result.stderr),
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "init failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_body("internal_error", e.to_string()),
            )
                .into_response()
        }
    }
}
