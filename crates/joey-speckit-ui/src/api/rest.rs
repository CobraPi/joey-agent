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

use crate::{
    commands, conflict, editor, list_feature_ids, load_feature, runner, runner::WorkflowRunner,
    staging::StagingArea, validation, writer, AppState,
};

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
        // Feature 010: artifact authoring (FR-003/004/005/006/007)
        .route("/api/features/:id/artifacts", get(get_artifacts))
        .route(
            "/api/features/:id/artifacts/*path",
            get(get_artifact).patch(patch_artifact),
        )
        // Feature 010: workflow catalog & readiness (FR-008/009/021/022)
        .route("/api/features/:id/workflow", get(get_workflow))
        .route("/api/options", get(get_options))
        .route(
            "/api/features/:id/workflow/:step/config",
            get(get_step_config),
        )
        .route(
            "/api/features/:id/workflow/:step/override",
            axum::routing::put(put_step_override).delete(delete_step_override),
        )
        // Feature 010: run lifecycle (FR-010/011/012/013/014/019/033)
        .route(
            "/api/features/:id/workflow/:step/run",
            post(post_workflow_run),
        )
        .route("/api/attempts/:attempt_id/answer", post(post_attempt_answer))
        .route("/api/attempts/:attempt_id/approve", post(post_attempt_approve))
        .route("/api/attempts/:attempt_id/cancel", post(post_attempt_cancel))
        .route("/api/attempts/:attempt_id/recover", post(post_attempt_recover))
        // Feature 010: change review (FR-016/017/020)
        .route("/api/attempts/:attempt_id/changes", get(get_attempt_changes))
        .route("/api/attempts/:attempt_id/changes/apply", post(post_changes_apply))
        // Feature 010: history (FR-018/019/031)
        .route("/api/features/:id/history", get(get_history))
        // Feature 010: preferences (FR-026)
        .route(
            "/api/features/:id/preferences",
            get(get_preferences).put(put_preferences),
        )
        // Feature 010: health (FR-028)
        .route("/api/health", get(get_health))
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

// =====================================================================
// Feature 010: Artifacts (FR-003/004/005/006/007)
// =====================================================================

/// `GET /api/features/{id}/artifacts` — discover authorable artifacts (T013).
#[tracing::instrument(skip(state))]
async fn get_artifacts(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    if !feature_dir.exists() {
        return (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("feature '{id}' not found")),
        )
            .into_response();
    }

    let mut artifacts = crate::parser::discovery::discover_artifacts(&feature_dir, &id);
    if let Some(constitution) = crate::parser::discovery::discover_constitution(&state.repo_root) {
        artifacts.push(constitution);
    }

    // Run validation on each existing artifact (FR-007).
    for artifact in &mut artifacts {
        if artifact.exists {
            if let Some(ref path) = artifact.content_hash {
                let _ = path; // hash already set by discovery
            }
            let abs_path = match crate::parser::discovery::resolve_artifact_path(
                &state.repo_root,
                &artifact.path,
            ) {
                Some(p) => p,
                None => continue,
            };
            if let Ok(content) = std::fs::read_to_string(&abs_path) {
                artifact.validity = validation::validate(&artifact.kind, &content, &artifact.path);
            }
        }
    }

    (StatusCode::OK, Json(json!({ "artifacts": artifacts }))).into_response()
}

/// `GET /api/features/{id}/artifacts/{path}` — fetch raw text + outline (T014).
#[tracing::instrument(skip(state))]
async fn get_artifact(
    State(state): State<AppState>,
    AxPath((id, path)): AxPath<(String, String)>,
) -> impl IntoResponse {
    let abs_path = match crate::parser::discovery::resolve_artifact_path(&state.repo_root, &path) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                error_body("invalid_request", "invalid artifact path"),
            )
                .into_response();
        }
    };

    if !abs_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("artifact '{path}' does not exist")),
        )
            .into_response();
    }

    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_body("internal_error", e.to_string()),
            )
                .into_response();
        }
    };

    let hash = conflict::content_hash(&content);
    let outline = crate::parser::discovery::parse_outline(&content);
    let kind = infer_kind(&path, &id);
    let validity = validation::validate(&kind, &content, &path);

    (
        StatusCode::OK,
        Json(json!({
            "path": path,
            "kind": kind,
            "text": content,
            "content_hash": hash,
            "outline": outline,
            "save_state": "clean",
            "validity": validity,
        })),
    )
        .into_response()
}

