//! Terminal tool: run shell commands on the local backend — port of
//! `tools/terminal_tool.py` + the local pieces of `tools/environments/`.
//!
//! Faithful behaviors: the full TERMINAL_SCHEMA (background / pty /
//! notify_on_complete / watch_patterns parameters), bash execution with
//! stderr merged into stdout on a single pipe, sanitized subprocess env,
//! session-persistent cwd, head/tail output truncation with the upstream
//! marker, ANSI stripping, secret redaction, the exit-code-meaning table, and
//! the timeout contract (default 180s, hard foreground max 600s).

use std::io::Read;
use std::time::Duration;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use serde_json::{json, Map, Value};

use crate::context::ToolContext;
use crate::guards::strip_ansi;
use crate::pyjson::dumps;
use crate::registry::{Tool, ToolResult};
use crate::truncate;

/// Hard cap on foreground timeout; override via TERMINAL_MAX_FOREGROUND_TIMEOUT.
fn foreground_max_timeout() -> u64 {
    std::env::var("TERMINAL_MAX_FOREGROUND_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(600)
}

/// Default foreground timeout: `terminal.timeout` config / TERMINAL_TIMEOUT env,
/// falling back to 180.
fn default_timeout(ctx: &ToolContext) -> u64 {
    if let Ok(v) = std::env::var("TERMINAL_TIMEOUT") {
        if let Ok(n) = v.parse::<u64>() {
            return n;
        }
    }
    ctx.config().get_i64("terminal.timeout", 180).max(1) as u64
}

/// Port of `tools/environments/local._find_bash` (POSIX branch; the Windows
/// Git-Bash candidate walk is reduced to a PATH probe).
fn find_bash() -> String {
    if cfg!(windows) {
        return which::which("bash")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "bash".to_string());
    }
    if let Ok(p) = which::which("bash") {
        return p.to_string_lossy().into_owned();
    }
    for candidate in ["/usr/bin/bash", "/bin/bash"] {
        if std::path::Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Tier-1 secrets stripped from EVERY spawned subprocess
/// (`tools/environments/local._ALWAYS_STRIP_KEYS`, rebranded).
const ALWAYS_STRIP_KEYS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GITHUB_APP_ID",
    "GITHUB_APP_PRIVATE_KEY_PATH",
    "GITHUB_APP_INSTALLATION_ID",
    "TELEGRAM_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "SLACK_SIGNING_SECRET",
    "GATEWAY_ALLOWED_USERS",
    "GATEWAY_ALLOW_ALL_USERS",
    "GATEWAY_RELAY_ID",
    "GATEWAY_RELAY_SECRET",
    "GATEWAY_RELAY_DELIVERY_KEY",
    "HASS_TOKEN",
    "EMAIL_PASSWORD",
    "JOEY_DASHBOARD_SESSION_TOKEN",
    "MODAL_TOKEN_ID",
    "MODAL_TOKEN_SECRET",
    "DAYTONA_API_KEY",
];

/// Build the sanitized subprocess environment (port of
/// `_sanitize_subprocess_env`: strip Joey-managed secrets, apply the HOME
/// contract via `joey_core::constants::apply_subprocess_home_env`).
fn sanitized_env() -> indexmap::IndexMap<String, String> {
    let mut env: indexmap::IndexMap<String, String> = std::env::vars().collect();
    for key in ALWAYS_STRIP_KEYS {
        env.shift_remove(*key);
    }
    let internal: Vec<String> = env
        .keys()
        .filter(|k| {
            k.starts_with("JOEY_PROVIDER_FORCE_")
                || (k.starts_with("AUXILIARY_")
                    && (k.ends_with("_API_KEY") || k.ends_with("_BASE_URL")))
        })
        .cloned()
        .collect();
    for key in internal {
        env.shift_remove(&key);
    }
    joey_core::constants::apply_subprocess_home_env(&mut env);
    env
}

/// Port of `_interpret_exit_code` — human-readable notes for non-erroneous
/// non-zero exit codes.
fn interpret_exit_code(command: &str, exit_code: i64) -> Option<&'static str> {
    if exit_code == 0 {
        return None;
    }
    static SPLIT_RE: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(r"\s*(?:\|\||&&|[|;])\s*").unwrap());
    let segments: Vec<&str> = SPLIT_RE.split(command).collect();
    let last_segment = segments.last().copied().unwrap_or(command).trim();
    let mut base_cmd = "";
    for w in last_segment.split_whitespace() {
        if w.contains('=') && !w.starts_with('-') {
            continue; // skip VAR=val
        }
        base_cmd = w.rsplit('/').next().unwrap_or(w);
        break;
    }
    if base_cmd.is_empty() {
        return None;
    }
    let note = match (base_cmd, exit_code) {
        ("grep", 1) | ("egrep", 1) | ("fgrep", 1) | ("rg", 1) | ("ag", 1) | ("ack", 1) => {
            "No matches found (not an error)"
        }
        ("diff", 1) | ("colordiff", 1) => "Files differ (expected, not an error)",
        ("find", 1) => "Some directories were inaccessible (partial results may still be valid)",
        ("test", 1) | ("[", 1) => "Condition evaluated to false (expected, not an error)",
        ("curl", 6) => "Could not resolve host",
        ("curl", 7) => "Failed to connect to host",
        ("curl", 22) => "HTTP response code indicated error (e.g. 404, 500)",
        ("curl", 28) => "Operation timed out",
        ("git", 1) => "Non-zero exit (often normal — e.g. 'git diff' returns 1 when files differ)",
        _ => return None,
    };
    Some(note)
}

