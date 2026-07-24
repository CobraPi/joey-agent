//! PreToolUse hooks (port of crush's `internal/hooks/`).
//!
//! User-defined shell commands that fire before a tool executes. Each hook
//! can:
//!   - **allow** — proceed normally
//!   - **deny** — block this specific tool call (returns a tool error)
//!   - **halt** — stop the entire turn (exit code 49)
//!   - **rewrite** — shallow-merge a JSON patch into the tool arguments
//!
//! Hooks are configured in `~/.joey/config.yaml` under the `hooks` key:
//!
//! ```yaml
//! hooks:
//!   - name: "lint-before-write"
//!     event: "PreToolUse"
//!     matcher: "write_file|patch"   # regex, empty = match all
//!     command: "my-linter --check"
//! ```
//!
//! The hook receives JSON on stdin with the event name, tool name, and tool
//! arguments. It exits 0 to allow, 2 to block the tool, or 49 to halt the
//! turn. For input rewriting, the hook writes JSON to stdout with an
//! `updated_input` field.

use std::process::Stdio;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

/// Hook event name constants.
pub const EVENT_PRE_TOOL_USE: &str = "PreToolUse";

/// Exit code that halts the entire turn (crush: 49 — between sysexits and
/// signal ranges).
pub const HALT_EXIT_CODE: i32 = 49;

/// Exit code that blocks the current tool call (crush: 2).
pub const BLOCK_EXIT_CODE: i32 = 2;

/// Default timeout for a single hook execution (crush abandons after ~1s
/// grace; we give the hook itself 10s).
const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 10;

/// A hook definition from config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    /// Human-readable name.
    #[serde(default)]
    pub name: String,
    /// Event type (only "PreToolUse" is supported).
    #[serde(default = "default_event")]
    pub event: String,
    /// Regex matcher for tool name. Empty or missing = match all tools.
    #[serde(default)]
    pub matcher: String,
    /// Shell command to execute.
    pub command: String,
    /// Optional timeout override in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn default_event() -> String {
    EVENT_PRE_TOOL_USE.to_string()
}

/// The input JSON sent to the hook process on stdin.
#[derive(Debug, Serialize)]
struct HookInput<'a> {
    event: &'a str,
    tool_name: &'a str,
    tool_input: &'a Value,
    session_id: &'a str,
    cwd: &'a str,
}

/// The result of a single hook execution.
#[derive(Debug, Clone)]
pub struct HookDecision {
    /// "allow", "deny", or "halt".
    pub action: HookAction,
    /// Human-readable reason (for deny/halt).
    pub reason: String,
    /// Shallow-merge patch for the tool arguments (JSON object).
    pub updated_input: Option<Value>,
}

/// What a hook decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    /// Proceed normally.
    Allow,
    /// Block this specific tool call.
    Deny,
    /// Stop the entire turn.
    Halt,
}

/// Aggregated result of all hooks for one event.
#[derive(Debug, Clone)]
pub struct HookAggregate {
    /// The most restrictive action across all hooks.
    pub action: HookAction,
    /// Combined reasons from all hooks that expressed an opinion.
    pub reasons: Vec<String>,
    /// Merged input rewrite (last-write-wins across hooks).
    pub updated_input: Option<Value>,
    /// Number of hooks that ran.
    pub hooks_run: usize,
}

impl HookAggregate {
    /// Whether this aggregate blocks the tool call.
    pub fn is_denied(&self) -> bool {
        matches!(self.action, HookAction::Deny | HookAction::Halt)
    }

    /// Whether this aggregate halts the entire turn.
    pub fn is_halted(&self) -> bool {
        matches!(self.action, HookAction::Halt)
    }
}

/// A compiled runner that executes PreToolUse hooks.
pub struct PreToolUseRunner {
    hooks: Vec<CompiledHook>,
    cwd: String,
}

struct CompiledHook {
    config: HookConfig,
    matcher: Option<Regex>,
}

