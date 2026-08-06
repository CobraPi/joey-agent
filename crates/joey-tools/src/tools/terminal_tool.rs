//! Terminal tool: run shell commands on the local backend — port of
//! `tools/terminal_tool.py` + the local pieces of `tools/environments/`.
//!
//! Faithful behaviors: the full TERMINAL_SCHEMA (background / pty /
//! notify_on_complete / watch_patterns parameters), bash execution with
//! stderr merged into stdout on a single pipe, sanitized subprocess env,
//! session-persistent cwd, head/tail output truncation with the upstream
//! marker, ANSI stripping, secret redaction, the exit-code-meaning table, and
//! the timeout contract (default 180s, hard foreground max 600s).

#[cfg(unix)]
use std::os::unix::io::AsRawFd as _;
use std::time::Duration;

use crate::file_tracker::FileTracker;

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

// ── Shell selection (feature 014: Windows platform support) ──────────────

/// Which shell the terminal tool uses to execute a command. Resolved once
/// per process and cached in `RESOLVED_SHELL` (FR-013).
///
/// On Unix, only `Bash` is ever produced (POSIX path). On Windows, `Bash`
/// is preferred (Git Bash if installed); `PowerShell` is the fallback
/// when bash is absent (FR-011).
#[derive(Clone, Debug, PartialEq)]
enum Shell {
    /// POSIX shell (bash, Git Bash). Path to the executable.
    Bash(String),
    /// PowerShell. Path to pwsh.exe or powershell.exe.
    PowerShell(String),
}

impl Shell {
    /// The executable path to spawn.
    fn argv0(&self) -> &str {
        match self {
            Shell::Bash(p) => p,
            Shell::PowerShell(p) => p,
        }
    }
}

/// Error returned when no usable shell is found on the system. Names the
/// shells that were probed so the terminal tool can surface a clear message
/// (FR-011: "no panic").
#[derive(Debug)]
struct ShellResolutionError {
    /// Shell names probed, in resolution order.
    tried: Vec<&'static str>,
}

impl std::fmt::Display for ShellResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "No usable shell found. Tried: {}. Install Git Bash (recommended) or PowerShell.",
            self.tried.join(", ")
        )
    }
}

impl std::error::Error for ShellResolutionError {}

/// Resolve which shell to use, following the platform-specific order
/// (contracts/shell-discovery.md):
///
/// - **Unix**: `bash` on PATH → `/usr/bin/bash` → `/bin/bash` → `$SHELL`
///   → `/bin/sh`. Always returns `Shell::Bash`.
/// - **Windows**: `bash` (Git Bash) → `pwsh` (PowerShell 7+) → `powershell`
///   (built-in Windows PowerShell). Returns `Shell::Bash` or
///   `Shell::PowerShell`, or `Err` if none found.
fn resolve_shell() -> Result<Shell, ShellResolutionError> {
    #[cfg(unix)]
    {
        if let Ok(p) = which::which("bash") {
            return Ok(Shell::Bash(p.to_string_lossy().into_owned()));
        }
        for candidate in ["/usr/bin/bash", "/bin/bash"] {
            if std::path::Path::new(candidate).is_file() {
                return Ok(Shell::Bash(candidate.to_string()));
            }
        }
        // Final fallback — always succeed on Unix (sh is guaranteed).
        Ok(Shell::Bash(
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
        ))
    }

    #[cfg(not(unix))]
    {
        if let Ok(p) = which::which("bash") {
            return Ok(Shell::Bash(p.to_string_lossy().into_owned()));
        }
        if let Ok(p) = which::which("pwsh") {
            return Ok(Shell::PowerShell(p.to_string_lossy().into_owned()));
        }
        if let Ok(p) = which::which("powershell") {
            return Ok(Shell::PowerShell(p.to_string_lossy().into_owned()));
        }
        Err(ShellResolutionError {
            tried: vec!["bash", "pwsh", "powershell"],
        })
    }
}

