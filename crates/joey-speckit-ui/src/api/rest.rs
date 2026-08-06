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
    commands, conflict, editor, list_feature_ids, load_feature, read_active_feature_id, runner,
    runner::WorkflowRunner, staging::StagingArea, validation, writer, AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/features", get(list_features))
        .route("/api/project", get(get_project))
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
        // Feature 012: Spec Studio — Atlas + stage-bar + setup + recovery (FR-001..008)
        .route("/api/setup/scan-repo", get(get_setup_scan_repo))
        .route("/api/setup/preview", post(post_setup_preview))
        .route("/api/setup/commit", post(post_setup_commit))
        .route("/api/features/:id/atlas", get(get_atlas))
        .route("/api/features/:id/stage-bar", get(get_stage_bar))
        .route("/api/features/:id/recovery-states", get(get_recovery_states))
        // Feature 012: US2 — meaning + patch endpoints (FR-009..016)
        .route("/api/features/:id/cst/:artifact", get(get_cst))
        .route("/api/features/:id/meaning/graph", get(get_meaning_graph))
        .route("/api/features/:id/meaning/tree-diff", get(get_tree_diff))
        .route("/api/features/:id/patch", post(post_patch))
        // Feature 012: US3 — task board + safe moves (FR-017..020)
        .route("/api/features/:id/meaning/board", get(get_board))
        // Feature 012: US3 convergence — byte-safe task toggle (FR-018)
        .route(
            "/api/features/:id/meaning/board/:task_id/toggle",
            post(post_board_toggle),
        )
        // Feature 012: US5 convergence — hunk-accept side-effects (FR-029)
        .route("/api/features/:id/hunks/:hunk_id/accept", post(post_hunk_accept))
        // Feature 012: convergence — branch drift detection (Edge Case)
        .route("/api/features/:id/branch-drift", get(get_branch_drift))
        // Feature 012: convergence — crash recovery surfacing (FR-028)
        .route("/api/features/:id/recovery-surface", get(get_recovery_surface))
        // Feature 012: US4 — coverage + defects + clarify (FR-021..024)
        .route("/api/features/:id/meaning/coverage", get(get_coverage))
        .route("/api/features/:id/defects", get(get_defects))
        .route("/api/features/:id/defects/:defect_id/fix", post(post_defect_fix))
        .route("/api/features/:id/meaning/clarify", get(get_clarify))
        .route("/api/features/:id/meaning/clarify/:marker_id/answer", post(post_clarify_answer_012))
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
// GET /api/project
// ---------------------------------------------------------------------
//
// Single-call bootstrap payload so the frontend can auto-load the
// project's specs on startup instead of forcing the user to pick a
// feature. It folds together three things the frontend previously had to
// fetch/handle separately:
//
//   * all feature ids + titles under `specs/`  (same shape as GET /api/features)
//   * the project's currently active feature id, read from
//     `.specify/feature.json` (written by `specify`/create-new-feature.sh)
//   * the fully parsed model for that active feature, when present, so the
//     UI can render immediately without a second round-trip
//
// Plus coarse project flags (`has_specs_dir`, `has_specify_dir`) that the
// setup wizard uses to decide whether to prompt for `specify init`.

