//! Tool input sanitization — port of crush's
//! `internal/agent/agent.go:951-967` sanitizeToolInput.
//!
//! Validates tool input JSON before execution. If the input is not valid
//! JSON (a common failure mode with smaller models), the tool result is
//! replaced with a helpful error instead of crashing.

use serde_json::Value;

/// Check if a tool call's arguments are valid JSON.
/// Returns (parsed_args, was_sanitized).
/// If sanitization occurs, returns (Null, true) so the caller can produce
/// the appropriate error result.
pub fn sanitize_tool_input(tool_name: &str, tool_call_id: &str, input: &str) -> (Value, bool) {
    match serde_json::from_str::<Value>(input) {
        Ok(parsed) => {
            // Even if parsed, verify it's an object (tool args must be objects).
            if parsed.is_object() || parsed.is_null() {
                (parsed, false)
            } else {
                // Valid JSON but not an object — treat as sanitized.
                (Value::Null, true)
            }
        }
        Err(_) => {
            // Invalid JSON — sanitized.
            (Value::Null, true)
        }
    }
}

/// Build the error result string for a sanitized (invalid JSON) tool call.
/// This matches crush's error message format.
pub fn sanitized_error_result(tool_name: &str, _tool_call_id: &str, raw_input: &str) -> String {
    // Show a preview of the invalid input for debugging.
    let preview = if raw_input.len() > 200 {
        format!("{}...", &raw_input[..200])
    } else {
        raw_input.to_string()
    };
    serde_json::json!({
        "error": format!(
            "Tool call failed: arguments were not valid JSON. \
The tool '{}' received malformed arguments. \
Please re-emit the tool call with valid JSON arguments.",
            tool_name
        ),
        "raw_input_preview": preview,
    })
    .to_string()
}

/// Validate that required parameters are present and have the right type.
/// Returns an error message if validation fails, None if OK.
pub fn validate_required_params(
    args: &Value,
    required: &[(&str, &str)],
) -> Option<String> {
    let obj = match args.as_object() {
        Some(o) => o,
        None => return Some("arguments must be a JSON object".to_string()),
    };
    for (param, expected_type) in required {
        match obj.get(*param) {
            None => {
                return Some(format!(
                    "missing required parameter '{}'. Expected type: {}.",
                    param, expected_type
                ));
            }
            Some(val) => {
                let actual_type = match val {
                    Value::String(_) => "string",
                    Value::Number(_) => "number",
                    Value::Bool(_) => "boolean",
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                    Value::Null => "null",
                };
                if *expected_type != actual_type && *expected_type != "any" {
                    return Some(format!(
                        "parameter '{}' must be {}, got {}.",
                        param, expected_type, actual_type
                    ));
                }
            }
        }
    }
    None
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_json() {
        let (parsed, sanitized) = sanitize_tool_input("read_file", "call_1", r#"{"path": "/tmp/test"}"#);
        assert!(!sanitized);
        assert_eq!(parsed["path"], "/tmp/test");
    }

    #[test]
    fn test_invalid_json() {
        let (_, sanitized) = sanitize_tool_input("read_file", "call_1", "not json");
        assert!(sanitized);
    }

    #[test]
    fn test_non_object_json() {
        let (_, sanitized) = sanitize_tool_input("read_file", "call_1", r#"[1, 2, 3]"#);
        assert!(sanitized);
    }

    #[test]
    fn test_empty_string() {
        let (_, sanitized) = sanitize_tool_input("read_file", "call_1", "");
        // Empty string is not valid JSON for object expectation,
        // but serde_json might parse empty as Null.
        assert!(sanitized);
    }

    #[test]
    fn test_sanitized_error_format() {
        let error = sanitized_error_result("read_file", "call_1", "garbage input");
        let parsed: Value = serde_json::from_str(&error).unwrap();
        assert!(parsed["error"].as_str().unwrap().contains("not valid JSON"));
        assert!(parsed["error"].as_str().unwrap().contains("read_file"));
    }

    #[test]
    fn test_validate_required_present() {
        let args = serde_json::json!({"path": "/tmp", "content": "hello"});
        assert!(validate_required_params(&args, &[("path", "string"), ("content", "string")]).is_none());
    }

    #[test]
    fn test_validate_required_missing() {
        let args = serde_json::json!({"path": "/tmp"});
        let err = validate_required_params(&args, &[("path", "string"), ("content", "string")]);
        assert!(err.is_some());
        assert!(err.unwrap().contains("content"));
    }

    #[test]
    fn test_validate_required_wrong_type() {
        let args = serde_json::json!({"path": 123});
        let err = validate_required_params(&args, &[("path", "string")]);
        assert!(err.is_some());
        assert!(err.unwrap().contains("string"));
    }
}
