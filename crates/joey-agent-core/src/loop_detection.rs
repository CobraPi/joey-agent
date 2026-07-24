//! Tool-call loop detection (port of crush's `internal/agent/loop_detection.go`).
//!
//! Tracks SHA-256 signatures of recent tool-call interactions (tool name +
//! arguments + result) in a sliding window. If any signature repeats more
//! than `max_repeats` times within `window_size` steps, the agent is stuck
//! in a loop and should be nudged to try a different approach.

use sha2::{Digest, Sha256};

/// Default sliding-window size (crush: 10).
const DEFAULT_WINDOW_SIZE: usize = 10;
/// Default max repeats before declaring a loop (crush: 5).
const DEFAULT_MAX_REPEATS: usize = 5;

/// One recorded tool interaction for loop-signature purposes.
#[derive(Debug, Clone)]
struct StepRecord {
    signature: String,
}

/// Detects repetitive tool-call patterns within a sliding window.
pub struct LoopDetector {
    window: Vec<StepRecord>,
    window_size: usize,
    max_repeats: usize,
}

impl LoopDetector {
    /// Create with defaults (window=10, max_repeats=5).
    pub fn new() -> Self {
        Self::with_params(DEFAULT_WINDOW_SIZE, DEFAULT_MAX_REPEATS)
    }

    /// Create with custom parameters.
    pub fn with_params(window_size: usize, max_repeats: usize) -> Self {
        Self {
            window: Vec::with_capacity(window_size.max(1)),
            window_size: window_size.max(1),
            max_repeats: max_repeats.max(1),
        }
    }

    /// Record a completed tool-call batch and return `true` if a loop is
    /// detected (the same tool-call signature appeared more than
    /// `max_repeats` times in the last `window_size` steps).
    ///
    /// The signature is computed from the tool name, its arguments, and the
    /// result content — so calling the same tool with different args or
    /// getting different results does NOT count as a repeat.
    pub fn record(&mut self, tool_name: &str, args: &str, result: &str) -> bool {
        let sig = compute_signature(tool_name, args, result);
        self.window.push(StepRecord {
            signature: sig.clone(),
        });

        // Trim to window size.
        if self.window.len() > self.window_size {
            let drain = self.window.len() - self.window_size;
            self.window.drain(..drain);
        }

        // Count occurrences of the latest signature.
        let count = self
            .window
            .iter()
            .filter(|s| s.signature == sig)
            .count();

        count > self.max_repeats
    }

    /// Reset the detector (e.g. on new user turn).
    pub fn reset(&mut self) {
        self.window.clear();
    }

    /// Whether any steps have been recorded.
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// The nudge message injected when a loop is detected.
    pub fn nudge_message() -> &'static str {
        "You appear to be stuck in a loop — repeating the same tool call \
         with identical arguments and getting the same result. Stop and \
         reconsider your approach. Try a materially different strategy, \
         tool, or argument set. If you believe this is a false positive, \
         explain why and continue."
    }
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute a stable SHA-256 hex signature for a tool interaction.
fn compute_signature(tool_name: &str, args: &str, result: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\x00");
    // Normalize args by sorting JSON keys to avoid trivial differences
    // (whitespace, key order) producing different signatures for the same
    // logical call.
    let normalized_args = normalize_args(args);
    hasher.update(normalized_args.as_bytes());
    hasher.update(b"\x00");
    // Truncate result to avoid hashing megabytes; the first 512 chars are
    // enough to distinguish different results for loop detection.
    let result_sample: String = result.chars().take(512).collect();
    hasher.update(result_sample.as_bytes());
    hasher.update(b"\x00");
    let hash = hasher.finalize();
    // Hex-encode
    hash.iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

/// Normalize JSON args for stable comparison: parse, re-serialize with
/// sorted keys. Falls back to the raw string if parsing fails.
fn normalize_args(args: &str) -> String {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    // Try parsing as JSON and re-serializing canonically.
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Object(map)) => {
            // Sort keys canonically.
            let mut sorted: Vec<(String, serde_json::Value)> =
                map.into_iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::to_string(&serde_json::Value::Object(
                sorted.into_iter().collect(),
            ))
            .unwrap_or_else(|_| trimmed.to_string())
        }
        Ok(other) => serde_json::to_string(&other).unwrap_or_else(|_| trimmed.to_string()),
        Err(_) => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_loop_with_distinct_calls() {
        let mut d = LoopDetector::with_params(5, 3);
        for i in 0..10 {
            let args = format!(r#"{{"path": "file{}.txt"}}"#, i);
            assert!(!d.record("read_file", &args, "content"));
        }
    }

    #[test]
    fn detects_repeated_identical_calls() {
        let mut d = LoopDetector::with_params(10, 3);
        let args = r#"{"path": "same.txt"}"#;
        let result = "same content";
        // First 4 calls with identical args+result: 4 > 3 triggers.
        for _ in 0..4 {
            // Should only trigger on the 4th (count=4, max_repeats=3)
        }
        assert!(!d.record("read_file", args, result)); // count=1
        assert!(!d.record("read_file", args, result)); // count=2
        assert!(!d.record("read_file", args, result)); // count=3
        assert!(d.record("read_file", args, result));  // count=4 > 3 → LOOP
    }

    #[test]
    fn different_results_dont_loop() {
        let mut d = LoopDetector::with_params(10, 3);
        let args = r#"{"path": "counter.txt"}"#;
        for i in 0..20 {
            let result = format!("count = {}", i);
            assert!(!d.record("read_file", args, &result));
        }
    }

    #[test]
    fn different_args_dont_loop() {
        let mut d = LoopDetector::with_params(10, 3);
        let result = "same result";
        for i in 0..20 {
            let args = format!(r#"{{"query": "search{}"}}"#, i);
            assert!(!d.record("search_files", &args, result));
        }
    }

    #[test]
    fn reset_clears_history() {
        let mut d = LoopDetector::with_params(10, 3);
        let args = r#"{"path": "x"}"#;
        d.record("read_file", args, "y");
        d.record("read_file", args, "y");
        d.record("read_file", args, "y");
        d.reset();
        assert!(d.is_empty());
        // After reset, same call shouldn't immediately loop.
        assert!(!d.record("read_file", args, "y"));
    }

    #[test]
    fn window_evicts_old_entries() {
        let mut d = LoopDetector::with_params(5, 3);
        let args = r#"{"path": "old.txt"}"#;
        // Fill with repeats, then evict with distinct calls.
        for _ in 0..4 {
            d.record("read_file", args, "old");
        }
        // Now 5 distinct calls push the repeats out of the window.
        for i in 0..5 {
            let a = format!(r#"{{"path": "new{}.txt"}}"#, i);
            d.record("read_file", &a, "new");
        }
        // The old repeats are evicted; one more identical call won't loop.
        assert!(!d.record("read_file", args, "old"));
    }

    #[test]
    fn arg_key_order_normalized() {
        let mut d = LoopDetector::with_params(10, 2);
        let args_a = r#"{"path": "x.txt", "offset": 0}"#;
        let args_b = r#"{"offset": 0, "path": "x.txt"}"#;
        // Different key order, same logical args → should count as the same.
        assert!(!d.record("read_file", args_a, "content"));
        assert!(!d.record("read_file", args_b, "content"));
        assert!(d.record("read_file", args_a, "content")); // count=3 > 2
    }
}