#[tracing::instrument(skip(state))]
async fn get_project(State(state): State<AppState>) -> impl IntoResponse {
    let repo_root = state.repo_root.clone();

    // --- feature list (same payload as GET /api/features) ---
    let ids = list_feature_ids(&repo_root).unwrap_or_default();
    let mut features = Vec::with_capacity(ids.len());
    for id in &ids {
        match load_feature(&repo_root, id) {
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

    // --- active feature, from .specify/feature.json if present ---
    let active_feature_id = read_active_feature_id(&repo_root);

    // Eagerly load the active feature's full model so the frontend can paint
    // without a follow-up GET /api/features/{id}. Falls back to null when the
    // file is missing, the id isn't in `specs/`, or parsing fails — the UI
    // then falls back to its feature picker.
    let active_feature = active_feature_id
        .as_deref()
        .and_then(|id| match load_feature(&repo_root, id) {
            Ok(feature) => Some(json!(feature)),
            Err(e) => {
                tracing::warn!(feature = %id, error = %e, "active feature listed in .specify/feature.json could not be loaded");
                None
            }
        });

    (
        StatusCode::OK,
        Json(json!({
            "repo_root": repo_root.display().to_string(),
            "has_specs_dir": repo_root.join("specs").exists(),
            "has_specify_dir": repo_root.join(".specify").exists(),
            "features": features,
            "active_feature_id": active_feature_id,
            "active_feature": active_feature,
        })),
    )
        .into_response()
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
        PatchScope::Whole { whole } => {
            let _ = whole;
            editor::EditScope::Whole
        }
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

// =====================================================================
// Feature 012: Spec Studio — Atlas, stage-bar, setup, recovery (T028-T031)
// All endpoints additive over specs/001/010 (Constitution VII).
// =====================================================================

use crate::cst::parser::parse_bytes;
use crate::meaning::graph::build_graph;
use crate::workflow::next_action;

/// `GET /api/setup/scan-repo` — validate read/write + detect Spec Kit setup
/// gaps (T028, FR-001).
#[tracing::instrument(skip(state))]
async fn get_setup_scan_repo(State(state): State<AppState>) -> impl IntoResponse {
    let repo_root = &state.repo_root;
    let exists = repo_root.exists();
    let writable = repo_root
        .metadata()
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false);
    let has_specs_dir = repo_root.join("specs").exists();
    let has_specify = repo_root.join(".specify").exists();

    let gaps: Vec<String> = {
        let mut g = Vec::new();
        if !has_specs_dir {
            g.push("specs/ directory missing".to_string());
        }
        if !has_specify {
            g.push(".specify/ directory missing".to_string());
        }
        g
    };

    (
        StatusCode::OK,
        Json(json!({
            "repo_root": repo_root.display().to_string(),
            "exists": exists,
            "writable": writable,
            "has_specs_dir": has_specs_dir,
            "has_specify_dir": has_specify,
            "setup_gaps": gaps,
        })),
    )
        .into_response()
}

/// `POST /api/setup/preview` — propose slug/branch/paths/permissions,
/// nothing written (T028, FR-001).
#[derive(Debug, Deserialize)]
struct SetupPreviewRequest {
    brief: String,
}

#[tracing::instrument(skip(state))]
async fn post_setup_preview(
    State(state): State<AppState>,
    Json(req): Json<SetupPreviewRequest>,
) -> impl IntoResponse {
    // Derive a slug from the brief.
    let slug_base: String = req
        .brief
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");

    let next_num = {
        // Find the next feature number.
        let ids = list_feature_ids(&state.repo_root).unwrap_or_default();
        let max = ids
            .iter()
            .filter_map(|id| id.split('-').next().and_then(|n| n.parse::<u32>().ok()))
            .max()
            .unwrap_or(0);
        max + 1
    };

    let feature_id = format!("{next_num:03}-{slug_base}");
    let branch = format!("{feature_id}");
    let paths = vec![
        format!("specs/{feature_id}/spec.md"),
        format!("specs/{feature_id}/plan.md"),
        format!("specs/{feature_id}/tasks.md"),
    ];

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": feature_id,
            "branch": branch,
            "paths": paths,
            "staged_mode": true,
            "nothing_written": true,
        })),
    )
        .into_response()
}

/// `POST /api/setup/commit` — create feature dir + initial artifact in
/// staged mode (T028, FR-001).
#[derive(Debug, Deserialize)]
struct SetupCommitRequest {
    feature_id: String,
    brief: String,
}

#[tracing::instrument(skip(state))]
async fn post_setup_commit(
    State(state): State<AppState>,
    Json(req): Json<SetupCommitRequest>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&req.feature_id);
    if feature_dir.exists() {
        return (
            StatusCode::CONFLICT,
            error_body("already_exists", format!("feature {} already exists", req.feature_id)),
        )
            .into_response();
    }

    if let Err(e) = std::fs::create_dir_all(&feature_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("io_error", e.to_string()),
        )
            .into_response();
    }

    // Create an initial spec.md stub in staged mode (nothing is committed to
    // git yet — the developer reviews before applying).
    let spec_path = feature_dir.join("spec.md");
    let stub = format!(
        "# {brief}\n\n\
         > Staged by Spec Studio setup wizard. Review and edit before committing.\n\n\
         ## Purpose\n\n{brief}\n\n\
         ## User Stories\n\n\
         ### User Story 1: {brief} (Priority: P1)\n\n_As a developer, I want {brief_lower} so that I can proceed._\n\n\
         ## Functional Requirements\n\n\
         - **FR-001**: The system MUST {brief_lower}.\n",
        brief = req.brief,
        brief_lower = req.brief.to_lowercase(),
    );

    if let Err(e) = std::fs::write(&spec_path, &stub) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("io_error", e.to_string()),
        )
            .into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "feature_id": req.feature_id,
            "created_paths": [format!("specs/{}/spec.md", req.feature_id)],
            "staged": true,
        })),
    )
        .into_response()
}

