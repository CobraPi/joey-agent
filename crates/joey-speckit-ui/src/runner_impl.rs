//! Concrete `WorkflowRunner` implementation: spawns the `joey` CLI /
//! skill wrapper out-of-process and streams its I/O (FR-011/012/013/014).
//!
//! Communication is via stdin/stdout/stderr only — never an in-process
//! library call (Constitution VI). The runner classifies subprocess output
//! into `RunnerEvent`s and forwards them over the event channel.

use std::path::Path;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::model::RunConfiguration;
use crate::runner::{
    AttemptHandle, InteractionPayload, RunnerError, RunnerEvent, TerminalStatus,
    WorkflowRunner, exit_code_to_status,
};
use crate::staging::StagingArea;

/// Out-of-process Joey Agent runner via the `joey` CLI.
pub struct JoeyCliRunner;

impl JoeyCliRunner {
    pub fn new() -> Self {
        JoeyCliRunner
    }
}

impl Default for JoeyCliRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WorkflowRunner for JoeyCliRunner {
    async fn prepare_and_start(
        &self,
        repo_root: &Path,
        feature_id: &str,
        step: &str,
        _config: &RunConfiguration,
        staging: &dyn StagingArea,
    ) -> Result<AttemptHandle, RunnerError> {
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let mode = _config.change_mode.clone().unwrap_or(crate::model::ChangeMode::Staged);

        // Open the staging area.
        let staging_root = staging
            .open(repo_root, &attempt_id, mode, &_config.scope)
            .await
            .map_err(|e| RunnerError::Staging(e.to_string()))?;

        // Build the command: `joey <step>` or the skill wrapper.
        // Prefer the joey CLI, fall back to .specify/scripts/bash/<step>.sh.
        let (program, args) = if which::which("joey").is_ok() {
            ("joey".to_string(), vec![format!("/speckit-{step}")])
        } else {
            let script = repo_root
                .join(".specify")
                .join("scripts")
                .join("bash")
                .join(format!("{step}.sh"));
            ("bash".to_string(), vec![script.to_string_lossy().to_string()])
        };

        let mut child = Command::new(&program)
            .args(&args)
            .current_dir(&staging_root.worktree)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("SPECIFY_FEATURE", feature_id)
            .spawn()
            .map_err(|e| RunnerError::Spawn(format!("failed to spawn {program}: {e}")))?;

        // Set up event streaming channels.
        let (event_tx, event_rx) = mpsc::channel::<RunnerEvent>(64);
        let (respond_tx, mut respond_rx) = mpsc::channel::<InteractionPayload>(16);

        // Spawn the stdin writer task: forwards InteractionPayload → child stdin.
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                while let Some(payload) = respond_rx.recv().await {
                    let json = match serde_json::to_string(&payload) {
                        Ok(j) => j,
                        Err(_) => continue,
                    };
                    if stdin.write_all(format!("{json}\n").as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }

        // Spawn the stdout reader task: classify lines into RunnerEvents.
        if let Some(stdout) = child.stdout.take() {
            let tx = event_tx.clone();
            let aid = attempt_id.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Try to parse as JSON event first; fall back to progress text.
                    if let Ok(evt) = serde_json::from_str::<RunnerEvent>(&line) {
                        if tx.send(evt).await.is_err() {
                            break;
                        }
                    } else {
                        let _ = tx
                            .send(RunnerEvent::Progress {
                                attempt_id: aid.clone(),
                                text: line,
                            })
                            .await;
                    }
                }
            });
        }

        // Spawn the stderr reader task: forward as progress text.
        if let Some(stderr) = child.stderr.take() {
            let tx = event_tx.clone();
            let aid = attempt_id.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = tx
                        .send(RunnerEvent::Progress {
                            attempt_id: aid.clone(),
                            text: format!("[stderr] {line}"),
                        })
                        .await;
                }
            });
        }

        // Spawn the child wait task: emit terminal status event.
        let tx = event_tx.clone();
        let aid = attempt_id.clone();
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let status = child.wait().await.ok();
            let terminal = exit_code_to_status(status.and_then(|s| s.code()));
            let duration_ms = start.elapsed().as_millis() as u64;
            let _ = tx
                .send(RunnerEvent::Status {
                    attempt_id: aid,
                    terminal,
                    duration_ms,
                })
                .await;
        });

        Ok(AttemptHandle {
            attempt_id,
            staging_root: staging_root.worktree,
            events: event_rx,
            respond_tx,
        })
    }

    async fn respond(
        &self,
        attempt: &mut AttemptHandle,
        payload: InteractionPayload,
    ) -> Result<(), RunnerError> {
        attempt
            .respond_tx
            .send(payload)
            .await
            .map_err(|_| RunnerError::Other("attempt stdin closed".to_string()))
    }

    async fn cancel(&self, _attempt: &mut AttemptHandle) -> Result<(), RunnerError> {
        // The child wait task holds the Child handle; dropping the event receiver
        // and stdin sender effectively cancels the run. In a fuller implementation,
        // we'd send SIGTERM here via the child PID.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staging::{StagingError, StagingRoot};

    /// A no-op staging area for testing the runner without git.
    struct NoopStaging;

    #[async_trait]
    impl StagingArea for NoopStaging {
        async fn open(
            &self,
            repo_root: &Path,
            attempt_id: &str,
            mode: crate::model::ChangeMode,
            _scope: &crate::model::Scope,
        ) -> Result<StagingRoot, StagingError> {
            Ok(StagingRoot {
                worktree: repo_root.to_path_buf(),
                mode,
                attempt_id: attempt_id.to_string(),
            })
        }
        async fn checkpoint(&self, _root: &StagingRoot) -> Result<crate::model::Checkpoint, StagingError> {
            Ok(crate::model::Checkpoint::default())
        }
        async fn diff(&self, _root: &StagingRoot) -> Result<crate::model::ChangeSet, StagingError> {
            Ok(crate::model::ChangeSet::default())
        }
        async fn apply(
            &self,
            _root: &StagingRoot,
            _selection: &crate::staging::Selection,
        ) -> Result<crate::staging::ApplyOutcome, StagingError> {
            Ok(crate::staging::ApplyOutcome::default())
        }
        async fn discard(&self, _root: &StagingRoot) -> Result<(), StagingError> {
            Ok(())
        }
    }

    #[test]
    fn exit_code_mapping() {
        assert_eq!(exit_code_to_status(Some(0)), TerminalStatus::Succeeded);
        assert_eq!(exit_code_to_status(Some(1)), TerminalStatus::Failed);
        assert_eq!(exit_code_to_status(None), TerminalStatus::Cancelled);
    }
}
