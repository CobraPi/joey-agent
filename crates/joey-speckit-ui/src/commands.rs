//! Thin wrappers that shell out to the existing `specify` CLI / SpecKit
//! bash scripts for clarify/analyze/implement-equivalent actions.
//!
//! These functions never reimplement SpecKit's own logic — they invoke it
//! as a subprocess and capture stdout/stderr, per plan.md ("shells out to
//! existing `specify` CLI / .specify/scripts/bash/*.sh scripts").

use std::path::Path;
use std::process::Output;

use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CommandResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for CommandResult {
    fn from(output: Output) -> Self {
        CommandResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }
}

/// Locate a `.specify/scripts/bash/<name>.sh` script under `repo_root`, if
/// present.
fn script_path(repo_root: &Path, name: &str) -> Option<std::path::PathBuf> {
    let candidate = repo_root
        .join(".specify")
        .join("scripts")
        .join("bash")
        .join(format!("{name}.sh"));
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Run `.specify/scripts/bash/<name>.sh <args>` if present, else fall back
/// to the `specify` CLI subcommand `specify <name> <args>`.
async fn run_script_or_cli(
    repo_root: &Path,
    script_name: &str,
    cli_subcommand: &str,
    args: &[&str],
) -> anyhow::Result<CommandResult> {
    let span = tracing::info_span!("run_script_or_cli", script_name, cli_subcommand);
    let _enter = span.enter();

    if let Some(script) = script_path(repo_root, script_name) {
        tracing::info!(script = %script.display(), "invoking speckit bash script");
        let output = Command::new("bash")
            .arg(&script)
            .args(args)
            .current_dir(repo_root)
            .output()
            .await?;
        return Ok(output.into());
    }

    tracing::info!(subcommand = cli_subcommand, "invoking specify CLI");
    let output = Command::new("specify")
        .arg(cli_subcommand)
        .args(args)
        .current_dir(repo_root)
        .output()
        .await?;
    Ok(output.into())
}

/// Equivalent of `/speckit-clarify` for a feature: kicks off a clarification
/// pass. Actual multi-turn Q/A streaming is handled by the caller via the
/// WebSocket session layer (api/ws.rs); this just invokes the underlying
/// tooling and captures its output.
pub async fn run_clarify(repo_root: &Path, feature_id: &str) -> anyhow::Result<CommandResult> {
    run_script_or_cli(repo_root, "clarify", "clarify", &[feature_id]).await
}

/// Equivalent of `/speckit-analyze` for a feature.
pub async fn run_analyze(repo_root: &Path, feature_id: &str) -> anyhow::Result<CommandResult> {
    run_script_or_cli(repo_root, "analyze", "analyze", &[feature_id]).await
}

/// Equivalent of `/speckit-implement` scoped to a single task id. Per
/// Clarifications Q3, this must never cascade to other tasks — callers are
/// responsible for passing only the single `task_id`.
pub async fn run_implement_task(
    repo_root: &Path,
    feature_id: &str,
    task_id: &str,
) -> anyhow::Result<CommandResult> {
    run_script_or_cli(
        repo_root,
        "implement",
        "implement",
        &[feature_id, "--task", task_id],
    )
    .await
}

/// Equivalent of `specify init --here --integration <agent> --script <type>`.
pub async fn run_init(
    repo_root: &Path,
    integration: &str,
    script: &str,
) -> anyhow::Result<CommandResult> {
    let span = tracing::info_span!("run_init", integration, script);
    let _enter = span.enter();
    let output = Command::new("specify")
        .arg("init")
        .arg("--here")
        .arg("--integration")
        .arg(integration)
        .arg("--script")
        .arg(script)
        .current_dir(repo_root)
        .output()
        .await?;
    Ok(output.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn script_path_none_when_absent() {
        let dir = tempdir().unwrap();
        assert!(script_path(dir.path(), "clarify").is_none());
    }

    #[test]
    fn script_path_some_when_present() {
        let dir = tempdir().unwrap();
        let script_dir = dir.path().join(".specify/scripts/bash");
        std::fs::create_dir_all(&script_dir).unwrap();
        std::fs::write(script_dir.join("clarify.sh"), "#!/bin/bash\necho ok\n").unwrap();
        assert!(script_path(dir.path(), "clarify").is_some());
    }
}