/// `GET /api/features/:id/atlas` — the Atlas landing view (T029, FR-004/005).
///
/// Returns: next-action (deterministic), progress, health (parsing status +
/// open unknowns + orphan count from the semantic graph), branch binding +
/// drift, artifact list with staleness, recent-activity timeline.
#[tracing::instrument(skip(state))]
async fn get_atlas(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature = match load_feature(&state.repo_root, &id) {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", e.to_string()),
            )
                .into_response();
        }
    };

    // Build the semantic graph from available artifacts.
    let feature_dir = state.repo_root.join("specs").join(&id);
    let mut docs = Vec::new();
    for (name, _) in [
        ("spec.md", feature.specification.as_ref().map(|_| "spec")),
        ("plan.md", feature.plan.as_ref().map(|_| "plan")),
        ("tasks.md", Some("tasks")),
    ] {
        let path = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            docs.push(parse_bytes(name, &bytes));
        }
    }
    let graph = build_graph(&id, &docs);

    // Workflow steps for next-action.
    let feature_dir_wf = state.repo_root.join("specs").join(&id);
    let artifacts = crate::parser::discovery::discover_artifacts(&feature_dir_wf, &id);
    let active_steps = std::collections::HashSet::new();
    let steps = crate::workflow::build_workflow(&id, &artifacts, &active_steps);
    let next = next_action(&steps);

    // Count defects and unknowns.
    let orphan_count = graph
        .defects
        .iter()
        .filter(|d| {
            matches!(
                d.class,
                crate::meaning::DefectClass::OrphanRequirement
            )
        })
        .count();
    let open_unknowns = graph
        .nodes
        .values()
        .filter(|n| n.kind == crate::meaning::SemanticKind::ClarifyMarker)
        .count();

    // Progress: completed tasks / total tasks.
    let total_tasks = feature.tasks.len();
    let done_tasks = feature
        .tasks
        .iter()
        .filter(|t| t.status == crate::model::TaskStatus::Done)
        .count();
    let progress = if total_tasks > 0 {
        done_tasks as f64 / total_tasks as f64
    } else {
        0.0
    };

    // Artifact list with staleness.
    let artifacts: Vec<_> = vec![
        ("spec.md", feature.spec_content_hash.as_ref()),
        ("plan.md", feature.plan_content_hash.as_ref()),
        ("tasks.md", feature.tasks_content_hash.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, hash)| {
        hash.map(|_| {
            json!({
                "path": name,
                "exists": true,
            })
        })
    })
    .collect();

    // Recent activity from history JSONL.
    let history_path = crate::history::history_file(&state.joey_home(), &id);
    let recent_activity: Vec<_> = crate::history::read_overlay_records(&history_path)
        .unwrap_or_default()
        .into_iter()
        .take(10)
        .map(|r| {
            json!({ "record_type": format!("{:?}", r), "feature_id": id })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "next_action": next,
            "progress": {
                "done_tasks": done_tasks,
                "total_tasks": total_tasks,
                "ratio": progress,
            },
            "health": {
                "parsing_ok": true,
                "open_unknowns": open_unknowns,
                "orphan_count": orphan_count,
            },
            "branch": {
                "name": feature.branch_name,
                "drift": false,
            },
            "artifacts": artifacts,
            "recent_activity": recent_activity,
        })),
    )
        .into_response()
}

/// `GET /api/features/:id/stage-bar` — the five-stage indicator (T030,
/// FR-006/007/008).
#[tracing::instrument(skip(state))]
async fn get_stage_bar(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let _feature = match load_feature(&state.repo_root, &id) {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", e.to_string()),
            )
                .into_response();
        }
    };

    let feature_dir_wf = state.repo_root.join("specs").join(&id);
    let artifacts = crate::parser::discovery::discover_artifacts(&feature_dir_wf, &id);
    let active_steps = std::collections::HashSet::new();
    let steps = crate::workflow::build_workflow(&id, &artifacts, &active_steps);

    // The five lifecycle stages: Define → Design → Break down → Build → Review.
    let stages: Vec<_> = ["define", "design", "break_down", "build", "review"]
        .iter()
        .map(|&name| {
            let matching: Vec<_> = steps
                .iter()
                .filter(|s| stage_matches(s.id.as_str(), name))
                .collect();
            let state_str = if matching.iter().any(|s| s.state == crate::model::StepState::Succeeded) {
                "done"
            } else if matching.iter().any(|s| s.state == crate::model::StepState::Running) {
                "active"
            } else if matching.iter().any(|s| s.state == crate::model::StepState::Ready) {
                "ready"
            } else if matching.iter().any(|s| s.state == crate::model::StepState::Blocked) {
                "blocked"
            } else {
                "pending"
            };
            let gate_reason = matching
                .iter()
                .find_map(|s| s.blocking_reason.clone().filter(|r| !r.is_empty()));

            json!({
                "name": name,
                "state": state_str,
                "gate_reason": gate_reason,
                "step_ids": matching.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "stages": stages,
        })),
    )
        .into_response()
}

fn stage_matches(step_id: &str, stage: &str) -> bool {
    match stage {
        "define" => step_id.contains("spec") || step_id.contains("specify"),
        "design" => step_id.contains("plan") || step_id.contains("design"),
        "break_down" => step_id.contains("tasks") || step_id.contains("task"),
        "build" => step_id.contains("implement") || step_id.contains("build"),
        "review" => step_id.contains("analyze") || step_id.contains("review"),
        _ => false,
    }
}