/// Per-process cache of the resolved shell (FR-013). Once a shell is
/// chosen, the session reuses it on every subsequent terminal call without
/// re-probing, so a session never flips between bash and PowerShell.
static RESOLVED_SHELL: Lazy<std::sync::Mutex<Option<Shell>>> =
    Lazy::new(|| std::sync::Mutex::new(None));

/// Return the cached shell, resolving + caching it on first call. Maps a
/// resolution failure to `None` (callers handle the absent-shell case).
fn cached_shell() -> Option<Shell> {
    let mut guard = RESOLVED_SHELL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(ref shell) = *guard {
        return Some(shell.clone());
    }
    match resolve_shell() {
        Ok(shell) => {
            *guard = Some(shell.clone());
            Some(shell)
        }
        Err(_) => None,
    }
}

// ── OutputChunkStream boundary (feature 014, FR-005) ─────────────────────
//
// Conceptual interface (NOT a `dyn` trait — two concrete cfg-selected types):
//
//   async fn next_chunk(&mut self) -> Option<Vec<u8>>;
//
// Invariants (both concrete impls):
//   - Returns Some(bytes) for each read chunk (≤ 64 KB).
//   - Returns None exactly once at EOF.
//   - Does NOT decode UTF-8 (caller does lossy decode) — bytes in, bytes out.
//   - Does NOT throttle or spill to disk (that's stream_output's job).
//
// Concrete impls:
//   - Unix  (`#[cfg(unix)]`):  UnixFdReader      — landed in US1 (T008)
//   - Win   (`#[cfg(not(unix))]`): WindowsPipeReader — landed in US2 (T013)
//
// The shared `stream_output` body operates on this boundary, not on
// AsyncFd directly, so it compiles cross-platform.

// ── WrapperScript (feature 014, FR-012) ───────────────────────────────────

/// A generated wrapper script that wraps the user's command to capture the
/// exit code and emit the CWD marker. Generated per-call from `Shell` +
/// user command; consumed immediately by the spawn path.
#[derive(Clone, Debug)]
struct WrapperScript {
    /// Dialect that generated this script (drives arg shape).
    shell: Shell,
    /// Shell executable path (shorthand for `shell.argv0()`).
    argv0: String,
    /// Args to pass after argv0: bash → ["-c", body]; PowerShell →
    /// ["-NoProfile", "-Command", body].
    args: Vec<String>,
    /// The full script text incl. user command + marker framing.
    body: String,
}