impl PreToolUseRunner {
    /// Build from config hooks list and the working directory.
    pub fn new(configs: Vec<HookConfig>, cwd: impl Into<String>) -> Self {
        let hooks = configs
            .into_iter()
            .filter(|c| c.event == EVENT_PRE_TOOL_USE)
            .filter_map(|c| {
                let matcher = if c.matcher.is_empty() {
                    None
                } else {
                    match Regex::new(&c.matcher) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            tracing::warn!(
                                "Hook '{}' matcher '{}' failed to compile: {}; skipping",
                                c.name, c.matcher, e
                            );
                            None
                        }
                    }
                };
                Some(CompiledHook { config: c, matcher })
            })
            .collect();
        Self {
            hooks,
            cwd: cwd.into(),
        }
    }

    /// Whether any hooks are registered.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Run all matching PreToolUse hooks for a tool call.
    ///
    /// Returns the aggregate decision. If no hooks match, returns an
    /// allow-all aggregate.
    pub async fn run(
        &self,
        tool_name: &str,
        tool_input: &Value,
        session_id: &str,
    ) -> HookAggregate {
        if self.hooks.is_empty() {
            return HookAggregate {
                action: HookAction::Allow,
                reasons: Vec::new(),
                updated_input: None,
                hooks_run: 0,
            };
        }

        let mut reasons = Vec::new();
        let mut action = HookAction::Allow;
        let mut updated_input: Option<Value> = None;
        let mut hooks_run = 0usize;

        for hook in &self.hooks {
            // Check matcher.
            if let Some(ref re) = hook.matcher {
                if !re.is_match(tool_name) {
                    continue;
                }
            }

            hooks_run += 1;
            let decision = self
                .run_one(hook, tool_name, tool_input, session_id)
                .await;

            if !decision.reason.is_empty() {
                reasons.push(format!("{}: {}", hook.config.name, decision.reason));
            }

            // Escalate action: halt > deny > allow.
            match decision.action {
                HookAction::Halt => {
                    action = HookAction::Halt;
                }
                HookAction::Deny => {
                    if action != HookAction::Halt {
                        action = HookAction::Deny;
                    }
                }
                HookAction::Allow => {}
            }

            // Merge input rewrite.
            if let Some(patch) = decision.updated_input {
                match &mut updated_input {
                    None => updated_input = Some(patch),
                    Some(existing) => {
                        if let (Some(existing_obj), Some(patch_obj)) =
                            (existing.as_object_mut(), patch.as_object())
                        {
                            for (k, v) in patch_obj {
                                existing_obj.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
            }
        }

        HookAggregate {
            action,
            reasons,
            updated_input,
            hooks_run,
        }
    }

    /// Execute a single hook.
    async fn run_one(
        &self,
        hook: &CompiledHook,
        tool_name: &str,
        tool_input: &Value,
        session_id: &str,
    ) -> HookDecision {
        let input = HookInput {
            event: &hook.config.event,
            tool_name,
            tool_input,
            session_id,
            cwd: &self.cwd,
        };
        let stdin_json = match serde_json::to_string(&input) {
            Ok(s) => s,
            Err(e) => {
                return HookDecision {
                    action: HookAction::Allow,
                    reason: format!("(hook input serialization failed: {})", e),
                    updated_input: None,
                }
            }
        };

        let timeout_secs = hook
            .config
            .timeout_secs
            .unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS);

        // Run the command via the shell.
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&hook.config.command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&hook.config.command);
            c
        };
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "Hook '{}' failed to spawn '{}': {}",
                    hook.config.name,
                    hook.config.command,
                    e
                );
                return HookDecision {
                    action: HookAction::Allow,
                    reason: format!("(hook spawn failed: {})", e),
                    updated_input: None,
                };
            }
        };

        // Write stdin.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(stdin_json.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }

        // Wait with timeout.
        let output = match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await
        {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                tracing::warn!("Hook '{}' wait failed: {}", hook.config.name, e);
                return HookDecision {
                    action: HookAction::Allow,
                    reason: format!("(hook wait failed: {})", e),
                    updated_input: None,
                };
            }
            Err(_) => {
                tracing::warn!(
                    "Hook '{}' timed out after {}s",
                    hook.config.name,
                    timeout_secs
                );
                return HookDecision {
                    action: HookAction::Allow,
                    reason: "(hook timed out)".to_string(),
                    updated_input: None,
                };
            }
        };

        let code = output.status.code().unwrap_or(0);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Parse exit code per crush convention.
        match code {
            0 => {
                // Allow. Check for stdout JSON with updated_input.
                let updated = parse_updated_input(&stdout);
                HookDecision {
                    action: HookAction::Allow,
                    reason: String::new(),
                    updated_input: updated,
                }
            }
            BLOCK_EXIT_CODE => {
                // Deny this tool call.
                let reason = extract_reason(&stdout, &stderr)
                    .map(String::from)
                    .unwrap_or_else(|| "blocked by hook".to_string());
                HookDecision {
                    action: HookAction::Deny,
                    reason,
                    updated_input: None,
                }
            }
            HALT_EXIT_CODE => {
                // Halt the entire turn.
                let reason = extract_reason(&stdout, &stderr)
                    .map(String::from)
                    .unwrap_or_else(|| "halted by hook".to_string());
                HookDecision {
                    action: HookAction::Halt,
                    reason,
                    updated_input: None,
                }
            }
            other => {
                // Any other non-zero exit is treated as allow (hook error
                // shouldn't block the agent), but we log it.
                tracing::debug!(
                    "Hook '{}' exited with code {} (non-standard, treating as allow)",
                    hook.config.name,
                    other
                );
                HookDecision {
                    action: HookAction::Allow,
                    reason: format!("(hook exited {}: {})", other, stderr.trim()),
                    updated_input: None,
                }
            }
        }
    }
}