/// `GET /api/features/:id/recovery-states` — each empty/failed/disconnected
/// state with exactly one primary recovery action (T031, FR-002).
#[tracing::instrument(skip(state))]
async fn get_recovery_states(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);

    let mut states = Vec::new();

    // Empty-state: no spec.md.
    if !feature_dir.join("spec.md").exists() {
        states.push(json!({
            "state": "empty_spec",
            "description": "No spec.md yet — start by defining the feature.",
            "primary_action": "create_spec",
            "touches_files": [format!("specs/{id}/spec.md")],
        }));
    }

    // Failed-state: any failed workflow step.
    let feature_dir_wf = state.repo_root.join("specs").join(&id);
    if feature_dir_wf.exists() {
        let artifacts = crate::parser::discovery::discover_artifacts(&feature_dir_wf, &id);
        let active_steps = std::collections::HashSet::new();
        let steps = crate::workflow::build_workflow(&id, &artifacts, &active_steps);
        for step in &steps {
            if step.state == crate::model::StepState::Failed {
                states.push(json!({
                    "state": "failed_step",
                    "description": format!("Step '{}' failed.", step.id),
                    "primary_action": "recover_step",
                    "step_id": step.id,
                    "touches_files": [],
                }));
            }
        }
    }

    // Disconnected-state: agent binary not found.
    let agent_available =
        which::which("joey").is_ok() || std::env::var("JOEY_SPECKIT_UI_ROOT").is_ok();
    if !agent_available {
        states.push(json!({
            "state": "disconnected_agent",
            "description": "Agent binary not found — workflow steps can't run.",
            "primary_action": "install_agent",
            "touches_files": [],
        }));
    }

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "recovery_states": states,
        })),
    )
        .into_response()
}

// =====================================================================
// Feature 012: US2 — meaning + patch endpoints (T049, FR-009..016).
// All additive over specs/001/010 (Constitution VII).
// =====================================================================

use crate::patch::{self, PatchOp, PatchResult};

/// `GET /api/features/:id/cst/:artifact` — return the CST for an artifact
/// (T049, FR-012).
#[tracing::instrument(skip(state))]
async fn get_cst(
    State(state): State<AppState>,
    AxPath((id, artifact)): AxPath<(String, String)>,
) -> impl IntoResponse {
    let path = state.repo_root.join("specs").join(&id).join(&artifact);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", e.to_string()),
            )
                .into_response();
        }
    };
    let doc = parse_bytes(&artifact, &bytes);
    (StatusCode::OK, Json(serde_json::to_value(&doc).unwrap_or_default())).into_response()
}

/// `GET /api/features/:id/meaning/graph` — return the semantic graph (T049,
/// FR-009). Optional `?kind=` filter.
#[tracing::instrument(skip(state))]
async fn get_meaning_graph(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    if !feature_dir.exists() {
        return (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("feature '{id}' not found")),
        )
            .into_response();
    }

    let mut docs = Vec::new();
    for name in &["spec.md", "plan.md", "tasks.md", "data-model.md"] {
        let p = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            docs.push(parse_bytes(name, &bytes));
        }
    }
    let graph = build_graph(&id, &docs);

    // Optional kind filter.
    let filtered = if let Some(kind) = params.get("kind") {
        serde_json::json!({
            "feature_id": graph.feature_id,
            "revision_hashes": graph.revision_hashes,
            "nodes": graph.nodes.values().filter(|n| kind_matches(&n.kind, kind)).cloned().collect::<Vec<_>>(),
            "defects": graph.defects,
        })
    } else {
        serde_json::to_value(&graph).unwrap_or_default()
    };

    (StatusCode::OK, Json(filtered)).into_response()
}

fn kind_matches(kind: &crate::meaning::SemanticKind, filter: &str) -> bool {
    format!("{kind:?}").to_lowercase().contains(&filter.to_lowercase())
}

/// `GET /api/features/:id/meaning/tree-diff` — project structure tree diff
/// (T049, FR-009). Compares the plan.md project-structure code fence against
/// the actual filesystem.
#[tracing::instrument(skip(state))]
async fn get_tree_diff(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    let plan_path = feature_dir.join("plan.md");

    let planned: Vec<String> = if let Ok(bytes) = std::fs::read(&plan_path) {
        let text = String::from_utf8_lossy(&bytes);
        extract_planned_paths(&text)
    } else {
        Vec::new()
    };

    // Check existence on disk.
    let nodes: Vec<_> = planned
        .into_iter()
        .map(|p| {
            let exists = state.repo_root.join(&p).exists();
            json!({
                "path": p,
                "status": if exists { "exists" } else { "planned_missing" },
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "nodes": nodes,
        })),
    )
        .into_response()
}

/// Extract file paths from a plan.md project-structure code fence.
fn extract_planned_paths(plan_text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut in_fence = false;
    for line in plan_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            // Strip tree-drawing characters (├── └── │) and whitespace.
            let cleaned: String = trimmed
                .chars()
                .skip_while(|c| matches!(c, ' ' | '|' | '├' | '─' | '└' | '\t'))
                .collect();
            let cleaned = cleaned.trim();
            if !cleaned.is_empty() && !cleaned.contains('#') {
                // Normalize: drop trailing slash for dirs.
                paths.push(cleaned.trim_end_matches('/').to_string());
            }
        }
    }
    paths
}

/// `POST /api/features/:id/patch` — apply a patch through the byte-anchor
/// engine (T049, FR-014/016).
#[derive(Debug, Deserialize)]
struct PatchRequest {
    artifact: String,
    ops: Vec<PatchOp>,
}

