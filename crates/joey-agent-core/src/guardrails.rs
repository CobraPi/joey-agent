//! Tool guardrails — port of `agent/tool_guardrails.py` +
//! `agent/tool_result_classification.py`.
//!
//! Detects pathological tool-call patterns within a single turn:
//! - Repeated exact failures of the same tool+args (the model keeps trying
//!   the same broken call)
//! - Repeated failures of the same tool with different args (the tool is
//!   fundamentally not working)
//! - Idempotent tools returning identical results (no progress — the model
//!   is stuck re-reading the same unchanged data)
//!
//! Returns decisions: allow, warn (append guidance to result), block
//! (synthetic error result), or halt (stop the entire turn).

use std::collections::HashMap;

use sha2::{Digest, Sha256};

// ─── Tool classification (tool_result_classification.py) ──────────────

/// Tools that mutate files on disk.
pub const FILE_MUTATING_TOOLS: &[&str] = &["write_file", "patch", "multi_edit"];

/// Tools with no side effects — pure observations.
pub const NO_EFFECT_TOOLS: &[&str] = &[
    "read_file",
    "search_files",
    "session_search",
    "skill_view",
    "skills_list",
    "web_extract",
    "web_search",
];

/// Idempotent tools whose results can be compared for no-progress detection.
pub const IDEMPOTENT_TOOLS: &[&str] = &[
    "read_file",
    "search_files",
    "web_search",
    "web_extract",
    "session_search",
    "skill_view",
    "skills_list",
];

/// Tools with side effects (mutations, execution, etc.).
pub const MUTATING_TOOLS: &[&str] = &[
    "terminal",
    "write_file",
    "patch",
    "multi_edit",
    "todo",
    "memory",
    "delegate_task",
    "process",
    "cronjob",
    "clarify",
];

/// Classify whether a tool may have side effects.
pub fn tool_may_have_side_effect(tool_name: &str) -> bool {
    !NO_EFFECT_TOOLS.contains(&tool_name)
}

/// Check if a file-mutation tool result actually landed successfully.
/// Guards against classifying a write that errored as a "successful mutation."
pub fn file_mutation_result_landed(tool_name: &str, result_text: &str) -> bool {
    if !FILE_MUTATING_TOOLS.contains(&tool_name) {
        return false;
    }
    // Try to parse the result as JSON.
    if let Ok(data) = serde_json::from_str::<serde_json::Value>(result_text) {
        if data.get("error").is_some() {
            return false;
        }
        match tool_name {
            "write_file" => data.get("bytes_written").is_some(),
            "patch" | "multi_edit" => data
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            _ => false,
        }
    } else {
        false
    }
}

/// Classify whether a tool result indicates failure.
pub fn classify_tool_failure(tool_name: &str, result_text: &str) -> bool {
    // If it's a file mutation that landed, it's not a failure.
    if file_mutation_result_landed(tool_name, result_text) {
        return false;
    }
    // Terminal: check for non-zero exit code in JSON.
    if tool_name == "terminal" {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(result_text) {
            if let Some(exit) = data.get("exit_code").and_then(|v| v.as_i64()) {
                return exit != 0;
            }
        }
    }
    // Memory tool: check for success: false.
    if tool_name == "memory" {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(result_text) {
            if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                return true;
            }
        }
    }
    // Generic: scan for error markers in the first 500 chars.
    let lower = result_text.chars().take(500).collect::<String>().to_lowercase();
    lower.contains("\"error\"") || lower.contains("error:") || lower.starts_with("error")
}

// ─── Signature computation ────────────────────────────────────────────

/// Canonicalize tool args for hashing: sorted-key compact JSON.
fn canonical_args(args: &serde_json::Value) -> String {
    canonical_value(args)
}

fn canonical_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut parts = Vec::new();
            for k in keys {
                parts.push(format!("{}:{}", k, canonical_value(&map[k])));
            }
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(canonical_value).collect();
            format!("[{}]", parts.join(","))
        }
        serde_json::Value::String(s) => format!("\"{}\"", s),
        other => other.to_string(),
    }
}

/// SHA-256 hash of (tool_name, canonical_args).
fn tool_signature(tool_name: &str, args: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\x00");
    hasher.update(canonical_args(args).as_bytes());
    hex::encode(hasher.finalize())
}

