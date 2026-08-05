//! joey-speckit-ui: local backend for the SpecKit Visual UI.
//!
//! Parses `spec.md`/`plan.md`/`tasks.md` under `specs/<feature>/` into a
//! typed model (see `model`), serves it plus conflict-checked writes over a
//! local HTTP+WebSocket API (see `api`), and watches feature directories
//! for external changes (see `watcher`).

pub mod api;
pub mod commands;
pub mod conflict;
pub mod cst;
pub mod editor;
pub mod history;
pub mod meaning;
pub mod model;
pub mod parser;
pub mod patch;
pub mod recovery;
pub mod runner;
pub mod runner_impl;
pub mod staging;
pub mod staging_impl;
pub mod ui_state;
pub mod validation;
pub mod watcher;
pub mod workflow;
pub mod writer;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::model::{Feature, WorkspacePreference};
use tokio::sync::{mpsc, Mutex};

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
    pub runs: Arc<Mutex<std::collections::HashMap<String, tokio::sync::broadcast::Sender<String>>>>,
    /// Project-level workflow step overrides (FR-034), keyed by
    /// `feature_id:step_id`.
    overrides: Arc<Mutex<std::collections::HashMap<String, OverrideEntry>>>,
    /// Workspace preferences per feature (FR-026).
    preferences: Arc<Mutex<std::collections::HashMap<String, WorkspacePreference>>>,
    /// Active workflow attempt handles (FR-011/013/014), keyed by attempt_id.
    /// Each entry holds the interaction sender (to forward answers/approvals
    /// to the subprocess) and the feature_id for scope-conflict checking.
    active_attempts: Arc<Mutex<std::collections::HashMap<String, ActiveAttempt>>>,
}

/// An active workflow attempt — holds the channels to communicate with the
/// running subprocess (FR-013/014) and metadata for conflict-guard checking.
#[derive(Clone)]
pub struct ActiveAttempt {
    /// Sender to forward interaction responses to the subprocess stdin.
    pub respond_tx: mpsc::Sender<crate::runner::InteractionPayload>,
    /// The feature this attempt belongs to.
    pub feature_id: String,
    /// The scope targets the attempt was started with (for FR-015 conflict guard).
    pub scope_paths: Vec<String>,
    /// Cancellation token — when all clones are dropped, the subprocess is killed.
    pub cancel: tokio_util::sync::CancellationToken,
}

/// A project-level override for a workflow step's instructions (FR-034).
#[derive(Debug, Clone, serde::Serialize)]
pub struct OverrideEntry {
    pub override_id: String,
    pub instructions: String,
}