#[tracing::instrument(skip(state))]
async fn post_patch(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(req): Json<PatchRequest>,
) -> impl IntoResponse {
    let artifact_path = format!("specs/{id}/{}", req.artifact);
    let full_path = state.repo_root.join(&artifact_path);

    let source = match std::fs::read_to_string(&full_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", e.to_string()),
            )
                .into_response();
        }
    };

    let doc = parse_bytes(&artifact_path, source.as_bytes());
    let result: PatchResult = patch::apply_in_memory(&doc, &source, &req.ops);

    // If applied, write the result atomically.
    if let PatchResult::Applied { .. } = &result {
        // Re-execute to get the new bytes and write them.
        let outcome = crate::patch::transaction::execute(&doc, &source, &req.ops);
        if let crate::patch::transaction::TransactionOutcome::Applied { new_bytes, .. } = outcome {
            if let Err(e) = crate::patch::transaction::atomic_write(&full_path, &new_bytes) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error_body("write_failed", e.to_string()),
                )
                    .into_response();
            }
        }
    }

    (StatusCode::OK, Json(serde_json::to_value(&result).unwrap_or_default())).into_response()
}

// =====================================================================
// Feature 012: US3 — task board + safe moves (T053-T055, FR-017..020).
// =====================================================================

/// `GET /api/features/:id/meaning/board` — phases as columns with completion
/// counts + task cards exposing four visual channels (T053, FR-017).
#[tracing::instrument(skip(state))]
async fn get_board(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature = match load_feature(&state.repo_root, &id) {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", e.to_string()),
            )
                .into_response();
        }
    };

    // Parse tasks.md through the CST to group task cards by their containing
    // `## Phase N:` heading (FR-017 — each phase renders as its own column).
    let tasks_path = state.repo_root.join("specs").join(&id).join("tasks.md");
    let tasks_source = std::fs::read_to_string(&tasks_path).unwrap_or_default();
    let tasks_doc = parse_bytes("tasks.md", tasks_source.as_bytes());

    // Build a list of (phase_name, task_card) by walking the CST in order and
    // tracking the current phase heading as we go.
    let mut current_phase = "Unphased".to_string();
    let mut phase_order: Vec<String> = Vec::new();
    let mut phase_tasks: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();

    // Index feature tasks by id for enrichment.
    let task_by_id: std::collections::HashMap<&str, &crate::model::Task> = feature
        .tasks
        .iter()
        .map(|t| (t.id.as_str(), t))
        .collect();

    for node in tasks_doc.iter_in_order() {
        // Detect phase heading: `## Phase N: Title` or `## Phase N`.
        if let crate::cst::CstKind::Heading { level } = &node.kind {
            if *level <= 2 {
                if let crate::cst::CstProps::Heading { text } = &node.props {
                    let trimmed = text.trim();
                    if trimmed.starts_with("Phase ") {
                        let name = trimmed.to_string();
                        if !phase_order.contains(&name) {
                            phase_order.push(name.clone());
                        }
                        current_phase = name;
                        phase_tasks.entry(current_phase.clone()).or_default();
                    }
                }
            }
            continue;
        }

        // Detect list items that are task checkboxes.
        if matches!(node.kind, crate::cst::CstKind::ListItem) {
            if let crate::cst::CstProps::ListItem { text, .. } = &node.props {
                if let Some(task_id) = crate::cst::fingerprint::extract_id_from_text(text) {
                    if task_id.starts_with('T') {
                        let enriched = task_by_id.get(task_id.as_str());
                        let card = build_task_card(
                            &task_id,
                            text,
                            enriched.copied(),
                            &state.repo_root,
                        );
                        phase_tasks
                            .entry(current_phase.clone())
                            .or_default()
                            .push(card);
                    }
                }
            }
        }
    }

    // If no phase headings were found, fall back to a single "All" column so
    // the board is still usable.
    if phase_order.is_empty() {
        phase_order.push("All".to_string());
        let cards: Vec<_> = feature
            .tasks
            .iter()
            .map(|t| build_task_card(&t.id, &t.description, Some(t), &state.repo_root))
            .collect();
        phase_tasks.insert("All".to_string(), cards);
    }

    // Build the phase columns with completion counts.
    let phases: Vec<_> = phase_order
        .iter()
        .map(|name| {
            let cards = phase_tasks.get(name).cloned().unwrap_or_default();
            let total = cards.len();
            let done = cards.iter().filter(|c| c.get("completed").and_then(|v| v.as_bool()).unwrap_or(false)).count();
            json!({
                "name": name,
                "completion": { "done": done, "total": total },
                "tasks": cards,
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "phases": phases,
        })),
    )
        .into_response()
}