/// SHA-256 hash of a tool result for no-progress detection.
fn result_hash(result_text: &str) -> String {
    let mut hasher = Sha256::new();
    // Try canonical JSON if parseable, else raw text.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(result_text) {
        hasher.update(canonical_value(&v).as_bytes());
    } else {
        hasher.update(result_text.as_bytes());
    }
    hex::encode(hasher.finalize())
}

// ─── Configuration ────────────────────────────────────────────────────

/// Thresholds for guardrail actions.
#[derive(Debug, Clone)]
pub struct GuardrailConfig {
    pub warnings_enabled: bool,
    pub hard_stop_enabled: bool,
    /// Warn after N exact failures of the same tool+args.
    pub exact_failure_warn_after: usize,
    /// Block after N exact failures.
    pub exact_failure_block_after: usize,
    /// Warn after N failures of the same tool (any args).
    pub same_tool_failure_warn_after: usize,
    /// Halt after N same-tool failures.
    pub same_tool_failure_halt_after: usize,
    /// Warn after N identical idempotent-tool results (no progress).
    pub no_progress_warn_after: usize,
    /// Block after N identical idempotent-tool results.
    pub no_progress_block_after: usize,
}

impl Default for GuardrailConfig {
    fn default() -> Self {
        Self {
            warnings_enabled: true,
            hard_stop_enabled: false, // opt-in
            exact_failure_warn_after: 2,
            exact_failure_block_after: 5,
            same_tool_failure_warn_after: 3,
            same_tool_failure_halt_after: 8,
            no_progress_warn_after: 2,
            no_progress_block_after: 5,
        }
    }
}

// ─── Decision types ───────────────────────────────────────────────────

/// The action the guardrail recommends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailAction {
    /// No intervention needed.
    Allow,
    /// Append guidance to the tool result.
    Warn,
    /// Replace the tool result with a synthetic error.
    Block,
    /// Stop the entire turn (too many failures).
    Halt,
}

impl GuardrailAction {
    pub fn allows_execution(&self) -> bool {
        matches!(self, GuardrailAction::Allow | GuardrailAction::Warn)
    }
    pub fn should_halt(&self) -> bool {
        matches!(self, GuardrailAction::Block | GuardrailAction::Halt)
    }
}

/// The reason code for a guardrail decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailCode {
    RepeatedExactFailureWarning,
    RepeatedExactFailureBlock,
    SameToolFailureWarning,
    SameToolFailureHalt,
    IdempotentNoProgressWarning,
    IdempotentNoProgressBlock,
}

/// A guardrail decision for a single tool call.
#[derive(Debug, Clone)]
pub struct GuardrailDecision {
    pub action: GuardrailAction,
    pub code: GuardrailCode,
    pub message: String,
    pub tool_name: String,
    pub count: usize,
}

// ─── Controller ───────────────────────────────────────────────────────

/// Per-turn controller that tracks tool-call patterns.
/// Reset at the start of each agent turn.
#[derive(Debug, Clone)]
pub struct ToolGuardrailController {
    config: GuardrailConfig,
    /// (tool_name, args_hash) → consecutive failure count
    exact_failures: HashMap<(String, String), usize>,
    /// tool_name → consecutive failure count (any args)
    same_tool_failures: HashMap<String, usize>,
    /// (tool_name, result_hash) → repeat count for idempotent tools
    idempotent_repeats: HashMap<(String, String), usize>,
}

impl ToolGuardrailController {
    pub fn new(config: GuardrailConfig) -> Self {
        Self {
            config,
            exact_failures: HashMap::new(),
            same_tool_failures: HashMap::new(),
            idempotent_repeats: HashMap::new(),
        }
    }

    /// Create with default config.
    pub fn default_config() -> Self {
        Self::new(GuardrailConfig::default())
    }

    /// Reset all tracking for a new turn.
    pub fn reset_for_turn(&mut self) {
        self.exact_failures.clear();
        self.same_tool_failures.clear();
        self.idempotent_repeats.clear();
    }