/// Server-advertised agent option catalog (FR-010).
#[derive(Debug, Clone, serde::Serialize)]
pub struct OptionsCatalog {
    pub revision: String,
    pub models: Vec<String>,
    pub reasoning_efforts: Vec<String>,
    pub max_iterations: MaxIterations,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MaxIterations {
    pub min: i32,
    pub max: i32,
    pub default: i32,
}

impl AppState {
    pub fn new(repo_root: PathBuf) -> Self {
        AppState {
            repo_root,
            runs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            overrides: Arc::new(Mutex::new(std::collections::HashMap::new())),
            preferences: Arc::new(Mutex::new(std::collections::HashMap::new())),
            active_attempts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Resolve the `~/.joey` home directory (override via `JOEY_HOME`).
    pub fn joey_home(&self) -> PathBuf {
        std::env::var("JOEY_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".joey")
            })
    }

    /// Compute the current options catalog with a content-hash revision (FR-010).
    pub fn options_catalog(&self) -> OptionsCatalog {
        let models = vec![
            "claude-sonnet-4-5".to_string(),
            "gpt-4o".to_string(),
            "default".to_string(),
        ];
        let reasoning_efforts = vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ];
        let max_iterations = MaxIterations {
            min: 1,
            max: 100,
            default: 25,
        };

        // Content-hash revision: hash the catalog contents.
        let revision_source = format!(
            "{}{:?}{}{}{}",
            "v1",
            models,
            reasoning_efforts.len(),
            max_iterations.min,
            max_iterations.max,
        );
        let revision = crate::conflict::content_hash(&revision_source);

        OptionsCatalog {
            revision,
            models,
            reasoning_efforts,
            max_iterations,
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

    /// Register an active attempt so interaction endpoints can reach it
    /// (FR-013/014). Returns nothing; the caller already holds the handle's
    /// event receiver for streaming.
    pub async fn register_attempt(
        &self,
        attempt_id: &str,
        respond_tx: mpsc::Sender<crate::runner::InteractionPayload>,
        feature_id: &str,
        scope_paths: Vec<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        self.active_attempts.lock().await.insert(
            attempt_id.to_string(),
            ActiveAttempt {
                respond_tx,
                feature_id: feature_id.to_string(),
                scope_paths,
                cancel,
            },
        );
    }

    /// Remove an active attempt once it reaches a terminal state.
    pub async fn remove_attempt(&self, attempt_id: &str) {
        self.active_attempts.lock().await.remove(attempt_id);
    }

    /// Get the interaction sender for an attempt (FR-013).
    pub async fn get_attempt_sender(
        &self,
        attempt_id: &str,
    ) -> Option<mpsc::Sender<crate::runner::InteractionPayload>> {
        self.active_attempts
            .lock()
            .await
            .get(attempt_id)
            .map(|a| a.respond_tx.clone())
    }

    /// Cancel an active attempt (FR-014).
    pub async fn cancel_attempt(&self, attempt_id: &str) -> bool {
        if let Some(attempt) = self.active_attempts.lock().await.get(attempt_id) {
            attempt.cancel.cancel();
            true
        } else {
            false
        }
    }

    /// FR-015 conflict guard: check whether any in-flight attempt's scope
    /// overlaps the candidate's target paths. Returns the conflicting
    /// attempt_id if found.
    pub async fn check_conflicting_run(
        &self,
        feature_id: &str,
        candidate_paths: &[String],
    ) -> Option<String> {
        let attempts = self.active_attempts.lock().await;
        for (aid, attempt) in attempts.iter() {
            if attempt.feature_id != feature_id {
                continue;
            }
            // Check path overlap.
            for candidate in candidate_paths {
                if attempt.scope_paths.iter().any(|p| paths_overlap(p, candidate)) {
                    return Some(aid.clone());
                }
            }
        }
        None
    }

    /// Get a project-level override for a feature+step (FR-034).
    pub async fn get_override(&self, feature_id: &str, step: &str) -> Option<OverrideEntry> {
        let key = format!("{feature_id}:{step}");
        self.overrides.lock().await.get(&key).cloned()
    }

    /// Set a project-level override, returning its id (FR-034).
    pub async fn set_override(
        &self,
        feature_id: &str,
        step: &str,
        instructions: String,
    ) -> String {
        let key = format!("{feature_id}:{step}");
        let override_id = uuid::Uuid::new_v4().to_string();
        self.overrides.lock().await.insert(
            key,
            OverrideEntry {
                override_id: override_id.clone(),
                instructions,
            },
        );
        override_id
    }

    /// Remove a project-level override (FR-034).
    pub async fn remove_override(&self, feature_id: &str, step: &str) {
        let key = format!("{feature_id}:{step}");
        self.overrides.lock().await.remove(&key);
    }

    /// Get workspace preferences for a feature (FR-026).
    pub fn get_preferences(&self, feature_id: &str) -> WorkspacePreference {
        self.preferences
            .try_lock()
            .ok()
            .and_then(|guard| guard.get(feature_id).cloned())
            .unwrap_or_default()
    }

    /// Set workspace preferences for a feature (FR-026).
    pub fn set_preferences(
        &self,
        feature_id: &str,
        prefs: &WorkspacePreference,
    ) -> anyhow::Result<()> {
        // Persist to ~/.joey/speckit-ui/preferences.json (FR-026).
        let prefs_dir = self.joey_home().join("speckit-ui");
        std::fs::create_dir_all(&prefs_dir)?;
        let prefs_path = prefs_dir.join("preferences.json");

        // Load existing, merge, write back.
        let mut all: std::collections::HashMap<String, WorkspacePreference> =
            std::fs::read_to_string(&prefs_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

        all.insert(feature_id.to_string(), prefs.clone());
        std::fs::write(&prefs_path, serde_json::to_string_pretty(&all)?)?;

        // Also update in-memory.
        if let Ok(mut guard) = self.preferences.try_lock() {
            guard.insert(feature_id.to_string(), prefs.clone());
        }

        Ok(())
    }
}

/// Check if two repo-relative paths overlap (one is a prefix of the other or
/// they refer to the same file). Used by the FR-015 conflict guard.
fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Directory prefix check: `specs/001/` overlaps `specs/001/plan.md`.
    let (longer, shorter) = if a.len() > b.len() {
        (a, b)
    } else {
        (b, a)
    };
    longer.starts_with(shorter) && longer[shorter.len()..].starts_with('/')
}