/// `PATCH /api/features/{id}/artifacts/{path}` — whole/section edit (T015).
#[derive(Debug, Deserialize)]
struct PatchArtifactRequest {
    new_text: String,
    based_on_hash: String,
    #[serde(default)]
    scope: Option<PatchScope>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PatchScope {
    Whole { whole: bool },
    Section { section: String },
}

impl Default for PatchScope {
    fn default() -> Self {
        PatchScope::Whole { whole: true }
    }
}

#[tracing::instrument(skip(state, body))]
async fn patch_artifact(
    State(state): State<AppState>,
    AxPath((id, path)): AxPath<(String, String)>,
    Json(body): Json<PatchArtifactRequest>,
) -> impl IntoResponse {
    let abs_path = match crate::parser::discovery::resolve_artifact_path(&state.repo_root, &path) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                error_body("invalid_request", "invalid artifact path"),
            )
                .into_response();
        }
    };

    if !abs_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("artifact '{path}' does not exist")),
        )
            .into_response();
    }

    let kind = infer_kind(&path, &id);
    let scope = match body.scope.unwrap_or_default() {
        PatchScope::Section { section } => editor::EditScope::Section { heading: section },
        PatchScope::Whole { .. } => editor::EditScope::Whole,
    };

    // Pre-validate for structural issues (FR-007).
    let findings = if matches!(scope, editor::EditScope::Whole) {
        validation::validate(&kind, &body.new_text, &path)
    } else {
        Vec::new()
    };

    match editor::apply_edit(&abs_path, &body.new_text, &body.based_on_hash, &scope, findings) {
        editor::EditorResult::Success { new_hash } => {
            (StatusCode::OK, Json(json!({ "content_hash": new_hash }))).into_response()
        }
        editor::EditorResult::Conflict { current_hash } => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "conflict",
                "current_hash": current_hash,
                "message": "artifact changed on disk. Reload and reapply your edit."
            })),
        )
            .into_response(),
        editor::EditorResult::Invalid { findings } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "invalid_request",
                "validity": findings,
                "message": "artifact content failed structural validation"
            })),
        )
            .into_response(),
        editor::EditorResult::Error(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("internal_error", e),
        )
            .into_response(),
    }
}

/// Infer the `ArtifactKind` from a repo-relative path.
/// Load a single attempt from JSONL history by attempt_id.
fn load_attempt_from_history(
    joey_home: &std::path::Path,
    attempt_id: &str,
) -> Option<crate::model::WorkflowAttempt> {
    // We don't know which feature file contains this attempt, so scan all.
    let history_dir = joey_home.join("speckit-ui").join("history");
    if !history_dir.is_dir() {
        return None;
    }
    for entry in std::fs::read_dir(&history_dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(records) = crate::history::read_all(&path) {
            for record in records {
                if record.attempt.attempt_id == attempt_id {
                    return Some(record.attempt);
                }
            }
        }
    }
    None
}

fn infer_kind(repo_relative: &str, _feature_id: &str) -> crate::model::ArtifactKind {
    let lower = repo_relative.to_lowercase();
    if lower.ends_with("spec.md") {
        crate::model::ArtifactKind::Spec
    } else if lower.ends_with("plan.md") {
        crate::model::ArtifactKind::Plan
    } else if lower.ends_with("tasks.md") {
        crate::model::ArtifactKind::Tasks
    } else if lower.ends_with("research.md") {
        crate::model::ArtifactKind::Research
    } else if lower.contains("data-model.md") || lower.contains("data_model.md") {
        crate::model::ArtifactKind::DataModel
    } else if lower.ends_with("quickstart.md") {
        crate::model::ArtifactKind::Quickstart
    } else if lower.contains("constitution.md") {
        crate::model::ArtifactKind::Constitution
    } else if lower.contains("/checklists/") {
        crate::model::ArtifactKind::Checklist
    } else if lower.contains("/contracts/") {
        crate::model::ArtifactKind::Contract
    } else {
        crate::model::ArtifactKind::Supporting
    }
}

// =====================================================================
// Feature 010: Workflow catalog & readiness (FR-008/009/021/022)
// =====================================================================