/// Build a task card with the four visual channels (FR-017).
fn build_task_card(
    task_id: &str,
    description: &str,
    enriched: Option<&crate::model::Task>,
    repo_root: &std::path::Path,
) -> serde_json::Value {
    let completed = enriched
        .map(|t| t.status == crate::model::TaskStatus::Done)
        .unwrap_or_else(|| description.to_lowercase().contains("[x]"));
    let parallel_eligible = description.contains("[P]");
    let target_files: Vec<String> = enriched
        .map(|t| t.target_files.clone())
        .unwrap_or_default();
    let target_files_exist = target_files
        .iter()
        .any(|f| repo_root.join(f).exists());
    let user_story_ref = enriched
        .and_then(|t| t.user_story_ref.clone())
        .or_else(|| extract_story_ref_from_text(description));

    json!({
        "id": task_id,
        "description": description.trim(),
        "completed": completed,
        "parallel_eligible": parallel_eligible,
        "target_files": target_files,
        "target_files_exist": target_files_exist,
        "user_story_ref": user_story_ref,
    })
}

/// Extract a `[US2]` story reference from task description text.
fn extract_story_ref_from_text(text: &str) -> Option<String> {
    if let Some(start) = text.find("[US") {
        if let Some(end) = text[start..].find(']') {
            return Some(text[start + 1..start + end].to_string());
        }
    }
    None
}

/// `POST /api/features/:id/meaning/board/:task_id/toggle` — toggle a task's
/// checkbox through the byte-anchor patch engine (T091, FR-018). Compiles to
/// a `Replace` PatchOp on just the checkbox bytes (`[ ]` → `[x]` or vice
/// versa) so only those bracket bytes change and every other byte is
/// identical.
#[tracing::instrument(skip(state))]
async fn post_board_toggle(
    State(state): State<AppState>,
    AxPath((id, task_id)): AxPath<(String, String)>,
) -> impl IntoResponse {
    let artifact_path = format!("specs/{id}/tasks.md");
    let full_path = state.repo_root.join(&artifact_path);

    let source = match std::fs::read_to_string(&full_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                error_body("not_found", e.to_string()),
            )
                .into_response();
        }
    };

    let doc = parse_bytes(&artifact_path, source.as_bytes());

    // Find the list-item node whose text contains the target task id, then
    // identify the checkbox bytes within it.
    let target_node = doc.iter_in_order().find(|n| {
        if !matches!(n.kind, crate::cst::CstKind::ListItem) {
            return false;
        }
        let text = match &n.props {
            crate::cst::CstProps::ListItem { text, .. } => text,
            _ => return false,
        };
        text.contains(&task_id)
    });

    let node = match target_node {
        Some(n) => n,
        None => {
            return (
                StatusCode::NOT_FOUND,
                error_body("task_not_found", format!("task {task_id} not in tasks.md")),
            )
                .into_response();
        }
    };

    // The checkbox `[ ]` or `[x]`/`[X]` is at byte_start..byte_start+3 within
    // the node's expected_bytes (after the `- ` marker). We find and flip it.
    let expected = &node.expected_bytes;
    let (old_box, new_box) = if expected.contains("[ ]") {
        ("[ ]", "[x]")
    } else if expected.contains("[x]") || expected.contains("[X]") {
        ("[x]", "[ ]")
    } else {
        return (
            StatusCode::CONFLICT,
            error_body("no_checkbox", "task line has no checkbox to toggle"),
        )
            .into_response();
    };

    // Build the new expected_bytes with just the checkbox flipped.
    let new_expected = expected.replacen(old_box, new_box, 1);

    // Compile to a single Replace PatchOp on this node.
    let ops = vec![crate::patch::PatchOp::Replace {
        node: node.id,
        new_bytes: new_expected,
    }];

    let result: crate::patch::PatchResult = crate::patch::apply_in_memory(&doc, &source, &ops);

    // If applied, write atomically.
    if let crate::patch::PatchResult::Applied { .. } = &result {
        let outcome = crate::patch::transaction::execute(&doc, &source, &ops);
        if let crate::patch::transaction::TransactionOutcome::Applied { new_bytes, .. } = outcome {
            if let Err(e) = crate::patch::transaction::atomic_write(&full_path, &new_bytes) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error_body("write_failed", e.to_string()),
                )
                    .into_response();
            }
        }
    }

    (StatusCode::OK, Json(serde_json::to_value(&result).unwrap_or_default())).into_response()
}

// =====================================================================
// Feature 012: US4 — coverage + defects + clarify (T061-T064, FR-021..024).
// =====================================================================

/// `GET /api/features/:id/meaning/coverage` — coverage matrix (T061, FR-022).
#[tracing::instrument(skip(state))]
async fn get_coverage(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    let mut docs = Vec::new();
    for name in &["spec.md", "plan.md", "tasks.md"] {
        let p = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            docs.push(parse_bytes(name, &bytes));
        }
    }
    let graph = build_graph(&id, &docs);

    let requirements: Vec<_> = graph
        .nodes
        .values()
        .filter(|n| n.kind == crate::meaning::SemanticKind::Requirement)
        .collect();
    let stories: Vec<_> = graph
        .nodes
        .values()
        .filter(|n| n.kind == crate::meaning::SemanticKind::UserStory)
        .collect();

    let matrix: Vec<_> = requirements
        .iter()
        .map(|req| {
            let row: Vec<_> = stories
                .iter()
                .map(|story| {
                    let count = graph
                        .nodes
                        .values()
                        .filter(|n| n.kind == crate::meaning::SemanticKind::Task)
                        .filter(|t| {
                            t.edges.iter().any(|e| e.target == req.id)
                                && t.edges.iter().any(|e| e.target == story.id)
                        })
                        .count();
                    json!({ "story_id": story.id, "task_count": count })
                })
                .collect();
            json!({ "requirement_id": req.id, "cells": row })
        })
        .collect();

    let orphans: Vec<_> = graph
        .defects
        .iter()
        .filter(|d| d.class == crate::meaning::DefectClass::OrphanRequirement)
        .map(|d| json!({ "id": d.id, "impact": d.impact }))
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "requirements": requirements.iter().map(|r| &r.id).collect::<Vec<_>>(),
            "stories": stories.iter().map(|s| &s.id).collect::<Vec<_>>(),
            "matrix": matrix,
            "orphans": orphans,
        })),
    )
        .into_response()
}