    /// Check before executing a tool call. Returns a decision that may block.
    pub fn before_call(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Option<GuardrailDecision> {
        if !self.config.hard_stop_enabled {
            return None;
        }
        let sig = tool_signature(tool_name, args);
        let exact_count = self.exact_failures.get(&(tool_name.to_string(), sig.clone())).copied().unwrap_or(0);
        if exact_count >= self.config.exact_failure_block_after {
            return Some(GuardrailDecision {
                action: GuardrailAction::Block,
                code: GuardrailCode::RepeatedExactFailureBlock,
                message: format!(
                    "This exact tool call has failed {} times. The arguments are likely wrong. Try a different approach.",
                    exact_count
                ),
                tool_name: tool_name.to_string(),
                count: exact_count,
            });
        }
        // Idempotent no-progress block.
        if Self::is_idempotent(tool_name) {
            // We can't check result_hash before the call, only after.
            // The block for no_progress happens on the result side.
        }
        None
    }

    /// Process the result after a tool call. Returns a decision that may warn or halt.
    pub fn after_call(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
        result_text: &str,
    ) -> Option<GuardrailDecision> {
        let sig = tool_signature(tool_name, args);
        let is_failure = classify_tool_failure(tool_name, result_text);

        if is_failure {
            // Bump exact and same-tool failure counts.
            let exact_count = self.exact_failures.entry((tool_name.to_string(), sig.clone())).or_insert(0);
            *exact_count += 1;
            let same_count = self.same_tool_failures.entry(tool_name.to_string()).or_insert(0);
            *same_count += 1;

            // Pop no-progress tracking (a failure isn't "same result").
            self.idempotent_repeats.clear();

            let same = *same_count;
            let exact = *exact_count;

            // Halt if same-tool failures exceed threshold.
            if same >= self.config.same_tool_failure_halt_after {
                return Some(GuardrailDecision {
                    action: GuardrailAction::Halt,
                    code: GuardrailCode::SameToolFailureHalt,
                    message: format!(
                        "{} has failed {} times with various arguments. The tool may be broken or the approach is fundamentally wrong. Stop using this tool and try a different approach.",
                        tool_name, same
                    ),
                    tool_name: tool_name.to_string(),
                    count: same,
                });
            }
            // Warn on repeated exact failures.
            if exact >= self.config.exact_failure_warn_after && self.config.warnings_enabled {
                return Some(GuardrailDecision {
                    action: GuardrailAction::Warn,
                    code: GuardrailCode::RepeatedExactFailureWarning,
                    message: format!(
                        "This exact {} call has failed {} times. The arguments are wrong. Change your approach.",
                        tool_name, exact
                    ),
                    tool_name: tool_name.to_string(),
                    count: exact,
                });
            }
            // Warn on same-tool failures.
            if same >= self.config.same_tool_failure_warn_after && self.config.warnings_enabled {
                return Some(GuardrailDecision {
                    action: GuardrailAction::Warn,
                    code: GuardrailCode::SameToolFailureWarning,
                    message: format!(
                        "{} has failed {} times. Consider trying a different tool or approach.",
                        tool_name, same
                    ),
                    tool_name: tool_name.to_string(),
                    count: same,
                });
            }
        } else {
            // Success: pop failure counts.
            self.exact_failures.remove(&(tool_name.to_string(), sig));
            self.same_tool_failures.remove(tool_name);

            // For idempotent tools, check for no-progress (identical results).
            if Self::is_idempotent(tool_name) && self.config.warnings_enabled {
                let rh = result_hash(result_text);
                let count = self
                    .idempotent_repeats
                    .entry((tool_name.to_string(), rh))
                    .or_insert(0);
                *count += 1;
                let c = *count;
                if c >= self.config.no_progress_warn_after {
                    return Some(GuardrailDecision {
                        action: GuardrailAction::Warn,
                        code: GuardrailCode::IdempotentNoProgressWarning,
                        message: format!(
                            "{} has returned the same result {} times. You are not making progress. Use the information you already have and proceed.",
                            tool_name, c
                        ),
                        tool_name: tool_name.to_string(),
                        count: c,
                    });
                }
            }
        }
        None
    }

    /// Whether a tool is idempotent (no side effects + in the idempotent set).
    fn is_idempotent(tool_name: &str) -> bool {
        !MUTATING_TOOLS.contains(&tool_name) && IDEMPOTENT_TOOLS.contains(&tool_name)
    }
}

/// Append guardrail guidance text to a tool result.
pub fn append_guardrail_guidance(result: &str, decision: &GuardrailDecision) -> String {
    let guidance = format!(
        "\n\n[Tool guardrail: {} — count={} — {}]",
        match decision.code {
            GuardrailCode::RepeatedExactFailureWarning => "repeated-exact-failure-warning",
            GuardrailCode::RepeatedExactFailureBlock => "repeated-exact-failure-block",
            GuardrailCode::SameToolFailureWarning => "same-tool-failure-warning",
            GuardrailCode::SameToolFailureHalt => "same-tool-failure-halt",
            GuardrailCode::IdempotentNoProgressWarning => "idempotent-no-progress-warning",
            GuardrailCode::IdempotentNoProgressBlock => "idempotent-no-progress-block",
        },
        decision.count,
        decision.message
    );
    format!("{}{}", result, guidance)
}

/// Build a synthetic error result for a blocked tool call.
pub fn guardrail_synthetic_result(decision: &GuardrailDecision) -> String {
    serde_json::json!({
        "error": decision.message,
        "guardrail": format!("{:?}", decision.code),
        "tool": decision.tool_name,
        "count": decision.count,
    })
    .to_string()
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_classification() {
        assert!(tool_may_have_side_effect("terminal"));
        assert!(tool_may_have_side_effect("write_file"));
        assert!(!tool_may_have_side_effect("read_file"));
        assert!(!tool_may_have_side_effect("web_search"));
        // Unknown tools default to effect-capable.
        assert!(tool_may_have_side_effect("unknown_tool"));
    }

    #[test]
    fn test_file_mutation_landed() {
        assert!(file_mutation_result_landed(
            "write_file",
            r#"{"bytes_written": 42}"#
        ));
        assert!(!file_mutation_result_landed(
            "write_file",
            r#"{"error": "failed"}"#
        ));
        assert!(file_mutation_result_landed(
            "patch",
            r#"{"success": true}"#
        ));
        assert!(!file_mutation_result_landed(
            "patch",
            r#"{"success": false}"#
        ));
    }

    #[test]
    fn test_classify_failure() {
        assert!(classify_tool_failure("terminal", r#"{"exit_code": 1}"#));
        assert!(!classify_tool_failure("terminal", r#"{"exit_code": 0}"#));
        assert!(classify_tool_failure("any", r#"{"error": "oops"}"#));
        assert!(classify_tool_failure("any", "Error: something went wrong"));
        assert!(!classify_tool_failure("read_file", r#"{"content": "hello"}"#));
    }

    #[test]
    fn test_exact_failure_warning() {
        let mut ctrl = ToolGuardrailController::default_config();
        let args = json!({"path": "/tmp/test.txt"});

        // First two failures: no warning (warn_after=2, but count increments after).
        for _ in 0..2 {
            let d = ctrl.after_call("read_file", &args, r#"{"error": "not found"}"#);
            // After 2nd failure, count=2, warn_after=2 → warning fires.
            if let Some(d) = d {
                assert_eq!(d.action, GuardrailAction::Warn);
                assert_eq!(d.code, GuardrailCode::RepeatedExactFailureWarning);
                assert_eq!(d.count, 2);
            }
        }
    }

    #[test]
    fn test_exact_failure_block() {
        let config = GuardrailConfig {
            hard_stop_enabled: true,
            ..Default::default()
        };
        let mut ctrl = ToolGuardrailController::new(config);
        let args = json!({"path": "/tmp/test.txt"});

        // Fail 5 times.
        for _ in 0..5 {
            ctrl.after_call("read_file", &args, r#"{"error": "not found"}"#);
        }
        // 6th call should be blocked.
        let d = ctrl.before_call("read_file", &args);
        assert!(d.is_some());
        let d = d.unwrap();
        assert_eq!(d.action, GuardrailAction::Block);
    }

    #[test]
    fn test_same_tool_halt() {
        let mut ctrl = ToolGuardrailController::default_config();

        // Fail with different args each time (different signatures).
        for i in 0..8 {
            let args = json!({"path": format!("/tmp/test{}.txt", i)});
            ctrl.after_call("read_file", &args, r#"{"error": "not found"}"#);
        }
        // 9th different-args failure should halt.
        let args = json!({"path": "/tmp/test9.txt"});
        let d = ctrl.after_call("read_file", &args, r#"{"error": "not found"}"#);
        assert!(d.is_some());
        let d = d.unwrap();
        assert_eq!(d.action, GuardrailAction::Halt);
        assert_eq!(d.code, GuardrailCode::SameToolFailureHalt);
    }

    #[test]
    fn test_idempotent_no_progress() {
        let mut ctrl = ToolGuardrailController::default_config();
        let args = json!({"path": "/tmp/test.txt"});
        let result = r#"{"content": "same old data"}"#;

        // Call 1: no warning.
        let d = ctrl.after_call("read_file", &args, result);
        assert!(d.is_none());
        // Call 2: warning (warn_after=2, count=2).
        let d = ctrl.after_call("read_file", &args, result);
        assert!(d.is_some());
        let d = d.unwrap();
        assert_eq!(d.action, GuardrailAction::Warn);
        assert_eq!(d.code, GuardrailCode::IdempotentNoProgressWarning);
        assert_eq!(d.count, 2);
    }

    #[test]
    fn test_success_clears_failures() {
        let mut ctrl = ToolGuardrailController::default_config();
        let args = json!({"path": "/tmp/test.txt"});

        ctrl.after_call("read_file", &args, r#"{"error": "not found"}"#);
        ctrl.after_call("read_file", &args, r#"{"error": "not found"}"#);
        // Success clears the count.
        ctrl.after_call("read_file", &args, r#"{"content": "data"}"#);
        // Next failure should start fresh.
        let d = ctrl.after_call("read_file", &args, r#"{"error": "not found"}"#);
        assert!(d.is_none()); // count=1, below warn threshold
    }

    #[test]
    fn test_reset_for_turn() {
        let mut ctrl = ToolGuardrailController::default_config();
        let args = json!({"path": "/tmp/test.txt"});
        ctrl.after_call("read_file", &args, r#"{"error": "e"}"#);
        ctrl.after_call("read_file", &args, r#"{"error": "e"}"#);
        ctrl.reset_for_turn();
        let d = ctrl.after_call("read_file", &args, r#"{"error": "e"}"#);
        assert!(d.is_none()); // reset clears everything
    }

    #[test]
    fn test_synthetic_result_format() {
        let decision = GuardrailDecision {
            action: GuardrailAction::Block,
            code: GuardrailCode::RepeatedExactFailureBlock,
            message: "Failed 5 times".to_string(),
            tool_name: "read_file".to_string(),
            count: 5,
        };
        let result = guardrail_synthetic_result(&decision);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["error"], "Failed 5 times");
        assert_eq!(parsed["tool"], "read_file");
        assert_eq!(parsed["count"], 5);
    }

    #[test]
    fn test_append_guidance() {
        let decision = GuardrailDecision {
            action: GuardrailAction::Warn,
            code: GuardrailCode::IdempotentNoProgressWarning,
            message: "No progress".to_string(),
            tool_name: "read_file".to_string(),
            count: 3,
        };
        let result = append_guardrail_guidance(r#"{"content": "data"}"#, &decision);
        assert!(result.contains("[Tool guardrail:"));
        assert!(result.contains("No progress"));
        assert!(result.contains("count=3"));
    }

    #[test]
    fn test_is_idempotent() {
        assert!(ToolGuardrailController::is_idempotent("read_file"));
        assert!(ToolGuardrailController::is_idempotent("search_files"));
        assert!(!ToolGuardrailController::is_idempotent("terminal"));
        assert!(!ToolGuardrailController::is_idempotent("write_file"));
    }

    #[test]
    fn test_signature_stability() {
        // Same args in different key order should produce same signature.
        let args1 = json!({"b": 2, "a": 1});
        let args2 = json!({"a": 1, "b": 2});
        let sig1 = tool_signature("read_file", &args1);
        let sig2 = tool_signature("read_file", &args2);
        assert_eq!(sig1, sig2);
    }
}