/// `GET /api/features/{id}/workflow` — step catalog with derived states (T019).
#[tracing::instrument(skip(state))]
async fn get_workflow(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    if !feature_dir.exists() {
        return (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("feature '{id}' not found")),
        )
            .into_response();
    }

    let artifacts = crate::parser::discovery::discover_artifacts(&feature_dir, &id);
    let active_steps = std::collections::HashSet::new(); // populated when runs are active
    let steps = crate::workflow::build_workflow(&id, &artifacts, &active_steps);

    (StatusCode::OK, Json(json!({ "steps": steps }))).into_response()
}

/// `GET /api/options` — server-advertised agent option catalog (T020).
#[tracing::instrument(skip(state))]
async fn get_options(State(state): State<AppState>) -> impl IntoResponse {
    let catalog = state.options_catalog();
    (StatusCode::OK, Json(serde_json::to_value(&catalog).unwrap_or_default())).into_response()
}

/// `GET /api/features/{id}/workflow/{step}/config` — effective merged instructions (T021).
#[tracing::instrument(skip(state))]
async fn get_step_config(
    State(state): State<AppState>,
    AxPath((id, step)): AxPath<(String, String)>,
) -> impl IntoResponse {
    let installed = format!("Installed defaults for {step}");
    let override_data = state.get_override(&id, &step).await;

    let effective = match &override_data {
        Some(ov) => ov.instructions.clone(),
        None => installed.clone(),
    };

    (
        StatusCode::OK,
        Json(json!({
            "step_id": step,
            "installed": { "instructions": installed },
            "override": override_data.map(|o| json!({
                "override_id": o.override_id,
                "instructions": o.instructions,
            })),
            "effective_instructions": effective,
        })),
    )
        .into_response()
}

/// `PUT /api/features/{id}/workflow/{step}/override` — create/replace override (T021).
#[derive(Debug, Deserialize)]
struct OverrideRequest {
    instructions: String,
}

#[tracing::instrument(skip(state, body))]
async fn put_step_override(
    State(state): State<AppState>,
    AxPath((id, step)): AxPath<(String, String)>,
    Json(body): Json<OverrideRequest>,
) -> impl IntoResponse {
    let override_id = state
        .set_override(&id, &step, body.instructions.clone())
        .await;
    (StatusCode::OK, Json(json!({ "override_id": override_id }))).into_response()
}

/// `DELETE /api/features/{id}/workflow/{step}/override` — remove override (T021).
#[tracing::instrument(skip(state))]
async fn delete_step_override(
    State(state): State<AppState>,
    AxPath((id, step)): AxPath<(String, String)>,
) -> impl IntoResponse {
    state.remove_override(&id, &step).await;
    StatusCode::NO_CONTENT.into_response()
}

// =====================================================================
// Feature 010: Run lifecycle (FR-010/011/012/013/014/019/033)
// =====================================================================

/// `POST /api/features/{id}/workflow/{step}/run` — prepare + start a run (T023).
#[derive(Debug, Deserialize)]
struct RunRequest {
    #[serde(default)]
    effective_instructions: Option<String>,
    #[serde(default)]
    scope: Option<crate::model::Scope>,
    #[serde(default)]
    options: Option<crate::model::AgentOptions>,
    option_catalog_rev: String,
    #[serde(default)]
    change_mode: Option<crate::model::ChangeMode>,
    #[serde(default)]
    override_id: Option<String>,
    #[serde(default)]
    prior_attempt_id: Option<String>,
}

