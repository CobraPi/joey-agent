//! joey-speckit-ui: local backend for the SpecKit Visual UI.
//!
//! Parses `spec.md`/`plan.md`/`tasks.md` under `specs/<feature>/` into a
//! typed model (see `model`), serves it plus conflict-checked writes over a
//! local HTTP+WebSocket API (see `api`), and watches feature directories
//! for external changes (see `watcher`).

pub mod api;
pub mod commands;
pub mod conflict;
pub mod model;
pub mod parser;
pub mod writer;
pub mod watcher;

use std::path::{Path, PathBuf};

use crate::model::Feature;

/// Load a single feature's fully parsed model from `specs/<id>/` under
/// `repo_root`.
pub fn load_feature(repo_root: &Path, id: &str) -> anyhow::Result<Feature> {
    let dir = repo_root.join("specs").join(id);
    if !dir.exists() {
        anyhow::bail!("feature directory not found: {}", dir.display());
    }

    let mut missing = Vec::new();

    let spec_path = dir.join("spec.md");
    let (specification, spec_hash) = if spec_path.exists() {
        let content = std::fs::read_to_string(&spec_path)?;
        let hash = crate::conflict::content_hash(&content);
        (Some(parser::spec::parse_spec(&content)), Some(hash))
    } else {
        missing.push("spec".to_string());
        (None, None)
    };

    let plan_path = dir.join("plan.md");
    let (plan, plan_hash) = if plan_path.exists() {
        let content = std::fs::read_to_string(&plan_path)?;
        let hash = crate::conflict::content_hash(&content);
        (Some(parser::plan::parse_plan(&content)), Some(hash))
    } else {
        missing.push("plan".to_string());
        (None, None)
    };

    let tasks_path = dir.join("tasks.md");
    let (tasks, tasks_hash) = if tasks_path.exists() {
        let content = std::fs::read_to_string(&tasks_path)?;
        let hash = crate::conflict::content_hash(&content);
        (parser::tasks::parse_tasks(&content), Some(hash))
    } else {
        missing.push("tasks".to_string());
        (Vec::new(), None)
    };

    Ok(Feature {
        id: id.to_string(),
        directory: dir.to_string_lossy().to_string(),
        branch_name: None,
        specification,
        plan,
        tasks,
        missing,
        spec_content_hash: spec_hash,
        plan_content_hash: plan_hash,
        tasks_content_hash: tasks_hash,
    })
}

/// List feature ids (directory names) under `repo_root/specs`.
pub fn list_feature_ids(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    let specs_dir = repo_root.join("specs");
    if !specs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&specs_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    Ok(ids)
}

/// Shared application state passed to all API handlers.
#[derive(Clone)]
pub struct AppState {
    pub repo_root: PathBuf,
    /// Live broadcast channels for in-flight clarify sessions and task-execution
    /// runs, keyed by session_id / run_id. Used to bridge a POST that kicks off
    /// a background subprocess to a WebSocket subscriber that streams its
    /// output (see `api::ws`). Entries are created on kickoff and removed once
    /// the run/session reaches a terminal state and all subscribers have had a
    /// chance to observe it.
    pub runs: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::broadcast::Sender<String>>>>,
}

impl AppState {
    pub fn new(repo_root: PathBuf) -> Self {
        AppState {
            repo_root,
            runs: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Create (or return existing) broadcast channel for a session/run id.
    pub async fn channel_for(&self, id: &str) -> tokio::sync::broadcast::Sender<String> {
        let mut runs = self.runs.lock().await;
        runs.entry(id.to_string())
            .or_insert_with(|| tokio::sync::broadcast::channel(64).0)
            .clone()
    }

    /// Remove a session/run's channel once it's finished and no longer needed.
    pub async fn remove_channel(&self, id: &str) {
        self.runs.lock().await.remove(id);
    }
}