/// Local redaction shim for terminal output (upstream routes through
/// `agent.redact.redact_terminal_output`, which is env-dump aware; the port's
/// joey-core exposes only `redact_secrets`, so both paths use it).
fn redact_terminal_output(output: &str, _command: &str) -> String {
    joey_core::redact::redact_secrets(output)
}

const CWD_MARKER: &str = "__JOEY_CWD_MARKER__";

pub struct Terminal;

static DESCRIPTION: Lazy<String> = Lazy::new(|| {
    "Execute shell commands on a Linux environment. Filesystem, current working directory, and exported environment variables persist between calls.\n\nDo NOT use cat/head/tail to read files — use read_file instead.\nDo NOT use grep/rg/find to search — use search_files instead.\nDo NOT use ls to list directories — use search_files(target='files') instead.\nDo NOT use sed/awk to edit files — use patch instead.\nDo NOT use echo/cat heredoc to create files — use write_file instead.\nReserve terminal for: builds, installs, git, processes, scripts, network, package managers, and anything that needs a shell.\nBecause exported environment state persists, activate a virtualenv or export setup variables once per session; do not re-source the same environment before every command unless a command proves the shell state was reset.\n\nForeground (default): Commands return INSTANTLY when done, even if the timeout is high. Set timeout=300 for long builds/scripts — you'll still get the result in seconds if it's fast. Prefer foreground for short commands.\nBackground: Set background=true to get a session_id. Almost always pair with notify_on_complete=true — bg without notify runs SILENTLY and you have no way to learn it finished short of calling process(action='poll') yourself. Two legitimate uses:\n  (1) Long-lived processes that never exit (servers, watchers, daemons) — silent is correct, there's no exit to notify on.\n  (2) Long-running bounded tasks (tests, builds, deploys, CI pollers, batch jobs) — MUST set notify_on_complete=true. Without it you'll either forget to poll or sit blocked waiting for the user to surface the result.\nFor servers/watchers, do NOT use shell-level background wrappers (nohup/disown/setsid/trailing '&') in foreground mode. Use background=true so Joey can track lifecycle and output.\nAfter starting a server, verify readiness with a health check or log signal, then run tests in a separate terminal() call. Avoid blind sleep loops.\nUse process(action=\"poll\") for progress checks, process(action=\"wait\") to block until done.\nWorking directory: Use 'workdir' for per-command cwd.\nPTY mode: Set pty=true for interactive CLI tools (Codex, Claude Code, Python REPL).\n\nDo NOT use vim/nano/interactive tools without pty=true — they hang without a pseudo-terminal. Pipe git output to cat if it might page.\n".to_string()
});