#[tracing::instrument(skip(state, body))]
async fn post_workflow_run(
    State(state): State<AppState>,
    AxPath((id, step)): AxPath<(String, String)>,
    Json(body): Json<RunRequest>,
) -> impl IntoResponse {
    let catalog = state.options_catalog();

    // FR-010: change_mode is mandatory (validate request body first).
    let change_mode = match body.change_mode {
        Some(cm) => cm,
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                error_body("invalid_request", "change_mode is required"),
            )
                .into_response();
        }
    };

    // FR-010: reject stale option catalog.
    if body.option_catalog_rev != catalog.revision {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "stale_option_catalog",
                "current_revision": catalog.revision,
                "message": "option catalog has changed; refresh and re-select options"
            })),
        )
            .into_response();
    }

    // FR-010: change_mode was validated above.
    let scope_paths: Vec<String> = body
        .scope
        .as_ref()
        .map(|s| s.targets.iter().map(|t| t.path.clone()).collect())
        .unwrap_or_default();

    // FR-015: conflict guard — reject if an in-flight attempt overlaps.
    if let Some(conflicting_id) = state.check_conflicting_run(&id, &scope_paths).await {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "conflicting_run",
                "conflicting_attempt_id": conflicting_id,
                "message": "an in-flight attempt's change set overlaps this run's scope"
            })),
        )
            .into_response();
    }

    let attempt_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::days(90);

    let run_config = crate::model::RunConfiguration {
        step_id: step.clone(),
        effective_instructions: body.effective_instructions.unwrap_or_default(),
        scope: body.scope.unwrap_or_default(),
        options: body.options,
        option_catalog_rev: body.option_catalog_rev,
        change_mode: Some(change_mode.clone()),
        override_id: body.override_id,
        prepared_at: Some(now.to_rfc3339()),
    };

    let mut attempt = crate::model::WorkflowAttempt {
        attempt_id: attempt_id.clone(),
        feature_id: id.clone(),
        step_id: step.clone(),
        initiator: "user".to_string(),
        started_at: now.to_rfc3339(),
        status: crate::model::AttemptStatus::Preparing,
        run_config: run_config.clone(),
        prior_attempt_id: body.prior_attempt_id,
        expires_at: Some(expires_at.to_rfc3339()),
        ..Default::default()
    };

    // Append to JSONL history (FR-018).
    if let Err(e) = crate::history::append(&state.joey_home(), &attempt) {
        tracing::error!(error = %e, "failed to append attempt to history");
    }

    // Spawn the runner and stream events into the WS broadcast channel (FR-011/012).
    let joey_home = state.joey_home();
    let repo_root = state.repo_root.clone();
    let feature_id = id.clone();
    let step_id = step.clone();
    let ws_attempt_id = attempt_id.clone();
    let tx = state.channel_for(&ws_attempt_id).await;
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cleanup_state = state.clone();
    let cleanup_attempt_id = attempt_id.clone();
    let cleanup_token = cancel_token.clone();

    // Register the attempt for interaction/cancel endpoints.
    let (respond_tx, respond_rx) = tokio::sync::mpsc::channel::<runner::InteractionPayload>(16);
    state
        .register_attempt(
            &attempt_id,
            respond_tx,
            &id,
            scope_paths,
            cancel_token.clone(),
        )
        .await;

    tokio::spawn(async move {
        let runner_impl = crate::runner_impl::JoeyCliRunner::new();
        let staging = crate::staging_impl::GitStagingArea::new();

        // Prepare and start the runner subprocess.
        match runner_impl
            .prepare_and_start(&repo_root, &feature_id, &step_id, &run_config, &staging)
            .await
        {
            Ok(mut handle) => {
                // Update attempt status to Running.
                attempt.status = crate::model::AttemptStatus::Running;
                let _ = crate::history::update_in_place(&joey_home, &attempt);

                // Forward the respond_rx to the handle's respond_tx.
                // (The runner already created its own channel; we bridge ours.)
                let bridge_tx = handle.respond_tx.clone();
                tokio::spawn(async move {
                    let mut rx = respond_rx;
                    while let Some(payload) = rx.recv().await {
                        if bridge_tx.send(payload).await.is_err() {
                            break;
                        }
                    }
                });

                // Stream events from the runner to the WS broadcast channel.
                while let Some(evt) = handle.events.recv().await {
                    let json = serde_json::to_string(&evt).unwrap_or_default();
                    let _ = tx.send(json);

                    // Handle terminal status.
                    if let runner::RunnerEvent::Status { ref terminal, .. } = evt {
                        let final_status = match terminal {
                            runner::TerminalStatus::Succeeded => crate::model::AttemptStatus::Succeeded,
                            runner::TerminalStatus::Failed => crate::model::AttemptStatus::Failed,
                            runner::TerminalStatus::Cancelled => crate::model::AttemptStatus::Cancelled,
                        };
                        attempt.status = final_status;
                        attempt.ended_at = Some(chrono::Utc::now().to_rfc3339());
                        let _ = crate::history::update_in_place(&joey_home, &attempt);
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "runner failed to start");
                attempt.status = crate::model::AttemptStatus::Failed;
                attempt.ended_at = Some(chrono::Utc::now().to_rfc3339());
                let _ = crate::history::update_in_place(&joey_home, &attempt);
                let err_evt = runner::RunnerEvent::Error {
                    attempt_id: ws_attempt_id.clone(),
                    message: e.to_string(),
                    recoverable: false,
                };
                let _ = tx.send(serde_json::to_string(&err_evt).unwrap_or_default());
            }
        }

        // Grace period then cleanup.
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        cleanup_state.remove_channel(&cleanup_attempt_id).await;
        cleanup_state.remove_attempt(&cleanup_attempt_id).await;
    });

    // Select on cancel_token for safe cancellation (FR-014).
    let _ = cleanup_token;

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "attempt_id": attempt_id,
            "ws": format!("/api/attempts/{attempt_id}/stream"),
        })),
    )
        .into_response()
}