/// Build the wrapper script for the resolved shell + user command.
///
/// - **Bash** (unchanged from the original `run_bash` wrapper,
///   terminal_tool.rs:476–480): captures `$?`, prints `$PWD` between
///   `CWD_MARKER` framing, exits with the captured code.
/// - **PowerShell** (new, FR-012): captures `$LASTEXITCODE` (falling back
///   to `$?`), prints `$PWD` between `CWD_MARKER` framing, exits with the
///   code. `-NoProfile` avoids the multi-hundred-ms profile-load penalty.
fn build_wrapper_script(shell: &Shell, command: &str, marker: &str) -> WrapperScript {
    match shell {
        Shell::Bash(path) => {
            let body = format!(
                "{command}\n__JOEY_STATUS=$?\nprintf '\\n{m}%s{m}' \"$PWD\"\nexit $__JOEY_STATUS",
                command = command,
                m = marker
            );
            WrapperScript {
                shell: shell.clone(),
                argv0: path.clone(),
                args: vec!["-c".to_string(), body.clone()],
                body,
            }
        }
        Shell::PowerShell(path) => {
            let body = format!(
                "{command}\n$code = $LASTEXITCODE\nif ($code -eq $null) {{ $code = if ($?) {{ 0 }} else {{ 1 }} }}\nWrite-Output \"`n{m}$PWD{m}\"\nexit $code",
                command = command,
                m = marker
            );
            WrapperScript {
                shell: shell.clone(),
                argv0: path.clone(),
                args: vec!["-NoProfile".to_string(), "-Command".to_string(), body.clone()],
                body,
            }
        }
    }
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
        // SAFETY: compile-time constant regex pattern; correctness verified at author time.
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

        // Compute cwd early so background mode can use it.
        let cwd = match &workdir {
            Some(w) => ctx.resolve_path(w),
            None => ctx.effective_cwd(),
        };

        // Background mode: spawn the process, register in ProcessRegistry,
        // and return the session_id immediately (FR-012, T069).
        if background {
            return self.execute_background(command, &cwd, args, ctx).await;
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

        // cwd was computed above (before the background check).

        // Feature 005 (T012): snapshot known-read files before running the
        // command so we can detect terminal-caused mutations afterward.
        let pre_snapshot = snapshot_tracked_files();

        let (raw_output, returncode, timed_out, interrupted) =
            run_command(&command, &cwd, effective_timeout, ctx).await;

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
        if interrupted {
            output.push_str("\n[Command interrupted by user]");
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

        // Feature 005 (T012): detect files mutated by this terminal command
        // and record them so the agent turn loop's drain emits FileChange
        // events with source: Terminal. We compare mtime+hash before/after.
        detect_terminal_mutations(&pre_snapshot);

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

impl Terminal {
    /// Spawn a command in the background, register it in the global
    /// ProcessRegistry, launch a reaper to drain its pipes, and return a
    /// session handle immediately (FR-012).
    async fn execute_background(
        &self,
        command: String,
        cwd: &std::path::Path,
        args: Value,
        ctx: &ToolContext,
    ) -> ToolResult {
        let notify_on_complete = args
            .get("notify_on_complete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let shell = match cached_shell() {
            Some(s) => s,
            None => {
                return ToolResult::Text(dumps(&json!({
                    "output": "",
                    "exit_code": -1,
                    "error": "No usable shell found. Tried: bash, pwsh, powershell.",
                    "status": "error",
                })));
            }
        };
        let mut cmd = tokio::process::Command::new(shell.argv0());
        match &shell {
            Shell::Bash(_) => {
                cmd.arg("-c").arg(&command);
            }
            Shell::PowerShell(_) => {
                cmd.arg("-NoProfile").arg("-Command").arg(&command);
            }
        }
        cmd.current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd.env_clear();
        for (k, v) in sanitized_env() {
            cmd.env(k, v);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::Text(dumps(&json!({
                    "output": "",
                    "exit_code": -1,
                    "error": format!("Failed to spawn background process: {}", e),
                    "status": "error",
                })));
            }
        };

        // Take the pipe readers so the reaper owns them. The stored `Child`
        // keeps neither — this is the core fix for the "pipes never read" bug
        // (the RingBuffer used to stay empty because nobody drained them).
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let session_id = format!("proc-{}", uuid::Uuid::new_v4().simple());

        // Register in the global ProcessRegistry.
        let registry = crate::tools::process_tool::process_registry();
        {
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            let mut session =
                crate::tools::process_tool::ProcessSession::new(
                    session_id.clone(),
                    child,
                    command.clone(),
                    cwd.display().to_string(),
                );
            session.notify_on_complete = notify_on_complete;
            reg.insert(session_id.clone(), session);
        }

        // Launch the reaper: it drains stdout/stderr into the ring buffers,
        // awaits exit, records the outcome, and (if requested) fires a
        // one-shot completion notice through the progress channel.
        let reaper_handle = crate::tools::process_tool::spawn_reaper(
            session_id.clone(),
            stdout,
            stderr,
            ctx.clone(),
        );
        {
            let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(session) = reg.get_mut(&session_id) {
                session.reaper_handle = Some(reaper_handle);
            }
        }

        ToolResult::Text(dumps(&json!({
            "output": format!("Background process started. Use process(action=\"poll\", session_id=\"{}\") to check output.", session_id),
            "exit_code": -1,
            "session_id": session_id,
            "status": "background",
        })))
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

/// Async source of output chunks — the cross-platform `OutputChunkStream`
/// boundary (feature 014, FR-005). See the doc comment near the top of the
/// file under "OutputChunkStream boundary".
///
/// Implementations:
/// - `UnixFdReader` (`#[cfg(unix)]`): wraps `tokio::io::unix::AsyncFd<OwnedFd>`.
/// - `WindowsPipeReader` (`#[cfg(not(unix))]`): wraps child stdout+stderr.
#[async_trait]
trait ChunkSource: Send {
    /// Read the next chunk (≤ 64 KB). Returns `None` at EOF.
    async fn next_chunk(&mut self) -> Option<Vec<u8>>;
}

/// Run `command` under the resolved shell with stderr merged into stdout on
/// a single pipe (os_pipe), a sanitized environment, and a timeout. Streams
/// progress via `ctx.emit_progress()`. Returns (combined_output, exit_code,
/// timed_out, interrupted).
///
/// Streaming architecture (feature 009, preserved byte-for-byte by feature 014):
/// - The os_pipe reader FD is wrapped in `tokio::io::AsyncFd` for native async
///   read-readiness (see research.md R2). This avoids `spawn_blocking` which
///   stalled the turn-driving task.
/// - Output chunks (≤ 64 KB) are written to an in-memory `String` for small
///   outputs or a `tempfile::NamedTempFile` for large ones (threshold: 4 KB).
/// - Each chunk emits a `ToolProgress` event (throttled to 50ms) so the user
///   sees live output.
/// - Silent commands (no output for ≥ 2s) get a "running… Ns" heartbeat.
#[cfg(unix)]
async fn run_command_unix(
    command: &str,
    cwd: &std::path::Path,
    timeout_secs: u64,
    ctx: &ToolContext,
) -> (String, i64, bool, bool) {
    let shell = match cached_shell() {
        Some(s) => s,
        None => {
            return (
                "No usable shell found. Install bash.".to_string(),
                -1,
                false,
                false,
            )
        }
    };
    let wrapper = build_wrapper_script(&shell, command, CWD_MARKER);

    let (mut reader, writer) = match os_pipe::pipe() {
        Ok(p) => p,
        Err(e) => return (format!("Failed to execute command: {}", e), -1, false, false),
    };
    let writer2 = match writer.try_clone() {
        Ok(w) => w,
        Err(e) => return (format!("Failed to execute command: {}", e), -1, false, false),
    };

    let mut cmd = tokio::process::Command::new(&wrapper.argv0);
    cmd.args(&wrapper.args)
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
        Err(e) => return (format!("Failed to spawn command: {}", e), -1, false, false),
    };
    // Parent must drop its writer ends or the reader never sees EOF.
    drop(cmd);

    // Wrap the os_pipe reader in AsyncFd for native async reads.
    // The reader is a `std::process::ChildStdin`-like FD that impls `AsRawFd`.
    let raw_fd = std::os::unix::io::AsRawFd::as_raw_fd(&reader);
    // SAFETY: the FD is owned by `reader` which stays alive for the duration
    // of this function. We dup it so AsyncFd has its own owned FD.
    let owned_fd = unsafe { libc::dup(raw_fd) };
    if owned_fd < 0 {
        // Fallback: blocking read (same as old behavior).
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut reader, &mut buf);
        let output = String::from_utf8_lossy(&buf).into_owned();
        let timed_out = false;
        let status = child.wait().await.ok();
        let code = exit_code_from_status(status, timed_out);
        return (output, code, timed_out, false);
    }
    let async_fd = match tokio::io::unix::AsyncFd::new(OwnedFd(owned_fd)) {
        Ok(fd) => fd,
        Err(_) => {
            // Fallback: blocking read.
            unsafe { libc::close(owned_fd) };
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut reader, &mut buf);
            let output = String::from_utf8_lossy(&buf).into_owned();
            let status = child.wait().await.ok();
            let code = exit_code_from_status(status, false);
            return (output, code, false, false);
        }
    };

    let reader = UnixFdReader {
        async_fd,
        buf: vec![0u8; 64 * 1024],
    };

    // Stream output with progress, timeout, and heartbeat.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let (output, interrupted) = stream_output(Box::new(reader), ctx, deadline).await;

    // On cooperative interrupt, kill the child immediately and return the
    // partial output captured so far. The agent's post-dispatch interrupt
    // check closes the turn; here we just stop the command promptly.
    if interrupted {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return (output, 124, false, true);
    }

    // Wait for the child to exit (it should already be done or about to be
    // since the pipe is closed after stream_output returns).
    let mut timed_out = false;
    let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(s)) => Some(s),
        Ok(Err(_)) => None,
        Err(_) => {
            // Child didn't exit within 5s of pipe EOF — kill it.
            timed_out = true;
            let _ = child.start_kill();
            let _ = child.wait().await;
            None
        }
    };

    // Check if the timeout fired during streaming (stream_output returns
    // early with partial output).
    if tokio::time::Instant::now() >= deadline {
        timed_out = true;
    }

    let code = if timed_out {
        124
    } else {
        exit_code_from_status(status, timed_out)
    };

    (output, code, timed_out, false)
}