#[async_trait]
impl Tool for Terminal {
    fn name(&self) -> &str {
        "terminal"
    }
    fn toolset(&self) -> &str {
        "terminal"
    }
    fn description(&self) -> &str {
        &DESCRIPTION
    }
    fn emoji(&self) -> &str {
        "💻"
    }
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    fn parameters(&self) -> Value {
        let fg_max = foreground_max_timeout();
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute on the VM"
                },
                "background": {
                    "type": "boolean",
                    "description": "Run the command in the background. Almost always pair with notify_on_complete=true — without it, the process runs silently and you'll have no way to learn it finished short of calling process(action='poll') yourself (easy to forget, leading to silent blindness on long jobs). Two legitimate patterns: (1) Long-lived processes that never exit (servers, watchers, daemons) — these stay silent because there's no exit to notify on. (2) Long-running bounded tasks (tests, builds, deploys, CI pollers, batch jobs) — these MUST set notify_on_complete=true. For short commands, prefer foreground with a generous timeout instead.",
                    "default": false
                },
                "timeout": {
                    "type": "integer",
                    "description": format!("Max seconds to wait (default: 180, foreground max: {fg_max}). Returns INSTANTLY when command finishes — set high for long tasks, you won't wait unnecessarily. Foreground timeout above {fg_max}s is rejected; use background=true for longer commands."),
                    "minimum": 1
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory for this command (absolute path). Defaults to the session working directory."
                },
                "pty": {
                    "type": "boolean",
                    "description": "Run in pseudo-terminal (PTY) mode for interactive CLI tools like Codex, Claude Code, or Python REPL. Only works with local and SSH backends. Default: false.",
                    "default": false
                },
                "notify_on_complete": {
                    "type": "boolean",
                    "description": "When true (and background=true), you'll be automatically notified exactly once when the process finishes. **This is the right choice for almost every long-running task** — tests, builds, deployments, multi-item batch jobs, anything that takes over a minute and has a defined end. Use this and keep working on other things; the system notifies you on exit. MUTUALLY EXCLUSIVE with watch_patterns — when both are set, watch_patterns is dropped.",
                    "default": false
                },
                "watch_patterns": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Strings to watch for in background process output. HARD RATE LIMIT: at most 1 notification per 15 seconds per process — matches arriving inside the cooldown are dropped. After 3 consecutive 15-second windows with dropped matches, watch_patterns is automatically disabled for that process and promoted to notify_on_complete behavior (one notification on exit, no more mid-process spam). USE ONLY for truly rare, one-shot mid-process signals on LONG-LIVED processes that will never exit on their own — e.g. ['Application startup complete'] on a server so you know when to hit its endpoint, or ['migration done'] on a daemon. DO NOT use for: (1) end-of-run markers like 'DONE'/'PASS' — use notify_on_complete instead; (2) error patterns like 'ERROR'/'Traceback' in loops or multi-item batch jobs — they fire on every iteration and you'll hit the strike limit fast; (3) anything you'd ever combine with notify_on_complete. When in doubt, choose notify_on_complete. MUTUALLY EXCLUSIVE with notify_on_complete — set one, not both."
                }
            },
            "required": ["command"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let Some(command) = args.get("command").and_then(|v| v.as_str()).map(str::to_string)
        else {
            return ToolResult::Text(dumps(&json!({
                "output": "",
                "exit_code": -1,
                "error": "Failed to execute command: command is required",
                "status": "error",
            })));
        };
        let background = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
        let pty = args.get("pty").and_then(|v| v.as_bool()).unwrap_or(false);
        let timeout_arg = args.get("timeout").and_then(|v| v.as_i64()).map(|t| t as u64);
        let workdir = args.get("workdir").and_then(|v| v.as_str()).map(str::to_string);

        // Honest not-supported stubs: the background process registry and PTY
        // driver are not ported yet; never silently run these as foreground.
        if background {
            return ToolResult::Text(dumps(&json!({
                "output": "",
                "exit_code": -1,
                "error": "background=true is not supported in this build: the process tool (background session registry) is unavailable. Run the command in the foreground with a generous timeout instead.",
                "status": "error",
            })));
        }
        if pty {
            return ToolResult::Text(dumps(&json!({
                "output": "",
                "exit_code": -1,
                "error": "pty=true is not supported in this build: the PTY session driver is unavailable. Run the command without pty, or use a non-interactive invocation.",
                "status": "error",
            })));
        }

        let fg_max = foreground_max_timeout();
        if let Some(t) = timeout_arg {
            if t > fg_max {
                return ToolResult::Text(dumps(&json!({
                    "error": format!(
                        "Foreground timeout {}s exceeds the maximum of {}s. Use background=true with notify_on_complete=true for long-running commands.",
                        t, fg_max
                    ),
                })));
            }
        }
        let effective_timeout = timeout_arg.unwrap_or_else(|| default_timeout(ctx));

        let cwd = match &workdir {
            Some(w) => ctx.resolve_path(w),
            None => ctx.effective_cwd(),
        };

        let (raw_output, returncode, timed_out) =
            run_bash(&command, &cwd, effective_timeout).await;

        // Spawn/exec failures surface in the error field (upstream:
        // {"output": "", "exit_code": -1, "error": "Command execution failed: ..."}).
        if returncode == -1 && !timed_out && raw_output.starts_with("Failed to ") {
            return ToolResult::Text(dumps(&json!({
                "output": "",
                "exit_code": -1,
                "error": format!("Command execution failed: {}", raw_output),
            })));
        }

        // Record the session's live cwd from the trailing marker.
        let (mut output, new_cwd) = extract_cwd_marker(&raw_output);
        if let Some(dir) = new_cwd {
            let p = std::path::PathBuf::from(dir);
            if p.is_dir() {
                ctx.state().terminal_cwd = Some(p);
            }
        }

        if timed_out {
            output.push_str(&format!("\n[Command timed out after {}s]", effective_timeout));
        }

        // Truncate output if too long, keeping both head and tail.
        let limits = truncate::get_tool_output_limits(ctx.config());
        output = truncate::truncate_terminal_output(&output, limits.max_bytes);
        // Strip ANSI escape sequences.
        output = strip_ansi(&output);
        // Redact secrets from command output.
        output = if output.is_empty() {
            output
        } else {
            redact_terminal_output(output.trim(), &command)
        };

        let exit_note = interpret_exit_code(&command, returncode);

        let mut result = Map::new();
        result.insert("output".into(), json!(output));
        result.insert("exit_code".into(), json!(returncode));
        if timed_out {
            result.insert(
                "error".into(),
                json!(format!("Command timed out after {} seconds", effective_timeout)),
            );
        } else {
            result.insert("error".into(), Value::Null);
        }
        if let Some(note) = exit_note {
            result.insert("exit_code_meaning".into(), json!(note));
        }
        ToolResult::Text(dumps(&Value::Object(result)))
    }
}