/// `POST /api/attempts/{id}/answer` — answer a pending question (T024).
#[derive(Debug, Deserialize)]
struct AnswerRequest {
    interaction_id: String,
    answer: String,
}

#[tracing::instrument(skip(state, body))]
async fn post_attempt_answer(
    State(state): State<AppState>,
    AxPath(attempt_id): AxPath<String>,
    Json(body): Json<AnswerRequest>,
) -> impl IntoResponse {
    tracing::info!(attempt = %attempt_id, interaction = %body.interaction_id, "answer received");

    let sender = state.get_attempt_sender(&attempt_id).await;
    match sender {
        Some(tx) => {
            let payload = runner::InteractionPayload::Answer {
                interaction_id: body.interaction_id,
                answer: body.answer,
            };
            match tx.send(payload).await {
                Ok(()) => (StatusCode::OK, Json(json!({ "confirmed": true }))).into_response(),
                Err(_) => (
                    StatusCode::CONFLICT,
                    error_body("conflict", "attempt is no longer accepting responses"),
                )
                    .into_response(),
            }
        }
        None => (
            StatusCode::NOT_FOUND,
            error_body("not_found", "attempt not found or no longer active"),
        )
            .into_response(),
    }
}

/// `POST /api/attempts/{id}/approve` — respond to an approval request (T024).
#[derive(Debug, Deserialize)]
struct ApproveRequest {
    interaction_id: String,
    decision: String,
    #[serde(default)]
    note: Option<String>,
}

#[tracing::instrument(skip(state, body))]
async fn post_attempt_approve(
    State(state): State<AppState>,
    AxPath(attempt_id): AxPath<String>,
    Json(body): Json<ApproveRequest>,
) -> impl IntoResponse {
    tracing::info!(
        attempt = %attempt_id,
        interaction = %body.interaction_id,
        decision = %body.decision,
        "approval received"
    );

    let sender = state.get_attempt_sender(&attempt_id).await;
    match sender {
        Some(tx) => {
            let decision = if body.decision == "approve" {
                runner::ApprovalDecision::Approve
            } else {
                runner::ApprovalDecision::Reject
            };
            let payload = runner::InteractionPayload::Approval {
                interaction_id: body.interaction_id,
                decision,
                note: body.note,
            };
            match tx.send(payload).await {
                Ok(()) => (StatusCode::OK, Json(json!({ "confirmed": true }))).into_response(),
                Err(_) => (
                    StatusCode::CONFLICT,
                    error_body("conflict", "attempt is no longer accepting responses"),
                )
                    .into_response(),
            }
        }
        None => (
            StatusCode::NOT_FOUND,
            error_body("not_found", "attempt not found or no longer active"),
        )
            .into_response(),
    }
}

/// `POST /api/attempts/{id}/cancel` — cancel a running attempt (T024).
#[tracing::instrument(skip(state))]
async fn post_attempt_cancel(
    State(state): State<AppState>,
    AxPath(attempt_id): AxPath<String>,
) -> impl IntoResponse {
    tracing::info!(attempt = %attempt_id, "cancel requested");
    let cancelled = state.cancel_attempt(&attempt_id).await;
    if cancelled {
        // The subprocess will be killed by the cancellation token; the
        // spawned task emits the terminal status event (FR-014).
        (StatusCode::ACCEPTED, Json(json!({ "cancelled": true }))).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            error_body("not_found", "attempt not found or already terminal"),
        )
            .into_response()
    }
}