/// Minimal Windows foreground command runner (feature 014 US1 stub).
///
/// For US1 this spawns the resolved shell and reads stdout+stderr to
/// completion (blocking-style via `AsyncReadExt`), returning the combined
/// output + exit code. Full streaming + PowerShell support lands in US2
/// (T013–T015) which replaces this body with the streaming equivalent.
#[cfg(not(unix))]
async fn run_command_windows(
    command: &str,
    cwd: &std::path::Path,
    timeout_secs: u64,
    ctx: &ToolContext,
) -> (String, i64, bool, bool) {
    use tokio::io::AsyncReadExt;

    let shell = match cached_shell() {
        Some(s) => s,
        None => {
            return (
                "No usable shell found. Tried: bash, pwsh, powershell.".to_string(),
                -1,
                false,
                false,
            )
        }
    };
    let wrapper = build_wrapper_script(&shell, command, CWD_MARKER);

    let mut cmd = tokio::process::Command::new(&wrapper.argv0);
    cmd.args(&wrapper.args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd.env_clear();
    for (k, v) in sanitized_env() {
        cmd.env(k, v);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (format!("Failed to spawn command: {}", e), -1, false, false),
    };

    let mut stdout = child.stdout.take().unwrap_or_else(|| {
        // Should not happen with Stdio::piped(), but guard anyway.
        // tokio doesn't let us construct a closed ChildStdout easily; use
        // the FD-less path by treating None as immediate EOF in the read loop.
        unreachable!("Stdio::piped() guarantees Some(stdout)")
    });
    let mut stderr = child
        .stderr
        .take()
        .unwrap_or_else(|| unreachable!("Stdio::piped() guarantees Some(stderr)"));

    // US1 stub: read both streams to completion, then merge. US2 (T015)
    // replaces this with the streaming WindowsPipeReader + stream_output.
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let read_out = stdout.read_to_end(&mut out_buf);
    let read_err = stderr.read_to_end(&mut err_buf);

    let (r1, r2) = tokio::join!(read_out, read_err);
    let _ = (r1, r2);

    let mut combined = out_buf;
    combined.extend_from_slice(&err_buf);
    let output = String::from_utf8_lossy(&combined).into_owned();

    let status = match tokio::time::timeout(
        Duration::from_secs(timeout_secs.max(1)),
        child.wait(),
    )
    .await
    {
        Ok(Ok(s)) => Some(s),
        Ok(Err(_)) => None,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            None
        }
    };

    let timed_out = status.is_none();
    let code = if timed_out {
        124
    } else {
        exit_code_from_status(status, timed_out)
    };

    // Suppress unused-warning for ctx (full streaming uses it in US2).
    let _ = ctx;

    (output, code, timed_out, false)
}