/// Parse a possible JSON object with `updated_input` from hook stdout.
fn parse_updated_input(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(trimmed).ok()?;
    parsed.get("updated_input").cloned()
}

/// Extract a human-readable reason from hook stdout or stderr.
fn extract_reason(stdout: &str, stderr: &str) -> Option<String> {
    // Try parsing stdout as JSON with a "reason" field first.
    if let Ok(parsed) = serde_json::from_str::<Value>(stdout.trim()) {
        if let Some(reason) = parsed.get("reason").and_then(|v| v.as_str()) {
            return Some(reason.to_string());
        }
    }
    // Fall back to the first non-empty line of stdout, then stderr.
    for text in [stdout, stderr] {
        let line = text.lines().find(|l| !l.trim().is_empty());
        if let Some(l) = line {
            return Some(l.trim().to_string());
        }
    }
    None
}

/// Parse hooks from the `hooks` config key.
pub fn load_hooks_from_config(
    config: &joey_core::Config,
) -> Vec<HookConfig> {
    let Some(raw) = config.get("hooks") else {
        return Vec::new();
    };
    // The config value is a YAML sequence of hook objects.
    match serde_yaml::to_string(&raw) {
        Ok(yaml_str) => serde_yaml::from_str::<Vec<HookConfig>>(&yaml_str).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_runner_allows_all() {
        let runner = PreToolUseRunner::new(vec![], "/tmp");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let agg = runtime.block_on(runner.run("read_file", &serde_json::json!({}), "s1"));
        assert_eq!(agg.action, HookAction::Allow);
        assert!(!agg.is_denied());
        assert_eq!(agg.hooks_run, 0);
    }

    #[test]
    fn matcher_filters_tools() {
        let hooks = vec![HookConfig {
            name: "block-write".into(),
            event: EVENT_PRE_TOOL_USE.into(),
            matcher: "write_file|patch".into(),
            command: "exit 2".into(),
            timeout_secs: None,
        }];
        let runner = PreToolUseRunner::new(hooks, "/tmp");

        let runtime = tokio::runtime::Runtime::new().unwrap();
        // read_file should NOT match (allowed).
        let agg =
            runtime.block_on(runner.run("read_file", &serde_json::json!({}), "s1"));
        assert_eq!(agg.action, HookAction::Allow);
        assert_eq!(agg.hooks_run, 0); // matcher filtered it out

        // write_file should match and deny.
        let agg =
            runtime.block_on(runner.run("write_file", &serde_json::json!({}), "s1"));
        assert_eq!(agg.action, HookAction::Deny);
        assert_eq!(agg.hooks_run, 1);
    }

    #[tokio::test]
    async fn deny_prevents_tool() {
        let hooks = vec![HookConfig {
            name: "always-deny".into(),
            event: EVENT_PRE_TOOL_USE.into(),
            matcher: "".into(),
            command: "echo '{\"reason\": \"forbidden\"}' && exit 2".into(),
            timeout_secs: None,
        }];
        let runner = PreToolUseRunner::new(hooks, "/tmp");
        let agg =
            runner.run("terminal", &serde_json::json!({}), "s1").await;
        assert!(agg.is_denied());
        assert!(agg.reasons.iter().any(|r| r.contains("forbidden")));
    }

    #[tokio::test]
    async fn halt_stops_turn() {
        let hooks = vec![HookConfig {
            name: "halt".into(),
            event: EVENT_PRE_TOOL_USE.into(),
            matcher: "".into(),
            command: "exit 49".into(),
            timeout_secs: None,
        }];
        let runner = PreToolUseRunner::new(hooks, "/tmp");
        let agg = runner.run("terminal", &serde_json::json!({}), "s1").await;
        assert!(agg.is_halted());
    }

    #[tokio::test]
    async fn allow_with_input_rewrite() {
        let hooks = vec![HookConfig {
            name: "rewrite".into(),
            event: EVENT_PRE_TOOL_USE.into(),
            matcher: "".into(),
            command: r#"echo '{"updated_input": {"injected": true}}'"#.into(),
            timeout_secs: None,
        }];
        let runner = PreToolUseRunner::new(hooks, "/tmp");
        let agg = runner.run("terminal", &serde_json::json!({}), "s1").await;
        assert_eq!(agg.action, HookAction::Allow);
        assert!(agg.updated_input.is_some());
        assert_eq!(
            agg.updated_input.unwrap().get("injected"),
            Some(&serde_json::json!(true))
        );
    }

    #[tokio::test]
    async fn halt_escalates_over_deny() {
        let hooks = vec![
            HookConfig {
                name: "deny-hook".into(),
                event: EVENT_PRE_TOOL_USE.into(),
                matcher: "".into(),
                command: "exit 2".into(),
                timeout_secs: None,
            },
            HookConfig {
                name: "halt-hook".into(),
                event: EVENT_PRE_TOOL_USE.into(),
                matcher: "".into(),
                command: "exit 49".into(),
                timeout_secs: None,
            },
        ];
        let runner = PreToolUseRunner::new(hooks, "/tmp");
        let agg = runner.run("terminal", &serde_json::json!({}), "s1").await;
        assert!(agg.is_halted());
        assert_eq!(agg.hooks_run, 2);
    }

    #[tokio::test]
    async fn timeout_allows_on_hang() {
        let hooks = vec![HookConfig {
            name: "slow-hook".into(),
            event: EVENT_PRE_TOOL_USE.into(),
            matcher: "".into(),
            command: "sleep 30".into(),
            timeout_secs: Some(1),
        }];
        let runner = PreToolUseRunner::new(hooks, "/tmp");
        let agg = runner.run("terminal", &serde_json::json!({}), "s1").await;
        assert_eq!(agg.action, HookAction::Allow); // timeout → allow
        assert_eq!(agg.hooks_run, 1);
    }
}