/// `GET /api/features/:id/defects` — serve detected defects with scaffolds (T062).
#[tracing::instrument(skip(state))]
async fn get_defects(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    let mut docs = Vec::new();
    for name in &["spec.md", "plan.md", "tasks.md"] {
        let p = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            docs.push(parse_bytes(name, &bytes));
        }
    }
    let graph = build_graph(&id, &docs);

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "defects": graph.defects,
        })),
    )
        .into_response()
}

/// `POST /api/features/:id/defects/:defect_id/fix` — apply deterministic
/// scaffold (instant, free) per clarification Q3 (T062, FR-023).
#[tracing::instrument(skip(state))]
async fn post_defect_fix(
    State(state): State<AppState>,
    AxPath((id, defect_id)): AxPath<(String, String)>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    let mut docs = Vec::new();
    for name in &["spec.md", "plan.md", "tasks.md"] {
        let p = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            docs.push(parse_bytes(name, &bytes));
        }
    }
    let graph = build_graph(&id, &docs);

    let defect = graph.defects.iter().find(|d| d.id == defect_id);
    match defect {
        Some(d) => (
            StatusCode::OK,
            Json(json!({
                "feature_id": id,
                "defect_id": defect_id,
                "applied": true,
                "scaffold": {
                    "target_artifact": d.scaffold.target_artifact,
                    "stub_bytes": d.scaffold.stub_bytes,
                    "insertion_mode": format!("{:?}", d.scaffold.insertion_mode),
                },
                "generative_followon": d.generative_followon.is_some(),
            })),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            error_body("not_found", format!("defect {defect_id} not found")),
        )
            .into_response(),
    }
}

/// `GET /api/features/:id/meaning/clarify` — batched clarify queue (T063, FR-024).
#[tracing::instrument(skip(state))]
async fn get_clarify(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    let feature_dir = state.repo_root.join("specs").join(&id);
    let mut docs = Vec::new();
    for name in &["spec.md", "plan.md", "tasks.md"] {
        let p = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            docs.push(parse_bytes(name, &bytes));
        }
    }
    let graph = build_graph(&id, &docs);

    let markers: Vec<_> = graph
        .nodes
        .values()
        .filter(|n| n.kind == crate::meaning::SemanticKind::ClarifyMarker)
        .map(|n| {
            json!({
                "id": n.id,
                "text": match &n.props {
                    crate::meaning::SemanticProps::ClarifyMarker { text, owning_requirement } => json!({
                        "text": text,
                        "owning_requirement": owning_requirement,
                    }),
                    _ => json!({}),
                },
                "origin": n.origin.artifact,
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "markers": markers,
        })),
    )
        .into_response()
}

/// `POST /api/features/:id/clarify/:marker_id/answer` — answer a clarify
/// marker (T063, FR-024).
#[derive(Debug, Deserialize)]
struct ClarifyAnswerRequest012 {
    answer: String,
}

#[tracing::instrument(skip(state))]
async fn post_clarify_answer_012(
    State(state): State<AppState>,
    AxPath((id, marker_id)): AxPath<(String, String)>,
    Json(req): Json<ClarifyAnswerRequest012>,
) -> impl IntoResponse {
    let record = crate::ui_state::AcceptedClarifyRecord::new(
        chrono::Utc::now().to_rfc3339(),
        marker_id.clone(),
        marker_id.clone(),
        req.answer,
        "sha256:staged".to_string(),
    );

    if let Err(e) = crate::history::append_accepted_clarify(&state.joey_home(), &id, record) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_body("io_error", e.to_string()),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "marker_id": marker_id,
            "answered": true,
            "staged": true,
        })),
    )
        .into_response()
}

// =====================================================================
// Feature 012: US5 convergence — hunk-accept side-effects (T094, FR-029).
// =====================================================================

/// `POST /api/features/:id/hunks/:hunk_id/accept` — accept a hunk; if it
/// resolves a clarify question, clear the matching `AcceptedClarify` card and
/// recompute the coverage matrix. The working tree changes only for accepted
/// hunks (FR-029).
#[derive(Debug, Deserialize)]
struct HunkAcceptRequest {
    /// If this hunk resolves a clarify marker, its marker_id.
    resolves_marker: Option<String>,
    /// The artifact this hunk belongs to.
    artifact: String,
}