/// Cross-platform dispatcher: selects the Unix or Windows implementation.
#[allow(unused_variables)]
async fn run_command(
    command: &str,
    cwd: &std::path::Path,
    timeout_secs: u64,
    ctx: &ToolContext,
) -> (String, i64, bool, bool) {
    #[cfg(unix)]
    {
        run_command_unix(command, cwd, timeout_secs, ctx).await
    }
    #[cfg(not(unix))]
    {
        run_command_windows(command, cwd, timeout_secs, ctx).await
    }
}

/// Wrapper to give a raw FD the `AsRawFd` impl that `AsyncFd` requires.
/// We own this FD (via `dup`) and must close it on drop.
#[cfg(unix)]
struct OwnedFd(std::os::unix::io::RawFd);

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for OwnedFd {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.0
    }
}

#[cfg(unix)]
impl Drop for OwnedFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

// SAFETY: OwnedFd is just a raw FD — safe to send/share across threads.
// The FD is not shared (we dup'd it), and close-on-drop is the only mutation.
#[cfg(unix)]
unsafe impl Send for OwnedFd {}
#[cfg(unix)]
unsafe impl Sync for OwnedFd {}

/// Unix concrete `OutputChunkStream` impl (T008). Wraps `AsyncFd<OwnedFd>`
/// and implements the `ChunkSource` trait by reading via readiness + libc.
/// This is a mechanical extraction of the read logic that previously lived
/// inline in `stream_output` — byte-for-byte identical behavior (feature 014
/// Principle VII).
#[cfg(unix)]
struct UnixFdReader {
    async_fd: tokio::io::unix::AsyncFd<OwnedFd>,
    buf: Vec<u8>,
}