/// Extract the trailing cwd marker printed by the wrapper script (the LAST
/// marker pair, so command output containing the marker text can't confuse it).
fn extract_cwd_marker(raw: &str) -> (String, Option<String>) {
    if let Some(close) = raw.rfind(CWD_MARKER) {
        if let Some(open) = raw[..close].rfind(CWD_MARKER) {
            let cwd = raw[open + CWD_MARKER.len()..close].to_string();
            let mut cleaned = String::new();
            cleaned.push_str(&raw[..open]);
            cleaned.push_str(&raw[close + CWD_MARKER.len()..]);
            // The wrapper prints "\n<marker>cwd<marker>" — drop that newline.
            if cleaned.ends_with('\n') {
                cleaned.pop();
            }
            return (cleaned, Some(cwd));
        }
    }
    (raw.to_string(), None)
}

/// Run `command` under bash with stderr merged into stdout on a single pipe
/// (os_pipe), a sanitized environment, and a timeout. Returns
/// (combined_output, exit_code, timed_out).
async fn run_bash(command: &str, cwd: &std::path::Path, timeout_secs: u64) -> (String, i64, bool) {
    let bash = find_bash();
    // Wrapper: preserve $? of the user command, then print the live cwd.
    let script = format!(
        "{command}\n__JOEY_STATUS=$?\nprintf '\\n{m}%s{m}' \"$PWD\"\nexit $__JOEY_STATUS",
        command = command,
        m = CWD_MARKER
    );

    let (mut reader, writer) = match os_pipe::pipe() {
        Ok(p) => p,
        Err(e) => return (format!("Failed to execute command: {}", e), -1, false),
    };
    let writer2 = match writer.try_clone() {
        Ok(w) => w,
        Err(e) => return (format!("Failed to execute command: {}", e), -1, false),
    };

    let mut cmd = tokio::process::Command::new(&bash);
    cmd.arg("-c")
        .arg(&script)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(writer))
        .stderr(std::process::Stdio::from(writer2));
    cmd.env_clear();
    for (k, v) in sanitized_env() {
        cmd.env(k, v);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (format!("Failed to spawn command: {}", e), -1, false),
    };
    // Parent must drop its writer ends or the reader never sees EOF.
    drop(cmd);

    let read_task = tokio::task::spawn_blocking(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });

    let mut timed_out = false;
    let status = match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(_)) => None,
        Err(_) => {
            timed_out = true;
            let _ = child.start_kill();
            let _ = child.wait().await;
            None
        }
    };

    let output = read_task.await.unwrap_or_default();
    let code: i64 = match status {
        Some(s) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                s.code().map(|c| c as i64).unwrap_or_else(|| -(s.signal().unwrap_or(1) as i64))
            }
            #[cfg(not(unix))]
            {
                s.code().map(|c| c as i64).unwrap_or(-1)
            }
        }
        None if timed_out => 124,
        None => -1,
    };
    (output, code, timed_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    fn ctx() -> ToolContext {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf(), Config::defaults(), "t");
        std::mem::forget(dir); // keep alive for test duration
        ctx
    }

    fn parse(r: &ToolResult) -> Value {
        serde_json::from_str(&r.to_content_string()).unwrap()
    }

    #[tokio::test]
    async fn envelope_shape() {
        let c = ctx();
        let v = parse(&Terminal.execute(json!({"command": "echo hello"}), &c).await);
        assert_eq!(v["output"], "hello");
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["error"], Value::Null);
    }

    #[tokio::test]
    async fn merged_stderr_and_exit_code() {
        let c = ctx();
        let v = parse(
            &Terminal.execute(json!({"command": "echo out; echo err >&2; exit 3"}), &c).await,
        );
        let out = v["output"].as_str().unwrap();
        assert!(out.contains("out"));
        assert!(out.contains("err"));
        assert_eq!(v["exit_code"], 3);
    }

    #[tokio::test]
    async fn cwd_persists_between_calls() {
        let c = ctx();
        let sub = c.cwd().join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        let _ = Terminal.execute(json!({"command": "cd subdir"}), &c).await;
        let v = parse(&Terminal.execute(json!({"command": "pwd"}), &c).await);
        assert!(v["output"].as_str().unwrap().ends_with("subdir"));
    }

    #[tokio::test]
    async fn exit_code_meaning_table() {
        let c = ctx();
        let v = parse(&Terminal.execute(json!({"command": "grep zz /dev/null"}), &c).await);
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["exit_code_meaning"], "No matches found (not an error)");
        assert_eq!(interpret_exit_code("diff a b", 1), Some("Files differ (expected, not an error)"));
        assert_eq!(interpret_exit_code("curl http://x", 7), Some("Failed to connect to host"));
        assert_eq!(interpret_exit_code("ls | grep x", 1), Some("No matches found (not an error)"));
        assert_eq!(interpret_exit_code("false", 1), None);
    }

    #[tokio::test]
    async fn timeout_keeps_partial_output() {
        let c = ctx();
        let v = parse(
            &Terminal
                .execute(json!({"command": "echo before; sleep 5; echo after", "timeout": 1}), &c)
                .await,
        );
        assert_eq!(v["exit_code"], 124);
        assert_eq!(v["error"], "Command timed out after 1 seconds");
        let out = v["output"].as_str().unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("[Command timed out after 1s]"));
    }

    #[tokio::test]
    async fn rejects_oversized_foreground_timeout() {
        let c = ctx();
        let v = parse(&Terminal.execute(json!({"command": "true", "timeout": 9999}), &c).await);
        assert_eq!(
            v["error"],
            "Foreground timeout 9999s exceeds the maximum of 600s. Use background=true with notify_on_complete=true for long-running commands."
        );
    }

    #[tokio::test]
    async fn background_and_pty_are_honest_stubs() {
        let c = ctx();
        let bg = parse(&Terminal.execute(json!({"command": "true", "background": true}), &c).await);
        assert!(bg["error"].as_str().unwrap().contains("process tool"));
        let pty = parse(&Terminal.execute(json!({"command": "true", "pty": true}), &c).await);
        assert!(pty["error"].as_str().unwrap().contains("pty=true is not supported"));
    }

    #[tokio::test]
    async fn ansi_is_stripped() {
        let c = ctx();
        let v = parse(
            &Terminal.execute(json!({"command": "printf '\\033[31mred\\033[0m\\n'"}), &c).await,
        );
        assert_eq!(v["output"], "red");
    }
}