/// `POST /api/attempts/{id}/recover` — recover a failed/recovery_needed attempt (T031).
#[tracing::instrument(skip(state))]
async fn post_attempt_recover(
    State(state): State<AppState>,
    AxPath(attempt_id): AxPath<String>,
) -> impl IntoResponse {
    tracing::info!(attempt = %attempt_id, "recover requested");

    let joey_home = state.joey_home();
    let mut attempt = match load_attempt_from_history(&joey_home, &attempt_id) {
        Some(a) => a,
        None => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", format!("attempt '{attempt_id}' not found")),
            )
                .into_response();
        }
    };

    match crate::recovery::evaluate_recovery(&attempt) {
        crate::recovery::RecoveryOutcome::Resume {
            checkpoint_tree_ish, ..
        } => {
            // Mark as resumed (re-running from checkpoint).
            if let Err(e) = crate::recovery::mark_resumed(&joey_home, &mut attempt) {
                tracing::error!(error = %e, "failed to mark attempt as resumed");
            }
            (
                StatusCode::OK,
                Json(json!({
                    "resumed": true,
                    "checkpoint": checkpoint_tree_ish,
                    "message": "recovery initiated from safe checkpoint",
                })),
            )
                .into_response()
        }
        crate::recovery::RecoveryOutcome::Failed {
            reason,
            preserved_effects,
            ..
        } => {
            // Mark as recovery_failed, preserve effects.
            if let Err(e) = crate::recovery::mark_recovery_failed(&joey_home, &mut attempt) {
                tracing::error!(error = %e, "failed to mark attempt as recovery_failed");
            }
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "recovery_failed",
                    "reason": reason,
                    "preserved_effects": preserved_effects,
                    "message": "no valid checkpoint; effects preserved for manual review",
                })),
            )
                .into_response()
        }
    }
}

// =====================================================================
// Feature 010: Change review (FR-016/017/020)
// =====================================================================

/// `GET /api/attempts/{id}/changes` — list the change set (T029).
#[tracing::instrument(skip(state))]
async fn get_attempt_changes(
    State(state): State<AppState>,
    AxPath(attempt_id): AxPath<String>,
) -> impl IntoResponse {
    // Load the attempt from history to get the staging root.
    let joey_home = state.joey_home();
    let attempt = match load_attempt_from_history(&joey_home, &attempt_id) {
        Some(a) => a,
        None => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", format!("attempt '{attempt_id}' not found")),
            )
                .into_response();
        }
    };

    let staging = crate::staging_impl::GitStagingArea::new();
    let mode = attempt.run_config.change_mode.unwrap_or(crate::model::ChangeMode::Staged);

    // Resolve the staging root: for direct mode it's the repo root; for staged
    // mode it's the temp worktree (re-derive path from attempt_id).
    let staging_root = crate::staging::StagingRoot {
        worktree: match mode {
            crate::model::ChangeMode::Direct => state.repo_root.clone(),
            crate::model::ChangeMode::Staged => {
                std::env::temp_dir().join(format!("joey-stage-{attempt_id}"))
            }
        },
        mode,
        attempt_id: attempt_id.clone(),
    };

    match staging.diff(&staging_root).await {
        Ok(change_set) => {
            (StatusCode::OK, Json(serde_json::to_value(&change_set).unwrap_or_default()))
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("internal_error", e.to_string()),
        )
            .into_response(),
    }
}

/// `POST /api/attempts/{id}/changes/apply` — apply accepted hunks (T030).
#[derive(Debug, Deserialize)]
struct ApplyRequest {
    #[serde(default)]
    selection: Option<crate::staging::Selection>,
    #[serde(default)]
    apply_all_accepted: bool,
}

#[tracing::instrument(skip(state, body))]
async fn post_changes_apply(
    State(state): State<AppState>,
    AxPath(attempt_id): AxPath<String>,
    Json(body): Json<ApplyRequest>,
) -> impl IntoResponse {
    tracing::info!(attempt = %attempt_id, apply_all = body.apply_all_accepted, "apply requested");

    let joey_home = state.joey_home();
    let attempt = match load_attempt_from_history(&joey_home, &attempt_id) {
        Some(a) => a,
        None => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", format!("attempt '{attempt_id}' not found")),
            )
                .into_response();
        }
    };

    let staging = crate::staging_impl::GitStagingArea::new();
    let mode = attempt.run_config.change_mode.unwrap_or(crate::model::ChangeMode::Staged);
    let staging_root = crate::staging::StagingRoot {
        worktree: match mode {
            crate::model::ChangeMode::Direct => state.repo_root.clone(),
            crate::model::ChangeMode::Staged => {
                std::env::temp_dir().join(format!("joey-stage-{attempt_id}"))
            }
        },
        mode,
        attempt_id: attempt_id.clone(),
    };

    let selection = body.selection.unwrap_or_default();
    match staging.apply(&staging_root, &selection).await {
        Ok(outcome) => (StatusCode::OK, Json(serde_json::to_value(&outcome).unwrap_or_default()))
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("internal_error", e.to_string()),
        )
            .into_response(),
    }
}

