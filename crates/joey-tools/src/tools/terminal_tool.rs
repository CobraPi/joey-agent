//! Terminal tool: run shell commands on the local backend
//! (port of `tools/terminal_tool.py` + `tools/environments/local.py`, local backend).

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::context::ToolContext;
use crate::registry::{tool_error, Tool, ToolResult};
use crate::truncate;

/// Default foreground timeout (upstream `terminal.timeout` = 180s).
const DEFAULT_TIMEOUT_SECS: u64 = 180;
/// Hard ceiling on a foreground command (upstream `FOREGROUND_MAX_TIMEOUT`).
const MAX_TIMEOUT_SECS: u64 = 600;

pub struct Terminal;

#[async_trait]
impl Tool for Terminal {
    fn name(&self) -> &str {
        "terminal"
    }
    fn toolset(&self) -> &str {
        "terminal"
    }
    fn description(&self) -> &str {
        "Run a shell command in the session's working directory and return its \
         combined stdout/stderr and exit code. Long output is truncated (head+tail)."
    }
    fn emoji(&self) -> &str {
        "💻"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command to run."},
                "workdir": {"type": "string", "description": "Working directory (default: session cwd)."},
                "timeout": {"type": "integer", "description": "Timeout in seconds.", "default": 180}
            },
            "required": ["command"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return tool_error("missing required parameter: command");
        };
        let timeout = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);
        let workdir = args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(|w| ctx.resolve_path(w))
            .unwrap_or_else(|| ctx.cwd().to_path_buf());

        let shell = if cfg!(windows) { "cmd" } else { "bash" };
        let shell_arg = if cfg!(windows) { "/C" } else { "-c" };

        let mut cmd = Command::new(shell);
        cmd.arg(shell_arg)
            .arg(command)
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return tool_error(format!("failed to spawn command: {}", e)),
        };

        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();

        let run = async {
            let mut out = String::new();
            let mut err = String::new();
            if let Some(so) = stdout.as_mut() {
                let _ = so.read_to_string(&mut out).await;
            }
            if let Some(se) = stderr.as_mut() {
                let _ = se.read_to_string(&mut err).await;
            }
            let status = child.wait().await;
            (out, err, status)
        };

        match tokio::time::timeout(Duration::from_secs(timeout), run).await {
            Ok((out, err, status)) => {
                let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
                let mut combined = String::new();
                if !out.is_empty() {
                    combined.push_str(&out);
                }
                if !err.is_empty() {
                    if !combined.is_empty() && !combined.ends_with('\n') {
                        combined.push('\n');
                    }
                    combined.push_str(&err);
                }
                let truncated = truncate::bounded_head_tail(&combined, truncate::DEFAULT_MAX_BYTES);
                let redacted = joey_core::redact::redact_secrets(&truncated);
                let body = if redacted.trim().is_empty() {
                    format!("(no output)\n[exit code: {}]", code)
                } else {
                    format!("{}\n[exit code: {}]", redacted.trim_end(), code)
                };
                ToolResult::Text(body)
            }
            Err(_) => {
                // Timed out — kill the child.
                let _ = child.start_kill();
                tool_error(format!("command timed out after {}s", timeout))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    #[tokio::test]
    async fn runs_echo() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), Config::defaults(), "t");
        let r = Terminal.execute(json!({"command": "echo hello"}), &ctx).await;
        let s = r.to_content_string();
        assert!(s.contains("hello"));
        assert!(s.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn reports_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), Config::defaults(), "t");
        let r = Terminal.execute(json!({"command": "exit 3"}), &ctx).await;
        assert!(r.to_content_string().contains("exit code: 3"));
    }
}