#[cfg(unix)]
#[async_trait]
impl ChunkSource for UnixFdReader {
    async fn next_chunk(&mut self) -> Option<Vec<u8>> {
        loop {
            let guard = self.async_fd.readable().await;
            let mut guard = match guard {
                Ok(g) => g,
                Err(_) => return None, // FD error — treat as EOF.
            };
            // Try a non-blocking read.
            let n = match guard.try_io(|inner| {
                let fd = inner.get_ref().as_raw_fd();
                let ret =
                    unsafe { libc::read(fd, self.buf.as_mut_ptr() as *mut _, self.buf.len()) };
                if ret < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(ret as usize)
                }
            }) {
                Ok(Ok(n)) => n,
                Ok(Err(_)) => return None, // EOF or error
                Err(_would_block) => {
                    // Spurious readiness — loop and try again.
                    continue;
                }
            };
            drop(guard);

            if n == 0 {
                // EOF
                return None;
            }
            return Some(self.buf[..n].to_vec());
        }
    }
}

/// Extract the exit code from an `ExitStatus` using the same logic as the
/// old `run_bash`.
fn exit_code_from_status(status: Option<std::process::ExitStatus>, timed_out: bool) -> i64 {
    match status {
        Some(s) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                s.code()
                    .map(|c| c as i64)
                    .unwrap_or_else(|| -(s.signal().unwrap_or(1) as i64))
            }
            #[cfg(not(unix))]
            {
                s.code().map(|c| c as i64).unwrap_or(-1)
            }
        }
        None if timed_out => 124,
        None => -1,
    }
}

