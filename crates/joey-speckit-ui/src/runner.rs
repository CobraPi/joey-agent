//! Out-of-process workflow runner contract (FR-011/012/013/014/033).
//!
//! Defines the `WorkflowRunner` trait — the single interface the backend
//! depends on instead of linking `joey-agent-core` (Constitution VI). See
//! `contracts/workflow-runner.md`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::model::RunConfiguration;
use crate::staging::StagingArea;

/// Error from runner operations.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("staging error: {0}")]
    Staging(String),
    #[error("other: {0}")]
    Other(String),
}

/// A handle to a running attempt — observed via the event receiver and
/// cancelled via `cancel()`.
pub struct AttemptHandle {
    pub attempt_id: String,
    pub staging_root: PathBuf,
    /// Line-delimited JSON `RunnerEvent` values, streamed from the subprocess.
    pub events: mpsc::Receiver<RunnerEvent>,
    /// Sender for interaction responses (answer/approve) written to stdin.
    pub respond_tx: mpsc::Sender<InteractionPayload>,
}

/// A response to a pending interaction, written to the child's stdin.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum InteractionPayload {
    /// Answer to a question (FR-013).
    Answer {
        interaction_id: String,
        answer: String,
    },
    /// Response to an approval request (FR-013/017).
    Approval {
        interaction_id: String,
        decision: ApprovalDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

/// Events streamed from the runner over WS `/api/attempts/{id}/stream`
/// (research.md §1). Each is one newline-delimited JSON record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum RunnerEvent {
    Progress {
        attempt_id: String,
        text: String,
    },
    Tool {
        attempt_id: String,
        name: String,
        summary: String,
    },
    Question {
        attempt_id: String,
        interaction_id: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choices: Option<Vec<String>>,
    },
    Approval {
        attempt_id: String,
        interaction_id: String,
        impact: String,
        boundary: String,
    },
    Output {
        attempt_id: String,
        file: String,
        added: i32,
        removed: i32,
    },
    Status {
        attempt_id: String,
        terminal: TerminalStatus,
        duration_ms: u64,
    },
    Error {
        attempt_id: String,
        message: String,
        recoverable: bool,
    },
}

/// Terminal status derived from the child exit code (contracts/workflow-runner.md).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Succeeded,
    Failed,
    Cancelled,
}

/// Map a child process exit code to a terminal status.
pub fn exit_code_to_status(code: Option<i32>) -> TerminalStatus {
    match code {
        Some(0) => TerminalStatus::Succeeded,
        Some(_) => TerminalStatus::Failed,
        None => TerminalStatus::Cancelled, // killed by signal
    }
}

/// The out-of-process runner contract. US2 (T022) provides the concrete
/// implementation that spawns the `joey` CLI.
#[async_trait]
pub trait WorkflowRunner: Send + Sync {
    /// Spawn the joey CLI / skill wrapper for `step` in the feature's repo
    /// context. Returns an attempt handle whose lifecycle is observed via the
    /// returned event stream + cancel handle.
    async fn prepare_and_start(
        &self,
        repo_root: &Path,
        feature_id: &str,
        step: &str,
        config: &RunConfiguration,
        staging: &dyn StagingArea,
    ) -> Result<AttemptHandle, RunnerError>;

    /// Send an answer/approval to a pending interaction (FR-013).
    async fn respond(
        &self,
        attempt: &mut AttemptHandle,
        payload: InteractionPayload,
    ) -> Result<(), RunnerError>;

    /// Cancel a running/waiting attempt safely (FR-014).
    async fn cancel(&self, attempt: &mut AttemptHandle) -> Result<(), RunnerError>;
}
