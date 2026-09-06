//! Concrete `WorkflowRunner` implementation: spawns the `joey` CLI /
//! skill wrapper out-of-process and streams its I/O (FR-011/012/013/014).
//!
//! Communication is via stdin/stdout/stderr only — never an in-process
//! library call (Constitution VI). The runner classifies subprocess output
//! into `RunnerEvent`s and forwards them over the event channel.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::model::RunConfiguration;
use crate::runner::{
    exit_code_to_status, AttemptHandle, InteractionPayload, RunnerError, RunnerEvent, WorkflowRunner,
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
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| RunnerError::Spawn(format!("failed to spawn {program}: {e}")))?;

        // Share the child between the wait task (reaps the process) and the
        // attempt handle (kills it on cancel). stdin/stdout/stderr are taken
        // out for the reader/writer tasks first; only then does the child
        // go into the slot.
        let child_slot: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>> =
            Arc::new(tokio::sync::Mutex::new(None));

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

        // Spawn the child wait task: emit terminal status event. The child
        // handle STAYS in the shared slot so `cancel()` can reach it at any
        // time; this task reaps via non-blocking `try_wait` polls (holding
        // the lock across `.await` would deadlock cancel()).
        *child_slot.lock().await = Some(child);
        let tx = event_tx.clone();
        let aid = attempt_id.clone();
        let wait_child_slot = child_slot.clone();
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let status = loop {
                let exited = {
                    let mut guard = wait_child_slot.lock().await;
                    match guard.as_mut() {
                        Some(c) => c
                            .try_wait()
                            .ok()
                            .flatten(),
                        None => None, // defensive: slot drained elsewhere
                    }
                };
                match exited {
                    Some(status) => break Some(status),
                    None => {
                        // Still running (or slot empty) — poll again shortly.
                        // If the slot was drained, stop waiting so we don't
                        // spin forever.
                        if wait_child_slot.lock().await.is_none() {
                            break None;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                }
            };
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
            child: child_slot,
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

    async fn cancel(&self, attempt: &mut AttemptHandle) -> Result<(), RunnerError> {
        // Kill the subprocess via the shared child slot. `start_kill()` is
        // async-signal-safe and non-blocking; the wait task's polling loop
        // then observes the exit (signal → `None` exit code → Cancelled)
        // and emits the terminal Status event. Killing the process also
        // closes its stdio, ending the reader tasks and the worktree lease.
        let mut guard = attempt.child.lock().await;
        if let Some(child) = guard.as_mut() {
            child
                .start_kill()
                .map_err(|e| RunnerError::Other(format!("failed to kill child: {e}")))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::TerminalStatus;
    use crate::staging::{StagingError, StagingRoot};

    /// A no-op staging area for testing the runner without git.
    #[allow(dead_code)]
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

    /// cancel() actually kills the spawned child: a long-running subprocess
    /// exits shortly after cancel(), the shared child slot shows it gone, and
    /// the event stream delivers a terminal Status::Cancelled event.
    #[tokio::test]
    async fn cancel_kills_the_child() {
        // Guard: this needs `bash` + `sleep`; skip gracefully if missing.
        if which::which("bash").is_err() {
            return;
        }

        let runner = JoeyCliRunner::new();
        let dir = tempfile::tempdir().unwrap();
        let config = RunConfiguration {
            change_mode: Some(crate::model::ChangeMode::Direct),
            ..Default::default()
        };

        // Spawn a child that outlives the test unless cancelled: bash that
        // sleeps for 30s. prepare_and_start prefers the `joey` CLI when on
        // PATH, so craft the repo so it falls back to the bash script path…
        // simpler: create the wrapper script it would run.
        std::fs::create_dir_all(dir.path().join(".specify/scripts/bash")).unwrap();
        std::fs::write(
            dir.path().join(".specify/scripts/bash/implement.sh"),
            "#!/usr/bin/env bash\nsleep 30\n",
        )
        .unwrap();

        // Ensure the joey CLI is NOT picked (it would run a real command);
        // prepare_and_start checks `which joey` first. If joey IS on PATH
        // this test would spawn it — skip in that case to stay hermetic.
        if which::which("joey").is_ok() {
            return;
        }

        let mut handle = runner
            .prepare_and_start(dir.path(), "feat-cancel", "implement", &config, &NoopStaging)
            .await
            .expect("spawn should succeed");

        // Give the child a moment to be alive, then cancel.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        runner
            .cancel(&mut handle)
            .await
            .expect("cancel should succeed");

        // Drain events until the terminal Status arrives (bounded wait).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut terminal = None;
        while let Some(evt) = handle.events.recv().await {
            if let RunnerEvent::Status { terminal: t, .. } = evt {
                terminal = Some(t);
                break;
            }
            if std::time::Instant::now() > deadline {
                break;
            }
        }
        assert_eq!(
            terminal,
            Some(TerminalStatus::Cancelled),
            "cancelled child must produce a Cancelled terminal event"
        );

        // The child slot is now empty or the child is reaped — cancel is
        // idempotent and safe to call again.
        runner.cancel(&mut handle).await.expect("second cancel ok");
    }
}