/// Stream output from a `ChunkSource`, emitting progress events, with
/// timeout and heartbeat. Returns the full accumulated output as a `String`.
///
/// This implements (feature 014, shared cross-platform body):
/// - T006: temp-file capture (outputs > 4 KB spill to disk, bounded memory)
/// - T007: chunk coalescing (50ms throttle window)
/// - T008: elapsed-time heartbeat (2s interval for silent commands)
/// - T012: cooperative interrupt (polls `ctx.is_interrupted()` and returns early)
///
/// Returns `(full_output, interrupted)` where `interrupted` is true when the
/// loop broke early because the user requested cancellation.
///
/// The platform-specific read logic lives in the `ChunkSource` impl
/// (`UnixFdReader` on Unix, `WindowsPipeReader` on Windows). Everything in
/// this function body is platform-neutral.
async fn stream_output(
    mut source: Box<dyn ChunkSource>,
    ctx: &ToolContext,
    deadline: tokio::time::Instant,
) -> (String, bool) {
    const CHUNK_SIZE: usize = 64 * 1024;
    const SMALL_THRESHOLD: usize = 4096;
    const THROTTLE_MS: u64 = 50;
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
    // Interrupt polling cadence: well under the ~3s cancellation target.
    const INTERRUPT_POLL: Duration = Duration::from_millis(100);

    // Output capture: start in-memory, spill to temp file if large.
    let mut mem_buf: Vec<u8> = Vec::new();
    let mut total_bytes: usize = 0;
    let mut temp_file: Option<tempfile::NamedTempFile> = None;

    // Throttling state. `last_emit = None` initially so the first chunk
    // emits immediately (the 50ms window only coalesces true bursts, not the
    // very first delta which otherwise gets glued to the next one).
    let mut last_emit: Option<tokio::time::Instant> = None;
    let mut pending_chunk: Vec<u8> = Vec::new();

    // Heartbeat state.
    let start = tokio::time::Instant::now();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await; // consume the immediate first tick
    // Interrupt polling timer.
    let mut interrupt_tick = tokio::time::interval(INTERRUPT_POLL);
    interrupt_tick.tick().await; // consume the immediate first tick

    let mut interrupted = false;

    loop {
        // Pin the read future so we can race it in select!. `next_chunk`
        // is cancellation-safe: re-creating it each iteration re-arms FD
        // readiness without losing data (the FD is non-blocking + edge-trigged).
        let read_fut = source.next_chunk();
        tokio::pin!(read_fut);

        tokio::select! {
            biased;

            // Cooperative interrupt (Ctrl-C): checked first so cancellation
            // takes priority over reading more output.
            _ = interrupt_tick.tick() => {
                if ctx.is_interrupted() {
                    flush_chunk(&pending_chunk, ctx, &mut last_emit);
                    interrupted = true;
                    break;
                }
                // Not interrupted — the read future is still pending; poll it
                // by falling through. We need to await read_fut after this
                // select arm fires, but select consumed the tick. Loop again
                // to re-create the read future (cheap; next_chunk is
                // cancellation-safe — it re-arms readiness on the FD).
                continue;
            }

            _ = tokio::time::sleep_until(deadline) => {
                // Timeout: flush pending, break.
                flush_chunk(&pending_chunk, ctx, &mut last_emit);
                break;
            }

            _ = heartbeat.tick() => {
                // Silent-command heartbeat.
                if pending_chunk.is_empty() {
                    let elapsed = start.elapsed().as_secs();
                    ctx.emit_progress(format!("running… {}s", elapsed));
                }
                continue;
            }

            chunk = &mut read_fut => {
                match chunk {
                    None => {
                        // EOF: flush pending and stop.
                        flush_chunk(&pending_chunk, ctx, &mut last_emit);
                        break;
                    }
                    Some(bytes) => {
                        let n = bytes.len();
                        let chunk = &bytes[..];

                        // Write to output capture.
                        total_bytes += n;
                        if total_bytes > SMALL_THRESHOLD {
                            // Spill to temp file.
                            if temp_file.is_none() {
                                let mut existing = std::mem::take(&mut mem_buf);
                                match tempfile::NamedTempFile::new() {
                                    Ok(mut f) => {
                                        use std::io::Write;
                                        let _ = f.write_all(&existing);
                                        let _ = f.write_all(chunk);
                                        temp_file = Some(f);
                                        existing.clear();
                                    }
                                    Err(_) => {
                                        // Can't create temp file — keep in memory.
                                        mem_buf.extend_from_slice(chunk);
                                    }
                                }
                            } else if let Some(ref mut f) = temp_file {
                                use std::io::Write;
                                let _ = f.write_all(chunk);
                            }
                        } else {
                            mem_buf.extend_from_slice(chunk);
                        }

                        // Throttle progress events: emit when 50ms have elapsed
                        // since the last emit (or the pending buffer is full).
                        pending_chunk.extend_from_slice(chunk);
                        let now = tokio::time::Instant::now();
                        let throttle_elapsed = last_emit
                            .map_or(true, |t| now.duration_since(t) >= Duration::from_millis(THROTTLE_MS));
                        if throttle_elapsed || pending_chunk.len() >= CHUNK_SIZE {
                            flush_chunk(&pending_chunk, ctx, &mut last_emit);
                            pending_chunk.clear();
                        }
                    }
                }
            }
        }
    }

    // Read back the full output.
    let output = if let Some(mut f) = temp_file {
        use std::io::{Seek, SeekFrom};
        let _ = f.seek(SeekFrom::Start(0));
        let mut full = String::new();
        let _ = std::io::Read::read_to_string(&mut f, &mut full);
        // Add any remaining bytes from mem_buf (shouldn't happen normally).
        full.push_str(&String::from_utf8_lossy(&mem_buf));
        full
    } else {
        String::from_utf8_lossy(&mem_buf).into_owned()
    };
    (output, interrupted)
}