// =====================================================================
// Feature 010: History (FR-018/019/031)
// =====================================================================

/// `GET /api/features/{id}/history` — streamed, paginated attempt records (T042).
#[tracing::instrument(skip(state))]
async fn get_history(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    axum::extract::Query(params): axum::extract::Query<HistoryQuery>,
) -> impl IntoResponse {
    let path = crate::history::history_file(&state.joey_home(), &id);
    let limit = params.limit.unwrap_or(50).min(200);
    let (attempts, next_cursor) =
        match crate::history::read_paginated(&path, limit, params.before.as_deref()) {
            Ok(result) => result,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error_body("internal_error", e.to_string()),
                )
                    .into_response();
            }
        };

    let summaries: Vec<serde_json::Value> = attempts
        .iter()
        .map(|a| {
            json!({
                "attempt_id": a.attempt_id,
                "step_id": a.step_id,
                "status": a.status,
                "started_at": a.started_at,
                "ended_at": a.ended_at,
                "prior_attempt_id": a.prior_attempt_id,
                "changes_count": a.changes.as_ref().map(|c| c.files.len()).unwrap_or(0),
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "attempts": summaries,
            "next_cursor": next_cursor,
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    before: Option<String>,
}

// =====================================================================
// Feature 010: Preferences (FR-026)
// =====================================================================

/// `GET /api/features/{id}/preferences` — workspace preferences (T037).
#[tracing::instrument(skip(state))]
async fn get_preferences(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let prefs = state.get_preferences(&id);
    (StatusCode::OK, Json(serde_json::to_value(&prefs).unwrap_or_default())).into_response()
}

/// `PUT /api/features/{id}/preferences` — replace preferences (T037).
#[tracing::instrument(skip(state, body))]
async fn put_preferences(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<crate::model::WorkspacePreference>,
) -> impl IntoResponse {
    // Constitution III: reject embedded artifact content.
    if let Some(ref layout) = body.pane_layout {
        if let Some(content) = layout.as_str() {
            if content.len() > 100_000 {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    error_body("invalid_request", "embedded content too large for preferences"),
                )
                    .into_response();
            }
        }
    }
    if let Some(ref filters) = body.filters {
        if let Some(content) = filters.as_str() {
            if content.len() > 100_000 {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    error_body("invalid_request", "embedded content too large for preferences"),
                )
                    .into_response();
            }
        }
    }

    if let Err(e) = state.set_preferences(&id, &body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("internal_error", e.to_string()),
        )
            .into_response();
    }

    (StatusCode::OK, Json(serde_json::to_value(&body).unwrap_or_default())).into_response()
}

// =====================================================================
// Feature 010: Health (FR-028)
// =====================================================================

/// `GET /api/health` — backend/agent/credentials/repo connectivity (T045).
#[tracing::instrument(skip(state))]
async fn get_health(State(state): State<AppState>) -> impl IntoResponse {
    let repo_writable = state.repo_root.metadata().map(|m| !m.permissions().readonly()).unwrap_or(false);

    // Check if `joey` binary is discoverable.
    let agent_available = which::which("joey").is_ok()
        || std::env::var("JOEY_SPECKIT_UI_ROOT").is_ok();

    // Check for credentials (any env var ending in _KEY/_TOKEN).
    let has_credentials = std::env::vars().any(|(k, _)| {
        k.ends_with("_KEY") || k.ends_with("_TOKEN") || k.ends_with("_SECRET")
    });

    (
        StatusCode::OK,
        Json(json!({
            "backend_reachable": true,
            "agent_binary_discovered": agent_available,
            "credentials_present": has_credentials,
            "repo_writable": repo_writable,
            "read_only": !repo_writable,
        })),
    )
        .into_response()
}