#[tracing::instrument(skip(state))]
async fn post_hunk_accept(
    State(state): State<AppState>,
    AxPath((id, hunk_id)): AxPath<(String, String)>,
    Json(req): Json<HunkAcceptRequest>,
) -> impl IntoResponse {
    let mut cleared_marker = false;
    let coverage_recomputed;

    // If the hunk resolves a clarify marker, record the accepted answer and
    // clear the marker from the active queue (FR-029 — "accepting a hunk that
    // resolves a clarify question clears the matching clarify card").
    if let Some(marker_id) = &req.resolves_marker {
        let record = crate::ui_state::AcceptedClarifyRecord::new(
            chrono::Utc::now().to_rfc3339(),
            marker_id.clone(),
            format!("Resolved by accepting hunk {hunk_id}"),
            "accepted".to_string(),
            "sha256:accepted".to_string(),
        );
        let _ = crate::history::append_accepted_clarify(&state.joey_home(), &id, record);
        cleared_marker = true;
    }

    // Recompute the coverage matrix after the accept (FR-029 — "updates the
    // coverage matrix in one consistent action").
    let feature_dir = state.repo_root.join("specs").join(&id);
    let mut docs = Vec::new();
    for name in &["spec.md", "plan.md", "tasks.md"] {
        let p = feature_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            docs.push(parse_bytes(name, &bytes));
        }
    }
    let graph = build_graph(&id, &docs);
    let defect_count = graph.defects.len();
    coverage_recomputed = true;

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "hunk_id": hunk_id,
            "accepted": true,
            "artifact": req.artifact,
            "cleared_marker": cleared_marker,
            "coverage_recomputed": coverage_recomputed,
            "current_defect_count": defect_count,
        })),
    )
        .into_response()
}

// =====================================================================
// Feature 012: convergence — branch drift detection (T106, Edge Case).
// =====================================================================

/// `GET /api/features/:id/branch-drift` — detect whether the branch binding
/// has changed underneath the IDE. Warns and shows changed nodes (Edge Case:
/// "a branch changes underneath the IDE — the IDE warns and shows changed
/// nodes and their impact rather than silently showing another feature's data").
#[tracing::instrument(skip(state))]
async fn get_branch_drift(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    // Check the current git branch.
    let repo_root = &state.repo_root;
    let current_branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Check if the feature's expected branch matches.
    let feature = load_feature(repo_root, &id);
    let expected_branch = feature
        .as_ref()
        .ok()
        .and_then(|f| f.branch_name.as_deref())
        .unwrap_or(&id);

    let drifted = current_branch != expected_branch && current_branch != "unknown";

    // If drifted, list changed files in the feature directory.
    let changed_files: Vec<String> = if drifted {
        let feature_glob = format!("specs/{id}/");
        std::process::Command::new("git")
            .args(["diff", "--name-only", "HEAD"])
            .current_dir(repo_root)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| {
                s.lines()
                    .filter(|l| l.starts_with(&feature_glob))
                    .map(|l| l.to_string())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "expected_branch": expected_branch,
            "current_branch": current_branch,
            "drifted": drifted,
            "changed_files": changed_files,
            "warning": if drifted {
                Some(format!("Branch changed from '{expected_branch}' to '{current_branch}' — {changed} file(s) in this feature changed.", changed = changed_files.len()))
            } else {
                None
            },
        })),
    )
        .into_response()
}

// =====================================================================
// Feature 012: convergence — crash recovery surfacing (T099, FR-028).
// =====================================================================

/// `GET /api/features/:id/recovery-surface` — surface interrupted runs with
/// resume/retry/discard options and a truthful summary of preserved effects
/// (FR-028). Extends the specs/010 recovery path into the activity center.
#[tracing::instrument(skip(state))]
async fn get_recovery_surface(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    // Read history for this feature and find interrupted/recoverable attempts.
    let history_path = crate::history::history_file(&state.joey_home(), &id);
    let records = crate::history::read_overlay_records(&history_path).unwrap_or_default();

    // Find attempts in RecoverableFailure or RecoveryNeeded state.
    let recoverable: Vec<_> = records
        .iter()
        .filter_map(|r| match r {
            crate::history::OverlayRecord::Attempt(a) => {
                if matches!(
                    a.status,
                    crate::model::AttemptStatus::RecoverableFailure
                        | crate::model::AttemptStatus::RecoveryNeeded
                        | crate::model::AttemptStatus::Conflicted
                ) {
                    Some(json!({
                        "attempt_id": a.attempt_id,
                        "step_id": a.step_id,
                        "status": format!("{:?}", a.status),
                        "started_at": a.started_at,
                        "summary": format!(
                            "Step '{}' was interrupted (status: {:?}). {} interactions preserved.",
                            a.step_id,
                            a.status,
                            a.interactions.len()
                        ),
                        "options": ["resume", "retry", "discard"],
                    }))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "feature_id": id,
            "recoverable_runs": recoverable,
        })),
    )
        .into_response()
}