/// Emit a progress event for a chunk (decodes as lossy UTF-8). The
/// `last_emit` timestamp is stamped to `Some(now)` so the throttle window
/// starts ticking from this emit.
fn flush_chunk(chunk: &[u8], ctx: &ToolContext, last_emit: &mut Option<tokio::time::Instant>) {
    if chunk.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(chunk);
    ctx.emit_progress(text.into_owned());
    *last_emit = Some(tokio::time::Instant::now());
}

// ── Feature 005: terminal-mutation detection (T012) ──────────────────────

/// Snapshot of a tracked file: its mtime and content hash at snapshot time.
#[derive(Debug, Clone)]
struct FileSnapshot {
    path: String,
    mtime: Option<std::time::SystemTime>,
    hash: Option<u64>,
}

/// Snapshot all files the agent has read this session (per
/// `FileTracker::read_files()`). Used as the "before" baseline for
/// detecting terminal-caused mutations.
fn snapshot_tracked_files() -> Vec<FileSnapshot> {
    use std::hash::{Hash, Hasher};
    FileTracker::read_files()
        .into_iter()
        .map(|path| {
            let meta = std::fs::metadata(&path).ok();
            let mtime = meta.as_ref().and_then(|m| m.modified().ok());
            let hash = std::fs::read(&path)
                .ok()
                .map(|bytes| {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    bytes.hash(&mut h);
                    h.finish()
                });
            FileSnapshot { path, mtime, hash }
        })
        .collect()
}

/// Compare the post-command state of each snapshotted file to its pre-command
/// snapshot. For any file whose mtime or content hash changed, record it via
/// `FileTracker::record_external_mutation` so the turn loop's drain emits a
/// `FileChange { source: Terminal }`.
fn detect_terminal_mutations(pre: &[FileSnapshot]) {
    use std::hash::{Hash, Hasher};
    for snap in pre {
        let now_meta = std::fs::metadata(&snap.path).ok();
        let now_mtime = now_meta.as_ref().and_then(|m| m.modified().ok());
        let now_hash = std::fs::read(&snap.path)
            .ok()
            .map(|bytes| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut h);
                h.finish()
            });
        // Changed if mtime differs OR hash differs (hash is authoritative;
        // mtime is a fast-path skip when unchanged).
        let mtime_changed = match (snap.mtime, now_mtime) {
            (Some(a), Some(b)) => a != b,
            _ => true, // treat missing metadata as "potentially changed"
        };
        let hash_changed = match (snap.hash, now_hash) {
            (Some(a), Some(b)) => a != b,
            (None, Some(_)) => true, // file gained content
            _ => false,
        };
        if mtime_changed || hash_changed {
            FileTracker::record_external_mutation(&snap.path);
        }
    }
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
        // Background mode now spawns a real process.
        let bg = parse(&Terminal.execute(json!({"command": "true", "background": true}), &c).await);
        assert_eq!(bg["status"], "background");
        assert!(bg["session_id"].as_str().unwrap().starts_with("proc-"));
        // PTY mode is still a stub.
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
